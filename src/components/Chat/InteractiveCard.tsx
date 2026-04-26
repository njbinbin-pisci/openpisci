import { useState, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { interactiveApi, koiApi, poolApi } from "../../services/tauri";
import type { KoiDefinition, PoolSession } from "../../services/tauri";
import "./InteractiveCard.css";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface UiBlock {
  type: string;
  id?: string;
  label?: string;
  value?: unknown;
  content?: string;
  options?: { value: string; label: string; description?: string }[];
  default?: unknown;
  placeholder?: string;
  show_when?: { field: string; equals: string };
  suggestions?: string[];
  allow_new?: boolean;
  min?: number;
  max?: number;
  step?: number;
  buttons?: UiButton[];
}

interface UiButton {
  id?: string;
  label: string;
  value?: unknown;
  style?: string;
}

interface UiDefinition {
  title?: string;
  description?: string;
  blocks: UiBlock[];
}

interface InteractiveCardProps {
  requestId: string;
  uiDefinition: UiDefinition;
  /** If set, card is read-only (already submitted) */
  submittedValues?: Record<string, unknown> | null;
}

// ---------------------------------------------------------------------------
// Sub-components for each block type
// ---------------------------------------------------------------------------

function TextBlock({ block }: { block: UiBlock }) {
  return <p className="ic-text">{block.content || ""}</p>;
}

function RadioBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  return (
    <fieldset className="ic-fieldset">
      {block.label && <legend className="ic-legend">{block.label}</legend>}
      <div className="ic-radio-group">
        {(block.options || []).map((opt) => (
          <label key={opt.value} className={`ic-radio-item${value === opt.value ? " ic-selected" : ""}`}>
            <input
              type="radio"
              name={block.id}
              value={opt.value}
              checked={value === opt.value}
              onChange={() => onChange(opt.value)}
              disabled={disabled}
            />
            <span className="ic-radio-label">{opt.label}</span>
            {opt.description && <span className="ic-radio-desc">{opt.description}</span>}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function CheckboxBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string[];
  onChange: (v: string[]) => void;
  disabled: boolean;
}) {
  const toggle = (v: string) => {
    onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v]);
  };
  return (
    <fieldset className="ic-fieldset">
      {block.label && <legend className="ic-legend">{block.label}</legend>}
      <div className="ic-checkbox-group">
        {(block.options || []).map((opt) => (
          <label key={opt.value} className={`ic-checkbox-item${value.includes(opt.value) ? " ic-selected" : ""}`}>
            <input
              type="checkbox"
              checked={value.includes(opt.value)}
              onChange={() => toggle(opt.value)}
              disabled={disabled}
            />
            <span className="ic-checkbox-label">{opt.label}</span>
            {opt.description && <span className="ic-checkbox-desc">{opt.description}</span>}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function TextInputBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  return (
    <div className="ic-field">
      {block.label && <label className="ic-label">{block.label}</label>}
      <input
        type="text"
        className="ic-input"
        value={value}
        placeholder={block.placeholder || ""}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
      />
    </div>
  );
}

function NumberInputBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: number;
  onChange: (v: number) => void;
  disabled: boolean;
}) {
  return (
    <div className="ic-field">
      {block.label && <label className="ic-label">{block.label}</label>}
      <input
        type="number"
        className="ic-input"
        value={Number.isFinite(value) ? value : 0}
        min={block.min}
        max={block.max}
        step={block.step ?? 1}
        placeholder={block.placeholder || ""}
        onChange={(e) => {
          const next = Number(e.target.value);
          onChange(Number.isFinite(next) ? next : 0);
        }}
        disabled={disabled}
      />
    </div>
  );
}

function SelectBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  return (
    <div className="ic-field">
      {block.label && <label className="ic-label">{block.label}</label>}
      <select className="ic-select" value={value} onChange={(e) => onChange(e.target.value)} disabled={disabled}>
        <option value="">{block.placeholder || "-- select --"}</option>
        {(block.options || []).map((opt) => (
          <option key={opt.value} value={opt.value}>{opt.label}</option>
        ))}
      </select>
    </div>
  );
}

function KoiPickerBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string[];
  onChange: (v: string[]) => void;
  disabled: boolean;
}) {
  const [kois, setKois] = useState<KoiDefinition[]>([]);
  useEffect(() => {
    koiApi.list().then(setKois).catch(() => {});
  }, []);

  const toggle = (id: string) => {
    onChange(value.includes(id) ? value.filter((x) => x !== id) : [...value, id]);
  };

  const suggested = new Set(block.suggestions || []);

  const sorted = useMemo(() => {
    return [...kois].sort((a, b) => {
      const aS = suggested.has(a.id) ? 0 : 1;
      const bS = suggested.has(b.id) ? 0 : 1;
      return aS - bS;
    });
  }, [kois, suggested]);

  return (
    <fieldset className="ic-fieldset">
      {block.label && <legend className="ic-legend">{block.label}</legend>}
      <div className="ic-koi-grid">
        {sorted.map((k) => (
          <button
            key={k.id}
            type="button"
            className={`ic-koi-card${value.includes(k.id) ? " ic-koi-selected" : ""}`}
            onClick={() => !disabled && toggle(k.id)}
            disabled={disabled}
            style={{ borderColor: value.includes(k.id) ? k.color : undefined }}
          >
            <span className="ic-koi-icon" style={{ background: k.color }}>{k.icon}</span>
            <span className="ic-koi-name">{k.name}</span>
            <span className="ic-koi-desc">{k.description.slice(0, 40)}{k.description.length > 40 ? "..." : ""}</span>
            {suggested.has(k.id) && <span className="ic-koi-badge">Recommended</span>}
          </button>
        ))}
        {kois.length === 0 && <span className="ic-muted">No Koi available</span>}
      </div>
    </fieldset>
  );
}

function ProjectPickerBlock({
  block,
  value,
  onChange,
  disabled,
}: {
  block: UiBlock;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const [projects, setProjects] = useState<PoolSession[]>([]);
  useEffect(() => {
    poolApi.listSessions().then(setProjects).catch(() => {});
  }, []);

  const statusIcon = (s: string) =>
    s === "active" ? "🟢" : s === "paused" ? "🟡" : "⚪";

  return (
    <div className="ic-field">
      {block.label && <label className="ic-label">{block.label}</label>}
      <select className="ic-select" value={value} onChange={(e) => onChange(e.target.value)} disabled={disabled}>
        {block.allow_new && <option value="__new__">+ New project</option>}
        {projects.map((p) => (
          <option key={p.id} value={p.id}>
            {statusIcon(p.status)} {p.name} [{p.status}]
          </option>
        ))}
        {projects.length === 0 && !block.allow_new && <option value="">No projects</option>}
      </select>
    </div>
  );
}

function ActionsBlock({
  block,
  onAction,
  disabled,
}: {
  block: UiBlock;
  onAction: (block: UiBlock, button: UiButton) => void;
  disabled: boolean;
}) {
  const buttons = block.buttons?.length
    ? block.buttons
    : [{
        id: block.id,
        label: block.label || "Submit",
        value: block.value ?? block.id ?? block.label ?? "submit",
        style: "primary",
      }];

  return (
    <div className="ic-actions">
      {buttons.map((btn, index) => (
        <button
          key={btn.id || `${String(btn.value ?? btn.label)}-${index}`}
          type="button"
          className={`ic-btn ic-btn-${btn.style || "default"}`}
          onClick={() => onAction(block, btn)}
          disabled={disabled}
        >
          {btn.label}
        </button>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main InteractiveCard
// ---------------------------------------------------------------------------

export default function InteractiveCard({ requestId, uiDefinition, submittedValues }: InteractiveCardProps) {
  const { t } = useTranslation();
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [submitted, setSubmitted] = useState(!!submittedValues);
  const [submitting, setSubmitting] = useState(false);

  // Initialize default values
  useEffect(() => {
    if (submittedValues) {
      setValues(submittedValues);
      return;
    }
    const defaults: Record<string, unknown> = {};
    for (const block of uiDefinition.blocks) {
      if (block.id && block.default !== undefined) {
        defaults[block.id] = block.default;
      }
      if (block.id && block.type === "koi_picker" && block.suggestions) {
        defaults[block.id] = [...block.suggestions];
      }
    }
    setValues(defaults);
  }, [uiDefinition, submittedValues]);

  const updateValue = (id: string, val: unknown) => {
    setValues((prev) => ({ ...prev, [id]: val }));
  };

  const isVisible = (block: UiBlock): boolean => {
    if (!block.show_when) return true;
    const fieldVal = values[block.show_when.field];
    if (Array.isArray(fieldVal)) return fieldVal.includes(block.show_when.equals);
    return fieldVal === block.show_when.equals;
  };

  const handleAction = async (block: UiBlock, button: UiButton) => {
    if (submitted || submitting) return;
    setSubmitting(true);
    try {
      const actionValue = button.value ?? button.id ?? button.label;
      const payload: Record<string, unknown> = {
        ...values,
        __action__: actionValue,
        __button__: {
          id: button.id,
          label: button.label,
          value: actionValue,
        },
      };
      if (block.id) {
        payload[block.id] = actionValue;
      }
      await interactiveApi.respond(requestId, payload);
      setSubmitted(true);
    } catch (e) {
      console.error("[InteractiveCard] respond error:", e);
    } finally {
      setSubmitting(false);
    }
  };

  const disabled = submitted || submitting;

  return (
    <div className={`interactive-card${submitted ? " ic-submitted" : ""}`}>
      {uiDefinition.title && <div className="ic-title">{uiDefinition.title}</div>}
      {uiDefinition.description && <p className="ic-description">{uiDefinition.description}</p>}

      <div className="ic-blocks">
        {uiDefinition.blocks.map((block, i) => {
          if (!isVisible(block)) return null;
          const key = block.id || `block-${i}`;

          switch (block.type) {
            case "text":
              return <TextBlock key={key} block={block} />;
            case "radio":
              return (
                <RadioBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string) ?? ""}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "checkbox":
              return (
                <CheckboxBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string[]) ?? []}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "text_input":
              return (
                <TextInputBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string) ?? ""}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "number_input":
              return (
                <NumberInputBlock
                  key={key}
                  block={block}
                  value={Number(values[block.id!]) || 0}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "select":
              return (
                <SelectBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string) ?? ""}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "koi_picker":
              return (
                <KoiPickerBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string[]) ?? []}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "project_picker":
              return (
                <ProjectPickerBlock
                  key={key}
                  block={block}
                  value={(values[block.id!] as string) ?? ""}
                  onChange={(v) => updateValue(block.id!, v)}
                  disabled={disabled}
                />
              );
            case "confirm":
            case "actions":
              return <ActionsBlock key={key} block={block} onAction={handleAction} disabled={disabled} />;
            case "divider":
              return <hr key={key} className="ic-divider" />;
            default:
              return null;
          }
        })}
      </div>

      {submitted && (
        <div className="ic-submitted-badge">{t("chat.interactiveSubmitted", "Submitted")}</div>
      )}
    </div>
  );
}
