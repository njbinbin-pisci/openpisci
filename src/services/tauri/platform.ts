/**
 * Tauri IPC — platform domain.
 *
 * Host / OS primitives: runtime & VM capability probing, window / overlay /
 * theme control, and UI-side resolution of permission + interactive-UI
 * prompts. Plus the cross-platform `openPath` helper.
 *
 * Mirrors Rust-side `src-tauri/src/commands/platform/*`.
 */
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// System / Runtimes
// ---------------------------------------------------------------------------

export interface RuntimeCheckItem {
  name: string;
  available: boolean;
  version: string | null;
  download_url: string;
  hint: string;
}

export interface SystemDependencyItem {
  key: string;
  name: string;
  feature: string;
  available: boolean;
  required: boolean;
  status: "ok" | "warning" | "missing";
  details: string | null;
  hint: string;
  remediation: string | null;
  action: SystemDependencyAction | null;
}

export interface SystemDependencyAction {
  kind: "install_command" | "open_url" | "open_settings";
  command: string | null;
  url: string | null;
}

export interface PrivilegeElevationCheckItem {
  key: string;
  name: string;
  available: boolean;
  required: boolean;
  status: "ok" | "warning" | "missing";
  details: string | null;
  hint: string;
  remediation: string | null;
  action: SystemDependencyAction | null;
}

export const systemApi = {
  getVmStatus: () =>
    invoke<{ backend: string; available: boolean; description: string }>("get_vm_status"),
  checkRuntimes: () => invoke<RuntimeCheckItem[]>("check_runtimes"),
  checkSystemDependencies: () =>
    invoke<SystemDependencyItem[]>("check_system_dependencies"),
  checkPrivilegeElevation: () =>
    invoke<PrivilegeElevationCheckItem[]>("check_privilege_elevation"),
  runSystemDependencyAction: (key: string) =>
    invoke<void>("run_system_dependency_action", { key }),
  setRuntimePath: (runtimeKey: string, exePath: string) =>
    invoke<RuntimeCheckItem[]>("set_runtime_path", { runtimeKey, exePath }),
};

// ---------------------------------------------------------------------------
// Window / Overlay / Theme
// ---------------------------------------------------------------------------

export const windowApi = {
  enterMinimalMode: () => invoke<void>("enter_minimal_mode"),
  exitMinimalMode: () => invoke<void>("exit_minimal_mode"),
  setOverlayPosition: (x: number, y: number) =>
    invoke<void>("set_overlay_position", { x, y }),
  saveOverlayPosition: (x: number, y: number) =>
    invoke<void>("save_overlay_position", { x, y }),
  setThemeBorder: (theme: "violet" | "gold") =>
    invoke<void>("set_window_theme_border", { theme }),
};

// ---------------------------------------------------------------------------
// Permission prompts (confirmation gates)
// ---------------------------------------------------------------------------

export const permissionApi = {
  respond: (requestId: string, approved: boolean) =>
    invoke<void>('respond_permission', { requestId, approved }),
};

// ---------------------------------------------------------------------------
// Interactive UI (chat_ui tool responses)
// ---------------------------------------------------------------------------

export const interactiveApi = {
  respond: (requestId: string, values: Record<string, unknown>) =>
    invoke<void>('respond_interactive_ui', { requestId, values }),
};

// ---------------------------------------------------------------------------
// File / Path utilities
// ---------------------------------------------------------------------------

/**
 * Open a local file or directory with the system default application.
 * On Windows, directories are opened with Explorer.exe directly,
 * which is more reliable than shell.open() for folder paths.
 */
export function openPath(path: string): Promise<void> {
  return invoke<void>("open_path", { path });
}
