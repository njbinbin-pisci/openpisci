/**
 * UIA Mouse Calibration — fullscreen overlay.
 *
 * This module renders the borderless, always-on-top, monitor-sized
 * window opened by `uia_calibration_open_overlay`. URL convention:
 *
 *     index.html?calibration=<monitor_index>
 *
 * Lifecycle:
 *   1. On mount, read `monitor_index` from the URL, call
 *      `uia_calibration_status()` to discover the monitor's physical
 *      rect + DPI, and render 5 numbered circles at relative positions
 *      (10/90 % corners + 50/50 % centre).
 *   2. User clicks each circle in order. For every click we capture
 *      the click event's CSS position, multiply by `devicePixelRatio`,
 *      and add the monitor's physical origin to get an *absolute*
 *      physical screen coordinate. That's the `user_target`.
 *   3. After click #5 we run a 3-second countdown so the user can lift
 *      their hand off the mouse, then invoke `uia_calibration_run_phase2`
 *      on the backend.
 *   4. Backend asks the UIA tool to click each of the same physical
 *      targets (with calibration bypassed) and reads `GetCursorPos`
 *      after each one. Progress streams back via the
 *      `uia_calibration_phase2_progress` event so we can dim each
 *      circle / show "Δ = N px" labels live.
 *   5. On completion we call `uia_calibration_finalize` to fit the
 *      linear model and persist it. The Tauri command tears down its
 *      own state; we just close the window via
 *      `uia_calibration_close_overlay()`.
 *
 * ESC at any point: cancels Phase 2 (if running) and closes the overlay.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  uiaCalibrationApi,
  UiaCalibrationClickSample,
  UiaCalibrationStatus,
} from "../../services/tauri/platform";

type Phase = "loading" | "user" | "countdown" | "pisci" | "saving" | "done" | "error";

interface CirclePlan {
  index: number;
  /** CSS px from window top-left. */
  cssX: number;
  cssY: number;
  /** Physical screen coords — `monitor.left + cssX * dpr`, etc. */
  physicalX: number;
  physicalY: number;
}

interface LiveSample extends UiaCalibrationClickSample {
  state: "pending" | "clicking" | "measured";
}

const RADIUS_PX = 28; // CSS px

function readMonitorIndexFromQuery(): number {
  const raw = new URLSearchParams(window.location.search).get("calibration");
  if (!raw) return 0;
  const n = parseInt(raw, 10);
  return Number.isFinite(n) ? n : 0;
}

export default function CalibrationOverlay() {
  const monitorIndex = useMemo(readMonitorIndexFromQuery, []);
  const dpr = window.devicePixelRatio || 1;

  const [phase, setPhase] = useState<Phase>("loading");
  const [errorMsg, setErrorMsg] = useState<string>("");
  const [status, setStatus] = useState<UiaCalibrationStatus | null>(null);
  const [userPoints, setUserPoints] = useState<[number, number][]>([]);
  // 3-second countdown after Phase 1 completes.
  const [countdown, setCountdown] = useState(3);
  // Live Pisci-click samples shown in the status area during Phase 2.
  const [samples, setSamples] = useState<LiveSample[]>([]);
  // Final residual RMS after fit; surfaces a green / amber banner.
  const [residualPx, setResidualPx] = useState<number | null>(null);
  const [savedAt, setSavedAt] = useState<string>("");
  const sampleCount = userPoints.length;

  // We need monitor info immediately. The status call is cheap.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const s = await uiaCalibrationApi.status();
        if (cancelled) return;
        if (monitorIndex >= s.monitors.length) {
          setErrorMsg(`monitor_index=${monitorIndex} out of range (have ${s.monitors.length})`);
          setPhase("error");
          return;
        }
        setStatus(s);
        setPhase("user");
      } catch (e) {
        if (cancelled) return;
        setErrorMsg(String(e));
        setPhase("error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [monitorIndex]);

  // ESC anywhere → abort + close.
  const cleanupAndClose = useCallback(async () => {
    try {
      await uiaCalibrationApi.cancelPhase2();
    } catch {
      /* noop */
    }
    try {
      await uiaCalibrationApi.closeOverlay();
    } catch {
      /* noop */
    }
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        cleanupAndClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cleanupAndClose]);

  // Compute the five circle positions once the status loads. We use
  // window.innerWidth / innerHeight (CSS px) so the relative layout
  // matches whatever monitor we're on regardless of DPI; the physical
  // screen coords are derived from the monitor rect + dpr.
  const circles: CirclePlan[] = useMemo(() => {
    if (!status) return [];
    const monitor = status.monitors[monitorIndex];
    if (!monitor) return [];
    const [monL, monT] = monitor.rect;
    const w = window.innerWidth;
    const h = window.innerHeight;
    const rel: Array<[number, number]> = [
      [0.1, 0.1], // 1: top-left
      [0.9, 0.1], // 2: top-right
      [0.1, 0.9], // 3: bottom-left
      [0.9, 0.9], // 4: bottom-right
      [0.5, 0.5], // 5: centre
    ];
    return rel.map(([fx, fy], i) => {
      const cssX = Math.round(fx * w);
      const cssY = Math.round(fy * h);
      // PerMonitorV2 means devicePixelRatio matches the monitor's
      // effective DPI scale, so converting CSS->physical with `* dpr`
      // gives the exact pixel the OS will hit-test against.
      return {
        index: i,
        cssX,
        cssY,
        physicalX: monL + Math.round(cssX * dpr),
        physicalY: monT + Math.round(cssY * dpr),
      };
    });
  }, [status, monitorIndex, dpr]);

  // Phase 1 click handling: only register a click when it's close
  // enough to the *next* target circle. Distance threshold is a few
  // times the circle radius so noisy clicks far off the mark don't
  // pollute the dataset.
  const handleClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (phase !== "user") return;
      const nextIdx = userPoints.length;
      if (nextIdx >= circles.length) return;
      const target = circles[nextIdx];
      const cssX = e.clientX;
      const cssY = e.clientY;
      const dx = cssX - target.cssX;
      const dy = cssY - target.cssY;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist > RADIUS_PX * 2.5) {
        // Tell the user to retry but don't advance.
        return;
      }
      const physicalX = (status?.monitors[monitorIndex]?.rect[0] ?? 0) + Math.round(cssX * dpr);
      const physicalY = (status?.monitors[monitorIndex]?.rect[1] ?? 0) + Math.round(cssY * dpr);
      const next = [...userPoints, [physicalX, physicalY] as [number, number]];
      setUserPoints(next);
      if (next.length === circles.length) {
        setPhase("countdown");
      }
    },
    [phase, userPoints, circles, status, monitorIndex, dpr],
  );

  // Phase 1 → Countdown → Phase 2
  useEffect(() => {
    if (phase !== "countdown") return;
    setCountdown(3);
    let n = 3;
    const id = setInterval(() => {
      n -= 1;
      setCountdown(n);
      if (n <= 0) {
        clearInterval(id);
        // Pre-seed the samples list so the user sees 5 pending slots.
        setSamples(
          userPoints.map((pt) => ({
            target: pt,
            actual: null,
            distance_px: null,
            error: null,
            state: "pending",
          })),
        );
        setPhase("pisci");
      }
    }, 1000);
    return () => clearInterval(id);
  }, [phase, userPoints]);

  // Listen to streaming progress events from the backend.
  useEffect(() => {
    let unlistenFn: (() => void) | null = null;
    listen<{
      index: number;
      total: number;
      phase: string;
      target?: [number, number];
      actual?: [number, number];
      distance_px?: number;
    }>("uia_calibration_phase2_progress", (e) => {
      const { index, phase: subphase, actual, distance_px } = e.payload;
      setSamples((prev) => {
        const copy = prev.slice();
        if (!copy[index]) return prev;
        if (subphase === "clicking") {
          copy[index] = { ...copy[index], state: "clicking" };
        } else if (subphase === "measured") {
          copy[index] = {
            ...copy[index],
            state: "measured",
            actual: actual ?? null,
            distance_px: distance_px ?? null,
          };
        }
        return copy;
      });
    }).then((fn) => {
      unlistenFn = fn;
    });
    return () => {
      if (unlistenFn) unlistenFn();
    };
  }, []);

  // Phase 2 driver — fires the backend run, then transitions to saving.
  useEffect(() => {
    if (phase !== "pisci") return;
    let cancelled = false;
    (async () => {
      try {
        const result = await uiaCalibrationApi.runPhase2(monitorIndex, userPoints);
        if (cancelled) return;
        if (result.cancelled) {
          setErrorMsg("已被用户取消 / Cancelled by user");
          setPhase("error");
          return;
        }
        if (result.timed_out) {
          setErrorMsg("Phase 2 超时 (180s) / timeout");
          setPhase("error");
          return;
        }
        // Persist the fit. Only include samples where we got a real
        // actual reading; if too few survived, refuse to save.
        const survivors = result.samples.filter((s) => s.actual !== null);
        if (survivors.length < 2) {
          setErrorMsg(
            `Phase 2 收集到的样本不足 (${survivors.length}/${userPoints.length})，无法拟合。`,
          );
          setPhase("error");
          return;
        }
        const fitTargets = survivors.map((s) => s.target);
        const fitActuals = survivors.map((s) => s.actual as [number, number]);
        setPhase("saving");
        const finalize = await uiaCalibrationApi.finalize(
          monitorIndex,
          fitTargets,
          fitActuals,
        );
        if (cancelled) return;
        setResidualPx(finalize.monitor.residual_rms_px);
        setSavedAt(finalize.monitor.calibrated_at);
        setPhase("done");
        // Auto-close the overlay after a few seconds so the user
        // doesn't have to click anything. The status banner still
        // remains visible long enough to read.
        setTimeout(() => {
          uiaCalibrationApi.closeOverlay().catch(() => {});
        }, 3500);
      } catch (e) {
        if (cancelled) return;
        setErrorMsg(String(e));
        setPhase("error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [phase, monitorIndex, userPoints]);

  // ─── Render ────────────────────────────────────────────────────

  return (
    <div
      onClick={handleClick}
      style={{
        position: "fixed",
        inset: 0,
        background: "#000",
        cursor: phase === "user" ? "crosshair" : "default",
        color: "#fff",
        userSelect: "none",
        fontFamily: "Inter, system-ui, sans-serif",
        overflow: "hidden",
      }}
    >
      {/* Circles */}
      {circles.map((c) => {
        const clicked = c.index < userPoints.length;
        const isNext = phase === "user" && c.index === userPoints.length;
        const isPisciTarget = phase === "pisci" && samples[c.index]?.state !== "pending";
        const sample = samples[c.index];
        const measured = sample?.state === "measured";

        return (
          <div
            key={c.index}
            style={{
              position: "absolute",
              left: c.cssX - RADIUS_PX,
              top: c.cssY - RADIUS_PX,
              width: RADIUS_PX * 2,
              height: RADIUS_PX * 2,
              borderRadius: "50%",
              border: `3px solid ${
                clicked && !measured
                  ? "#9ca3af"
                  : measured && (sample?.distance_px ?? 99) < 5
                  ? "#22c55e"
                  : measured
                  ? "#f59e0b"
                  : isNext
                  ? "#22d3ee"
                  : isPisciTarget
                  ? "#a78bfa"
                  : "#475569"
              }`,
              background:
                clicked && !measured
                  ? "rgba(156,163,175,0.15)"
                  : isNext
                  ? "rgba(34,211,238,0.18)"
                  : measured
                  ? "rgba(34,197,94,0.18)"
                  : "transparent",
              boxShadow: isNext ? "0 0 16px rgba(34,211,238,0.6)" : "none",
              transition: "all 120ms ease",
              pointerEvents: "none",
            }}
          >
            <div
              style={{
                position: "absolute",
                top: -28,
                left: "50%",
                transform: "translateX(-50%)",
                fontSize: 16,
                fontWeight: 700,
                color: "#e5e7eb",
              }}
            >
              {c.index + 1}
            </div>
            {/* Centre dot — visual target */}
            <div
              style={{
                position: "absolute",
                left: "50%",
                top: "50%",
                width: 4,
                height: 4,
                borderRadius: "50%",
                background: "#fff",
                transform: "translate(-50%, -50%)",
              }}
            />
            {measured && sample?.actual && (
              <div
                style={{
                  position: "absolute",
                  top: RADIUS_PX * 2 + 6,
                  left: "50%",
                  transform: "translateX(-50%)",
                  fontSize: 12,
                  color: (sample.distance_px ?? 99) < 5 ? "#22c55e" : "#f59e0b",
                  whiteSpace: "nowrap",
                }}
              >
                Δ {Math.round(sample.distance_px ?? 0)}px
              </div>
            )}
          </div>
        );
      })}

      {/* Status banner — positioned between circle #5 (50%) and bottom
          edge so it never overlaps any target. */}
      <div
        style={{
          position: "absolute",
          left: "50%",
          top: "73%",
          transform: "translateX(-50%)",
          minWidth: "min(640px, 80vw)",
          maxWidth: "80vw",
          background: "rgba(15, 23, 42, 0.85)",
          border: "1px solid rgba(148, 163, 184, 0.3)",
          padding: "16px 20px",
          borderRadius: 10,
          textAlign: "center",
          backdropFilter: "blur(8px)",
        }}
      >
        <StatusContent
          phase={phase}
          monitorIndex={monitorIndex}
          status={status}
          clickedCount={userPoints.length}
          totalCircles={5}
          countdown={countdown}
          samples={samples}
          residualPx={residualPx}
          savedAt={savedAt}
          errorMsg={errorMsg}
          sampleCount={sampleCount}
        />
        <div
          style={{
            marginTop: 12,
            fontSize: 11,
            color: "rgba(203, 213, 225, 0.7)",
          }}
        >
          按 ESC 取消 · Press ESC to cancel
        </div>
      </div>
    </div>
  );
}

function StatusContent({
  phase,
  monitorIndex,
  status,
  clickedCount,
  totalCircles,
  countdown,
  samples,
  residualPx,
  savedAt,
  errorMsg,
  sampleCount,
}: {
  phase: Phase;
  monitorIndex: number;
  status: UiaCalibrationStatus | null;
  clickedCount: number;
  totalCircles: number;
  countdown: number;
  samples: LiveSample[];
  residualPx: number | null;
  savedAt: string;
  errorMsg: string;
  sampleCount: number;
}) {
  const monitor = status?.monitors[monitorIndex];
  const monitorLabel = monitor
    ? `Monitor ${monitor.index} (${monitor.rect[2] - monitor.rect[0]}×${
        monitor.rect[3] - monitor.rect[1]
      } @ ${monitor.scale_percent}%)`
    : `Monitor #${monitorIndex}`;

  if (phase === "loading") {
    return <div>Loading monitor info…</div>;
  }
  if (phase === "error") {
    return (
      <div style={{ color: "#f87171" }}>
        ⚠ {errorMsg || "Unknown error"}
      </div>
    );
  }
  if (phase === "user") {
    return (
      <>
        <div style={{ fontSize: 18, fontWeight: 600 }}>
          UIA 鼠标精度校准 · UIA Mouse Calibration
        </div>
        <div style={{ marginTop: 6, fontSize: 13, color: "#cbd5e1" }}>
          {monitorLabel}
        </div>
        <div style={{ marginTop: 12, fontSize: 14 }}>
          请按顺序点击每个圆圈中心 · Click the centre of each circle in order:
          <strong> {clickedCount + 1}</strong> / {totalCircles}
        </div>
      </>
    );
  }
  if (phase === "countdown") {
    return (
      <>
        <div style={{ fontSize: 16, fontWeight: 600, color: "#22c55e" }}>
          ✓ 用户校准完成 · User calibration complete
        </div>
        <div style={{ marginTop: 8, fontSize: 14, color: "#fef08a" }}>
          请勿移动鼠标 · DO NOT touch mouse — Pisci 将在 {countdown} 秒后开始
        </div>
      </>
    );
  }
  if (phase === "pisci") {
    const measured = samples.filter((s) => s.state === "measured").length;
    const current = samples.findIndex((s) => s.state === "clicking");
    return (
      <>
        <div style={{ fontSize: 16, fontWeight: 600 }}>
          Pisci 正在测试点击 · Pisci is clicking… ({measured}/{sampleCount})
        </div>
        {current >= 0 && (
          <div style={{ marginTop: 6, fontSize: 13, color: "#a78bfa" }}>
            正在点击 #{current + 1}…
          </div>
        )}
        <div style={{ marginTop: 8, fontSize: 12, color: "#cbd5e1" }}>
          请不要移动鼠标 · Do not move the mouse
        </div>
      </>
    );
  }
  if (phase === "saving") {
    return <div>计算线性拟合中 · Computing linear fit…</div>;
  }
  if (phase === "done") {
    const okThreshold = 3;
    const goodFit = residualPx !== null && residualPx < okThreshold;
    return (
      <>
        <div
          style={{
            fontSize: 18,
            fontWeight: 700,
            color: goodFit ? "#22c55e" : "#f59e0b",
          }}
        >
          {goodFit ? "✓ 校准已保存" : "⚠ 残差偏大"} · Calibration{" "}
          {goodFit ? "saved" : "saved (high residual)"}
        </div>
        <div style={{ marginTop: 8, fontSize: 13, color: "#cbd5e1" }}>
          残差 RMS · Residual RMS:{" "}
          <strong>{residualPx !== null ? residualPx.toFixed(2) : "—"} px</strong>
        </div>
        {savedAt && (
          <div style={{ marginTop: 4, fontSize: 11, color: "#94a3b8" }}>
            {new Date(savedAt).toLocaleString()}
          </div>
        )}
        {!goodFit && (
          <div style={{ marginTop: 8, fontSize: 12, color: "#fbbf24" }}>
            如仍有偏差，请重试或检查显示器 / DPI 设置
          </div>
        )}
      </>
    );
  }
  return null;
}
