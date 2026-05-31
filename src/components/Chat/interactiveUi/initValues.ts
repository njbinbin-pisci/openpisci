import type { UiBlock, UiDefinition } from "./protocol";
import { CUSTOM_OPTION_VALUE } from "./protocol";
import { collectValueBlocks, flattenBlocks } from "./flatten";
import { mergeDataModel } from "./patch";

function defaultForBlock(block: UiBlock): unknown | undefined {
  if (block.default !== undefined) return block.default;
  switch (block.type) {
    case "checkbox":
    case "tags":
    case "koi_picker":
      return block.suggestions ? [...block.suggestions] : [];
    case "switch":
      return false;
    case "number_input":
    case "slider":
    case "progress":
      if (typeof block.min === "number") return block.min;
      if (block.type === "progress") return 0;
      return 0;
    case "link_list":
      return "";
    case "file_picker":
      return "";
    default:
      return undefined;
  }
}

/** Resolve initial form state from definition (not submitted snapshot). */
export function buildInitialValues(
  blocks: UiBlock[],
  data?: Record<string, unknown>,
): Record<string, unknown> {
  const values: Record<string, unknown> = data ? { ...data } : {};
  for (const block of flattenBlocks(blocks)) {
    if (!block.id) continue;
    if (values[block.id] !== undefined) continue;
    const d = defaultForBlock(block);
    if (d !== undefined) values[block.id] = d;
  }
  return values;
}

export function buildInitialValuesFromDefinition(
  def: UiDefinition,
  wizardStep = 0,
): Record<string, unknown> {
  const blocks = collectValueBlocks(def, wizardStep);
  return buildInitialValues(blocks, def.data);
}

/** When loading submitted snapshot, normalize custom-option sentinel to stored string. */
export function normalizeSubmittedValues(
  def: UiDefinition,
  submitted: Record<string, unknown>,
): Record<string, unknown> {
  const out = { ...submitted };
  const allBlocks = def.mode === "wizard" && def.steps?.length
    ? def.steps.flatMap((s) => s.blocks)
    : def.blocks;
  for (const block of flattenBlocks(allBlocks)) {
    if (!block.id || !block.allow_custom) continue;
    const v = out[block.id];
    if (v === CUSTOM_OPTION_VALUE) out[block.id] = "";
  }
  return mergeDataModel(def, out);
}
