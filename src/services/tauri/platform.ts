/**
 * Tauri IPC — platform domain.
 *
 * Host / OS primitives: runtime & VM capability probing, window / overlay /
 * theme control, and UI-side resolution of permission + interactive-UI
 * prompts. Plus the cross-platform `openPath` helper.
 *
 * Mirrors Rust-side `src-tauri/src/commands/platform/*`.
 */
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// System / Runtimes
// ---------------------------------------------------------------------------

export interface RuntimeCheckItem {
  name: string;
  available: boolean;
  version: string | null;
  download_url: string;
  hint: string;
}

export interface SystemDependencyItem {
  key: string;
  name: string;
  feature: string;
  available: boolean;
  required: boolean;
  status: "ok" | "warning" | "missing";
  details: string | null;
  hint: string;
  remediation: string | null;
  action: SystemDependencyAction | null;
}

export interface SystemDependencyAction {
  kind: "install_command" | "open_url" | "open_settings";
  command: string | null;
  url: string | null;
}

export interface PrivilegeElevationCheckItem {
  key: string;
  name: string;
  available: boolean;
  required: boolean;
  status: "ok" | "warning" | "missing";
  details: string | null;
  hint: string;
  remediation: string | null;
  action: SystemDependencyAction | null;
}

export const systemApi = {
  getVmStatus: () =>
    invoke<{ backend: string; available: boolean; description: string }>("get_vm_status"),
  checkRuntimes: () => invoke<RuntimeCheckItem[]>("check_runtimes"),
  checkSystemDependencies: () =>
    invoke<SystemDependencyItem[]>("check_system_dependencies"),
  checkPrivilegeElevation: () =>
    invoke<PrivilegeElevationCheckItem[]>("check_privilege_elevation"),
  runSystemDependencyAction: (key: string) =>
    invoke<void>("run_system_dependency_action", { key }),
  setRuntimePath: (runtimeKey: string, exePath: string) =>
    invoke<RuntimeCheckItem[]>("set_runtime_path", { runtimeKey, exePath }),
};

// ---------------------------------------------------------------------------
// Window / Overlay / Theme
// ---------------------------------------------------------------------------

export const windowApi = {
  enterMinimalMode: () => invoke<void>("enter_minimal_mode"),
  exitMinimalMode: () => invoke<void>("exit_minimal_mode"),
  quitApp: () => invoke<void>("quit_app"),
  setOverlayPosition: (x: number, y: number) =>
    invoke<void>("set_overlay_position", { x, y }),
  saveOverlayPosition: (x: number, y: number) =>
    invoke<void>("save_overlay_position", { x, y }),
  setThemeBorder: (theme: "violet" | "gold") =>
    invoke<void>("set_window_theme_border", { theme }),
};

// ---------------------------------------------------------------------------
// Permission prompts (confirmation gates)
// ---------------------------------------------------------------------------

export const permissionApi = {
  respond: (requestId: string, approved: boolean) =>
    invoke<void>('respond_permission', { requestId, approved }),
};

// ---------------------------------------------------------------------------
// Interactive UI (chat_ui tool responses)
// ---------------------------------------------------------------------------

export const interactiveApi = {
  respond: (requestId: string, values: Record<string, unknown>) =>
    invoke<void>('respond_interactive_ui', { requestId, values }),
};

// ---------------------------------------------------------------------------
// UIA mouse-precision calibration (Windows only)
// ---------------------------------------------------------------------------

export interface MonitorSnapshot {
  index: number;
  primary: boolean;
  /** [left, top, right, bottom] physical pixels. */
  rect: [number, number, number, number];
  dpi_x: number;
  dpi_y: number;
  scale_percent: number;
  device: string;
}

export interface MonitorCalibration {
  monitor_index: number;
  monitor_rect: [number, number, number, number];
  scale_x: number;
  offset_x: number;
  scale_y: number;
  offset_y: number;
  residual_rms_px: number;
  sample_count: number;
  calibrated_at: string;
}

export interface UiaCalibrationStatus {
  virtual_screen: [number, number, number, number];
  monitors: MonitorSnapshot[];
  fingerprint: string;
  is_valid: boolean;
  monitors_calibrated: MonitorCalibration[];
  file_path: string;
}

export interface UiaCalibrationOverlayInfo {
  monitor_index: number;
  monitor_rect: [number, number, number, number];
  dpi_x: number;
  dpi_y: number;
  scale_percent: number;
}

export interface UiaCalibrationClickSample {
  target: [number, number];
  actual: [number, number] | null;
  distance_px: number | null;
  error: string | null;
}

export interface UiaCalibrationPhase2Result {
  samples: UiaCalibrationClickSample[];
  cancelled: boolean;
  timed_out: boolean;
  elapsed_ms: number;
}

export interface UiaCalibrationFinalizeResult {
  monitor: MonitorCalibration;
  file_path: string;
}

export const uiaCalibrationApi = {
  status: () => invoke<UiaCalibrationStatus>("uia_calibration_status"),
  clear: () => invoke<void>("uia_calibration_clear"),
  openOverlay: (monitorIndex: number) =>
    invoke<UiaCalibrationOverlayInfo>("uia_calibration_open_overlay", {
      monitorIndex,
    }),
  closeOverlay: () => invoke<void>("uia_calibration_close_overlay"),
  runPhase2: (monitorIndex: number, userPoints: [number, number][]) =>
    invoke<UiaCalibrationPhase2Result>("uia_calibration_run_phase2", {
      request: { monitor_index: monitorIndex, user_points: userPoints },
    }),
  cancelPhase2: () => invoke<void>("uia_calibration_cancel_phase2"),
  finalize: (
    monitorIndex: number,
    userPoints: [number, number][],
    pisciActuals: [number, number][],
  ) =>
    invoke<UiaCalibrationFinalizeResult>("uia_calibration_finalize", {
      request: {
        monitor_index: monitorIndex,
        user_points: userPoints,
        pisci_actuals: pisciActuals,
      },
    }),
};

// ---------------------------------------------------------------------------
// File / Path utilities
// ---------------------------------------------------------------------------

/**
 * Open a local file or directory with the system default application.
 * On Windows, directories are opened with Explorer.exe directly,
 * which is more reliable than shell.open() for folder paths.
 */
export function openPath(path: string): Promise<void> {
  return invoke<void>("open_path", { path });
}
