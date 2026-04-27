/** Returns true for sessions that are internal/system and should not appear in the
 *  user-facing session list (heartbeat, pisci_inbox, pool coordinators, etc.). */
export function isInternalSession(session: { source?: string | null; id?: string | null } | undefined | null): boolean {
  if (!session) return false;
  const id = session.id ?? "";
  return session.source === "heartbeat"
    || session.source === "heartbeat_pool"
    || session.source === "pisci_inbox_global"
    || session.source === "pisci_inbox_pool"
    || session.source === "pisci_internal"
    || session.source === "pisci_pool"
    || session.source === "pisci_heartbeat_global"
    || session.source === "pisci_heartbeat_pool"
    || session.id === "heartbeat"
    || session.id === "pisci_inbox_global"
    || id.startsWith("pisci_pool_")
    || id.startsWith("koi_runtime_")
    || id.startsWith("koi_notify_")
    || id.startsWith("koi_");
}
