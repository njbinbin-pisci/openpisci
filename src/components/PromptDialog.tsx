import { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";

interface PromptDialogProps {
  open: boolean;
  title: string;
  /** Optional hint below the title */
  message?: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  loading?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Themed single-field input dialog — same overlay/chrome as ConfirmDialog. */
export default function PromptDialog({
  open,
  title,
  message,
  value,
  onChange,
  placeholder,
  confirmLabel,
  cancelLabel,
  loading = false,
  error,
  onConfirm,
  onCancel,
}: PromptDialogProps) {
  const { t } = useTranslation();
  const inputRef = useRef<HTMLInputElement>(null);
  const resolvedConfirm = confirmLabel ?? t("common.confirm");
  const resolvedCancel = cancelLabel ?? t("common.cancel");

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (!open || loading) return;
      if (e.key === "Escape") onCancel();
      if (e.key === "Enter" && value.trim()) onConfirm();
    },
    [open, loading, onCancel, onConfirm, value],
  );

  useEffect(() => {
    if (!open) return;
    document.addEventListener("keydown", handleKeyDown);
    const timer = window.setTimeout(() => inputRef.current?.focus(), 0);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      window.clearTimeout(timer);
    };
  }, [open, handleKeyDown]);

  if (!open) return null;

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
          maxWidth: 420,
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
            marginBottom: message ? 8 : 14,
          }}
        >
          {title}
        </div>
        {message ? (
          <div
            style={{
              fontSize: 13,
              color: "var(--text-secondary)",
              marginBottom: 14,
              lineHeight: 1.5,
            }}
          >
            {message}
          </div>
        ) : null}
        <input
          ref={inputRef}
          type="text"
          className="input"
          value={value}
          placeholder={placeholder}
          disabled={loading}
          onChange={(e) => onChange(e.target.value)}
          style={{ width: "100%", marginBottom: error ? 8 : 18 }}
        />
        {error ? (
          <div style={{ fontSize: 12, color: "#dc3545", marginBottom: 14 }}>{error}</div>
        ) : null}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button
            type="button"
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
          <button
            type="button"
            onClick={onConfirm}
            disabled={loading || !value.trim()}
            style={{
              padding: "6px 16px",
              fontSize: 13,
              fontWeight: 600,
              border: "none",
              borderRadius: 6,
              cursor: loading || !value.trim() ? "default" : "pointer",
              opacity: loading || !value.trim() ? 0.6 : 1,
              background: "var(--accent)",
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
