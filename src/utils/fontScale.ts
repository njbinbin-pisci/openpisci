/** UI font zoom multiplier applied via CSS `zoom` on `<html>`. */
export const FONT_SCALE_OPTIONS = [1, 1.25, 1.5, 1.75, 2] as const;
export type FontScale = (typeof FONT_SCALE_OPTIONS)[number];

const STORAGE_KEY = "piscis-font-scale";

export function normalizeFontScale(value: unknown): FontScale {
  const n = typeof value === "number" ? value : parseFloat(String(value ?? ""));
  if (FONT_SCALE_OPTIONS.includes(n as FontScale)) {
    return n as FontScale;
  }
  return 1;
}

export function getFontScale(): FontScale {
  try {
    return normalizeFontScale(localStorage.getItem(STORAGE_KEY));
  } catch {
    return 1;
  }
}

export function setFontScale(scale: FontScale): void {
  try {
    localStorage.setItem(STORAGE_KEY, String(scale));
  } catch {
    /* ignore quota / private mode */
  }
  applyFontScale(scale);
}

export function applyFontScale(scale: FontScale): void {
  document.documentElement.style.setProperty("--font-scale", String(scale));
}
