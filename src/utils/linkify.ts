/**
 * Shared utilities for linkifying local file paths in chat message content.
 * Used by both the main Chat view and the Pool Chat view.
 */

// Matches a Windows or Unix local path (bare, not inside a markdown link)
const LOCAL_PATH_RE =
  /(?<!\]\()(?<![`\w/\\])(((?:[A-Za-z]:[\\/]|\\\\)[^\s`"'<>[\]()（）【】]+)|(?:\/(?:home|Users|tmp|var|etc|opt|srv|mnt|data)\/[^\s`"'<>[\]()（）【】]+))/g;

// Matches a path wrapped in backticks: `C:\...` or `/home/user/...`
const BACKTICK_PATH_RE =
  /`(((?:[A-Za-z]:[\\/]|\\\\)[^\s`"'<>[\]()（）【】]+)|(?:\/(?:home|Users|tmp|var|etc|opt|srv|mnt|data)\/[^\s`"'<>[\]()（）【】]+))`/g;

// Splits text on existing markdown links so we can skip already-linked segments
const EXISTING_LINK_RE = /(\[[^\]]*\]\([^)]*\))/g;

function pathToUri(p: string): string {
  const forward = p.replace(/\\/g, "/");
  return forward.startsWith("//")
    ? `file:${encodeURI(forward)}`
    : `file:///${encodeURI(forward.replace(/^\//, ""))}`;
}

/**
 * Convert local file paths in text to Markdown link syntax [path](file:///...).
 * Handles both bare paths and backtick-wrapped paths.
 * Does not double-wrap paths that are already inside a Markdown link.
 */
export function linkifyPaths(text: string): string {
  // Pass 1: replace backtick-wrapped paths in non-linked segments
  const pass1 = text.split(EXISTING_LINK_RE).map((part, i) => {
    if (i % 2 === 1) return part; // already a markdown link — leave alone
    return part.replace(BACKTICK_PATH_RE, (_m, p) => `[${p}](${pathToUri(p)})`);
  }).join("");

  // Pass 2: replace bare paths, skipping already-linked segments (including those from pass1)
  return pass1.split(EXISTING_LINK_RE).map((part, i) => {
    if (i % 2 === 1) return part;
    return part.replace(LOCAL_PATH_RE, (match) => `[${match}](${pathToUri(match)})`);
  }).join("");
}

/**
 * Strip SEND_FILE: / SEND_IMAGE: marker lines from display text.
 * These markers are consumed by the backend for file dispatch.
 */
export function stripSendMarkers(text: string): string {
  return text
    .split("\n")
    .filter((line) => !/^\s*SEND_(FILE|IMAGE):/i.test(line))
    .join("\n")
    .trim();
}

/**
 * Returns true if the href points to a local file path.
 */
export function isLocalPath(href: string | undefined): boolean {
  if (!href) return false;
  return href.startsWith("file://") || /^[A-Za-z]:[\\/]/.test(href) || href.startsWith("\\\\");
}

/**
 * Convert a file:// URI back to a native OS path for shell.open().
 */
export function uriToNativePath(uri: string): string {
  if (uri.startsWith("file:///")) {
    return decodeURIComponent(uri.slice(8)).replace(/\//g, "\\");
  }
  if (uri.startsWith("file://")) {
    return decodeURIComponent(uri.slice(7));
  }
  return uri;
}
