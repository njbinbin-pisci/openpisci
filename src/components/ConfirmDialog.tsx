import { useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** "danger" (red) or "primary" (accent) — defaults to "danger" */
  variant?: "danger" | "primary";
  /** Alert mode: single OK button, no cancel */
  alert?: boolean;
  loading?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  cancelLabel,
  variant = "danger",
  alert = false,
  loading = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const { t } = useTranslation();
  const resolvedConfirm = confirmLabel ?? t("common.confirm");
  const resolvedCancel = cancelLabel ?? t("common.cancel");
  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape" && !loading) onCancel();
    },
    [loading, onCancel],
  );

  useEffect(() => {
    if (!open) return;
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open, handleKeyDown]);

  if (!open) return null;

  const isDanger = variant === "danger";

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 9999,
        background: "rgba(0,0,0,0.45)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
      onClick={() => !loading && onCancel()}
    >
      <div
        style={{
          background: "var(--bg-primary)",
          borderRadius: 12,
          padding: "24px 28px",
          maxWidth: 400,
          width: "90%",
          boxShadow: "0 8px 32px rgba(0,0,0,0.3)",
          border: "1px solid var(--border)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          style={{
            fontSize: 15,
            fontWeight: 600,
            color: "var(--text-primary)",
            marginBottom: 10,
          }}
        >
          {title}
        </div>
        <div
          style={{
            fontSize: 13,
            color: "var(--text-secondary)",
            marginBottom: 20,
            lineHeight: 1.5,
          }}
        >
          {message}
        </div>
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          {!alert && (
            <button
              onClick={onCancel}
              disabled={loading}
              style={{
                padding: "6px 16px",
                fontSize: 13,
                background: "var(--bg-secondary)",
                border: "1px solid var(--border)",
                borderRadius: 6,
                color: "var(--text-secondary)",
                cursor: loading ? "default" : "pointer",
              }}
            >
              {resolvedCancel}
            </button>
          )}
          <button
            onClick={onConfirm}
            disabled={loading}
            style={{
              padding: "6px 16px",
              fontSize: 13,
              fontWeight: 600,
              border: "none",
              borderRadius: 6,
              cursor: loading ? "default" : "pointer",
              opacity: loading ? 0.6 : 1,
              background: isDanger ? "#dc3545" : "var(--accent)",
              color: "#fff",
            }}
          >
            {loading ? "..." : resolvedConfirm}
          </button>
        </div>
      </div>
    </div>
  );
}
