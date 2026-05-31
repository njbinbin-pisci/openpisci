/** Chat UI Protocol v1/v2 — shared types (see docs/chat-ui-protocol.md). */

export const CHAT_UI_PROTOCOL_VERSION = "2";

export const CUSTOM_OPTION_VALUE = "__custom__";

export type UiButtonStyle = "primary" | "danger" | "default";
export type UiButtonEmit = "submit" | "action";

export interface UiOption {
  value: string;
  label: string;
  description?: string;
  /** For link_list: optional icon or URL hint */
  href?: string;
}

export interface UiButton {
  id?: string;
  label: string;
  value?: unknown;
  style?: UiButtonStyle;
  /** v2: `action` completes the tool with __action_type__ action (card stays editable); `submit` ends the form. */
  emit?: UiButtonEmit;
}

export interface ShowWhen {
  field: string;
  equals?: string | number | boolean;
  one_of?: (string | number | boolean)[];
  not_equals?: string | number | boolean;
}

export type UiBlockType =
  | "text"
  | "divider"
  | "section"
  | "row"
  | "column"
  | "card"
  | "image"
  | "code_preview"
  | "progress"
  | "link_list"
  | "text_input"
  | "number_input"
  | "slider"
  | "switch"
  | "date"
  | "time"
  | "datetime"
  | "select"
  | "radio"
  | "checkbox"
  | "tags"
  | "koi_picker"
  | "project_picker"
  | "file_picker"
  | "confirm"
  | "actions";

export interface UiBlock {
  type: UiBlockType | string;
  id?: string;
  label?: string;
  description?: string;
  content?: string;
  value?: unknown;
  options?: UiOption[];
  default?: unknown;
  placeholder?: string;
  show_when?: ShowWhen;
  suggestions?: string[];
  allow_new?: boolean;
  allow_custom?: boolean;
  custom_label?: string;
  required?: boolean;
  disabled?: boolean;
  min?: number | string;
  max?: number | string;
  step?: number;
  min_length?: number;
  max_length?: number;
  pattern?: string;
  multiline?: boolean;
  rows?: number;
  input_mode?: "text" | "email" | "url" | "password";
  buttons?: UiButton[];
  /** Layout children (row, column, card) */
  blocks?: UiBlock[];
  /** image */
  url?: string;
  alt?: string;
  /** code_preview */
  language?: string;
  /** progress: 0..max or 0..1 when max omitted */
  /** file_picker */
  accept?: string;
  multiple?: boolean;
  /** link_list: sets field id when item clicked */
  href?: string;
}

export interface UiWizardStep {
  id?: string;
  label?: string;
  description?: string;
  blocks: UiBlock[];
}

export type UiMode = "form" | "display" | "wizard";

export interface UiDefinition {
  protocol_version?: string;
  mode?: UiMode;
  title?: string;
  description?: string;
  submit_label?: string;
  blocks: UiBlock[];
  /** v2: initial data model merged into field values */
  data?: Record<string, unknown>;
  /** v2 wizard */
  steps?: UiWizardStep[];
}

export interface UiPatch {
  title?: string;
  description?: string;
  submit_label?: string;
  mode?: UiMode;
  data?: Record<string, unknown>;
  blocks?: UiBlock[];
  steps?: UiWizardStep[];
  /** 0-based step index to show in wizard mode */
  wizard_step?: number;
  /** Enable final submit after a non-terminal action */
  reopen_submit?: boolean;
}

export const VALUE_BLOCK_TYPES = new Set<string>([
  "text_input",
  "number_input",
  "slider",
  "switch",
  "date",
  "time",
  "datetime",
  "select",
  "radio",
  "checkbox",
  "tags",
  "koi_picker",
  "project_picker",
  "file_picker",
  "link_list",
  "progress",
]);

export const LAYOUT_BLOCK_TYPES = new Set<string>(["row", "column", "card"]);

export const ACTION_BLOCK_TYPES = new Set<string>(["actions", "confirm"]);

export function isValueBlock(block: UiBlock): boolean {
  return !!block.id && VALUE_BLOCK_TYPES.has(block.type);
}

export function isActionBlock(block: UiBlock): boolean {
  return ACTION_BLOCK_TYPES.has(block.type);
}

export function protocolVersion(def: UiDefinition): string {
  const v = def.protocol_version;
  if (v === "1" || v === "2") return v;
  return CHAT_UI_PROTOCOL_VERSION;
}
