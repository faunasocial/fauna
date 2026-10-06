export interface Identity {
  secretHex: string;
  actorId: string;
  handle?: string;
  domain?: string;
  tier?: string;
  /** This identity has been verified against its nest **on this load** — the
   *  silent challenge in `store.ts`'s `refreshFromServer` came back `verified`,
   *  which costs a real WS-RPC round trip.
   *
   *  Deliberately NOT persisted and NOT derived from the cached handle: it is
   *  the *live-session* signal, so it must be false while the app is showing a
   *  cached identity it has not reached the nest as. This is what the e2e state
   *  provider publishes as `session.authenticated`
   *  (`tests/e2e-unified/web-bridge/agent.js`), the cross-app observable for
   *  "this identity reached a WORKING session" that `helpers/waiting.py`'s
   *  `await_session_actor` asserts on all 7 apps. */
  registered?: boolean;
}

export interface DecodedEmail {
  from: string;
  to: string;
  subject: string;
  body: string;
  timestamp: number;
  valid: boolean;
  encrypted: boolean;
  post_id: string;
  sender_node?: string;
  attachments?: { hash: string; media_type: string; size_bytes: number }[];
}

export interface GroupAttachment {
  hash: string;
  media_type: string;
  size_bytes: number;
}

/**
 * A direct-message inbox entry. `body`/`from`/`timestamp`/`encrypted` are set
 * when the message is received or sent; the `_`-prefixed fields are optional
 * client-side annotations the inbox template renders when present (read state,
 * content label, attachments, encrypted-blob handle).
 */
export interface DmMessage {
  body: string;
  from: string;
  timestamp: number;
  encrypted: boolean;
  _read?: boolean;
  _content_label?: string;
  attachments?: GroupAttachment[];
  _encrypted_media?: {
    blob_hash: string;
    epoch: number;
    channel_id: string;
  };
}

export interface FeedDefinition {
  feed_id: string;
  name: string;
  rules: FilterRule[];
  combination: string;
  created_at: number;
}

// FilterRule matches Rust's serde externally-tagged enum format.
// Each rule is an object with a single key (the variant name) mapping to its data.
export type FilterRule =
  | { MinReplies: { count: number } }
  | { MinReposts: { count: number } }
  | { HasMedia: { required: boolean } }
  | { IsReply: { required: boolean } }
  | { HasHashtag: { tags: string[] } }
  | { CreatedAfter: { age_microseconds: number } }
  | { Source: { protocols: string[] } }
  | { BodyContains: { terms: string[] } }
  | { BodyExcludes: { terms: string[] } }
  // Confidences ride as per-mille u16 (×1000) — the dag-cbor wire forbids
  // floats (FilterRule float-free). e.g. 0.5 → 500.
  | { LabelBelow: { category: string; max_confidence_permille: number } }
  | { LabelAbove: { category: string; min_confidence_permille: number } };

export interface FeedPost {
  post_id: string;
  author: string;
  created_at: number;
  tags: string[];
  has_media: boolean;
  is_reply: boolean;
  source: string;
  // The nest's index body from the feed query (`FeedPostItem.body`). Rendered as
  // the post text immediately, and as the fallback when the per-post signed-body
  // decode is pending or fails — the same source linux/windows display
  // (`post_list.rs` renders `item.body`). The client decode below then enriches
  // it (media, quotes, structured cards).
  body: string;
  // Filled in client-side after the signed post body is decoded
  // (`fetchAndDecodePost`); surfaced to e2e state as `body` / `media_hash`.
  decoded_body?: string;
  decoded_media_hash?: string;
}

export interface FeedQueryResult {
  posts: FeedPost[];
  cursor: number | null;
}

export interface Knock {
  id: number;
  sender: string;      // hex actor_id
  sender_node: string;
  summary: string;
  created_at: number;
}

export interface Contact {
  peer_id: string;     // hex actor_id
  status: string;      // "pending" | "accepted" | "confirmed" | "blocked"
  accepted_at?: number;
  created_at?: number;
  handle?: string;     // peer's public handle (local peer with a handle; absent for federated)
  domain?: string;     // nest's handle domain (absent for federated)
}
