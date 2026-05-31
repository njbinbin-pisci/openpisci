import type { UiDefinition, UiPatch } from "./protocol";

export type { UiPatch } from "./protocol";

/** Merge agent `chat_ui_patch` payload into a live card definition. */
export function applyUiPatch(def: UiDefinition, patch: UiPatch): UiDefinition {
  const next: UiDefinition = {
    ...def,
    protocol_version: def.protocol_version ?? "2",
  };
  if (patch.title !== undefined) next.title = patch.title;
  if (patch.description !== undefined) next.description = patch.description;
  if (patch.submit_label !== undefined) next.submit_label = patch.submit_label;
  if (patch.mode !== undefined) next.mode = patch.mode;
  if (patch.data !== undefined) {
    next.data = { ...(def.data ?? {}), ...patch.data };
  }
  if (patch.blocks !== undefined) {
    next.blocks = patch.blocks;
  }
  if (patch.steps !== undefined) {
    next.steps = patch.steps;
  }
  return next;
}

/** Merge `data` defaults into form values (v2). */
export function mergeDataModel(
  def: UiDefinition,
  values: Record<string, unknown>,
): Record<string, unknown> {
  if (!def.data || typeof def.data !== "object") return values;
  return { ...def.data, ...values };
}
