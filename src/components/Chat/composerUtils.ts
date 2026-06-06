import { readFile, writeFile } from "@tauri-apps/plugin-fs";
import { join } from "@tauri-apps/api/path";
import { tempDir } from "@tauri-apps/api/path";
import type { ChatAttachment } from "../../services/tauri";

export interface PendingAttachmentItem {
  id: string;
  attachment: ChatAttachment;
  preview?: string;
}

const IMAGE_EXTS = ["png", "jpg", "jpeg", "gif", "webp"];

const MIME_MAP: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 8192;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.slice(i, i + chunk));
  }
  return btoa(binary);
}

export function isImageFilename(filename: string): boolean {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  return IMAGE_EXTS.includes(ext);
}

export async function buildAttachmentFromPath(filePath: string): Promise<PendingAttachmentItem> {
  const filename = filePath.split(/[\\/]/).pop() ?? filePath;
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  const id = `att_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;

  if (isImageFilename(filename)) {
    const bytes = await readFile(filePath);
    const mediaType = MIME_MAP[ext] ?? "image/jpeg";
    const b64 = bytesToBase64(bytes);
    return {
      id,
      attachment: { media_type: mediaType, path: filePath, data: b64, filename },
      preview: `data:${mediaType};base64,${b64}`,
    };
  }

  return {
    id,
    attachment: { media_type: "application/octet-stream", path: filePath, filename },
  };
}

export async function buildAttachmentFromBlob(
  blob: Blob,
  filename: string,
): Promise<PendingAttachmentItem> {
  const id = `att_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
  const ext = filename.split(".").pop()?.toLowerCase() ?? "png";
  const mediaType = blob.type || MIME_MAP[ext] || "image/png";
  const buffer = await blob.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  const b64 = bytesToBase64(bytes);

  let path: string | undefined;
  try {
    const tmp = await tempDir();
    const safeName = filename.replace(/[^\w.\-]/g, "_") || `paste.${ext}`;
    path = await join(tmp, `piscis-paste-${Date.now()}-${safeName}`);
    await writeFile(path, bytes);
  } catch {
    path = undefined;
  }

  return {
    id,
    attachment: {
      media_type: mediaType,
      path,
      data: b64,
      filename: filename || `paste.${ext}`,
    },
    preview: `data:${mediaType};base64,${b64}`,
  };
}
