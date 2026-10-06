// Routed through the `fauna.notifications.*` WS-RPC kinds via the `rpc.ts`
// singleton façade (the HTTP twins were deleted nest-side). The connection
// actor is the calling actor, so the legacy `{actor_id}` path param is dropped.
// The wire row names the notification kind `notif_type`; the SPA's
// `UnifiedNotification` keeps the `type` key, so we rename on the way out.

import * as rpc from './rpc';
import { notificationText, type NotificationText } from './wasm';
import { resolveLocalized } from './i18n/localized';

// The Notifications page's per-row icon — the shared `notif_type` → emoji
// mapping (`fauna_core::notification_glyph`, also used by linux) rather than
// a page-local switch statement (`docs/goal/behavior/notifications.md` §
// Where logic lives).
export { notificationTypeGlyph } from './wasm';

export interface UnifiedNotification {
  id: number;
  type: string;
  source: string;
  sender_id: string | null;
  content_id: string | null;
  subject_uri: string | null;
  summary: string;
  /** What the row paints — decided in shared Rust off the row as it arrived. */
  text: NotificationText;
  is_read: boolean;
  created_at: number;
}

export interface NotificationListResponse {
  notifications: UnifiedNotification[];
  cursor: number | null;
}

export async function getNotifications(
  secretHex: string,
  cursor?: number,
  limit = 25,
): Promise<NotificationListResponse> {
  const reply = await rpc.notificationsList(secretHex, cursor, limit);
  return {
    notifications: reply.notifications.map((n) => ({
      id: n.id,
      type: n.notif_type,
      source: n.source,
      sender_id: n.sender_id ?? null,
      content_id: n.content_id ?? null,
      subject_uri: n.subject_uri ?? null,
      summary: n.summary,
      // Decided here, while the row still has its wire shape — the shared
      // decision reads `body` and `summary` off it (`notifications.md`
      // § Localized body).
      text: notificationText(n),
      is_read: n.is_read,
      created_at: n.created_at,
    })),
    cursor: reply.cursor ?? null,
  };
}

/** The sentence a row paints: the shared decision's localized arm resolved
 *  through this app's own catalog, or its verbatim `summary` as-is. */
export function notificationRowText(n: UnifiedNotification): string {
  return n.text.kind === 'localized' ? resolveLocalized(n.text) : n.text.text;
}

export async function markNotificationsRead(
  secretHex: string,
  upTo?: number,
): Promise<{ marked_read: number }> {
  return rpc.notificationsMarkRead(secretHex, upTo);
}
