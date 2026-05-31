import type { TFunction } from "i18next";
import type { UiBlock } from "./protocol";
import { CUSTOM_OPTION_VALUE, isValueBlock } from "./protocol";
import { flattenBlocks } from "./flatten";
import { isBlockVisible } from "./visibility";

export type FieldErrors = Record<string, string>;

function isEmptyValue(block: UiBlock, value: unknown): boolean {
  if (value === undefined || value === null) return true;
  if (block.type === "switch") return false;
  if (typeof value === "boolean") return false;
  if (typeof value === "number") return !Number.isFinite(value);
  if (Array.isArray(value)) return value.length === 0;
  if (typeof value === "string") {
    if (value === CUSTOM_OPTION_VALUE) return true;
    return value.trim() === "";
  }
  return false;
}

function validateText(block: UiBlock, value: unknown, t: TFunction): string | null {
  const s = String(value ?? "");
  if (block.min_length != null && s.length < block.min_length) {
    return t("chat.interactiveMinLength", { count: block.min_length, defaultValue: `At least ${block.min_length} characters` });
  }
  if (block.max_length != null && s.length > block.max_length) {
    return t("chat.interactiveMaxLength", { count: block.max_length, defaultValue: `At most ${block.max_length} characters` });
  }
  if (block.pattern) {
    try {
      const re = new RegExp(block.pattern);
      if (!re.test(s)) {
        return block.description || t("chat.interactivePattern", { defaultValue: "Invalid format" });
      }
    } catch {
      /* ignore invalid agent-supplied pattern */
    }
  }
  if (block.input_mode === "email" && s && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(s)) {
    return t("chat.interactiveEmail", { defaultValue: "Enter a valid email" });
  }
  if (block.input_mode === "url" && s) {
    try {
      new URL(s);
    } catch {
      return t("chat.interactiveUrl", { defaultValue: "Enter a valid URL" });
    }
  }
  return null;
}

function validateNumber(block: UiBlock, value: unknown, t: TFunction): string | null {
  const n = Number(value);
  if (!Number.isFinite(n)) {
    return t("chat.interactiveNumber", { defaultValue: "Enter a valid number" });
  }
  if (typeof block.min === "number" && n < block.min) {
    return t("chat.interactiveMin", { min: block.min, defaultValue: `Minimum ${block.min}` });
  }
  if (typeof block.max === "number" && n > block.max) {
    return t("chat.interactiveMax", { max: block.max, defaultValue: `Maximum ${block.max}` });
  }
  return null;
}

function validateDateLike(
  block: UiBlock,
  value: unknown,
  t: TFunction,
): string | null {
  const s = String(value ?? "").trim();
  if (!s) return null;
  const min = typeof block.min === "string" ? block.min : undefined;
  const max = typeof block.max === "string" ? block.max : undefined;
  if (min && s < min) {
    return t("chat.interactiveDateMin", { min, defaultValue: `Not before ${min}` });
  }
  if (max && s > max) {
    return t("chat.interactiveDateMax", { max, defaultValue: `Not after ${max}` });
  }
  return null;
}

function validateMulti(
  block: UiBlock,
  value: unknown,
  t: TFunction,
): string | null {
  const arr = Array.isArray(value) ? value.filter((x) => x !== CUSTOM_OPTION_VALUE && String(x).trim()) : [];
  if (typeof block.min === "number" && arr.length < block.min) {
    return t("chat.interactiveMinSelections", {
      count: block.min,
      defaultValue: `Select at least ${block.min}`,
    });
  }
  if (typeof block.max === "number" && arr.length > block.max) {
    return t("chat.interactiveMaxSelections", {
      count: block.max,
      defaultValue: `Select at most ${block.max}`,
    });
  }
  return null;
}

export function validateInteractiveForm(
  blocks: UiBlock[],
  values: Record<string, unknown>,
  t: TFunction,
): FieldErrors {
  const errors: FieldErrors = {};

  for (const block of flattenBlocks(blocks)) {
    if (!isValueBlock(block) || !block.id) continue;
    if (!isBlockVisible(block, values)) continue;

    const value = values[block.id];
    if (block.required && isEmptyValue(block, value)) {
      errors[block.id] = t("chat.interactiveRequired", { defaultValue: "Required" });
      continue;
    }
    if (isEmptyValue(block, value)) continue;

    let msg: string | null = null;
    switch (block.type) {
      case "text_input":
        msg = validateText(block, value, t);
        break;
      case "number_input":
      case "slider":
        msg = validateNumber(block, value, t);
        break;
      case "date":
      case "time":
      case "datetime":
        msg = validateDateLike(block, value, t);
        break;
      case "checkbox":
      case "tags":
      case "koi_picker":
        msg = validateMulti(block, value, t);
        break;
      case "select":
      case "radio":
        if (value === CUSTOM_OPTION_VALUE) {
          msg = t("chat.interactiveCustomEmpty", { defaultValue: "Enter a custom value" });
        }
        break;
      default:
        break;
    }
    if (msg) errors[block.id] = msg;
  }

  return errors;
}
