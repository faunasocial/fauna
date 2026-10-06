//! Shared relay-list resolution — the single parse point every
//! outbound-publish site funnels the `nostr_accounts.relay_list` column
//! (nullable TEXT holding a JSON `Vec<String>`) through.
//!
//! Before this module, five sites resolved the same column divergently:
//! `sync_worker.rs`, `mod.rs::propagate_post_delete`, and the DM send handler
//! parsed JSON and fell back to [`DEFAULT_RELAYS`] on `None`/parse failure;
//! `content_handlers.rs::publish_signed_handler` parsed JSON but fell back to
//! an **empty** vec on `None`/parse failure — a NULL or corrupt `relay_list`
//! made `nostr.events.publish_signed` enqueue loudly, then
//! [`super::sync_worker`]'s `handle_outbound` iterated zero relays, so the
//! publish silently vanished; `interact.rs::parse_relay_list` parsed the
//! column as **comma-separated**, which never matched the JSON the column
//! actually holds (`bridge_provider.rs::update_settings` writes
//! `serde_json::to_string`-shaped JSON), and warned-and-dropped on empty.
//!
//! **`[]` is user intent, not "unconfigured" (ratified 2026-08-11 — the
//! explicit-empty semantic flip).** Every relay UI offers remove-relay down to
//! zero, so a user-emptied list and a never-configured one are distinguishable
//! at the wire and must not collapse to the same fallback: silently keeping a
//! user's events flowing to the public defaults they just removed is exactly
//! the un-consented publish the "user always controls their data" invariant
//! forbids. [`resolve_relay_urls`] now returns an empty vec for `Some("[]")`
//! (`None` still falls back to [`DEFAULT_RELAYS`] — it means "never
//! configured"); the synchronous publish-driving call sites
//! (`content_handlers.rs::publish_signed_handler`, `interact.rs`'s
//! like/repost, and a DM send on the leg — `bridge_leg::send_precheck`) refuse
//! loudly on an empty result instead of enqueueing/spawning a publish that
//! would reach zero relays. The
//! background/read call sites (`sync_worker.rs`'s inbound subscribe,
//! `mod.rs::propagate_post_delete`'s best-effort crosspost) already degrade
//! correctly on an empty vec — an empty relay set to subscribe from or
//! crosspost to is simply a no-op, not an error.
//!
//! **Unparseable JSON is corruption, not "never configured" (2026-08-12,
//! `nest/common.md` § Unreadable stored values).** For the *publish* list the
//! unreadable content may have been exactly the `[]` the user chose, so
//! [`resolve_relay_urls`] resolves it to publish-nowhere — loud through the
//! same refuse-on-empty callers above — never to the defaults the user may
//! have removed. The *hints* column routes reads, not user data, so
//! [`resolve_relay_hints`] keeps the defaults fallback there.

/// Default relays to use when an account has never configured a relay list
/// (a `NULL` `relay_list`), and the fetch-side fallback for unreadable
/// [`resolve_relay_hints`].
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
];

/// Resolve the stored **publish** relay list (`nostr_accounts.relay_list`,
/// nullable TEXT holding a JSON `Vec<String>`) to a concrete relay URL list.
///
/// - `None` — never configured — falls back to [`DEFAULT_RELAYS`].
/// - A parseable list is taken verbatim, **`[]` included**: the user removed
///   every relay, and a publish caller must treat that as "nowhere to
///   publish", not silently substitute the defaults the user just removed
///   (the explicit-empty flip, 2026-08-11 — module note).
/// - **Unparseable JSON resolves to empty, never to the defaults**: the row
///   was authored, and its unreadable content may have been exactly that `[]`
///   (or the pruned list) — corruption must not ship the user's events to
///   relays they may have removed. Empty is the conservative arm and it is
///   loud, because every synchronous publish caller already refuses on an
///   empty result (`nest/common.md` § Unreadable stored values — the
///   authored-choice direction, stated here per its rule 3). Re-saving the
///   relay list from any app rewrites the row — the in-app recovery.
pub(crate) fn resolve_relay_urls(relay_list: Option<&str>) -> Vec<String> {
    match relay_list {
        None => default_relays_vec(),
        Some(raw) => match serde_json::from_str(raw) {
            Ok(urls) => urls,
            Err(e) => {
                tracing::warn!(
                    "stored relay list is unreadable ({e}); resolving to \
                     publish-nowhere, not to the default relays"
                );
                Vec::new()
            }
        },
    }
}

/// Resolve a stored relay-*hints* column (`nostr_follows.relay_hints` — where
/// to subscribe from for a follow). Today the column's only production writer
/// is the user-authenticated `fauna.bridges.add_follow` (`extra.relay_hints`,
/// `bridge_provider.rs::add_follow`), so the hints are user-supplied like the
/// publish list; a future NIP-65/NIP-05 hint ingestion would make them
/// network-supplied, which is why every dial they route is guarded now
/// ([`relay_dial_policy`]). `None` **and unparseable JSON**
/// fall back to [`DEFAULT_RELAYS`]: corruption folds to the defaults because
/// hints route *reads* — the fold widens where we fetch from, never where user
/// data goes, and an empty fold would silently kill the follow's inbound
/// subscription (`nest/common.md` § Unreadable stored values — the
/// display/derived direction, stated here per its rule 3). A parseable list is
/// taken verbatim, empty included.
pub(crate) fn resolve_relay_hints(relay_hints: Option<&str>) -> Vec<String> {
    let parsed: Option<Vec<String>> = relay_hints.and_then(|s| serde_json::from_str(s).ok());
    match parsed {
        Some(urls) => urls,
        None => default_relays_vec(),
    }
}

fn default_relays_vec() -> Vec<String> {
    DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect()
}

/// The policy every outbound relay dial on this nest runs under — the SSRF
/// seat for the four dial sites (the sync worker's pool, the bunker drain, the
/// interaction fan-out, the NIP-46 remote-link handshake), all of which pass it
/// to `RelayClient::connect`. Every relay URL is caller-supplied
/// (the user's publish list, a pasted bunker string, a follow's hints, a paired
/// nest's URL), so the production posture is the shared guard's:
/// globally-routable addresses only, with **no** private-network allowance for
/// a relay a user lists (`nest/network-exposure.md` § Rulings F7).
///
/// Under `test-hooks` **only**, and only when `FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`
/// is set, the policy admits a **loopback** relay so an e2e can point a nest at
/// a peer relay on `127.0.0.1:<port>` — the same double gate (feature **and**
/// env) as ActivityPub's `FAUNA_TEST_AP_ALLOW_LOOPBACK`, and just as narrow:
/// loopback alone, never a private range or IMDS. Production is built without
/// `test-hooks`, so none of it is compiled into the shipping nest. Computed
/// once into `NostrState::relay_dial_policy` (production boot and
/// `AppState::for_test` alike); an in-process test that needs a loopback
/// relay sets `RelayDialPolicy::PublicOrLoopback` on its own state, or passes
/// it to `NostrSyncWorker::new` — a dependency build carries neither the
/// feature nor the env.
pub fn relay_dial_policy() -> fauna_bridge_nostr::relay_client::RelayDialPolicy {
    use fauna_bridge_nostr::relay_client::RelayDialPolicy;
    #[cfg(feature = "test-hooks")]
    if std::env::var_os("FAUNA_TEST_NOSTR_ALLOW_LOOPBACK").is_some() {
        return RelayDialPolicy::PublicOrLoopback;
    }
    RelayDialPolicy::PublicOnly
}

/// Convert a paired nest's base URL (the `nest_pairings.nest_url` shape — an
/// `https://host[:port]` base, e.g. `https://peer.test`) to that peer's Nostr
/// relay endpoint `wss://host[:port]/nostr`. `http` maps to `ws` (a plaintext
/// dev peer); an already-`wss`/`ws` base is taken as-is. A URL that is none of
/// those yields `None`. A trailing slash on the base is tolerated.
pub(crate) fn nest_url_to_relay_url(nest_url: &str) -> Option<String> {
    let trimmed = nest_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let base = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("wss://") || trimmed.starts_with("ws://") {
        trimmed.to_string()
    } else {
        return None;
    };
    Some(format!("{base}/nostr"))
}

/// The public relay URL to advertise / hand out for `actor_hex` — the
/// `nostr_push` peer's `wss://<host>/nostr` when the actor holds such a pairing,
/// else `None` so the caller falls back to the box's own domain (R10 (account-data-plane.md § The ratified decisions)). A paired
/// **head** is private and has no reachable public domain; its public serving
/// box is its Nostr face, so its NIP-46 `bunker://…?relay=` hint and its
/// NIP-65 (10002) / NIP-17 (10050) self-advertisement must name that box, not
/// `wss://<own-lan-domain>/nostr`. Shared by both sites so the resolution lives
/// in one place. When several `nostr_push` pairings exist, the first with a
/// usable `nest_url` wins (a household head normally pairs to one public box).
///
/// `pub` so the tier_3 proxy-delegation test drives the resolver both
/// consumers (`create_invite`'s connect string, the NIP-65/10050
/// advertisement) share, with a real `AppState` + pairing rows.
pub async fn preferred_public_relay_url(
    state: &crate::routes::AppState,
    actor_hex: &str,
) -> Option<String> {
    let actor = fauna_core::hex32::decode(actor_hex).ok()?;
    let pairings = state.db.list_pairings_for_actor(&actor).await.ok()?;
    pairings.iter().find_map(|p| {
        p.capabilities
            .iter()
            .any(|c| c == fauna_protocol::pair::capability::NOSTR_PUSH)
            .then(|| p.nest_url.as_deref().and_then(nest_url_to_relay_url))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production dial policy is the shared guard's, under either feature
    /// set: a test build without the e2e env behaves exactly like the shipping
    /// nest, so compiling `test-hooks` in never loosens the relay guard by
    /// itself.
    #[test]
    fn the_dial_policy_is_public_only_without_the_e2e_env() {
        if std::env::var_os("FAUNA_TEST_NOSTR_ALLOW_LOOPBACK").is_some() {
            return; // an e2e-shaped environment; the pin is about the default
        }
        assert_eq!(
            relay_dial_policy(),
            fauna_bridge_nostr::relay_client::RelayDialPolicy::PublicOnly
        );
    }

    #[test]
    fn some_valid_json_parses_to_its_urls() {
        let urls = resolve_relay_urls(Some(r#"["wss://a.example","wss://b.example"]"#));
        assert_eq!(urls, vec!["wss://a.example", "wss://b.example"]);
    }

    #[test]
    fn none_falls_back_to_default_relays() {
        assert_eq!(resolve_relay_urls(None), default_relays_vec());
    }

    #[test]
    fn corrupt_json_resolves_to_publish_nowhere_never_to_defaults() {
        // An unreadable stored value is not "never configured": the row was
        // authored, and its unreadable content may have been the `[]` (or the
        // pruned list) the user chose — substituting the public defaults would
        // ship their events to relays they may have removed. Empty is the
        // conservative arm, and it is loud: every synchronous publish caller
        // refuses on an empty result (`nest/common.md` § Unreadable stored
        // values, the authored-choice direction).
        let empty: Vec<String> = Vec::new();
        assert_eq!(resolve_relay_urls(Some("not json")), empty);
    }

    #[test]
    fn corrupt_relay_hints_still_fall_back_to_defaults() {
        // Hints route *reads* (which relays to subscribe from for a follow);
        // folding corruption to the defaults widens where we fetch from, never
        // where user data goes — and an empty fold would silently kill the
        // follow's inbound subscription instead.
        assert_eq!(resolve_relay_hints(Some("not json")), default_relays_vec());
        assert_eq!(resolve_relay_hints(None), default_relays_vec());
        let empty: Vec<String> = Vec::new();
        assert_eq!(resolve_relay_hints(Some("[]")), empty);
    }

    #[test]
    fn explicit_empty_list_is_respected_not_defaulted() {
        // The user removed every relay — an intentional opt-out, distinct from
        // never having configured one. Must NOT silently substitute the public
        // defaults the user just removed.
        let empty: Vec<String> = Vec::new();
        assert_eq!(resolve_relay_urls(Some("[]")), empty);
    }

    #[test]
    fn nest_url_maps_https_base_to_wss_nostr_endpoint() {
        assert_eq!(
            nest_url_to_relay_url("https://peer.test").as_deref(),
            Some("wss://peer.test/nostr")
        );
        // Port + trailing slash tolerated.
        assert_eq!(
            nest_url_to_relay_url("https://peer.test:8443/").as_deref(),
            Some("wss://peer.test:8443/nostr")
        );
        // http → ws (a plaintext dev peer).
        assert_eq!(
            nest_url_to_relay_url("http://127.0.0.1:9000").as_deref(),
            Some("ws://127.0.0.1:9000/nostr")
        );
        // Already-ws base is taken as-is (with the /nostr path appended).
        assert_eq!(
            nest_url_to_relay_url("wss://peer.test").as_deref(),
            Some("wss://peer.test/nostr")
        );
        // Neither a URL nor empty → None (caller keeps its own-domain fallback).
        assert_eq!(nest_url_to_relay_url("peer.test"), None);
        assert_eq!(nest_url_to_relay_url("   "), None);
    }

    #[test]
    fn comma_separated_string_is_not_json_and_publishes_nowhere() {
        // `interact.rs` used to parse this column as comma-separated, which
        // never matched the JSON the column actually stores — assert the
        // unified parser treats a comma-separated value as unparseable JSON,
        // not as a relay list, so a pre-existing bad write can't silently
        // resolve to bogus single-entry "relays" (nor to the defaults: it is
        // an authored value this build cannot read, same as any corruption).
        let empty: Vec<String> = Vec::new();
        assert_eq!(
            resolve_relay_urls(Some("wss://a.example,wss://b.example")),
            empty
        );
    }
}
