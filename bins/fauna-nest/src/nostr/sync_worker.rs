//! Background worker for bidirectional sync with external Nostr relays.
//!
//! - Outbound: publishes Fauna posts to users' configured Nostr relays
//! - Inbound: subscribes to followed Nostr pubkeys and stores events as Fauna posts

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use fauna_bridge_nostr::nip01::{ClientMessage, RelayMessage};
use fauna_bridge_nostr::relay_client::{RelayClient, RelayDialPolicy};
use fauna_bridge_nostr::signing::verify_event;
use fauna_bridge_nostr::types::{Event, Filter};
use fauna_segment_store::SegmentManager;

use crate::db::CacheDb;
use crate::nostr::relays::{nest_url_to_relay_url, resolve_relay_hints};
use crate::nostr::store;
use crate::nostr::{bunker, db, inbound_lifecycle};

/// An outbound event to publish to external Nostr relays.
#[derive(Debug)]
pub struct OutboundEvent {
    /// The signed Nostr event to publish.
    pub event: Event,
    /// Relay URLs to publish to.
    pub relay_urls: Vec<String>,
}

/// Build a NIP-65 (kind 10002) relay list metadata event and sign it.
///
/// This publishes the user's preferred relay list so other clients know
/// where to send events destined for this pubkey.
pub fn build_nip65_event(
    keypair: &fauna_bridge_nostr::signing::Keypair,
    relay_list_json: &str,
) -> Option<Event> {
    let relay_urls: Vec<String> = serde_json::from_str(relay_list_json).ok()?;
    if relay_urls.is_empty() {
        return None;
    }

    let tags: Vec<fauna_bridge_nostr::types::Tag> = relay_urls
        .into_iter()
        .map(|url| fauna_bridge_nostr::types::Tag::new(vec!["r".to_string(), url]))
        .collect();

    let now = fauna_core::data::Timestamp::now_secs() as u64;

    let unsigned = fauna_bridge_nostr::types::UnsignedEvent {
        pubkey: keypair.public_key_bytes(),
        created_at: now,
        kind: 10002,
        tags,
        content: String::new(),
    };

    Some(keypair.sign_event(unsigned))
}

/// Build a kind-10050 (NIP-17 DM-inbox relay list) event and sign it.
///
/// Advertises which relays accept the user's NIP-17 gift-wrap DMs. For a Fauna
/// nest that is its own `/nostr` endpoint — the inbox role (slice B): other
/// clients read this list to learn where to deposit gift wraps addressed to this
/// pubkey. Mirrors [`build_nip65_event`], but with kind 10050 and `relay` tags.
pub fn build_nip10050_event(
    keypair: &fauna_bridge_nostr::signing::Keypair,
    dm_relay_urls: &[String],
) -> Option<Event> {
    if dm_relay_urls.is_empty() {
        return None;
    }

    let tags: Vec<fauna_bridge_nostr::types::Tag> = dm_relay_urls
        .iter()
        .map(|url| fauna_bridge_nostr::types::Tag::new(vec!["relay".to_string(), url.clone()]))
        .collect();

    let now = fauna_core::data::Timestamp::now_secs() as u64;

    let unsigned = fauna_bridge_nostr::types::UnsignedEvent {
        pubkey: keypair.public_key_bytes(),
        created_at: now,
        kind: 10050,
        tags,
        content: String::new(),
    };

    Some(keypair.sign_event(unsigned))
}

/// The Nostr sync worker manages connections to external relays.
pub struct NostrSyncWorker {
    db: Arc<CacheDb>,
    /// The `__post` segment store — [`Self::materialize_exposed`] reads a
    /// post's body segment-first (`segments::post::load_post_body`) the same
    /// way `routes::get_post_core` does.
    post_segments: Arc<SegmentManager>,
    outbound_rx: mpsc::Receiver<OutboundEvent>,
    // (Removed 2026-07-19) A `relay_tx` broadcast sender was held here for a
    // planned re-broadcast of *inbound* relay events (followed third-party
    // authors' content pulled from external relays) to the nest's live relay
    // subscribers. That is exactly the read/aggregator-relay role the wider-
    // posture ruling REJECTED as target state — not deferred (`nostr.md`
    // § The relay event store → *Wider posture*, 2026-07-15). Local-account
    // events reach live subscribers via the relay endpoint's own broadcast
    // channel (`state.nostr.relay_tx`), which is untouched.
    /// Wake nudge for the bunker-drain reconciler (fed by
    /// `state.nostr.bunker_wake_tx`; `fauna.nostr.bunker.create_invite` sends
    /// one per mint) — a fresh signer's `#p` filter must go live on the paired
    /// public box *now*, not at the next 60s tick, or the app's first
    /// (ephemeral, never-replayed) request misses the standing subscription.
    bunker_wake_rx: mpsc::Receiver<()>,
    /// 32-byte Ed25519 signing key seed from the nest identity, used to
    /// decrypt Nostr private keys for gift-wrap processing.
    nest_signing_key_bytes: [u8; 32],
    /// The whole app state, held for two consumers: the **zap purchase leg**
    /// (a believed receipt that meets its target tier's asking price goes
    /// through `payment_core::apply_payment`, the engine's one entry point for
    /// every payment mechanism — `monetization.md` § Per-post pay-to-unlock)
    /// and the **takedown-retraction reconcile** on the tick
    /// ([`crate::nostr::retract_taken_down_posts`]).
    ///
    /// `None` means the policy plane is unreadable, so the zap arm **drops**
    /// the receipt rather than ingesting it ungated (§ Fail posture — "the gate
    /// fails closed for the operation, never open"); the takedown reconcile is
    /// skipped too (the takedown handler's immediate propagation is unaffected —
    /// it has its own state). Both the production spawn and the test constructor
    /// attach state, so nothing in tree relies on the stateless shape: it is a
    /// degraded mode, not a second behavior to keep working.
    state: Option<Arc<crate::AppState>>,
    /// The SSRF seat for every relay this worker dials — publish lists, follow
    /// hints, paired serving boxes — passed to `RelayClient::connect` at the
    /// pool and the bunker drain. Production boot passes
    /// `state.nostr.relay_dial_policy`; it is a constructor argument rather
    /// than read at the dial so an in-process test whose peer nest sits on
    /// loopback can say so without a feature or an env var.
    dial_policy: RelayDialPolicy,
}

impl NostrSyncWorker {
    pub fn new(
        db: Arc<CacheDb>,
        post_segments: Arc<SegmentManager>,
        outbound_rx: mpsc::Receiver<OutboundEvent>,
        bunker_wake_rx: mpsc::Receiver<()>,
        nest_signing_key_bytes: [u8; 32],
        dial_policy: RelayDialPolicy,
    ) -> Self {
        Self {
            db,
            post_segments,
            outbound_rx,
            bunker_wake_rx,
            nest_signing_key_bytes,
            state: None,
            dial_policy,
        }
    }

    /// Attach the app state so believed zaps can complete a purchase and the
    /// tick can reconcile takedown retractions. The production spawn always
    /// does this; a worker without it records every believed zap as a tip and
    /// leaves retraction to the takedown handler's immediate path.
    pub fn with_state(mut self, state: Arc<crate::AppState>) -> Self {
        self.state = Some(state);
        self
    }

    /// Run the sync worker. This spawns as a long-lived tokio task.
    pub async fn run(mut self) {
        tracing::info!("Nostr sync worker started");

        let mut relay_connections: HashMap<String, RelayClient> = HashMap::new();
        let mut inbound_interval = tokio::time::interval(std::time::Duration::from_secs(60));
        let mut active_subscriptions: HashMap<String, Vec<String>> = HashMap::new(); // relay_url -> [sub_ids]
        // relay_url -> the spawned low-latency NIP-46 drain on that paired
        // public box (R10 (account-data-plane.md § The ratified decisions)) — reconciled on the tick and on bunker-wake nudges.
        let mut bunker_drains: HashMap<String, BunkerDrain> = HashMap::new();

        loop {
            tokio::select! {
                // Process outbound events
                Some(outbound) = self.outbound_rx.recv() => {
                    // The nsec-deposit gate: the box relays as a user's agent
                    // only when a user deposited a key, so skip the work (but
                    // keep the loop alive, draining the channel) on an
                    // ungated box (`nostr.md` § The bridging gate).
                    if !self.bridging_enabled().await {
                        continue;
                    }
                    self.handle_outbound(&mut relay_connections, outbound).await;
                }

                // A bunker roster change (invite mint) — reconcile the drains
                // immediately so the fresh signer's `#p` filter is standing on
                // the paired public box before the app's first request.
                Some(()) = self.bunker_wake_rx.recv() => {
                    if !self.bridging_enabled().await {
                        continue;
                    }
                    self.reconcile_bunker_drains(&mut bunker_drains).await;
                }

                // Periodic inbound sync: refresh subscriptions for followed Nostr pubkeys
                _ = inbound_interval.tick() => {
                    if !self.bridging_enabled().await {
                        continue;
                    }
                    self.refresh_inbound_subscriptions(
                        &mut relay_connections,
                        &mut active_subscriptions,
                    ).await;
                    // NIP-46 bunker proxy leg (R10): keep one dedicated
                    // low-latency drain task per paired public serving box
                    // (spawn/respawn/kill to match pairings + the signer
                    // roster). Interactive NIP-46 cannot ride this loop's 60s
                    // drain cadence — T6 pins the bound.
                    self.reconcile_bunker_drains(&mut bunker_drains).await;
                    // Materialize any exposed posts created since the last sweep
                    // (the immediate path is the `expose_content` toggle in
                    // `NostrProvider::update_settings`; this is the safety net
                    // for posts authored while exposure was already on).
                    self.materialize_exposed().await;
                    // The Nostr DM leg's outbox (`bridge_leg::drain_outbox`):
                    // the family's `send` queues an item and nudges a drain at
                    // once; this is the safety net for an item that nudge
                    // missed. Spawned for the takedown reason below — the
                    // drain awaits a send on this worker's own channel.
                    if let Some(state) = self.state.clone() {
                        let scope = state.clone();
                        scope.spawn_scoped(async move {
                            if let Err(e) = crate::nostr::bridge_leg::drain_outbox(&state).await {
                                tracing::warn!("nostr sync: drain the DM leg's outbox: {e:#}");
                            }
                        });
                    }
                    // Takedown-retraction reconcile (`moderation.md` § Legal
                    // takedown): chase any post under a legal takedown whose
                    // kind-5 propagation never ran. Spawned, never inlined —
                    // `propagate_post_delete` awaits a send on this worker's
                    // own outbound channel, which from inside this tick arm
                    // would deadlock once the channel is full.
                    if let Some(state) = self.state.clone() {
                        let scope = state.clone();
                        scope.spawn_scoped(async move {
                            match crate::nostr::retract_taken_down_posts(&state).await {
                                Ok(n) if n > 0 => {
                                    tracing::info!("nostr sync: retracted {n} taken-down posts");
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::warn!("nostr sync: takedown reconcile failed: {e}");
                                }
                            }
                        });
                    }
                    // NIP-40: reclaim disk held by expired events. Not a
                    // correctness requirement — `store::query_events` already
                    // excludes expired rows from being served — so this is
                    // pure hygiene, gated on `bridging_enabled` above only
                    // because an ungated box's `/nostr` endpoint 503s and so
                    // never writes a new row to sweep.
                    self.sweep_expired_events().await;
                }

                else => {
                    tracing::info!("Nostr sync worker shutting down");
                    break;
                }
            }

            // Drain any incoming events from all relay connections. Ingest
            // acts on the depositors' behalf (and unwraps their DMs), so it
            // is gated too; an ungated box opens no relay connections above,
            // but guard explicitly to be safe.
            if self.bridging_enabled().await {
                self.drain_inbound_events(&mut relay_connections).await;
            }
        }

        // Worker shutdown: the drain tasks hold their own connections and would
        // otherwise outlive the loop. Dropping the map is what kills them
        // (`impl Drop for BunkerDrain`) — deliberately NOT an abort loop here,
        // because this line is reached only on the channels-closed path. The
        // generation teardown cancels instead, dropping this whole future
        // mid-await, and an abort loop written here would never run on it.
        drop(bunker_drains);
    }

    /// Server-side Nostr bridging acts as a user's Nostr agent, so it runs
    /// only where a user deposited an nsec (the explicit per-user trust act —
    /// `docs/goal/ui/nostr.md` § The bridging gate). Same predicate as
    /// `crate::nostr::nostr_bridging_available` (which the worker cannot call:
    /// it has no `AppState`), read straight off the DB. See that function for
    /// the full gate contract, including why the widening is safe (inbound
    /// gift-wrap DM content seals at ingest — [`Self::process_gift_wrap`]).
    async fn bridging_enabled(&self) -> bool {
        let conn = self.db.conn().await;
        db::any_nsec_deposited(&conn).unwrap_or_else(|e| {
            tracing::warn!("nostr sync worker: read nsec deposits: {e}");
            false // fail closed
        })
    }

    /// Materialize any not-yet-materialized exposed posts across all depositor
    /// accounts into the relay event store (translated + signed once). The
    /// per-account nsec is decrypted only when that account actually has new
    /// posts to sign ([`store::materialize_all_exposed`]).
    async fn materialize_exposed(&self) {
        match store::materialize_all_exposed(
            &self.db,
            &self.post_segments,
            &self.nest_signing_key_bytes,
        )
        .await
        {
            Ok(n) if n > 0 => tracing::info!("nostr sync: materialized {n} exposed posts"),
            Ok(_) => {}
            Err(e) => tracing::warn!("nostr sync: materialize sweep failed: {e}"),
        }
    }

    /// NIP-40 expiry on **both** nostr planes.
    ///
    /// Relay store ([`store::sweep_expired`]): disk hygiene only —
    /// `store::query_events` already excludes expired rows from being served.
    ///
    /// Sweep plane ([`inbound_lifecycle::sweep_expired_inbound`]): the actual
    /// correctness mechanism, because no `content` reader filters on
    /// `expires_at`. Both run on the same tick so neither plane can be the one
    /// nobody remembered.
    async fn sweep_expired_events(&self) {
        let conn = self.db.conn().await;
        match store::sweep_expired(&conn) {
            Ok(n) if n > 0 => tracing::info!("nostr sync: swept {n} expired events"),
            Ok(_) => {}
            Err(e) => tracing::warn!("nostr sync: expired-event sweep failed: {e}"),
        }
        drop(conn);
        inbound_lifecycle::sweep_expired_inbound(&self.db).await;
    }

    async fn handle_outbound(
        &self,
        relay_connections: &mut HashMap<String, RelayClient>,
        outbound: OutboundEvent,
    ) {
        for relay_url in &outbound.relay_urls {
            let client = match get_or_connect(relay_connections, relay_url, self.dial_policy).await
            {
                Some(c) => c,
                None => continue,
            };

            match client.publish(outbound.event.clone()).await {
                Ok(true) => {
                    tracing::debug!("nostr sync: published {} to {relay_url}", outbound.event.id);
                }
                Ok(false) => {
                    tracing::warn!(
                        "nostr sync: relay {relay_url} rejected event {}",
                        outbound.event.id
                    );
                }
                Err(e) => {
                    tracing::warn!("nostr sync: publish to {relay_url} failed: {e}");
                    relay_connections.remove(relay_url);
                }
            }
        }
    }

    async fn refresh_inbound_subscriptions(
        &self,
        relay_connections: &mut HashMap<String, RelayClient>,
        active_subscriptions: &mut HashMap<String, Vec<String>>,
    ) {
        // Get all follows across all local users
        let conn = self.db.conn().await;
        let follows = {
            // Collect follows from users who opted into inbound_to_feed AND
            // deposited an nsec — bridging runs per-user on the deposit trust
            // act (`nostr.md` § The bridging gate), and only a deposited key
            // can unwrap the gift-wrap DMs the subscription also pulls.
            let accounts: Vec<db::NostrAccount> = conn
                .prepare(
                    "SELECT actor_id, nostr_pubkey, signing_mode, encrypted_privkey,
                            nip46_bunker_url, relay_list, expose_content, auto_publish,
                            publish_replies, publish_reactions, inbound_to_feed,
                            created_at, updated_at
                     FROM nostr_accounts
                     WHERE inbound_to_feed = 1 AND encrypted_privkey IS NOT NULL",
                )
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| {
                        Ok(db::NostrAccount {
                            actor_id: row.get(0)?,
                            nostr_pubkey: row.get(1)?,
                            signing_mode: row.get(2)?,
                            encrypted_privkey: row.get(3)?,
                            nip46_bunker_url: row.get(4)?,
                            relay_list: row.get(5)?,
                            expose_content: row.get::<_, i32>(6)? != 0,
                            auto_publish: row.get::<_, i32>(7)? != 0,
                            publish_replies: row.get::<_, i32>(8)? != 0,
                            publish_reactions: row.get::<_, i32>(9)? != 0,
                            inbound_to_feed: row.get::<_, i32>(10)? != 0,
                            created_at: row.get(11)?,
                            updated_at: row.get(12)?,
                        })
                    })
                    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                })
                .unwrap_or_default();

            let mut all_follows = Vec::new();
            for acct in &accounts {
                if let Ok(f) = db::list_follows(&conn, &acct.actor_id) {
                    all_follows.extend(f);
                }
            }
            all_follows
        };
        drop(conn);

        if follows.is_empty() {
            return;
        }

        // Group follows by relay
        let mut relay_to_pubkeys: HashMap<String, Vec<String>> = HashMap::new();
        for follow in &follows {
            let relays = resolve_relay_hints(follow.relay_hints.as_deref());

            for relay_url in relays {
                relay_to_pubkeys
                    .entry(relay_url)
                    .or_default()
                    .push(follow.nostr_pubkey.clone());
            }
        }

        // Subscribe on each relay
        for (relay_url, pubkeys) in &relay_to_pubkeys {
            let client = match get_or_connect(relay_connections, relay_url, self.dial_policy).await
            {
                Some(c) => c,
                None => continue,
            };

            // Check last_seen for catch-up
            let conn = self.db.conn().await;
            let since = pubkeys
                .iter()
                .filter_map(|pk| db::get_relay_state(&conn, relay_url, pk).ok().flatten())
                .min();
            drop(conn);

            // char-based truncation: a byte slice panics mid-char on a
            // non-ASCII (IDN) relay URL, and follow relay hints are external
            // data; this runs inside the worker loop.
            let sub_id: String = format!("fauna-inbound-{relay_url}")
                .chars()
                .take(30)
                .collect();

            // Close previous subscription if any
            if let Some(old_subs) = active_subscriptions.get(relay_url) {
                for old_sub in old_subs {
                    let _ = client.close_subscription(old_sub).await;
                }
            }

            let post_filter = Filter {
                authors: Some(pubkeys.clone()),
                since: since.map(|s| s as u64),
                kinds: Some(vec![1, 5, 6, 7, 34550, 30402, 30311, 30009, 8]), // text, deletion, repost, reaction, community, classified, live-activity, badge-def, badge-award
                limit: Some(200),
                ..Default::default()
            };

            // The bridged-author filter (`bridges.md` § Unified feed ingestion
            // → *Bridged authors*): each followed author's kind 0. Deliberately
            // **no `since`** — kind 0 is replaceable, so the relay serves each
            // author's one current profile, and the posts' catch-up cursor
            // would skip a profile older than itself (the same trap the
            // freshness window fell into, `process_inbound_event`). Bounded by
            // the author count, not by a window.
            let metadata_filter = Filter {
                authors: Some(pubkeys.clone()),
                kinds: Some(vec![fauna_bridge_nostr::types::kind::METADATA]),
                limit: Some(pubkeys.len().max(1) as u64),
                ..Default::default()
            };

            let mut zap_tags = std::collections::HashMap::new();
            zap_tags.insert("#p".to_string(), pubkeys.clone());
            let zap_filter = Filter {
                kinds: Some(vec![9735]),
                tags: zap_tags,
                since: since.map(|s| s as u64),
                limit: Some(100),
                ..Default::default()
            };

            // NIP-17 gift-wrap DMs (kind 1059) addressed to the subscribed pubkeys.
            let mut dm_tags = std::collections::HashMap::new();
            dm_tags.insert("#p".to_string(), pubkeys.clone());
            let dm_filter = Filter {
                kinds: Some(vec![1059]),
                tags: dm_tags,
                since: since.map(|s| s as u64),
                limit: Some(200),
                ..Default::default()
            };

            if let Err(e) = client
                .subscribe(
                    &sub_id,
                    vec![post_filter, metadata_filter, zap_filter, dm_filter],
                )
                .await
            {
                tracing::warn!("nostr sync: subscribe on {relay_url} failed: {e}");
                relay_connections.remove(relay_url);
                continue;
            }

            active_subscriptions.insert(relay_url.clone(), vec![sub_id]);
        }
    }

    /// Refresh the NIP-46 bunker proxy subscription (R10) on each paired public
    /// serving box. Gate: this head hosts at least one bunker signer **and**
    /// holds a `nostr_push` pairing (whose `nest_url` names the public box). For
    /// each such box, (re-)subscribe `kinds:[24133], #p:[all local signer
    /// pubkeys]` — a REQ with a stable id replaces the prior filter, so this is
    /// idempotent per tick. A box with no local signers, or no such pairing,
    /// subscribes to nothing (the head-only, keyless, or unpaired cases).
    /// Reconcile the set of spawned [`bunker_drain_task`]s against the current
    /// bunker-signer roster + `nostr_push` pairings (R10): one dedicated
    /// low-latency drain per paired public serving box while ≥1 local signer
    /// exists; kill drains whose pairing (or the whole roster) went away;
    /// respawn on a dead task (connection drop — reconnect within one tick) or
    /// a roster change (the `#p` filter is baked into the running task's
    /// subscription, so a mint/unlink means resubscribe-by-respawn; the wake
    /// arm in [`Self::run`] makes the mint case immediate).
    async fn reconcile_bunker_drains(&self, drains: &mut HashMap<String, BunkerDrain>) {
        // (a) local bunker signers — the `#p` filter, sorted so it doubles as
        // the respawn fingerprint. Empty → no drains at all.
        let mut signer_pubkeys = {
            let conn = self.db.conn().await;
            db::list_bunker_signer_pubkeys(&conn).unwrap_or_default()
        };
        signer_pubkeys.sort();
        // (b) the paired public serving box relay URL(s), from `nostr_push`
        // pairings' `nest_url` (an https base → `wss://host/nostr`).
        let peer_relays: std::collections::HashSet<String> = if signer_pubkeys.is_empty() {
            Default::default()
        } else {
            self.db
                .list_pairings_with_capability(fauna_protocol::pair::capability::NOSTR_PUSH)
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|p| p.nest_url.as_deref().and_then(nest_url_to_relay_url))
                .collect()
        };

        // Dropping the record aborts its task (`impl Drop for BunkerDrain`), so
        // every removal below — de-paired relay, respawn replace — kills the
        // old drain by construction rather than by an abort call beside it.
        drains.retain(|relay_url, _| peer_relays.contains(relay_url));

        for relay_url in peer_relays {
            let respawn = match drains.get(&relay_url) {
                Some(d) => d.task.is_finished() || d.signer_fingerprint != signer_pubkeys,
                None => true,
            };
            if !respawn {
                continue;
            }
            drains.remove(&relay_url);
            // spawn-ok(returns-handle-for-scope): the handle is stored in
            // `BunkerDrain`, whose `Drop` aborts it — so the task dies with the
            // worker future the generation scope cancels. Not `spawn_scoped`:
            // the worker deliberately reconciles (respawns on roster change,
            // reconnects on a dead task) *within* a generation, which needs an
            // abortable handle it owns, and adopting each drain into
            // `serve_tasks` would leak one supervisor per respawn.
            let task = tokio::spawn(bunker_drain_task(
                self.db.clone(),
                self.nest_signing_key_bytes,
                relay_url.clone(),
                signer_pubkeys.clone(),
                self.dial_policy,
            ));
            drains.insert(
                relay_url,
                BunkerDrain {
                    signer_fingerprint: signer_pubkeys.clone(),
                    task,
                },
            );
        }
    }

    /// Thin method wrapper over the free [`respond_to_bunker_request`] — kept
    /// for the defensive kind-24133 arm in [`Self::drain_inbound_events`].
    async fn build_bunker_response(&self, request: &Event) -> Option<Event> {
        respond_to_bunker_request(&self.db, &self.nest_signing_key_bytes, request).await
    }

    async fn drain_inbound_events(&self, relay_connections: &mut HashMap<String, RelayClient>) {
        let relay_urls: Vec<String> = relay_connections.keys().cloned().collect();
        for relay_url in relay_urls {
            let client = match relay_connections.get_mut(&relay_url) {
                Some(c) => c,
                None => continue,
            };

            // Non-blocking receive — drain available messages
            loop {
                let recv =
                    tokio::time::timeout(std::time::Duration::from_millis(10), client.recv()).await;

                match recv {
                    Ok(Ok(Some(RelayMessage::Event { event, .. }))) => {
                        if event.kind == 24133 {
                            // Defensive arm: the bunker proxy leg normally rides
                            // its own dedicated connection ([`bunker_drain_task`]
                            // — these shared connections no longer subscribe to
                            // kind 24133), but a stray transport event must
                            // never fall into `process_inbound_event`'s store
                            // path (ephemeral kinds are transported, not
                            // stored). Answer it here the same way — `send`,
                            // never `publish` (publish awaits its own OK and
                            // would swallow this live subscription's
                            // interleaved events).
                            if let Some(response) = self.build_bunker_response(&event).await
                                && let Err(e) = client.send(&ClientMessage::Event(response)).await
                            {
                                tracing::warn!(
                                    "nostr bunker proxy: publish response to {relay_url} failed: {e}"
                                );
                                relay_connections.remove(&relay_url);
                                break;
                            }
                        } else {
                            self.process_inbound_event(&relay_url, event).await;
                        }
                    }
                    Ok(Ok(Some(RelayMessage::Eose(_)))) => {
                        // End of stored events, stop draining this relay
                        break;
                    }
                    Ok(Ok(Some(_))) => continue, // OK, NOTICE, AUTH — skip
                    Ok(Ok(None)) => {
                        // Connection closed
                        tracing::warn!("nostr sync: relay {relay_url} disconnected");
                        relay_connections.remove(&relay_url);
                        break;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("nostr sync: relay {relay_url} error: {e}");
                        relay_connections.remove(&relay_url);
                        break;
                    }
                    Err(_) => break, // timeout — no more messages
                }
            }
        }
    }

    /// Try to unwrap a NIP-17 gift wrap for any local account that is the
    /// recipient. Thin method wrapper over the free [`process_gift_wrap_inbound`]
    /// — the `/nostr` relay's unauthenticated gift-wrap inbox (slice B) calls
    /// that same seam directly (it has no `NostrSyncWorker`, only an `AppState`),
    /// so both delivery paths feed one S8.9 seal-at-rest implementation.
    async fn process_gift_wrap(&self, event: Event) {
        process_gift_wrap_inbound(&self.db, &self.nest_signing_key_bytes, event).await;
    }

    async fn process_inbound_event(&self, relay_url: &str, event: Event) {
        // Verify signature
        if !verify_event(&event) {
            tracing::debug!(
                "nostr sync: rejected invalid event {} from {relay_url}",
                event.id
            );
            return;
        }

        // Refuse a **future-dated** event. There is deliberately no lower
        // bound: this arm is not a freshness window, it is an anti-abuse rule
        // with its own justification — the feed is recency-ordered, so an
        // event claiming to be from next week pins itself to the top of it
        // forever.
        //
        // ⚠ **There used to be a symmetric ±1h lower arm, and it discarded
        // exactly what the sweep exists to fetch.** `refresh_inbound_
        // subscriptions` builds each filter's `since` from a per-`(relay,
        // pubkey)` cursor whose whole purpose is to re-request what this nest
        // missed, and a fresh nest with no cursor takes `limit: 200` of
        // whatever the relay holds — so a nest offline for two hours pulled
        // its backlog and dropped every event of it one function later, at
        // debug level, and a followed author's post from yesterday was never
        // swept at all. Worse, the cursor only ever advances on a *successful*
        // ingest, so the window also kept it from being established: nothing
        // but the live tail could bootstrap it.
        //
        // Age is the wrong axis and no horizon constant replaces it. Volume is
        // already bounded by the relay's newest-first `limit` and by the
        // author gate below, and a translated post carries the **event's own**
        // `created_at`, so a backfill sorts into the feed's recency order
        // rather than flooding its top. A horizon would buy no bound and add a
        // silent failure — a low-traffic author whose last post predates it
        // reads as an empty feed on a fresh nest. `nostr.md` § Implementation
        // status today owns the ruling; ActivityPub's inbox, the sibling
        // ingest plane, likewise applies no lower age bound.
        //
        // ⚠ **Kind 1059 is exempt from even the upper arm, and must be** —
        // NIP-59 *requires* a gift wrap's outer `created_at` be randomized
        // (this codebase's own `nip17::randomize_timestamp` picks ±48h)
        // precisely so a relay cannot time-correlate DMs, and that draw runs
        // in both directions. The nest's own relay inbox (slice B,
        // `store::store_event`) never applied a window at all.
        //
        // The bound itself is the bridged planes' shared one, the same the
        // ActivityPub inbox applies to a Note: one comparison, one cushion
        // (`feed.md` § The read model owns both).
        if event.kind != 1059
            && crate::storage::reject_future_bridged_created_at(fauna_core::data::Timestamp(
                event.created_at.saturating_mul(1_000_000),
            ))
            .is_err()
        {
            tracing::debug!(
                "nostr sync: rejected future-dated event {} from {relay_url}",
                event.id
            );
            return;
        }

        // Validate size
        if event.content.len() > 65536 {
            tracing::debug!(
                "nostr sync: rejected oversized event {} from {relay_url}",
                event.id
            );
            return;
        }

        // Check if we already have this event
        let conn = self.db.conn().await;
        if db::get_event_by_nostr_id(&conn, &event.id)
            .ok()
            .flatten()
            .is_some()
        {
            drop(conn);
            return; // already stored
        }

        // Handle gift-wrapped DMs (kind 1059 — NIP-17).
        // These are addressed to local accounts, not authored by them.
        if event.kind == 1059 {
            drop(conn);
            self.process_gift_wrap(event).await;
            return;
        }

        // Handle zap receipts (kind 9735) — authored by Lightning service providers,
        // not by followed users, so they cannot be translated to Fauna posts.
        //
        // **Ingress A of the NIP-57 trust gate** (`monetization.md` § Zap
        // receipts — the trust model). The event's own signature is already
        // verified above, and it buys the receipt nothing: a kind-9735 is
        // signed by the *recipient's* LNURL/wallet server — a key Fauna knows
        // nothing about a priori — and is plain signed JSON anyone may mint
        // naming any recipient, whose `bolt11` no part of this system checks
        // against a real Lightning payment. This subscription is filtered by
        // `#p` only, with no `authors` constraint, so without this gate any
        // signature-valid kind-9735 naming a local pubkey on any relay we read
        // would be believed and summed.
        //
        // The gate is applied here at ingest rather than at read, per the
        // ratified rule: filtering at read would leave forged rows resting on
        // the box and would have to be re-applied by every future reader.
        // Excised with the `zaps` member: with this arm gone a kind-9735 falls
        // through to the author gate below, and a zap receipt's author is the
        // payee's LNURL/wallet server — a key nobody follows — so it is dropped
        // there. "No listener" reached by removing the door, not by adding a
        // refusing arm (a `cfg(not(...))` inside this `nostr`-gated subtree would
        // be dark to both arms of `nest-lib-test-check`).
        #[cfg(feature = "zaps")]
        if event.kind == 9735 {
            match crate::nostr::zap_ingest::classify_incoming_zap(&conn, &event) {
                fauna_bridge_nostr::nip57::ZapVerdict::Trusted(zap) => {
                    // Resolve the Fauna coordinates while the connection is
                    // held, then hand off: the purchase judgement needs the
                    // tier row behind the async `CacheDb`, which is this same
                    // mutex (`monetization.md` § Per-post pay-to-unlock — the
                    // tip↔purchase split).
                    let subject = crate::nostr::zap_ingest::resolve_zap_subject(&conn, &zap);
                    drop(conn);

                    // **Gate surface `zaps.receipt.ingest`** — the same shared
                    // decision ingress B makes, so the two doors cannot drift
                    // on *whether the payee may run this plane* any more than
                    // they can on *whose signature counts*. A refusal drops the
                    // receipt: nothing recorded, nothing summed, no purchase.
                    //
                    // No `AppState` means the policy plane is unreadable, and
                    // § Fail posture is explicit that an unreadable gate refuses
                    // the operation rather than admitting it. The production
                    // spawn always attaches state (`with_state`), and so does
                    // the test constructor — a stateless worker is not a shape
                    // this arm silently treats as ungated.
                    let Some(state) = self.state.as_ref() else {
                        tracing::warn!(
                            event_id = %event.id,
                            "dropping zap receipt: no app state, so the feature gate cannot be evaluated"
                        );
                        return;
                    };
                    if let Err(e) = crate::nostr::zap_ingest::gate_receipt_ingest(state, &zap).await
                    {
                        tracing::debug!(
                            event_id = %event.id,
                            code = %e.code,
                            "zap receipt refused by the feature gate (ingress A)"
                        );
                        return;
                    }

                    let purchased = match subject.as_ref() {
                        Some(s) => {
                            crate::nostr::zap_ingest::apply_zap_purchase(state, &zap, s).await
                        }
                        None => None,
                    };
                    let conn = self.db.conn().await;
                    crate::nostr::zap_ingest::record_zap_as(&conn, &zap, purchased.as_deref());
                    drop(conn);
                    return;
                }
                fauna_bridge_nostr::nip57::ZapVerdict::Untrusted(reason) => {
                    // Dropped, never stored. Logged at debug because an
                    // undesignated receipt is the ordinary case on a public
                    // relay, not an incident — the payee simply has not opted
                    // this signer in.
                    tracing::debug!(
                        event_id = %event.id,
                        signer = %event.pubkey,
                        ?reason,
                        "dropping untrusted zap receipt (ingress A)"
                    );
                }
            }
            drop(conn);
            return;
        }

        // ── The author gate ────────────────────────────────────────
        //
        // Everything below this line is served by the subscription's
        // `post_filter`, the one filter carrying an `authors:` constraint — and
        // that constraint is enforced by the **relay**, which is an untrusted
        // party (its URL comes partly from external `follow.relay_hints`).
        // Nothing above re-checks it: a real event from a stranger passes the
        // signature verify, the freshness window, the size cap and dedup
        // identically to one from a followed author, so a hostile or buggy
        // relay could rest arbitrary authors' content in `content` and —
        // since a later change — in the user's Search corpus.
        //
        // Applied at ingest rather than at read, per the rule the kind-9735 arm
        // above already cites: filtering at read leaves forged rows resting on
        // the box, to be re-filtered by every future reader.
        //
        // **Why here and not at the top of this function:** the two arms above
        // are `#p`-addressed, not author-addressed, and are author-unconstrained
        // *by design* — a NIP-59 gift wrap is signed by a one-time throwaway
        // key that is nobody's follow, and a kind-9735 receipt is signed by the
        // payee's LNURL server. Each already carries its own gate (the
        // recipient's seal key; ingress A's designation check). A follow check
        // hoisted above them would kill every inbound DM and every zap.
        if !db::is_followed_by_sweeping_account(&conn, &event.pubkey) {
            drop(conn);
            tracing::debug!(
                event_id = %event.id,
                author = %&event.pubkey[..8.min(event.pubkey.len())],
                %relay_url,
                "dropping event from an author no sweeping account follows"
            );
            return;
        }

        // A followed author's kind-0 metadata (`bridges.md` § Unified feed
        // ingestion → *Bridged authors*): their face, projected under the
        // synthetic id their notes rest under. Below the author gate on
        // purpose — kind 0 rides the author-constrained `metadata_filter`, so
        // a stranger's is dropped exactly like a stranger's note. Never an
        // event row, never a post, never a cursor advance.
        if event.kind == fauna_bridge_nostr::types::kind::METADATA {
            if let Err(e) = inbound_lifecycle::ingest_metadata_event(&conn, &event) {
                tracing::warn!(
                    "nostr sync: project kind-0 metadata {} failed: {e:#}",
                    event.id
                );
            }
            drop(conn);
            return;
        }

        // Handle deletion requests (kind 5 — NIP-09) against the SWEEP plane.
        //
        // The subscription has always asked for kind 5, and until 2026-08-02
        // nothing applied one: `translate::nostr_event_to_fauna` bails on the
        // kind, so a followed author's deletion fell through to the translate
        // arm below and was dropped as a debug-level "translate failed" — the
        // author retracted, and their post rested here indefinitely. This is
        // the arm that honours it (`nostr::inbound_lifecycle`, whose lookups
        // are author-scoped and `inbound`-only, so a remote kind-5 can never
        // reach a local user's own post).
        if event.kind == 5 {
            drop(conn);
            inbound_lifecycle::apply_inbound_deletion(&self.db, &event).await;
            return;
        }

        // Handle badge awards (kind 8 — NIP-58).
        if event.kind == 8 {
            if let Ok(award) = fauna_bridge_nostr::nip58::parse_badge_award(&event) {
                for awardee in &award.awardees {
                    let _ = db::insert_badge(
                        &conn,
                        &award.badge_definition,
                        None,
                        None,
                        awardee,
                        event.created_at as i64,
                    );
                }
            }
            drop(conn);
            return;
        }

        drop(conn);

        // Translate to a Fauna post and record it — the write half lives in
        // `inbound_lifecycle` beside this plane's NIP-09 and NIP-40 arms, so
        // the whole ingest contract (expiry rejection, the map row, the
        // Search-corpus hook) is one testable function rather than a block
        // reachable only from a live relay connection.
        match inbound_lifecycle::ingest_translated_event(&self.db, &self.post_segments, &event)
            .await
        {
            Ok(Some(_post_id)) => {
                let conn = self.db.conn().await;
                let _ = db::upsert_relay_state(
                    &conn,
                    relay_url,
                    &event.pubkey,
                    event.created_at as i64,
                );
                drop(conn);
                tracing::debug!(
                    "nostr sync: stored inbound event {} from {}",
                    event.id,
                    &event.pubkey[..8]
                );
            }
            // Not stored (already expired, or untranslatable) — the callee logs
            // which. Deliberately no `upsert_relay_state`: the catch-up cursor
            // must not advance past an event this nest chose not to keep.
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "nostr sync: ingest inbound event {} failed: {e:#}",
                    event.id
                );
            }
        }
    }
}

/// Narrow an untrusted, counterparty-chosen Unix timestamp to `i64` for
/// storage, WITHOUT wraparound. A bare `as i64` on a `u64` >= 2^63 produces a
/// negative value — exactly the kind of silent corruption a sender-controlled
/// field must never be allowed to cause. Saturates at `i64::MAX` instead: closed by construction, not a
/// range check bolted on after the fact.
fn clamp_u64_timestamp_to_i64(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

/// Unwrap a NIP-17 gift wrap for the local account named in its `p` tag, seal
/// the plaintext through the D2 resolver, and deposit it in the bridged
/// conversation family through the Nostr leg ([`crate::nostr::bridge_leg`];
/// the plaintext is discarded). Shared by the sync worker's inbound relay drain
/// ([`NostrSyncWorker::process_gift_wrap`]) **and** the `/nostr` relay's
/// unauthenticated gift-wrap inbox (slice B) — both delivery paths feed this one
/// S8.9 seal-at-rest seam. The deposit licenses the in-flight unwrap, not
/// plaintext at rest: a recipient with no MSEK-derived seal key on file gets no
/// stored row (fail-closed), never a plaintext one.
pub(crate) async fn process_gift_wrap_inbound(
    db: &CacheDb,
    nest_signing_key_bytes: &[u8; 32],
    event: Event,
) {
    // The gift wrap's `p` tag identifies the recipient pubkey.
    let recipient_pubkey = event.tags.iter().find_map(|t| {
        if t.name() == Some("p") {
            t.value().map(|v| v.to_string())
        } else {
            None
        }
    });

    let recipient_pubkey = match recipient_pubkey {
        Some(pk) => pk,
        None => {
            tracing::debug!("nostr sync: gift wrap {} has no p tag", event.id);
            return;
        }
    };

    // Look up the local account for this recipient pubkey.
    let conn = db.conn().await;
    let acct = match db::get_account_by_pubkey(&conn, &recipient_pubkey) {
        Ok(Some(a)) => a,
        Ok(None) => {
            drop(conn);
            return; // not a local account
        }
        Err(e) => {
            tracing::warn!("nostr sync: gift wrap lookup failed: {e}");
            drop(conn);
            return;
        }
    };

    // Only accounts with a stored private key can decrypt.
    let encrypted_privkey = match acct.encrypted_privkey {
        Some(ref ct) => ct.clone(),
        None => {
            drop(conn);
            return;
        }
    };

    drop(conn);
    let actor_id: [u8; 32] = match fauna_core::hex32::decode(&acct.actor_id) {
        Ok(b) => b,
        Err(_) => {
            tracing::warn!(
                "nostr sync: gift wrap {}: malformed account actor_id",
                event.id
            );
            return;
        }
    };

    // The DM plane is the bridged family's, deposited through the leg seam
    // (`bridge_legs::deposit_gated`), which dedupes on the wrap's event id and holds
    // the family's per-principal and store-wide inbound caps — so every
    // delivery path (the /nostr inbox, the external-relay drain, any future
    // caller) inherits both, even where the `nostr_events`-side caps can't see
    // it (a duplicate resend dedupes there; NIP-40 expiry churn is swept there
    // while DM rows rightly persist).
    // Asked here too, before the nsec decrypt — no unwrap cost for a wrap the
    // deposit would refuse.
    match crate::bridge_legs::precheck(db, &crate::bridge_legs::NOSTR, &actor_id, &event.id).await {
        Ok(Some(crate::db::bridged_conversations::DepositPrecheck::Duplicate)) => {
            tracing::debug!("nostr sync: gift wrap {} already sealed — skip", event.id);
            return;
        }
        Ok(Some(crate::db::bridged_conversations::DepositPrecheck::Full)) => {
            tracing::warn!(
                "nostr sync: gift wrap {}: recipient's DM plane is at capacity — \
                 DM not stored (the wrap stays fetchable from the relay)",
                event.id
            );
            return;
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!("nostr sync: gift wrap {} precheck failed: {e}", event.id);
        }
    }

    let secret_bytes = match crate::nostr::key_crypto::decrypt_nostr_privkey(
        nest_signing_key_bytes,
        &encrypted_privkey,
    ) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("nostr sync: gift wrap decrypt privkey failed: {e}");
            return;
        }
    };

    let dm = match fauna_bridge_nostr::nip17::unwrap_gift_wrap(&secret_bytes, &event) {
        Ok(dm) => dm,
        Err(e) => {
            tracing::debug!("nostr sync: unwrap gift wrap {} failed: {e}", event.id);
            return;
        }
    };

    // Determine the peer — the identity every downstream gate keys on.
    //
    // ⚠ **THE DISCRIMINATOR MUST BE AUTHENTICATED, AND ONLY ONE OF THESE TWO
    // FIELDS IS**.
    // `dm.sender_pubkey` is the SEAL's pubkey, and the seal's content was just
    // NIP-44-opened with `our_secret × that pubkey` — an AEAD open, so a wrong
    // pubkey cannot decrypt and the value is proven. `dm.recipient_pubkey` is
    // the RUMOR's `p` tag: plaintext inside the seal, chosen freely by whoever
    // sent it, checked by nothing.
    //
    // This branch used to ask `dm.recipient_pubkey == recipient_pubkey`, i.e.
    // it let the sender pick which arm ran AND, in the `else` arm, supply the
    // peer identity itself. A guardian-`block`ed peer spelling the rumor's `p`
    // tag as anything but the ward's canonical pubkey — uppercase, or absent
    // (which arrived here as `""`) — took the `else` arm, the verdict lookup
    // below keyed on their own string, found no `block`, and the DM was sealed
    // and stored. That breaks `family-safety.md` § The bridge-DM gate's
    // "a `block`ed peer's new DMs never land" outright, with no fault, race, or
    // cooperation from the ward.
    //
    // So the question the branch asks is now "did WE send this?", answered from
    // the authenticated half alone. The `else` arm survives and is still
    // meaningful: a NIP-17 sender wraps a copy of their own outbound DM back to
    // themselves, so a wrap we are the outer recipient of may be our own sent
    // copy — and for that one, the peer really is the rumor's recipient. But we
    // reach that arm only when the SEAL proves we authored it, which no third
    // party can forge (they would need our secret to produce a ciphertext that
    // opens under `our_secret × our_pubkey`).
    //
    // Both sides are canonicalized before they are compared or stored: the
    // values come off the wire verbatim, so their spelling is the relay's
    // choice, and a raw `==` here would be spelling-sensitive for exactly the
    // reason the outbound send was. A pubkey this
    // bridge cannot parse as 32-byte hex falls back to its raw string.
    let canonical =
        |pk: &str| crate::nostr::canonical_peer_pubkey(pk).unwrap_or_else(|| pk.to_string());
    let own_pubkey = canonical(&acct.nostr_pubkey);
    let sender_pubkey = canonical(&dm.sender_pubkey);
    let peer_pubkey = if sender_pubkey == own_pubkey {
        // Our own sent copy: the counterparty is the rumor's recipient. Safe to
        // trust here precisely because the seal proves we wrote it.
        canonical(&dm.recipient_pubkey)
    } else {
        sender_pubkey.clone()
    };

    // S8.9: the deposit licenses the in-flight unwrap above, not plaintext at
    // rest — the DM body is sealed to the recipient's MSEK-derived key through
    // the D2 resolver before storage (below), fail-closed (a recipient with no
    // seal key on file gets no stored row, never a plaintext one; the gift wrap
    // stays fetchable from the relays).
    //
    // The family bridge-DM gate (`family-safety.md` § The bridge-DM gate).
    //
    // Composed HERE, at the one shared inbound seam, and deliberately not at
    // either caller: both live delivery paths — the sync worker's relay drain
    // and the `/nostr` relay's unauthenticated gift-wrap inbox — funnel through
    // this function, and a gate installed on one while a sibling wrote the same
    // store is exactly the bypass class.
    // § Enforcement points' standing rule binds every future inbound bridge-DM
    // write path the moment it goes live.
    //
    // Only `Blocked` acts here, and it acts *before storage* — the RCPT-reject
    // analogue: an explicit guardian deny refuses NEW arrivals, and never
    // touches a stored row. `Held` deliberately stores exactly as `Deliver`
    // does: the hold is computed at read time from (knob, verdict row), never
    // stored, so relaxing the knob releases every held conversation by
    // construction (§ Don't do these — "don't store bridge-DM hold state").
    //
    // A read failure declines to store rather than storing past an unread
    // policy: the gift wrap stays fetchable from the relay, so this loses no
    // message, and it matches the seal-key fail-closed arm.
    //
    // The verdict, the seal and the deposit are the first-party leg seam's
    // (`bridge_legs::deposit_gated`) — one copy for all three legs. What this
    // function owns is the half no other leg shares: WHICH identity the
    // verdict keys on, proven above.
    //
    // `dm.created_at` is the RUMOR's
    // `created_at` — plaintext inside the seal, signed by nobody, chosen
    // freely by the sender. It is worth storing for display, but it must
    // never be load-bearing for order or pagination (that's `received_at`,
    // the nest's own clock) — and even as a display value it must not be
    // allowed to wrap negative on the `u64` -> `i64` narrowing (a
    // `created_at` >= 2^63 would otherwise sort beneath everything and fall
    // outside a `before` cursor's window). Saturating, not a bolted-on range
    // check: the conversion itself cannot produce an invalid value.
    let display_created_at = clamp_u64_timestamp_to_i64(dm.created_at);

    use crate::bridge_legs::Inbound;
    let inbound = crate::bridge_legs::InboundDm {
        peer: &peer_pubkey,
        sender: &sender_pubkey,
        self_address: &own_pubkey,
        far_message_id: &event.id,
        plaintext: dm.content.as_bytes(),
        created_at_ms: display_created_at.saturating_mul(1000),
    };
    match crate::bridge_legs::deposit_gated(db, &crate::bridge_legs::NOSTR, &actor_id, &inbound)
        .await
    {
        Ok(Inbound::Stored) => tracing::debug!(
            "nostr sync: stored inbound DM {} (sealed) from {}",
            event.id,
            &dm.sender_pubkey[..8.min(dm.sender_pubkey.len())]
        ),
        Ok(Inbound::Duplicate) => {
            tracing::debug!("nostr sync: gift wrap {} already sealed — skip", event.id);
        }
        Ok(Inbound::Blocked) => tracing::debug!(
            "nostr sync: gift wrap {}: peer blocked by guardian — DM not stored",
            event.id
        ),
        Ok(Inbound::NoSealKey) => tracing::warn!(
            "nostr sync: gift wrap {}: recipient has no seal key on file — \
             DM not stored (fail closed)",
            event.id
        ),
        Ok(Inbound::Full) => tracing::warn!(
            "nostr sync: gift wrap {}: recipient's DM plane is at capacity — \
             DM not stored (the wrap stays fetchable from the relay)",
            event.id
        ),
        Err(e) => tracing::warn!(
            "nostr sync: gift wrap {}: {e:#} — DM not stored (fail closed; the wrap \
             stays fetchable from the relay)",
            event.id
        ),
    }
}

/// One spawned low-latency NIP-46 drain (R10) on a paired public serving
/// box's relay, tracked by [`NostrSyncWorker::reconcile_bunker_drains`].
struct BunkerDrain {
    /// The sorted signer-pubkey roster baked into the running task's `#p`
    /// filter — a mismatch against the current roster means respawn.
    signer_fingerprint: Vec<String>,
    task: tokio::task::JoinHandle<()>,
}

/// **The drain dies with its record — structurally, not by anyone remembering
/// to abort it** (the defect class named as the one a
/// spawn-grepping ratchet structurally cannot see).
///
/// A `JoinHandle` **detaches** on drop; it does not abort. The worker used to
/// rely on an explicit abort loop after its `run()` select-loop `break`, which
/// looks complete and is reachable — but only on the channels-closed path.
/// The generation teardown does not take that path: [`AppState::spawn_scoped`]
/// wraps the worker in a `select!` against the generation token, so a cancel
/// **drops** `run()`'s future mid-await. The `break` never executes, the
/// cleanup loop never runs, and every drain's `JoinHandle` is dropped —
/// detaching a task that holds `nest_signing_key_bytes` and a live relay
/// connection.
///
/// After a deployment-seed rotation that is a task answering NIP-46 signing
/// requests on a **public** relay with the **superseded** key, indefinitely,
/// beside the successor generation's own drain on the same relay with the same
/// `#p` filter — against `box-recovery.md` § No dual serving, and invisible to
/// every other gate.
///
/// Owning the abort here makes it hold for *every* way a record leaves the
/// map — reconcile drop, respawn replace, worker exit, worker cancel — which
/// is why the eager `abort()` call sites were removed rather than kept beside
/// it: two mechanisms for one invariant is how the cancel path got missed.
impl Drop for BunkerDrain {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The dedicated low-latency NIP-46 bunker drain (R10): own connection to the
/// paired public box's relay, a standing `kinds:[24133], #p:[our signers]`
/// subscription, and a blocking `recv()` loop — a request is answered within
/// transport latency, never the worker's 60s tick (NIP-46 is interactive; SDK
/// client timeouts are ~10s — the tier_3 T6 proof pins the bound). The `#p`
/// filter is load-bearing DoS containment: a subscription for *all* 24133s
/// would let anyone spamming the public box's open ephemeral fall-through
/// (rate-limited only per p-tag key there) flood this private head.
///
/// Responses go back over THIS same connection via `send()`, never
/// `publish()` (publish awaits its own OK and would swallow the live
/// subscription's interleaved events; the loop reads the peer's OK frames and
/// skips them). On any connection error the task just returns — the
/// reconciler respawns it on the next tick (≤60s reconnect; steady-state
/// latency is what matters, and a respawn also re-fires on the next mint's
/// wake nudge).
async fn bunker_drain_task(
    db: Arc<CacheDb>,
    nest_key: [u8; 32],
    relay_url: String,
    signer_pubkeys: Vec<String>,
    dial_policy: RelayDialPolicy,
) {
    // The paired serving box's URL is what the pairing recorded — verified
    // under the worker's policy like every other relay.
    let mut client = match RelayClient::connect(&relay_url, dial_policy).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("nostr bunker drain: connect to {relay_url} failed: {e}");
            return;
        }
    };
    let mut tags = HashMap::new();
    tags.insert("#p".to_string(), signer_pubkeys);
    let filter = Filter {
        kinds: Some(vec![24133]),
        tags,
        ..Default::default()
    };
    // One subscription per dedicated connection — a constant id suffices.
    if let Err(e) = client.subscribe("fauna-bunker", vec![filter]).await {
        tracing::warn!("nostr bunker drain: subscribe on {relay_url} failed: {e}");
        return;
    }
    loop {
        match client.recv().await {
            Ok(Some(RelayMessage::Event { event, .. })) if event.kind == 24133 => {
                // Membership + signature re-verified inside; a miss is ignored
                // (another head's traffic on a shared public box).
                if let Some(response) = respond_to_bunker_request(&db, &nest_key, &event).await
                    && let Err(e) = client.send(&ClientMessage::Event(response)).await
                {
                    tracing::warn!("nostr bunker drain: response send to {relay_url} failed: {e}");
                    return;
                }
            }
            Ok(Some(_)) => continue, // EOSE, our own responses' OKs, NOTICE
            Ok(None) => {
                tracing::warn!("nostr bunker drain: {relay_url} disconnected");
                return;
            }
            Err(e) => {
                tracing::warn!("nostr bunker drain: {relay_url} error: {e}");
                return;
            }
        }
    }
}

/// Build the signer-authored NIP-46 response for a kind-24133 request that
/// arrived on a paired public box's relay (R10). `None` when the request
/// doesn't verify, has no `p` tag, or names a signer this box does not host
/// (the subscription's `#p` filter already scopes to our signers, but
/// membership + signature are re-checked before the deposited key is ever
/// touched). The deposited nsec + the bunker roster both live here on the
/// head, so the response is built in-process via the shared
/// [`bunker::execute_bunker_request`] core — the same core the public box's
/// relay carve-out runs; only the transport differs (the caller publishes
/// this back to the peer relay).
async fn respond_to_bunker_request(
    db: &Arc<CacheDb>,
    nest_key: &[u8; 32],
    request: &Event,
) -> Option<Event> {
    // Defensive: never execute against the deposited key without a valid
    // signature, even though the peer relay already accepted the event.
    if !verify_event(request) {
        return None;
    }
    let signer_pubkey = request.tags.iter().find_map(|t| {
        if t.name() == Some("p") {
            t.value().map(str::to_string)
        } else {
            None
        }
    })?;
    let now = crate::db::now_epoch_secs() as u64;
    let conn = db.conn().await;
    // Membership: one of our locally-hosted signers? (`?` short-circuits —
    // a miss drops `conn` and returns `None`.)
    bunker::signer_actor(&conn, &signer_pubkey).ok().flatten()?;
    let result = bunker::execute_bunker_request(&conn, nest_key, &signer_pubkey, request, now);
    drop(conn);
    match result {
        Ok(response) => Some(response),
        Err(e) => {
            tracing::warn!("nostr bunker proxy: build response failed: {e}");
            None
        }
    }
}

/// Get an existing relay connection or create a new one. `url` is always
/// caller-supplied (a publish list, a follow's hints, a paired nest's URL), so
/// the dial runs under `dial_policy` and a refused target costs no socket.
async fn get_or_connect<'a>(
    connections: &'a mut HashMap<String, RelayClient>,
    url: &str,
    dial_policy: RelayDialPolicy,
) -> Option<&'a mut RelayClient> {
    if !connections.contains_key(url) {
        match RelayClient::connect(url, dial_policy).await {
            Ok(c) => {
                tracing::info!("nostr sync: connected to {url}");
                connections.insert(url.to_string(), c);
            }
            Err(e) => {
                tracing::warn!("nostr sync: failed to connect to {url}: {e}");
                return None;
            }
        }
    }
    connections.get_mut(url)
}

#[cfg(test)]
mod tests {
    //! The S8.9 at-rest contract for inbound gift-wrap DMs: the worker
    //! unwraps in flight (licensed by the recipient's nsec deposit) and then
    //! seals the plaintext through the D2 resolver before storage — a
    //! bridging box writes no plaintext DM body at rest, fail-closed.

    use super::*;
    use crate::nostr::key_crypto::encrypt_nostr_privkey;
    use fauna_bridge_nostr::nip17::wrap_dm;
    use fauna_bridge_nostr::signing::Keypair;

    const NEST_KEY: [u8; 32] = [0x0Au8; 32];

    /// Observation seam for "this task's future was dropped" — the only thing
    /// an *aborted* task can tell us, since its tail never runs. Latency-
    /// independent: the assertions below poll this flag to a named generous
    /// budget rather than sleeping a guessed interval.
    struct DropSignal(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A task that never finishes on its own, plus the flag that flips when it
    /// is aborted (i.e. when its future is dropped).
    fn immortal_task() -> (
        tokio::task::JoinHandle<()>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = DropSignal(flag.clone());
        // spawn-ok(test)
        let task = tokio::spawn(async move {
            let _signal = signal;
            std::future::pending::<()>().await;
        });
        (task, flag)
    }

    /// Poll `flag` to a generous budget. Green runs pay only the real latency.
    async fn await_flag(flag: &std::sync::atomic::AtomicBool, what: &str) {
        const BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
        let deadline = tokio::time::Instant::now() + BUDGET;
        while tokio::time::Instant::now() < deadline {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("{what} did not happen within {BUDGET:?}");
    }

    /// **A bunker drain dies with its record — the unit half.**
    ///
    /// A `JoinHandle` detaches on drop; only `Drop for BunkerDrain` makes
    /// leaving the map mean "stop draining". Delete that impl and this reds.
    #[tokio::test]
    async fn dropping_a_bunker_drain_record_aborts_its_task() {
        let (task, aborted) = immortal_task();
        let drain = BunkerDrain {
            signer_fingerprint: vec!["deadbeef".to_string()],
            task,
        };
        assert!(
            !aborted.load(std::sync::atomic::Ordering::SeqCst),
            "beside-control: the task must still be alive before the drop, or \
             this test would pass without the Drop impl doing anything"
        );
        drop(drain);
        await_flag(&aborted, "the dropped BunkerDrain's task aborting").await;
    }

    /// **A bunker drain dies with its record — the composition half, which is
    /// the shape the defect actually had.**
    ///
    /// The worker is generation-scoped, and `AppState::spawn_scoped` cancels by
    /// **dropping** the worker future: `run()`'s `break` never executes, so the
    /// post-loop abort loop it used to rely on never ran. What must kill the
    /// drains is the map being dropped with the future — which is only true if
    /// the record owns its abort. This models exactly that: drains in a map
    /// held across an await, inside a scoped task, cancelled at the generation.
    ///
    /// Without it, a rotation leaves NIP-46 drains answering signing requests
    /// on a public relay with the superseded nest key.
    #[tokio::test]
    async fn generation_cancel_kills_the_workers_bunker_drains() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(crate::routes::AppState::for_test(db));
        let (task, aborted) = immortal_task();
        // The worker must have actually *reached* its await with the drain in
        // the map before we cancel — otherwise the future is dropped before its
        // first poll and the raw `JoinHandle` capture is merely detached, which
        // would let this test pass with no `Drop` impl at all. Handshake rather
        // than a yield count: latency-independent, and it makes the beside-
        // control below mean something.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        state.spawn_scoped(async move {
            let mut drains: HashMap<String, BunkerDrain> = HashMap::new();
            drains.insert(
                "wss://peer.example/nostr".to_string(),
                BunkerDrain {
                    signer_fingerprint: vec!["deadbeef".to_string()],
                    task,
                },
            );
            let _ = ready_tx.send(());
            // The worker's own shape: the map lives across the select loop's
            // awaits, so a cancel drops it *here*, mid-await — never after the
            // `break` an abort loop would have been written below.
            std::future::pending::<()>().await;
            drop(drains);
        });

        ready_rx
            .await
            .expect("the scoped worker stand-in never started");
        assert!(
            !aborted.load(std::sync::atomic::Ordering::SeqCst),
            "beside-control: the drain must be alive while the generation is"
        );
        state.serve_generation.cancel();
        await_flag(&aborted, "the generation cancel reaching the bunker drain").await;
    }

    /// A worker over a fresh in-memory db with the nostr tables created, and a
    /// tempdir-backed `__post` segment store (unused by these tests, but a
    /// real construction argument now that materialize reads segment-first).
    /// Per-call sequence (pid + atomic counter) keeps each instance's dir
    /// test-unique, the same convention `AppState::for_test` uses.
    async fn worker() -> (NostrSyncWorker, Arc<CacheDb>) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let post_segments = Arc::new(SegmentManager::new(
            std::env::temp_dir().join(format!(
                "fauna-test-nostr-sync-worker-post-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )),
            "post",
        ));
        let (_tx, rx) = mpsc::channel(4);
        let (_wake_tx, wake_rx) = mpsc::channel(1);
        (
            // State attached, exactly as the production spawn does: the zap arm
            // consults the feature gate, and a stateless worker would drop every
            // receipt rather than exercise the path these tests are about.
            NostrSyncWorker::new(
                db.clone(),
                post_segments,
                rx,
                wake_rx,
                NEST_KEY,
                // The production policy: these tests never dial a relay, and
                // the two below that do are the pins that it refuses loopback.
                crate::nostr::relays::relay_dial_policy(),
            )
            .with_state(Arc::new(crate::routes::AppState::for_test(db.clone()))),
            db,
        )
    }

    /// **Source: a follow's relay hints.** A follow whose `relay_hints` name a
    /// loopback relay never makes the worker open a socket under the
    /// production policy — the listener behind the hint sees no
    /// connection — while the same rows under the loopback allowance connect,
    /// so the refusal is the guard's and not a broken fixture.
    #[tokio::test]
    async fn a_follow_hint_at_loopback_is_refused_before_any_tcp_connect() {
        use fauna_bridge_nostr::relay_client::test_support::ProbeListener;
        for (policy, want_connection) in [
            (RelayDialPolicy::PublicOnly, false),
            (RelayDialPolicy::PublicOrLoopback, true),
        ] {
            let (worker, db) = worker().await;
            let worker = NostrSyncWorker {
                dial_policy: policy,
                ..worker
            };
            link_custodial_account(&db, "actor-hint").await;
            let probe = ProbeListener::bind().await;
            {
                let conn = db.conn().await;
                db::add_follow(
                    &conn,
                    "actor-hint",
                    &"b".repeat(64),
                    None,
                    Some(&serde_json::json!([probe.url]).to_string()),
                )
                .unwrap();
            }
            let mut connections = HashMap::new();
            let mut active = HashMap::new();
            // The connect either lands on the probe (allowance) or is refused
            // before a socket opens (production); a WebSocket handshake against
            // the bare probe then fails, which the worker tolerates.
            let refresh = worker.refresh_inbound_subscriptions(&mut connections, &mut active);
            let (_, saw) = tokio::join!(
                tokio::time::timeout(std::time::Duration::from_secs(5), refresh),
                probe.saw_a_connection_within(std::time::Duration::from_millis(500)),
            );
            assert_eq!(saw, want_connection, "under {policy:?}");
        }
    }

    /// **Source: a paired serving box.** The bunker drain's relay URL comes
    /// from a `nostr_push` pairing's `nest_url`; at loopback the production
    /// policy refuses it before any socket opens, and the drain
    /// returns for the reconciler to retry — never a connection to the probe.
    #[tokio::test]
    async fn a_paired_relay_at_loopback_is_refused_before_any_tcp_connect() {
        use fauna_bridge_nostr::relay_client::test_support::ProbeListener;
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        crate::nostr::init_db(&db).await.unwrap();
        let probe = ProbeListener::bind().await;
        let drain = bunker_drain_task(
            db,
            NEST_KEY,
            probe.url.clone(),
            vec!["c".repeat(64)],
            RelayDialPolicy::PublicOnly,
        );
        let (_, saw) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(5), drain),
            probe.saw_a_connection_within(std::time::Duration::from_millis(500)),
        );
        assert!(!saw, "the guard must refuse before opening a socket");
    }

    /// Link a custodial account for `actor` and return its Nostr keypair. The
    /// worker's key is seated as the deployment seed too — the Nostr DM leg
    /// wraps its own key under it on its first deposit.
    async fn link_custodial_account(db: &CacheDb, actor_hex: &str) -> Keypair {
        db.set_nest_keypair(&NEST_KEY, &[0u8; 32]).await.unwrap();
        let kp = Keypair::generate();
        let encrypted = encrypt_nostr_privkey(&NEST_KEY, &kp.secret_bytes()).unwrap();
        let conn = db.conn().await;
        db::link_account(
            &conn,
            actor_hex,
            &kp.public_key_hex(),
            "generated",
            Some(&encrypted),
            None,
            None,
        )
        .unwrap();
        kp
    }

    /// Store a real, signature-valid kind-1 note authored by `author` in the
    /// worker's relay event store and return its id — the zap subject. The
    /// subject binding refuses a receipt whose `e`-tagged event this
    /// box does not hold authored by the payee, so a believable fixture must
    /// rest a real payee-authored note first.
    async fn store_note_by(db: &CacheDb, author: &Keypair) -> String {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let note = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![],
            content: "a zappable note".into(),
        });
        let conn = db.conn().await;
        let outcome = store::store_event(&conn, &note, false).expect("store note");
        assert!(outcome.is_newly_stored(), "fixture note must store");
        note.id
    }

    /// A real, signature-valid kind-9735 from `signer` naming `payee_pubkey`.
    /// `created_at` is **now**, not a fixed constant. That mattered more than
    /// it does today: `process_inbound_event` used to reject anything outside
    /// ±1h before it ever reached the zap arm, so a fixed *past* timestamp made
    /// these tests vacuously green. The lower arm is gone (a sweep
    /// with a catch-up cursor must accept old events), so only the **future**
    /// direction can still short-circuit the arm; `now` keeps clear of it.
    fn signed_zap_receipt(
        signer: &Keypair,
        payee_pubkey: &str,
        target_event_id: &str,
    ) -> fauna_bridge_nostr::types::Event {
        use fauna_bridge_nostr::types::{Tag, UnsignedEvent};
        let now = fauna_core::data::Timestamp::now_secs() as u64;
        let description = serde_json::json!({
            "kind": 9734,
            "pubkey": "d".repeat(64),
            "tags": [["p", payee_pubkey]],
            "content": "",
        })
        .to_string();
        signer.sign_event(UnsignedEvent {
            pubkey: signer.public_key_bytes(),
            created_at: now,
            kind: 9735,
            tags: vec![
                Tag::new(vec!["p".into(), payee_pubkey.to_string()]),
                Tag::new(vec!["e".into(), target_event_id.to_string()]),
                Tag::new(vec!["bolt11".into(), "lnbc210n1pjfake".into()]),
                Tag::new(vec!["description".into(), description]),
            ],
            content: String::new(),
        })
    }

    /// Ingress A's **call site**, not just the shared gate: the NIP-57 trust
    /// decision must actually run inside `process_inbound_event`. Driving the
    /// real worker is what stops a future edit from deleting the gate call
    /// while the shared-gate unit tests stay green
    /// (`monetization.md` § Zap receipts — the trust model: the gate applies
    /// at *every* ingress).
    #[tokio::test]
    async fn inbound_zap_from_a_designated_signer_is_recorded() {
        let (worker, db) = worker().await;
        let actor_hex = hex::encode([0x11u8; 32]);
        let payee = link_custodial_account(&db, &actor_hex).await;
        let signer = Keypair::generate();
        {
            let conn = db.conn().await;
            db::add_zap_signer(&conn, &actor_hex, &signer.public_key_hex(), "Alby").unwrap();
        }

        // The zapped event is the payee's OWN, held on this box.
        let note = store_note_by(&db, &payee).await;
        let ev = signed_zap_receipt(&signer, &payee.public_key_hex(), &note);
        worker.process_inbound_event("wss://relay.test", ev).await;

        let conn = db.conn().await;
        assert_eq!(
            db::get_zap_total(&conn, &note).unwrap(),
            (21_000, 1),
            "a designated signer's receipt is believed and counted"
        );
    }

    /// The defect this slice closes, at the real call site: before the gate,
    /// any signature-valid kind-9735 naming a local pubkey — on any relay the
    /// nest reads, from any signer — was inserted and summed.
    #[tokio::test]
    async fn inbound_zap_from_an_undesignated_signer_is_dropped() {
        let (worker, db) = worker().await;
        let actor_hex = hex::encode([0x11u8; 32]);
        let payee = link_custodial_account(&db, &actor_hex).await;
        // The payee designated somebody — just not this stranger.
        {
            let conn = db.conn().await;
            db::add_zap_signer(
                &conn,
                &actor_hex,
                &Keypair::generate().public_key_hex(),
                "Alby",
            )
            .unwrap();
        }
        let stranger = Keypair::generate();

        let ev = signed_zap_receipt(&stranger, &payee.public_key_hex(), "post-1");
        worker.process_inbound_event("wss://relay.test", ev).await;

        let conn = db.conn().await;
        assert_eq!(
            db::get_zap_total(&conn, "post-1").unwrap(),
            (0, 0),
            "a perfectly valid signature from an undesignated signer buys nothing"
        );
    }

    /// at the real ingress-A call site: a designated signer of one
    /// payee attributing a zap to *another* payee's stored event is dropped.
    /// Before the subject binding, `process_inbound_event` summed it onto the
    /// victim's post.
    #[tokio::test]
    async fn inbound_zap_cross_attributed_to_another_users_event_is_dropped() {
        let (worker, db) = worker().await;
        let attacker_hex = hex::encode([0x11u8; 32]);
        let victim_hex = hex::encode([0x22u8; 32]);
        let attacker = link_custodial_account(&db, &attacker_hex).await;
        let victim = link_custodial_account(&db, &victim_hex).await;
        let signer = Keypair::generate();
        {
            let conn = db.conn().await;
            db::add_zap_signer(&conn, &attacker_hex, &signer.public_key_hex(), "Alby").unwrap();
        }

        // The victim's own post; the attacker names themselves in `p`.
        let victim_note = store_note_by(&db, &victim).await;
        let ev = signed_zap_receipt(&signer, &attacker.public_key_hex(), &victim_note);
        worker.process_inbound_event("wss://relay.test", ev).await;

        let conn = db.conn().await;
        assert_eq!(
            db::get_zap_total(&conn, &victim_note).unwrap(),
            (0, 0),
            "a designated signer cannot attribute a zap to another user's event"
        );
    }

    /// The ratified out-of-the-box default at the real call site: a fresh nest
    /// has designated nobody, so it believes nobody.
    #[tokio::test]
    async fn inbound_zap_is_dropped_when_the_payee_designated_nobody() {
        let (worker, db) = worker().await;
        let actor_hex = hex::encode([0x11u8; 32]);
        let payee = link_custodial_account(&db, &actor_hex).await;

        let ev = signed_zap_receipt(&Keypair::generate(), &payee.public_key_hex(), "post-1");
        worker.process_inbound_event("wss://relay.test", ev).await;

        let conn = db.conn().await;
        assert_eq!(db::get_zap_total(&conn, "post-1").unwrap(), (0, 0));
    }

    #[test]
    fn nip10050_advertises_dm_inbox_relays() {
        use fauna_bridge_nostr::signing::verify_event;
        let kp = Keypair::generate();
        let url = "wss://nest.example/nostr".to_string();
        let ev = build_nip10050_event(&kp, std::slice::from_ref(&url)).expect("built");

        assert_eq!(ev.kind, 10050);
        assert!(
            verify_event(&ev),
            "the advertisement is signed by the account"
        );
        assert!(
            ev.tags
                .iter()
                .any(|t| t.name() == Some("relay") && t.value() == Some(&url)),
            "the nest's DM-inbox URL is advertised as a `relay` tag"
        );
        // No relays → no event (nothing to advertise).
        assert!(build_nip10050_event(&kp, &[]).is_none());
    }

    #[tokio::test]
    async fn inbound_gift_wrap_dm_seals_at_rest() {
        let (worker, db) = worker().await;
        let actor = [0x71u8; 32];
        let actor_hex = hex::encode(actor);
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;

        // The recipient provisioned an MSEK-derived seal key (the D2 row).
        let msek = [0x42u8; 32];
        crate::test_support::seed_recipient_seal_key(&db, &actor, &msek).await;

        let sender_kp = Keypair::generate();
        let plaintext = "meet at the old mill at dawn";
        let gift_wrap = wrap_dm(&sender_kp, &recipient_kp.public_key_bytes(), plaintext).unwrap();

        worker.process_gift_wrap(gift_wrap).await;

        // The at-rest probe reads the STORED bytes (the vacuous-green trap:
        // the sealed bytes must exist, must not embed the plaintext, and
        // must open to it — there is no plaintext column left to check).
        let conn = db.conn().await;
        let (sealed, direction): (Vec<u8>, String) = conn
            .query_row(
                "SELECT sealed_content, direction FROM bridge_conversation_messages
                  WHERE actor_id = ?1",
                [&actor[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("a DM row was stored");
        drop(conn);

        assert_eq!(direction, "in");
        assert!(
            !sealed
                .windows(plaintext.len())
                .any(|w| w == plaintext.as_bytes()),
            "the stored sealed bytes must not embed the plaintext"
        );
        let opened = crate::test_support::open_recipient_record(&sealed, &msek);
        assert_eq!(opened, plaintext.as_bytes());
    }

    /// The rumor's
    /// `created_at` is the sender's own UNSIGNED claim — plaintext inside the
    /// seal, verified by nobody. A gift wrap naming `u64::MAX` must not (a)
    /// pin its conversation's sort key to that value, or (b) wrap negative on
    /// the `u64` -> `i64` narrowing. Both the conversation list's `last_at`
    /// (the sort key) and the per-message `received_at` must reflect the
    /// nest's OWN clock, bounded to "now" — never the attacker's claim — and
    /// the display-only `created_at` must saturate rather than wrap.
    #[tokio::test]
    async fn a_far_future_rumor_created_at_does_not_pin_the_conversation() {
        let (worker, db) = worker().await;
        let actor = [0x64u8; 32];
        let actor_hex = hex::encode(actor);
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        let attacker = Keypair::generate();
        let gift_wrap = fauna_bridge_nostr::nip17::wrap_dm_with_rumor_created_at(
            &attacker,
            &recipient_kp.public_key_bytes(),
            "malicious future-dated message",
            u64::MAX,
        )
        .unwrap();
        worker.process_gift_wrap(gift_wrap).await;

        let leg = &crate::db::bridged_conversations::NOSTR_LEG_PRINCIPAL_ID;
        let convs = db.summarize_bridged_rooms(&actor, Some(leg)).await.unwrap();
        assert_eq!(
            convs.len(),
            1,
            "the wrap must still store — this is not a refusal test"
        );

        let now = fauna_core::data::Timestamp::now_millis() as i64;
        assert!(
            (now - 60_000..=now + 5_000).contains(&convs[0].last_received_at),
            "the conversation's sort key (last_at) must be the nest's OWN \
             received time, never the attacker's claimed rumor created_at \
             (u64::MAX as i64 would be {}) — got last_at={}, now={now}. A \
             sender able to control this pins their conversation to the top \
             of the ward's DM list permanently.",
            i64::MAX,
            convs[0].last_received_at,
        );

        let room = db.list_bridged_rooms(&actor).await.unwrap().remove(0);
        let msgs = db
            .fetch_bridged_inbox(&actor, Some(&room.room_id), 0, 10)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].created_at,
            i64::MAX,
            "an out-of-range rumor created_at must SATURATE at i64::MAX for \
             display, never silently wrap negative"
        );
        assert!(
            (now - 60_000..=now + 5_000).contains(&msgs[0].received_at),
            "the message's own received_at must independently be the \
             nest's clock, got {}, now={now}",
            msgs[0].received_at,
        );
    }

    #[tokio::test]
    async fn inbound_gift_wrap_without_seal_key_stores_nothing() {
        let (worker, db) = worker().await;
        let actor = [0x72u8; 32];
        let actor_hex = hex::encode(actor);
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        // Deliberately NO seal key seeded: the D2 resolver returns None.

        let sender_kp = Keypair::generate();
        let gift_wrap = wrap_dm(
            &sender_kp,
            &recipient_kp.public_key_bytes(),
            "must not rest in the clear",
        )
        .unwrap();

        worker.process_gift_wrap(gift_wrap).await;

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "fail-closed: no seal key on file → the DM is not stored (never plaintext)"
        );
    }

    // ── the family bridge-DM gate at the shared inbound seam (v25) ──────
    //
    // `family-safety.md` § The bridge-DM gate. These drive the REAL ingest path
    // (`process_gift_wrap`→`process_gift_wrap_inbound`), which is the point: the
    // verdict primitive is unit-tested in `fauna_core::data`, but only these
    // pin that the seam actually consults it *before* storing.

    /// A guardian-`block`ed peer's DM is refused **before storage** — the
    /// RCPT-reject analogue (§ The bridge-DM gate: *"the ingest thereafter
    /// refuses new DMs from that peer before storage"*).
    #[tokio::test]
    async fn a_blocked_peer_s_inbound_dm_is_never_stored() {
        let (worker, db) = worker().await;
        let guardian = [0x81u8; 32];
        let actor = [0x82u8; 32];
        let actor_hex = hex::encode(actor);
        db.create_user_with_handle(&guardian, "personal", "parent81", None)
            .await
            .unwrap();
        db.create_user_with_handle(&actor, "personal", "kid82", Some(&guardian[..]))
            .await
            .unwrap();
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        let sender_kp = Keypair::generate();
        // The guardian denied this peer.
        db.set_dm_peer_verdict(
            &actor[..],
            crate::bridge_legs::NOSTR.bridge_id,
            &sender_kp.public_key_hex(),
            fauna_core::data::DmPeerVerdict::Block,
        )
        .await
        .unwrap();

        let gift_wrap = wrap_dm(&sender_kp, &recipient_kp.public_key_bytes(), "hello?").unwrap();
        worker.process_gift_wrap(gift_wrap).await;

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "a guardian-blocked peer's new DM must never reach storage"
        );
    }

    /// The same block, against a sender who **lies about the rumor's recipient**
    /// — the bypass this test was written for.
    ///
    /// The rumor's `p` tag is plaintext inside the seal, signed by nobody, so a
    /// hostile sender picks it freely. The gate used to decide *which* pubkey
    /// was the peer by comparing that tag to the recipient, and in the
    /// mismatched arm it keyed the verdict lookup on the tag's own value — so a
    /// blocked peer spelling it as anything else found no `block` and their DM
    /// was stored. Three spellings a real attacker can send, none of which
    /// `wrap_dm` can express (it always writes the honest lowercase tag, which
    /// is exactly why the sibling test above stayed green through the bug):
    ///
    ///   * `None` — no `p` tag at all. It used to arrive as `""`.
    ///   * uppercase — the ward's own pubkey, differently spelled. It used to
    ///     store the DM as a *self*-conversation.
    ///   * a third party's pubkey — an unrelated identity entirely.
    ///
    /// Each must be refused before storage, exactly as the honest one is
    /// (`family-safety.md` § The bridge-DM gate: *"a `block`ed peer's new DMs
    /// never land"*).
    #[tokio::test]
    async fn a_blocked_peer_cannot_walk_past_the_gate_by_lying_about_the_rumor_recipient() {
        let guardian = [0x91u8; 32];
        let actor = [0x92u8; 32];
        let actor_hex = hex::encode(actor);

        // Every variant gets its own worker + store, so one leaking a row
        // cannot be mistaken for another's, and the report below is unambiguous
        // about WHICH spellings got through. Failures are COLLECTED rather than
        // asserted in the loop: a regression here should name every spelling it
        // reopens in one run, not just the alphabetically first.
        let mut leaked: Vec<(&str, Vec<String>)> = Vec::new();
        for label in ["absent", "uppercase-own", "third-party"] {
            let (worker, db) = worker().await;
            db.create_user_with_handle(&guardian, "personal", "parent91", None)
                .await
                .unwrap();
            db.create_user_with_handle(&actor, "personal", "kid92", Some(&guardian[..]))
                .await
                .unwrap();
            let recipient_kp = link_custodial_account(&db, &actor_hex).await;
            crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

            let sender_kp = Keypair::generate();
            db.set_dm_peer_verdict(
                &actor[..],
                crate::bridge_legs::NOSTR.bridge_id,
                &sender_kp.public_key_hex(),
                fauna_core::data::DmPeerVerdict::Block,
            )
            .await
            .unwrap();

            let spelled = match label {
                "absent" => None,
                "uppercase-own" => Some(recipient_kp.public_key_hex().to_uppercase()),
                _ => Some(Keypair::generate().public_key_hex()),
            };
            let gift_wrap = fauna_bridge_nostr::nip17::wrap_dm_with_rumor_recipient(
                &sender_kp,
                &recipient_kp.public_key_bytes(),
                "hello?",
                spelled.as_deref(),
            )
            .unwrap();
            worker.process_gift_wrap(gift_wrap).await;

            let conn = db.conn().await;
            let stored: Vec<String> = conn
                .prepare(
                    "SELECT r.far_room_id FROM bridge_conversation_messages m
                       JOIN bridge_conversation_rooms r ON r.room_id = m.room_id",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            if !stored.is_empty() {
                leaked.push((label, stored));
            }
        }
        assert!(
            leaked.is_empty(),
            "a guardian-blocked peer got past the gate by lying about the rumor's \
             `p` tag: {leaked:?} (spelling → the peer_pubkey the row was keyed on). \
             The identity the verdict keys on must come from the SEAL, which the \
             NIP-44 open authenticates, never from the rumor, which is signed by \
             nobody"
        );
    }

    /// The other direction, which the fix must not break: the ward's **own sent
    /// copy**, arriving back through a relay, still attributes to the rumor's
    /// recipient rather than to the ward themselves.
    ///
    /// A NIP-17 sender wraps a copy of their outbound DM to themselves, so this
    /// is an ordinary wrap the ward is the outer recipient of — and here the
    /// rumor's `p` tag IS trustworthy, because the seal proves the ward wrote
    /// it. Without this pin the fix could trade one mis-attribution (a blocked
    /// peer keyed on their own string) for another (every sent copy filed as a
    /// self-conversation).
    #[tokio::test]
    async fn the_wards_own_sent_copy_still_attributes_to_the_rumor_recipient() {
        let (worker, db) = worker().await;
        let actor = [0x93u8; 32];
        let actor_hex = hex::encode(actor);
        db.create_user_with_handle(&actor, "personal", "adult93", None)
            .await
            .unwrap();
        let own_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        // We sealed it (sender = us), addressed to ourselves on the outside,
        // with the rumor naming the real counterparty — the sent-copy shape.
        let peer_kp = Keypair::generate();
        let gift_wrap = fauna_bridge_nostr::nip17::wrap_dm_with_rumor_recipient(
            &own_kp,
            &own_kp.public_key_bytes(),
            "my own sent copy",
            Some(&peer_kp.public_key_hex()),
        )
        .unwrap();
        worker.process_gift_wrap(gift_wrap).await;

        let conn = db.conn().await;
        let stored: Vec<String> = conn
            .prepare(
                "SELECT r.far_room_id FROM bridge_conversation_messages m
                       JOIN bridge_conversation_rooms r ON r.room_id = m.room_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            stored,
            vec![peer_kp.public_key_hex()],
            "our own sent copy must file under the counterparty the rumor names, \
             not under ourselves"
        );
    }

    /// A **held** conversation stores exactly as a delivered one does — the hold
    /// is computed at read time, never a placement (§ Don't do these: *"don't
    /// store bridge-DM hold state"*). This is the pin that keeps a future
    /// session from "optimizing" the hold into a stored flag: nothing about the
    /// stored row may differ, which is what makes a knob relax release
    /// everything by construction.
    #[tokio::test]
    async fn a_held_cold_peer_s_dm_is_stored_exactly_as_a_delivered_one() {
        let (worker, db) = worker().await;
        let guardian = [0x83u8; 32];
        let actor = [0x84u8; 32];
        let actor_hex = hex::encode(actor);
        db.create_user_with_handle(&guardian, "personal", "parent83", None)
            .await
            .unwrap();
        db.create_user_with_handle(&actor, "personal", "kid84", Some(&guardian[..]))
            .await
            .unwrap();
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        let msek = [0x42u8; 32];
        crate::test_support::seed_recipient_seal_key(&db, &actor, &msek).await;

        // The knob holds cold peers, and this sender carries no verdict row.
        db.update_guardian_policy(
            &actor[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            Some("hold"),
            None,
        )
        .await
        .unwrap();

        let sender_kp = Keypair::generate();
        let plaintext = "hi, remember me from the game?";
        let gift_wrap = wrap_dm(&sender_kp, &recipient_kp.public_key_bytes(), plaintext).unwrap();
        worker.process_gift_wrap(gift_wrap).await;

        let conn = db.conn().await;
        let sealed: Vec<u8> = conn
            .query_row(
                "SELECT sealed_content FROM bridge_conversation_messages WHERE actor_id = ?1",
                [&actor[..]],
                |row| row.get(0),
            )
            .expect("a held DM is STORED — nothing is withheld pre-decision");
        drop(conn);
        // …and it is the ward's to read, sealed to them exactly as ever: reading
        // is never gated (§ The trust shape invariant 4).
        let opened = crate::test_support::open_recipient_record(&sealed, &msek);
        assert_eq!(opened, plaintext.as_bytes());

        // No hold state was stored anywhere — the whole design.
        let conn = db.conn().await;
        let verdict_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM guardian_dm_peers", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            verdict_rows, 0,
            "holding must write NO row — hold-ness is computed from (knob, absent row)"
        );
    }

    /// An **unsupervised** account is never gated, even by a stale verdict row:
    /// the gate short-circuits on the absent policy (the Slice F device-marker
    /// precedent — *"marked AND currently supervised"*, never the flag alone).
    #[tokio::test]
    async fn an_unsupervised_recipient_is_never_gated_by_a_stale_verdict_row() {
        let (worker, db) = worker().await;
        let actor = [0x85u8; 32];
        let actor_hex = hex::encode(actor);
        db.create_user_with_handle(&actor, "personal", "grownup85", None)
            .await
            .unwrap();
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        // Force a block row onto an account with no guardianship — the state a
        // graduation crash could conceivably leave behind. It must be inert.
        let sender_kp = Keypair::generate();
        {
            let conn = db.conn().await;
            conn.execute(
                "INSERT INTO guardian_dm_peers
                     (supervised_actor_id, bridge_id, peer_id, verdict, added_by, created_at)
                 VALUES (?1, ?2, ?3, 'block', 'guardian', 0)",
                rusqlite::params![
                    &actor[..],
                    crate::bridge_legs::NOSTR.bridge_id,
                    sender_kp.public_key_hex()
                ],
            )
            .unwrap();
        }

        let gift_wrap = wrap_dm(&sender_kp, &recipient_kp.public_key_bytes(), "hi").unwrap();
        worker.process_gift_wrap(gift_wrap).await;

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "a stale verdict row must never gate an account with no guardian"
        );
    }

    /// Make `author` a followed pubkey of a sweeping account — one that
    /// satisfies the very predicate `refresh_inbound_subscriptions` selects on
    /// (`inbound_to_feed = 1 AND encrypted_privkey IS NOT NULL`), so the
    /// fixture and the subscription agree on who this nest actually asked for.
    async fn follow_author(db: &CacheDb, follower_actor_hex: &str, author: &Keypair) {
        link_custodial_account(db, follower_actor_hex).await;
        let conn = db.conn().await;
        db::add_follow(
            &conn,
            follower_actor_hex,
            &author.public_key_hex(),
            None,
            None,
        )
        .unwrap();
    }

    /// A signature-valid inbound note from a followed author, driven through
    /// the real dispatcher. Returns its Fauna post id hex.
    ///
    /// ⚠ The follow row is **load-bearing, not decoration**: until 2026-08-05
    /// this helper generated an author nobody followed and asserted the note
    /// was stored anyway — encoding, the missing ingest gate, as the
    /// expectation. Every caller depends on the author being genuinely
    /// followed.
    async fn sweep_note_through_dispatcher(
        worker: &NostrSyncWorker,
        db: &CacheDb,
        author: &Keypair,
        body: &str,
    ) -> String {
        follow_author(db, &hex::encode([0x51u8; 32]), author).await;
        use fauna_bridge_nostr::types::UnsignedEvent;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let note = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now,
            kind: 1,
            tags: vec![],
            content: body.into(),
        });
        let id = note.id.clone();
        worker.process_inbound_event("wss://relay.test", note).await;
        let conn = db.conn().await;
        db::get_event_by_nostr_id(&conn, &id)
            .unwrap()
            .expect("the dispatcher should have swept the note in")
            .fauna_post_id
    }

    /// A kind-0 event from `author`, signature-valid, dated `created_at`.
    fn metadata_event(author: &Keypair, created_at: u64, content: &str) -> Event {
        use fauna_bridge_nostr::types::UnsignedEvent;
        author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at,
            kind: fauna_bridge_nostr::types::kind::METADATA,
            tags: vec![],
            content: content.into(),
        })
    }

    /// The bridged-author transit point (`bridges.md` § Unified feed ingestion
    /// → *Bridged authors*): a followed author's kind 0 becomes their face
    /// under the synthetic id their notes rest under; it is never stored as an
    /// event; and a stranger's kind 0 is dropped at the author gate exactly
    /// like a stranger's note.
    #[tokio::test]
    async fn a_followed_authors_kind_0_projects_their_face_and_a_strangers_does_not() {
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        follow_author(&db, &hex::encode([0x52u8; 32]), &author).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let ev = metadata_event(
            &author,
            now - 60,
            r#"{"name":"alice","display_name":"Alice A.","picture":"https://p.example/a.png","nip05":"alice@example.com"}"#,
        );
        let ev_id = ev.id.clone();
        worker.process_inbound_event("wss://relay.test", ev).await;

        let synthetic =
            fauna_bridge_nostr::translate::synthetic_actor_id(&author.public_key_bytes()).0;
        let conn = db.conn().await;
        let face = crate::db::bridge_authors::get(&conn, &synthetic)
            .unwrap()
            .expect("the followed author's kind 0 projects a face")
            .display();
        assert_eq!(face.handle.as_deref(), Some("alice@example.com"));
        assert_eq!(face.display_name.as_deref(), Some("Alice A."));
        assert_eq!(
            face.avatar_url.as_deref(),
            Some("/api/v1/media/proxy?url=https%3A%2F%2Fp.example%2Fa.png")
        );
        assert!(
            db::get_event_by_nostr_id(&conn, &ev_id).unwrap().is_none(),
            "a kind 0 is projected, never stored as an event"
        );

        // A lagging relay serving an OLDER kind 0 never regresses the face.
        drop(conn);
        let stale = metadata_event(&author, now - 3600, r#"{"name":"old-alice"}"#);
        worker
            .process_inbound_event("wss://relay.test", stale)
            .await;
        let conn = db.conn().await;
        assert_eq!(
            crate::db::bridge_authors::get(&conn, &synthetic)
                .unwrap()
                .unwrap()
                .display()
                .display_name
                .as_deref(),
            Some("Alice A.")
        );
        drop(conn);

        // A stranger nobody follows: dropped at the author gate.
        let stranger = Keypair::generate();
        let ev = metadata_event(&stranger, now - 60, r#"{"name":"mallory"}"#);
        worker.process_inbound_event("wss://relay.test", ev).await;
        let conn = db.conn().await;
        assert!(
            crate::db::bridge_authors::get(
                &conn,
                &fauna_bridge_nostr::translate::synthetic_actor_id(&stranger.public_key_bytes()).0
            )
            .unwrap()
            .is_none(),
            "a stranger's kind 0 must not project a face"
        );
    }

    /// A followed author's kind-1 reply carrying `tags`, driven through the
    /// real dispatcher (the author is already followed). Returns its post id.
    async fn sweep_tagged_note(
        worker: &NostrSyncWorker,
        db: &CacheDb,
        author: &Keypair,
        tags: Vec<fauna_bridge_nostr::types::Tag>,
    ) -> String {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let note = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now,
            kind: 1,
            tags,
            content: "a reply".into(),
        });
        let id = note.id.clone();
        worker.process_inbound_event("wss://relay.test", note).await;
        let conn = db.conn().await;
        db::get_event_by_nostr_id(&conn, &id)
            .unwrap()
            .expect("the dispatcher should have swept the reply in")
            .fauna_post_id
    }

    async fn swept_references(
        worker: &NostrSyncWorker,
        db: &CacheDb,
        post_id: &str,
    ) -> Vec<fauna_core::data::Reference> {
        let id = fauna_core::hex32::decode(post_id).unwrap();
        let body = crate::segments::post::load_post_body(&worker.post_segments, db, &id)
            .await
            .unwrap()
            .expect("body");
        crate::db::posts::decode_stored_post(&body)
            .unwrap()
            .references
    }

    /// `nostr.md` § Replying to and quoting a nostr note → *Reference
    /// resolution*, inbound: a swept reply to a swept parent threads under the
    /// parent's LOCAL id (what `nostr_event_map` holds) and moves its
    /// `reply_count`; a reply to a parent this nest never mapped stays
    /// top-level.
    #[tokio::test]
    async fn a_swept_reply_to_a_swept_parent_threads_and_counts() {
        use fauna_bridge_nostr::types::Tag;
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        let parent_id = sweep_note_through_dispatcher(&worker, &db, &author, "the parent").await;
        let parent_event = {
            let conn = db.conn().await;
            db::get_event_by_fauna_id(&conn, &parent_id)
                .unwrap()
                .unwrap()
                .nostr_event_id
        };
        let parent_digest = fauna_core::hex32::decode(&parent_id).unwrap();

        let reply_id = sweep_tagged_note(
            &worker,
            &db,
            &author,
            vec![Tag::new(vec![
                "e".into(),
                parent_event,
                "".into(),
                "root".into(),
            ])],
        )
        .await;
        assert_eq!(
            swept_references(&worker, &db, &reply_id).await,
            vec![fauna_core::data::Reference::Reply {
                post_id: fauna_core::data::ContentHash::from_digest_raw(parent_digest),
            }]
        );
        assert_eq!(
            db.get_engagement_counts(&parent_digest)
                .await
                .unwrap()
                .reply_count,
            1
        );

        let orphan_id = sweep_tagged_note(
            &worker,
            &db,
            &author,
            vec![Tag::new(vec![
                "e".into(),
                "9".repeat(64),
                "".into(),
                "reply".into(),
            ])],
        )
        .await;
        assert!(swept_references(&worker, &db, &orphan_id).await.is_empty());
    }

    /// The dispatcher still sweeps an ordinary note in after the ingest body
    /// moved to `inbound_lifecycle` — the routing half of that extraction.
    #[tokio::test]
    async fn the_dispatcher_still_sweeps_an_ordinary_note_in() {
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        let post_id = sweep_note_through_dispatcher(&worker, &db, &author, "a rhubarb note").await;

        let digest = fauna_core::hex32::decode(&post_id).unwrap();
        let conn = db.conn().await;
        assert!(
            crate::db::content::get_content(&conn, &digest)
                .unwrap()
                .is_some(),
            "the translated post must rest in `content`"
        );
    }

    /// **the sweep must not trust the `authors:` filter it sent.**
    /// That filter is enforced by the *relay*, an untrusted party: a hostile or
    /// buggy one simply returns events nobody asked for, and until this gate
    /// existed `process_inbound_event` re-checked nothing — signature, ±1h
    /// window, 64 KB cap and dedup all pass for a real event from a stranger.
    /// So a stranger's note rested in `content` and — since a later change — in
    /// the user's Search corpus, attributed to an arbitrary pubkey.
    ///
    /// The gate is applied at ingest rather than at read, per the rule the
    /// kind-9735 arm in this same file already ratifies: filtering at read
    /// leaves forged rows resting on the box for every future reader to
    /// re-filter.
    #[tokio::test]
    async fn an_unfollowed_authors_note_is_refused_at_ingest() {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let (worker, db) = worker().await;
        // A sweeping account exists and follows *somebody* — so the refusal
        // below is the author check biting, not an empty-follow-table accident.
        let followed = Keypair::generate();
        follow_author(&db, &hex::encode([0x61u8; 32]), &followed).await;

        let stranger = Keypair::generate();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let note = stranger.sign_event(UnsignedEvent {
            pubkey: stranger.public_key_bytes(),
            created_at: now,
            kind: 1,
            tags: vec![],
            content: "a stranger's note the relay was never asked for".into(),
        });
        let event_id = note.id.clone();
        worker.process_inbound_event("wss://relay.test", note).await;

        let conn = db.conn().await;
        assert!(
            db::get_event_by_nostr_id(&conn, &event_id)
                .unwrap()
                .is_none(),
            "an unfollowed author's event must not be mapped or rested"
        );
        let corpus: i64 = conn
            .query_row("SELECT COUNT(*) FROM bridge_index_map", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            corpus, 0,
            "and it must not reach the user's Search corpus either"
        );
    }

    /// **the catch-up cursor asks for old events; the freshness window
    /// threw every one of them away.** `refresh_inbound_subscriptions` builds
    /// `since` from a per-`(relay, pubkey)` cursor whose entire purpose is to
    /// re-request what this nest missed, and a fresh nest with no cursor takes
    /// `limit: 200` of whatever the relay holds. A ±1h lower arm one function
    /// later discarded all of it at debug level, so a nest offline for two
    /// hours pulled its backlog and dropped it, and a followed author's post
    /// from yesterday was never swept at all.
    ///
    /// The lower arm is **removed** rather than widened to a horizon constant,
    /// and the reasoning is the part a future session should not re-derive:
    /// age is the wrong axis. Volume is already bounded by the relay's
    /// newest-first `limit`, and a translated post carries the *event's own*
    /// `created_at`, so a backfill sorts into the feed's recency order instead
    /// of flooding its top. A horizon constant would buy no bound and add a
    /// silent failure — a low-traffic author whose last post predates it reads
    /// as an empty feed on a fresh nest, which is the works-out-of-the-box
    /// invariant pointing *against* the constant. The sibling ingest plane
    /// agrees: ActivityPub's inbox applies no lower age bound either.
    ///
    /// The **upper** arm stays and is asserted below as a beside-control, so
    /// this pin cannot be satisfied by deleting the window wholesale: refusing
    /// a future-dated event is an independent anti-abuse rule (a recency-
    /// ordered feed is otherwise pinnable forever by a hostile `created_at`).
    #[tokio::test]
    async fn a_followed_authors_older_post_is_swept_in_and_only_future_dating_is_refused() {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        follow_author(&db, &hex::encode([0x71u8; 32]), &author).await;
        let now = fauna_core::data::Timestamp::now_secs() as u64;

        // Three days back — deep inside the backlog the `since` cursor exists
        // to re-request, and far outside the old ±1h window.
        let old = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now - 3 * 86_400,
            kind: 1,
            tags: vec![],
            content: "a followed author's post from three days ago".into(),
        });
        let old_id = old.id.clone();
        worker.process_inbound_event("wss://relay.test", old).await;

        // Beside-control: still refused, for a reason the lower arm never had.
        let future = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now + 7 * 86_400,
            kind: 1,
            tags: vec![],
            content: "a future-dated note pinning itself to the top".into(),
        });
        let future_id = future.id.clone();
        worker
            .process_inbound_event("wss://relay.test", future)
            .await;

        let conn = db.conn().await;
        assert!(
            db::get_event_by_nostr_id(&conn, &old_id).unwrap().is_some(),
            "a followed author's older post must be swept in, not discarded as stale"
        );
        assert!(
            db::get_event_by_nostr_id(&conn, &future_id)
                .unwrap()
                .is_none(),
            "a future-dated event must still be refused"
        );
    }

    /// The upper arm is the bridged planes' bound, not the native one
    /// (`docs/goal/ui/feed.md` § The read model): a followed author whose
    /// device clock runs a few minutes fast still lands, because here a
    /// refusal is silent to them.
    #[tokio::test]
    async fn a_followed_authors_note_minutes_ahead_sits_inside_the_bridged_cushion() {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        follow_author(&db, &hex::encode([0x72u8; 32]), &author).await;
        let now = fauna_core::data::Timestamp::now_secs() as u64;

        let fast = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now + 10 * 60,
            kind: 1,
            tags: vec![],
            content: "posted from a clock ten minutes fast".into(),
        });
        let fast_id = fast.id.clone();
        worker.process_inbound_event("wss://relay.test", fast).await;

        let conn = db.conn().await;
        assert!(
            db::get_event_by_nostr_id(&conn, &fast_id)
                .unwrap()
                .is_some(),
            "an event ten minutes ahead sits inside the bridged cushion and must be swept in"
        );
    }

    /// The same gate must not touch the two `#p`-addressed arms, which are
    /// author-unconstrained **by design** — a NIP-59 gift wrap is signed by a
    /// one-time ephemeral key that is nobody's follow, so a follow check
    /// applied one arm too early silently kills every inbound DM.
    ///
    /// ⚠ **This also pins a second defect found while walking the flow, which
    /// has nothing to do with the author gate:** `process_inbound_event`
    /// applied its ±1h freshness window to kind 1059, but NIP-59 *requires*
    /// that timestamp be randomized into the past (±48h here) so relays cannot
    /// time-correlate DMs. Only ~2% of conformant wraps fell inside the window,
    /// so ~98% of every DM the sweep pulled from an external relay was dropped
    /// silently. The assertion is written as an **invariant over many draws**
    /// rather than one sample precisely because the timestamp is random: a
    /// single-wrap test would have passed 1 run in 50 and read as flake.
    #[tokio::test]
    async fn every_conformant_gift_wrap_survives_the_dispatcher_whatever_its_timestamp() {
        let (worker, db) = worker().await;
        let actor = [0x63u8; 32];
        let actor_hex = hex::encode(actor);
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        // 20 independent draws of NIP-59's ±48h randomization. Under the old
        // window each had a ~2.1% chance of surviving, so this is red with
        // probability 1 - 0.021^20 — indistinguishable from certainty.
        const DRAWS: i64 = 20;
        for i in 0..DRAWS {
            let sender_kp = Keypair::generate();
            let wrap = wrap_dm(
                &sender_kp,
                &recipient_kp.public_key_bytes(),
                &format!("dm number {i}"),
            )
            .unwrap();
            worker.process_inbound_event("wss://relay.test", wrap).await;
        }

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, DRAWS,
            "every NIP-59-conformant gift wrap must be ingested regardless of \
             its mandated-random timestamp"
        );
    }

    /// The author gate must not reach the gift-wrap arm.
    #[tokio::test]
    async fn the_author_gate_does_not_reach_the_gift_wrap_arm() {
        let (worker, db) = worker().await;
        let actor = [0x62u8; 32];
        let actor_hex = hex::encode(actor);
        let recipient_kp = link_custodial_account(&db, &actor_hex).await;
        crate::test_support::seed_recipient_seal_key(&db, &actor, &[0x42u8; 32]).await;

        let sender_kp = Keypair::generate();
        let gift_wrap = wrap_dm(&sender_kp, &recipient_kp.public_key_bytes(), "hi").unwrap();
        worker
            .process_inbound_event("wss://relay.test", gift_wrap)
            .await;

        let conn = db.conn().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bridge_conversation_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "a gift wrap from an unfollowed ephemeral key is the ordinary DM case"
        );
    }

    /// **The last mile of the NIP-09 sweep arm.** `apply_inbound_deletion` is
    /// covered by `tests/conformance_nostr_inbound_lifecycle.rs`; what this
    /// pins is that `process_inbound_event` actually ROUTES a kind 5 to it.
    /// The subscription has always asked for kind 5, and before this arm
    /// existed the event fell through to the translate step, which bails on
    /// the kind — so the deletion was dropped as a debug log line and the
    /// author's retracted post rested here forever. A dispatcher-level test is
    /// the only thing that catches that class of regression.
    #[tokio::test]
    async fn an_inbound_kind5_is_routed_to_the_sweep_deletion_arm() {
        use fauna_bridge_nostr::types::{Tag, UnsignedEvent};
        let (worker, db) = worker().await;
        let author = Keypair::generate();
        let post_id = sweep_note_through_dispatcher(&worker, &db, &author, "a rhubarb note").await;
        let event_id = {
            let conn = db.conn().await;
            db::inbound_event_id_for_post(&conn, &post_id)
                .unwrap()
                .expect("swept rows are mapped inbound")
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let deletion = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: now,
            kind: 5,
            tags: vec![Tag::new(vec!["e".into(), event_id])],
            content: String::new(),
        });
        worker
            .process_inbound_event("wss://relay.test", deletion)
            .await;

        let digest = fauna_core::hex32::decode(&post_id).unwrap();
        let conn = db.conn().await;
        assert!(
            crate::db::content::get_content(&conn, &digest)
                .unwrap()
                .is_none(),
            "the dispatcher must apply a followed author's kind-5, not drop it"
        );
    }
}
