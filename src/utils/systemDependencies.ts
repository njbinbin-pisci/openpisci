import type { PrivilegeElevationCheckItem, SystemDependencyItem } from "../services/tauri";

const DEPENDENCY_REMEDIATION_KEYS: Record<string, string> = {
  "linux-session": "settings.depRemediation_linux_session",
  xdotool: "settings.depRemediation_xdotool",
  wmctrl: "settings.depRemediation_wmctrl",
  xclip: "settings.depRemediation_xclip",
  cliclick: "settings.depRemediation_cliclick",
  osascript: "settings.depRemediation_osascript",
  "macos-accessibility": "settings.depRemediation_macos_accessibility",
  powershell: "settings.depRemediation_powershell",
  "uia-runtime": "settings.depRemediation_uia_runtime",
  "wmi-service": "settings.depRemediation_wmi_service",
  "office-installation": "settings.depRemediation_office_installation",
};

const PRIVILEGE_REMEDIATION_KEYS: Record<string, string> = {
  "linux-pkexec": "settings.privElevationRemediation_linux_pkexec",
  "linux-polkit-agent": "settings.privElevationRemediation_linux_polkit_agent",
  "linux-graphical-session": "settings.privElevationRemediation_linux_graphical_session",
  "linux-session-bus": "settings.privElevationRemediation_linux_session_bus",
  "macos-admin-dialog": "settings.privElevationRemediation_macos_admin_dialog",
  "windows-uac": "settings.privElevationRemediation_windows_uac",
};

export function localizedDependencyRemediation(
  t: (key: string, options?: Record<string, unknown>) => string,
  item: SystemDependencyItem,
): string | null {
  const translationKey = DEPENDENCY_REMEDIATION_KEYS[item.key];
  return translationKey ? t(translationKey) : item.remediation;
}

export function localizedPrivilegeElevationRemediation(
  t: (key: string, options?: Record<string, unknown>) => string,
  item: PrivilegeElevationCheckItem,
): string | null {
  const translationKey = PRIVILEGE_REMEDIATION_KEYS[item.key];
  return translationKey ? t(translationKey) : item.remediation;
}