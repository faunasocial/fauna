pub mod account_entry_crypto;
pub mod api_error;
/// The `fauna://` in-app routes an OS surface opens the app on (the Windows
/// Explorer Share leaf's hand-off) — one grammar for the builder and the parser.
pub mod app_route;
/// Whether one failed catch-up apply is transient or permanent for that change
/// — the classification the sync anchor's accounting law turns on
/// (`docs/goal/behavior/file-sync.md` § *A failed change must not strand the
/// device*).
pub mod apply_failure;
pub mod atproto_identity_rows;
pub mod attachment_limits;
/// Authoritative-direct DNS visibility (raw non-recursive hickory query) — the
/// DNS-01 propagation gate's honest readiness signal, shared by the client-side
/// probe and the nest's `fauna.dns.probe_txt_visible` handler.
#[cfg(feature = "network")]
pub mod authoritative_dns;
pub mod backup_state;
pub mod blessed_nest_rows;
pub mod blob_seal;
pub mod bluesky_web_url;
/// Byte-string serde for `Vec<[u8; N]>` — the fixed-width form `serde_bytes`
/// refuses (`docs/goal/architecture/serialization.md` § "Fixed-size byte arrays").
pub mod byte_array;
pub mod caltime;
pub mod carried;
pub mod chunk;
pub mod chunk_crypto;
pub mod chunk_seal;
pub mod chunker;
pub mod chunker_stream;
pub mod claim_code;
pub mod compress;
pub mod contact_overlay;
pub mod content_category;
pub mod content_line;
pub mod control_chars;
pub mod counterparty_url;
pub mod crypto;
pub mod custodian_endpoints;
pub mod custodies_held;
pub mod custody_ceremony;
pub mod custody_ceremony_rows;
pub mod custody_grant;
pub mod custody_policy;
pub mod custody_receipt;
pub mod data;
pub mod day_bucket;
/// Absorb a burst of `Notify` pulses within a debounce window before
/// returning — the wait a background pass takes so several rapid triggers
/// coalesce into one run instead of one per pulse.
#[cfg(feature = "network")]
pub mod debounce;
pub mod delegation;
pub mod deployment_seed_rows;
pub mod device_endpoints;
pub mod device_id;
mod domain_key;
pub mod encoding;
pub mod error;
pub mod ext_kind;
pub mod feature_gate;
pub mod file_download;
pub mod fleet_removal;
pub mod folder_key_rows;
pub mod folder_keys;
pub mod followed_media;
pub mod format;
#[cfg(not(target_arch = "wasm32"))]
pub mod fs_lock;
pub mod generation;
pub mod grant_event;
pub mod group_ceremony;
pub mod group_content;
pub mod group_generation;
pub mod group_scope;
pub mod hex32;
pub mod human_code;
pub mod ical;
pub mod identity;
pub mod identity_op;
pub mod identity_qr;
pub mod imf_date;
pub mod kdf;
pub mod keyed_staging;
pub mod keyword;
pub mod label;
pub mod label_custody;
/// The whole-record latest-wins rule every stamped join shares.
pub(crate) mod latest_wins;
pub mod load_cache;
pub mod log_redact;
pub mod mail_aliases;
pub mod mail_auth;
pub mod mail_rows;
pub mod mail_scan;
pub mod mailbox;
pub mod manifest_crypto;
pub mod markdown;
pub mod mime_wrap;
pub mod money;
pub mod nat_mode;
pub mod nest_reseal;
mod nonce_truncate;
pub mod nostr_confirmation;
pub mod notes;
pub mod notification_glyph;
pub mod notification_type;
pub mod obligation;
pub mod observer;
pub mod path_crypto;
pub mod path_guard;
pub mod peer_anchor_rows;
pub mod platform_ids;
pub mod read_marker;
pub mod recovery;
pub mod refused_change_rows;
pub mod region_authority;
pub mod region_policy;
pub mod render;
pub mod room_post;
pub mod secret;
#[cfg(not(target_arch = "wasm32"))]
pub mod secret_file;
mod secret_uri;
pub mod seen_set;
/// A `select!` arm that goes quiet when its `Option`-gated resource is
/// absent, instead of a per-caller `None => pend forever` match.
#[cfg(feature = "network")]
pub mod select_pending;
pub mod share_endpoints;
pub mod source;
pub mod source_glyph;
pub mod structured;
pub mod subscription;
pub mod subscription_rows;
pub mod succession_ledger;
pub mod text_heuristic;
#[cfg(test)]
mod text_heuristic_parity;
pub mod version;
pub mod web;

#[cfg(feature = "format_text")]
pub mod format_text;

#[cfg(feature = "qr_render")]
pub mod qr_matrix;

#[cfg(feature = "sqlite-schema-meta")]
pub mod sqlite_schema_meta;

#[cfg(feature = "network")]
pub mod api;
#[cfg(feature = "network")]
pub mod feed;
// `resolve` is split: the pure input parsing (`parse_handle`, `is_actor_id`,
// `classify_recipient`, the TXT/URL string helpers) is always compiled so wasm + the
// standalone client crates classify recipient input from one definition; only the
// DNS/async resolution fns inside it stay `#[cfg(feature = "network")]`-gated (tokio +
// hickory). Same rationale as `scoring` below — pure logic doesn't belong behind `network`.
pub mod resolve;
// The RSVP vocabulary (`RsvpState` / `RsvpResponse`) — one closed set, shared by
// every app and by `ical`'s PARTSTAT projection. Pure serde + no deps, so it sits
// outside every feature gate; the `uniffi` derive is cfg_attr'd like `nat_mode`.
pub mod rsvp;
// `scoring` is pure wire types (`FilterRule`, the labeler registry and the
// scoring-metadata bus) — only serde + `data`/`identity` deps, no
// redb/tokio/hickory. It belongs outside the `network` gate so the feed-rule
// encoder (`fauna-client-feed::encode_filter_rule`) can build `FilterRule` on
// wasm + in standalone client crates, not only when some workspace sibling
// happens to enable `network` via feature unification.
pub mod scoring;
// Screen-time policy value type + (Slice E) the pure window/budget decision fn —
// the family-safety pillar-3 sibling of obligation.rs's content engine.
pub mod screen_time;
#[cfg(feature = "network")]
pub mod social_graph;
#[cfg(feature = "network")]
pub mod storage;
// `sync` is pure wire types + the `path_hash`/`normalize_rel_path` wire-key
// derivations (serde + blake3 + std — no redb/tokio/hickory). Ungated for the
// same rationale as `scoring` above: wasm + standalone client crates
// (`fauna-media-machine::nest_api::ws_rpc` derives `path_hash` for the
// `fauna.files.versions.*` kinds on wasm32) must reach it without the
// `network` dep set.
pub mod sync;
// Unconditional (dep-free consts): the WS-RPC message-size cap is consumed by
// nest *and* the native apps, which do not all enable the `network` feature.
pub mod transport;

pub mod behavioral;
pub mod engagement;
pub mod intent_detector;
pub mod localized;
pub mod maybe_send;
pub mod process_hook;
pub mod progress;
pub mod restore_branch;
pub mod share;
pub mod snapshot_lock;

pub use maybe_send::MaybeSendSync;
pub use snapshot_lock::clone_locked;

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_core");
