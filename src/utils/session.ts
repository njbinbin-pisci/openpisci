/** Returns true for sessions that are internal/system and should not appear in the
 *  user-facing session list (heartbeat, pisci_inbox, pool coordinators, etc.). */
export function isInternalSession(session: { source?: string | null; id?: string | null } | undefined | null): boolean {
  if (!session) return false;
  return session.source === "heartbeat"
    || session.source === "heartbeat_pool"
    || session.source === "pisci_inbox_global"
    || session.source === "pisci_inbox_pool"
    || session.source === "pisci_internal"
    || session.id === "heartbeat"
    || session.id === "pisci_inbox_global"
    || session.id?.startsWith("pisci_pool_") === true;
}
