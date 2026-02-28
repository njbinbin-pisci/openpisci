/**
 * Render 🐟 emoji to 512x512 PNG for tauri icon
 * Run: node scripts/emoji-to-png.mjs
 *
 * Uses @napi-rs/canvas - emoji rendering depends on system emoji font
 * (Segoe UI Emoji on Windows, Apple Color Emoji on macOS)
 */
import { writeFile } from "fs/promises";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { createCanvas } from "@napi-rs/canvas";

const __dirname = dirname(fileURLToPath(import.meta.url));
const outPath = join(__dirname, "..", "fish-emoji-512.png");
const size = 512;

const canvas = createCanvas(size, size);
const ctx = canvas.getContext("2d");

ctx.fillStyle = "#1a1530";
ctx.fillRect(0, 0, size, size);
ctx.fillStyle = "#fff";
ctx.font = "360px Segoe UI Emoji, Apple Color Emoji, Noto Color Emoji";
ctx.textAlign = "center";
ctx.textBaseline = "middle";
ctx.fillText("🐟", size / 2, size / 2);

const png = canvas.toBuffer("image/png");
await writeFile(outPath, png);
console.log("Saved:", outPath);
