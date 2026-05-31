import type { ReactNode } from "react";
import type { UiBlock } from "./protocol";

export function ImageBlock({ block }: { block: UiBlock }) {
  if (!block.url) return null;
  return (
    <figure className="ic-image-wrap">
      <img className="ic-image" src={block.url} alt={block.alt || block.label || ""} loading="lazy" />
      {block.label && <figcaption className="ic-image-caption">{block.label}</figcaption>}
    </figure>
  );
}

export function CodePreviewBlock({ block }: { block: UiBlock }) {
  const lang = block.language || "text";
  return (
    <div className="ic-code-preview">
      {block.label && <div className="ic-code-label">{block.label}</div>}
      <pre className="ic-code-pre">
        <code className={`language-${lang}`}>{block.content || ""}</code>
      </pre>
    </div>
  );
}

export function ProgressBlock({
  block,
  value,
}: {
  block: UiBlock;
  value: number;
}) {
  const max = typeof block.max === "number" ? block.max : 1;
  const min = typeof block.min === "number" ? block.min : 0;
  const pct = max > min ? Math.min(100, Math.max(0, ((value - min) / (max - min)) * 100)) : 0;
  return (
    <div className="ic-field ic-progress">
      {block.label && (
        <div className="ic-label ic-progress-label">
          {block.label}
          <span className="ic-progress-value">{value}{max !== 1 ? ` / ${max}` : ""}</span>
        </div>
      )}
      <div className="ic-progress-track" role="progressbar" aria-valuenow={value} aria-valuemin={min} aria-valuemax={max}>
        <div className="ic-progress-fill" style={{ width: `${pct}%` }} />
      </div>
      {block.description && <p className="ic-field-hint">{block.description}</p>}
    </div>
  );
}

export function LinkListBlock({
  block,
  value,
  onSelect,
  disabled,
}: {
  block: UiBlock;
  value: string;
  onSelect: (v: string) => void;
  disabled: boolean;
}) {
  const items = block.options ?? [];
  return (
    <div className="ic-field ic-link-list">
      {block.label && <div className="ic-label">{block.label}</div>}
      {block.description && <p className="ic-field-hint">{block.description}</p>}
      <ul className="ic-link-list-items">
        {items.map((opt) => (
          <li key={opt.value}>
            <button
              type="button"
              className={`ic-link-item${value === opt.value ? " ic-link-item-selected" : ""}`}
              disabled={disabled}
              onClick={() => onSelect(opt.value)}
            >
              <span className="ic-link-item-label">{opt.label}</span>
              {opt.description && <span className="ic-link-item-desc">{opt.description}</span>}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function FilePickerBlock({
  block,
  value,
  onChange,
  disabled,
  error,
}: {
  block: UiBlock;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
  error?: string;
}) {
  const pick = async () => {
    if (disabled) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: !!block.multiple,
        filters: block.accept
          ? [{ name: block.accept, extensions: block.accept.split(",").map((x) => x.trim().replace(/^\./, "")) }]
          : undefined,
      });
      if (typeof selected === "string") onChange(selected);
      else if (Array.isArray(selected) && selected[0]) onChange(selected[0]);
    } catch (e) {
      console.error("[FilePickerBlock]", e);
    }
  };

  return (
    <div className={`ic-field${error ? " ic-field-error" : ""}`}>
      {block.label && <label className="ic-label">{block.label}</label>}
      <div className="ic-file-picker-row">
        <input type="text" className="ic-input" readOnly value={value} placeholder={block.placeholder || ""} />
        <button type="button" className="ic-btn ic-btn-default" onClick={pick} disabled={disabled}>
          Browse…
        </button>
      </div>
      {error && <span className="ic-error">{error}</span>}
    </div>
  );
}

export function LayoutBlock({
  block,
  renderChild,
}: {
  block: UiBlock;
  renderChild: (child: UiBlock, index: number) => ReactNode;
}) {
  const children = block.blocks ?? [];
  const className =
    block.type === "row"
      ? "ic-row"
      : block.type === "column"
        ? "ic-column"
        : "ic-card";

  return (
    <div className={className}>
      {block.type === "card" && block.label && <div className="ic-card-title">{block.label}</div>}
      {block.type === "card" && block.description && <p className="ic-card-desc">{block.description}</p>}
      <div className={block.type === "row" ? "ic-row-inner" : block.type === "column" ? "ic-column-inner" : "ic-card-inner"}>
        {children.map(renderChild)}
      </div>
    </div>
  );
}
