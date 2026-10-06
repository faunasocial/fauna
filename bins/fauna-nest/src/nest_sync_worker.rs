//! Background worker that periodically syncs namespaces **and relays inbound +
//! Sent mail** with the nests a private nest's users have paired it with. The
//! pull/push/relay originate over the long-lived federation WS-RPC channel
//! (Spec Y2 slice 4), reusing one pooled connection per peer across all
//! actors/namespaces in a cycle. The channel is the sole Fauna↔Fauna carrier
//! (slice 5 retired the HTTP interim).
//!
//! **The target is the pairing row** (`private-mode.md` § Implementation status
//! today, ruled 2026-10-01): each paired actor syncs against the `nest_url` its
//! own `fauna.pair.add` recorded — there is no deployment-wide pull target and
//! no config-file seed. The worker runs on every nest and acts only while the
//! resolved NAT mode is private.
//!
//! The mail arm is the private (residential) end of the
//! `deployment-home-with-public-relay.md` § Inbound mail relay: per pairing
//! row it drains the paired relay's sealed `__mail` over
//! `fauna.federation.sync.mail_pull`, appends each verbatim to the local
//! `__mail`, then acks over `.mail_ack` so the public nest tombstones + purges
//! (the no-mail-on-public property). See [`relay_actor_mail`].

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tokio::time;

use crate::db::pairing::PairingTargetRow;
use crate::federation_pool::PairingTargetTrust;
use crate::routes::AppState;

/// What the pairing rows say a paired URL's pin must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairPin {
    /// Exactly one nest is recorded at the URL: dial only it.
    Pinned([u8; 32]),
    /// No row records an id at the URL: nothing to expect.
    Unseeded,
    /// Several distinct nests recorded at one URL — not a fault (two users
    /// may have paired with different nests that once answered there): left
    /// unpinned, with a warning.
    Ambiguous(usize),
}

/// Choose the pin for `url` from the pairing rows' `(peer_nest_id, nest_url)`
/// pairs: the distinct ids of the rows recorded at `url` (trailing slash
/// aside). One pins, none is [`PairPin::Unseeded`], several
/// [`PairPin::Ambiguous`]. A malformed id is skipped.
pub fn select_pair_pin(url: &str, rows: &[(Vec<u8>, Option<String>)]) -> PairPin {
    let target = url.trim_end_matches('/');
    let ids: BTreeSet<[u8; 32]> = rows
        .iter()
        .filter(|(_, u)| {
            u.as_deref()
                .is_some_and(|u| u.trim_end_matches('/') == target)
        })
        .filter_map(|(id, _)| <[u8; 32]>::try_from(id.as_slice()).ok())
        .collect();
    match ids.len() {
        0 => PairPin::Unseeded,
        1 => PairPin::Pinned(*ids.iter().next().expect("one id")),
        n => PairPin::Ambiguous(n),
    }
}

/// The pool's pairing-target table for `rows`: every recorded `nest_url`,
/// exempt from the address guard's global-address arm only when a live row
/// naming it belongs to an actor who is an admin of this nest
/// (`private-mode.md` § Pairing Flow, re-decided 2026-10-01). Any other row's
/// URL is a request-named URL and passes the guard whole. Each URL is pinned
/// per [`select_pair_pin`] (expired rows included — a row's identity outlives
/// its authorization) over the rows that decide it: for an exempt URL the
/// live admin rows alone, so a non-admin's row naming it with another id
/// cannot make it ambiguous and so unpin it; for any other URL every row
/// (`federation.md` § Peer-auth model → *Discovery trust rule*).
pub fn pairing_target_table(rows: &[PairingTargetRow]) -> HashMap<String, PairingTargetTrust> {
    let peers = |admin_only: bool| -> Vec<(Vec<u8>, Option<String>)> {
        rows.iter()
            .filter(|r| !admin_only || (r.actor_is_admin && !r.expired))
            .map(|r| (r.peer_nest_id.clone(), r.nest_url.clone()))
            .collect()
    };
    let (all_peers, admin_peers) = (peers(false), peers(true));
    let mut exempt: HashMap<String, bool> = HashMap::new();
    for row in rows {
        let Some(url) = row.nest_url.as_deref() else {
            continue;
        };
        *exempt
            .entry(url.trim_end_matches('/').to_string())
            .or_default() |= row.actor_is_admin && !row.expired;
    }
    exempt
        .into_iter()
        .map(|(key, exempt)| {
            let deciding = if exempt { &admin_peers } else { &all_peers };
            let pin = match select_pair_pin(&key, deciding) {
                PairPin::Pinned(id) => Some(id),
                PairPin::Unseeded => None,
                PairPin::Ambiguous(n) => {
                    tracing::warn!(
                        peer_url = %key,
                        "pairing targets: {n} paired nests recorded at this URL — it stays unpinned"
                    );
                    None
                }
            };
            (key, PairingTargetTrust { exempt, pin })
        })
        .collect()
}

/// Re-read the pairing rows and the admin roster and rebuild the pool's
/// pairing-target table ([`pairing_target_table`] →
/// `FederationChannelPool::set_pairing_targets`), so every paired URL is
/// pinned to the nest its rows record and only an admin's is exempt.
/// Called before every sync, outbox and exchange pass, after every pairing
/// write and after every admin roster change — the exemption is decided from
/// the row's actor as the roster stands at the dial, never stored. Returns the
/// rows for the caller's own pass; `None` (and the table left as it was) when
/// the read failed.
pub async fn refresh_pairing_targets(state: &AppState) -> Option<Vec<PairingTargetRow>> {
    match state.db.list_pairing_targets().await {
        Ok(rows) => {
            state
                .federation_pool
                .set_pairing_targets(pairing_target_table(&rows));
            Some(rows)
        }
        Err(e) => {
            tracing::warn!("pairing targets: pairing read failed, keeping the current table: {e}");
            None
        }
    }
}

/// Whether the resolved NAT mode is private — the one condition every
/// private-side worker pass acts under (the live client-set
/// `AppState.node_mode`, never the boot seed).
pub async fn is_private(state: &AppState) -> bool {
    *state.node_mode.read().await == crate::config::NodeMode::Private
}

/// Per-(target URL, actor hex) cursors a [`run_sync_cycle`] carries between
/// cycles.
pub type SyncWatermarks = HashMap<(String, String), i64>;

pub async fn run_sync_worker(state: Arc<AppState>) {
    // (target url, actor_id_hex) -> last-seen sequence of the actor's
    // self-namespace on that target.
    let mut watermarks = SyncWatermarks::new();
    // (target url, actor_id_hex) -> highest mail seq durably relayed AND acked
    // (so that relay has purged it).
    let mut mail_watermarks = SyncWatermarks::new();

    loop {
        // A failed pairing read backs off longer (30s) than an idle or
        // completed cycle (10s) — preserving the pre-extraction cadence.
        let backoff = match run_sync_cycle(&state, &mut watermarks, &mut mail_watermarks).await {
            SyncCycleOutcome::ListFailed => Duration::from_secs(30),
            SyncCycleOutcome::NotPrivate | SyncCycleOutcome::Processed(_) => {
                Duration::from_secs(10)
            }
        };
        time::sleep(backoff).await;
    }
}

/// Outcome of one [`run_sync_cycle`], driving the worker's backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncCycleOutcome {
    /// The resolved NAT mode is not private: the cycle did nothing.
    NotPrivate,
    /// `n` live pairing rows carrying a `nest_url` were processed this cycle
    /// (`0` = none → the worker idles). Short backoff.
    Processed(usize),
    /// The pairing read itself failed; longer backoff before retry.
    ListFailed,
}

/// One sync cycle over every live pairing row **on this nest**: for each, pull/push
/// its actor's self-namespace and relay its actor's mail from the nest at the
/// row's own `nest_url`.
///
/// **The local-row gate is load-bearing for the home-with-public-relay
/// deployment.** A private nest only relays for actors that hold a local
/// `nest_pairings` row — seeded when the user links *this* nest from their own
/// client (`fauna.pair.add` over the bearer connection to this nest). A fresh
/// private nest with an empty pairing table therefore correctly relays nothing;
/// the public-side pull-gate row alone (which authorizes the pull) is *not*
/// enough to make this worker fire. A row with no `nest_url` has no target and
/// is skipped (one log line per cycle). `pub` so the tier_3
/// `conformance_cross_nest_mail_relay` test drives the real worker cycle —
/// including this gate — over the channel, not just the per-actor
/// [`relay_actor_mail`] helper.
pub async fn run_sync_cycle(
    state: &Arc<AppState>,
    watermarks: &mut SyncWatermarks,
    mail_watermarks: &mut SyncWatermarks,
) -> SyncCycleOutcome {
    if !is_private(state).await {
        return SyncCycleOutcome::NotPrivate;
    }
    // Rebuild the pins and the exemption from the rows and the roster as they
    // stand now, then dial from the same read.
    let Some(rows) = refresh_pairing_targets(state).await else {
        tracing::error!("sync worker: failed to list the pairing rows");
        return SyncCycleOutcome::ListFailed;
    };

    let live: Vec<&PairingTargetRow> = rows.iter().filter(|r| !r.expired).collect();
    let unaddressed = live.iter().filter(|r| r.nest_url.is_none()).count();
    if unaddressed > 0 {
        tracing::info!(
            "sync worker: {unaddressed} pairing rows record no nest_url and have no target; skipped"
        );
    }

    let mut processed = 0usize;
    for row in live {
        let Some(target) = row.nest_url.as_deref() else {
            continue;
        };
        let target = target.trim_end_matches('/').to_string();
        let actor_id = &row.actor_id;
        let actor_hex = hex::encode(actor_id);
        processed += 1;

        // The actor's self-namespace (= the actor pubkey) — the one namespace
        // a pairing reaches (`private-mode.md` § Namespace Sync), and synced
        // even when this nest holds no local entries under it yet: that is
        // where the LAN-TLS cert (Slice 4) and other actor-scoped entries
        // arrive from the peer. Never another account's: one user's pairing
        // must not carry a housemate's namespace to that user's relay, and the peer's door refuses it anyway.
        let key = (target.clone(), actor_hex.clone());
        let since = watermarks.get(&key).copied().unwrap_or(0);
        if let Some(up_to) =
            sync_actor_namespace_once(state, &target, actor_id, &actor_hex, actor_id, since).await
        {
            watermarks.insert(key.clone(), up_to);
        }

        // Mail relay: drain the paired nest's sealed __mail for this actor,
        // append each verbatim locally, and ack so the paired nest purges.
        let actor32: [u8; 32] = match actor_id.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => {
                tracing::warn!("paired actor_id not 32 bytes, skipping mail relay: {actor_hex}");
                continue;
            }
        };
        let mail_since = mail_watermarks.get(&key).copied().unwrap_or(0);
        if let Some(new_watermark) =
            relay_actor_mail(state, &target, &actor32, &actor_hex, mail_since).await
        {
            mail_watermarks.insert(key, new_watermark);
        }

        // Nostr proxy-delegation relay (spec P2.4): if the actor has a locally
        // deposited nsec, bridge its relay to the paired public serving box —
        // push the head's own `origin='ingest'` rows public-ward, then pull the
        // externally-deposited ones head-ward. Unlike mail's in-memory
        // watermarks, the cursors are DB-persisted (`nostr_federation_cursors`),
        // so no HashMap is threaded through the cycle. A keyless/proxied actor
        // has no deposited nsec and is skipped silently inside the arm.
        #[cfg(feature = "nostr")]
        {
            let _ = relay_actor_nostr(state, &target, &actor32, &actor_hex).await;
        }
    }

    SyncCycleOutcome::Processed(processed)
}

/// Sync one (namespace, actor) pair with the paired nest in a single cycle:
/// pull the peer's entries (writing each `"remote"`, and **installing a LAN-TLS
/// cert entry** sealed to this nest — see
/// [`crate::lan_cert::install_client_issued_cert`]), then push this nest's local
/// entries. Returns the new pull watermark (`up_to`) when the pull succeeded,
/// else `None` (keep the prior watermark). The push runs regardless of the pull
/// outcome, matching the pre-factor loop.
///
/// `pub` so the tier_3 `conformance_cross_nest_lan_cert` test drives the real
/// worker step over the channel (mirrors [`relay_actor_mail`]).
pub async fn sync_actor_namespace_once(
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id: &[u8],
    actor_hex: &str,
    ns: &[u8],
    since: i64,
) -> Option<i64> {
    // Pull from the peer over the federation channel (the sole Fauna↔Fauna
    // carrier since slice 5), yielding the byte-native `FedSyncPullReply`.
    let new_watermark = match crate::federation_pool::originate_sync_pull(
        &state.federation_pool,
        state,
        peer_url,
        actor_hex,
        ns,
        since,
    )
    .await
    {
        Ok(response) => {
            for entry in &response.entries {
                let _ = state
                    .db
                    .namespace_put_with_source(
                        ns,
                        &entry.entry_id,
                        &entry.ciphertext,
                        &entry.actor_sig,
                        "remote",
                    )
                    .await;
                // LAN-TLS cert distribution (Slice 4): consume the well-known cert
                // entry pulled from the paired nest — verify the actor's
                // authorization, unseal with this nest's identity key, and write
                // the PEM plaintext to acme_dir for the MDA's seal-on-read path.
                //
                // No NAT-axis gate: the **seal target is the authorization**
                // (`lan_cert` module docs). A nest can only open a blob sealed to
                // its own identity key, so a peer's cert is inert here regardless
                // of mode, and a private nest that later flips to public must not
                // silently stop consuming its own certs.
                if entry.entry_id.as_slice() == fauna_mls::wrapped_blob::LAN_TLS_CERT_ENTRY_ID {
                    crate::lan_cert::install_client_issued_cert(
                        state,
                        actor_id,
                        &entry.ciphertext,
                        &entry.actor_sig,
                    )
                    .await;
                }
            }
            Some(response.up_to)
        }
        Err(e) => {
            tracing::warn!(
                "sync pull (channel) failed for actor {} namespace {}: {e}",
                actor_hex,
                hex::encode(ns)
            );
            None
        }
    };

    // Push local entries to the peer (regardless of pull outcome, as before).
    let local_only: Vec<_> = state
        .db
        .namespace_entries_since(ns, since, 1000)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.source == "local")
        .collect();
    if !local_only.is_empty()
        && let Err(e) = crate::federation_pool::originate_sync_push(
            &state.federation_pool,
            state,
            peer_url,
            actor_hex,
            ns,
            &local_only,
        )
        .await
    {
        tracing::warn!("sync push (channel) failed: {e}");
    }

    new_watermark
}

/// Relay one actor's mail from the paired public nest, one poll cycle. Pulls the
/// sealed `__mail` records after `since_seq` over
/// `fauna.federation.sync.mail_pull`, appends each verbatim to the local
/// `__mail` (identity + dedup derive from the verbatim bytes inside
/// `append_sealed_record`, so a re-pull after a lost ack does not
/// double-append), then acks over `.mail_ack` — which tombstones + purges
/// them on the public nest.
///
/// Returns `Some(new_watermark)` when the ack advanced the cursor, else `None`
/// (nothing new, or a transient failure to retry next cycle).
///
/// **Mail acks destructively (purge-on-ack), unlike the namespace sync above
/// (which never purges on pull).** So this only acks up to the highest
/// **contiguous** successfully-stored seq: if appending record N fails
/// transiently, records ≤ N-1 are acked (and purged) but N and everything after
/// it stay live on the public nest and are re-pulled next cycle. Acking past a
/// failed append would purge mail the private nest never durably stored — data
/// loss. Channel-only: a `PoolError` is a transient transport failure the caller
/// retries next cycle (mail is a new feature with no legacy HTTP peer).
///
/// `pub` so the tier_3 `conformance_cross_nest_mail_relay` test drives the real
/// worker step (not just the raw originators).
pub async fn relay_actor_mail(
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id: &[u8; 32],
    actor_hex: &str,
    since_seq: i64,
) -> Option<i64> {
    let reply = match crate::federation_pool::originate_mail_pull(
        &state.federation_pool,
        state,
        peer_url,
        actor_hex,
        since_seq,
    )
    .await
    {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!("mail relay pull (channel) failed for actor {actor_hex}: {e}");
            return None;
        }
    };
    if reply.records.is_empty() {
        return None;
    }

    // Records arrive oldest-first (seq ascending). `ack_through` tracks the
    // highest contiguous seq durably on the private side; the first failure
    // freezes it (see the doc comment — purge-on-ack must not outrun storage).
    let mut ack_through = since_seq;
    let mut contiguous = true;
    for rec in &reply.records {
        // Identity is derived from the verbatim bytes at the append itself
        // (`message-segment-store.md` § Record identity per kind), so the
        // re-pull-after-lost-ack dedup lives INSIDE `append_sealed_record`
        // (scoped, under the seq lock) and is stable across the relay by
        // construction — the wire's carried `record_id` is not consulted.
        let stored = {
            match fauna_mail::segments::MailFloorMetadata::decode(&rec.floor) {
                Ok(floor) => {
                    // Derive the placement target BEFORE `floor` is moved into
                    // the append (the append re-stamps `seq` but leaves these
                    // fields untouched).
                    let (mailbox, initial_flags) = relayed_mailbox_and_flags(&floor);
                    // The ORIGIN nest's own receipt instant, forwarded
                    // verbatim — never re-stamped here, because this box is
                    // not where the message was received, and never taken
                    // from `floor.timestamp` (the sender's own `Date:`
                    // header). `message-segment-store.md` § Invariants is why
                    // this field survives the relay while `stored_at` is
                    // overwritten locally; `imap-server.md` § SEARCH is why it,
                    // and not the header, is what INTERNALDATE means.
                    let internal_date = floor.received_at / 1000;
                    let sender_domain = floor.sender_domain.clone();
                    // A continuation PART relays like any record (it must — so it
                    // gets a local seq and the contiguous ack advances through
                    // it), but it is invisible to every content surface, so it
                    // gets NO IMAP placement row. Only the HEAD (a normal, non-
                    // part floor) places and becomes IMAP-visible; the head's
                    // serve-join then reads the parts. (message-segment-store.md
                    // § Continuation records — parts carry no placement/projection
                    // row.)
                    let is_continuation_part =
                        floor.continuation_role == fauna_mail::segments::CONTINUATION_ROLE_PART;
                    match append_relayed_record(state, actor_id, &rec.envelope, floor).await {
                        Ok(outcome) => {
                            // The segment is durably stored; now make it readable
                            // over IMAP / the client inbox-fetch by PLACING it
                            // into its mailbox — the relay's sibling of the
                            // MTA-ingest placement in `persist_decoded_inbound_mail`.
                            // Without this the relayed record lands in `__mail`
                            // but is invisible to the MDA (the IMAP fetch joins
                            // `bridge_imap_messages`). The home box assigns its
                            // OWN local UID; it does not inherit the source's.
                            // See `deployment-home-with-public-relay.md`
                            // § Inbound mail step 5. A continuation part is
                            // deliberately NOT placed (see above) but still counts
                            // as `stored` so the ack cursor advances past it. A
                            // dedup hit (`inserted == false`) is likewise not
                            // re-placed — the record placed when it first stored.
                            // Placement keys on the DERIVED content-hash digest,
                            // never the wire's carried id (a peer's carried id is not
                            // trusted to match this store's identity).
                            if !is_continuation_part && outcome.inserted {
                                place_relayed_record(
                                    state,
                                    actor_id,
                                    &outcome.cid.digest(),
                                    mailbox,
                                    internal_date,
                                    initial_flags,
                                    &sender_domain,
                                )
                                .await;
                            }
                            true
                        }
                        Err(e) => {
                            tracing::warn!("mail relay append failed for actor {actor_hex}: {e}");
                            false
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "mail relay decode forwarded floor failed for actor {actor_hex}: {e}"
                    );
                    false
                }
            }
        };

        if stored && contiguous {
            ack_through = rec.seq;
        } else if !stored {
            contiguous = false;
        }
    }

    if ack_through <= since_seq {
        // Nothing durably stored beyond the cursor this cycle — retry next time.
        return None;
    }

    match crate::federation_pool::originate_mail_ack(
        &state.federation_pool,
        state,
        peer_url,
        actor_hex,
        ack_through,
    )
    .await
    {
        Ok(_purged) => Some(ack_through),
        Err(e) => {
            // The records are stored locally but not yet purged on the public
            // nest; dedup makes the re-pull + re-ack next cycle safe.
            tracing::warn!("mail relay ack (channel) failed for actor {actor_hex}: {e}");
            None
        }
    }
}

/// Bridge one actor's Nostr relay with the paired public serving box, one poll
/// cycle (spec P2.4). Gate: the actor has a **locally deposited nsec** — a
/// keyless/proxied actor runs no agent acts, so the arm returns `0` silently.
/// The head owns and persists the compound `(stored_at, id)` cursors per
/// `(actor, peer public box)` in `nostr_federation_cursors`, advancing each only
/// after the corresponding leg succeeds (at-least-once + event-id dedup =
/// exactly-once effect, crash-safe).
///
/// **Push** selects the head's `origin='ingest'` rows in the `pubkey ∪ #p(1059)`
/// scope after the push cursor, pages them within one WS frame, sends over
/// `fauna.federation.sync.nostr_push` (carrying `pubkey`/`relay_list` so the box
/// auto-provisions the proxied account, spec R9 (account-data-plane.md § The ratified decisions)), and on a successful reply
/// advances the push cursor past the whole batch — rejects too, each logged
/// loudly (spec R6). **Pull** fetches the public box's externally-deposited
/// `origin='ingest'` rows after the pull cursor and ingests each (sealing wraps
/// via the deposited key the gate guarantees), advancing the pull cursor to the
/// reply's `up_to_*` only if the whole page ingested with no transient error — a
/// transient DB failure leaves the cursor unmoved so the page re-pulls next cycle
/// (dedup makes it idempotent), never advancing past a not-durably-ingested row.
///
/// Returns the number of events processed (pushed + ingested) this cycle. `pub`
/// so the tier_3 proxy-delegation conformance test can drive the real arm.
#[cfg(feature = "nostr")]
pub async fn relay_actor_nostr(
    state: &Arc<AppState>,
    peer_url: &str,
    actor_id: &[u8; 32],
    actor_hex: &str,
) -> usize {
    let _ = actor_id; // scoped by `actor_hex`; kept for signature parity with the mail arm.
    // Selection page cap (mirrors the mail relay's 500).
    const NOSTR_RELAY_LIMIT: usize = crate::segments::SERVE_PAGE_MAX_RECORDS as usize;

    // Gate: a locally deposited nsec. A keyless/proxied actor has none → skip.
    let (pubkey, relay_list) = {
        let conn = state.db.conn().await;
        match crate::nostr::db::get_account(&conn, actor_hex) {
            Ok(Some(acct)) if acct.encrypted_privkey.is_some() => {
                (acct.nostr_pubkey, acct.relay_list)
            }
            Ok(_) => return 0,
            Err(e) => {
                tracing::warn!("nostr relay: account lookup failed for actor {actor_hex}: {e}");
                return 0;
            }
        }
    };

    // The peer public box's nest id keys the persisted cursor row. The pool
    // resolves URL→nest_id (cached), so this is the same verified id the push/
    // pull dial targets — the cursor lives in one coordinate space (spec R5).
    let peer_nest_id = match state.federation_pool.resolve_peer_nest_id(peer_url).await {
        Ok(id) => hex::encode(id),
        Err(e) => {
            tracing::warn!("nostr relay: resolve peer nest_id failed for actor {actor_hex}: {e}");
            return 0;
        }
    };

    let cursors = {
        let conn = state.db.conn().await;
        crate::nostr::db::get_federation_cursors(&conn, actor_hex, &peer_nest_id)
            .ok()
            .flatten()
            .unwrap_or_default()
    };

    let mut processed = 0;
    processed += relay_actor_nostr_push(
        state,
        peer_url,
        actor_hex,
        &peer_nest_id,
        &pubkey,
        relay_list.as_deref(),
        (cursors.push_stored_at, &cursors.push_id),
        NOSTR_RELAY_LIMIT,
    )
    .await;
    processed += relay_actor_nostr_pull(
        state,
        peer_url,
        actor_hex,
        &peer_nest_id,
        &pubkey,
        (cursors.pull_stored_at, &cursors.pull_id),
    )
    .await;
    processed
}

/// The head→public **push** half of [`relay_actor_nostr`]. Returns the number of
/// events pushed (the page size) on a successful reply, else `0`.
#[cfg(feature = "nostr")]
#[allow(clippy::too_many_arguments)]
async fn relay_actor_nostr_push(
    state: &Arc<AppState>,
    peer_url: &str,
    actor_hex: &str,
    peer_nest_id: &str,
    pubkey: &str,
    relay_list: Option<&str>,
    after: (i64, &str),
    limit: usize,
) -> usize {
    let rows = {
        let conn = state.db.conn().await;
        match crate::nostr::federation::list_events_for_push(&conn, pubkey, after, limit) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("nostr relay push: select failed for actor {actor_hex}: {e}");
                return 0;
            }
        }
    };
    if rows.is_empty() {
        return 0;
    }
    let (page, rest) = crate::segments::take_page_within_budget(rows, |r| r.raw_json.len());
    if page.is_empty() {
        if let Some(r) = rest.first() {
            tracing::error!(
                actor = %actor_hex,
                stored_at = r.stored_at,
                id = %r.id,
                record_bytes = r.raw_json.len(),
                "nostr relay push: a single stored event exceeds the WS frame budget \
                 — cannot advance past it (remedy is a targeted heal, never a skip)"
            );
        }
        return 0;
    }
    // The batch cursor is the last row in the page (advances past rejects too).
    let (last_stored_at, last_id) = {
        let last = page.last().expect("page is non-empty");
        (last.stored_at, last.id.clone())
    };
    let events = page
        .iter()
        .map(|r| crate::federation_handlers::FedNostrEvent {
            raw_json: r.raw_json.clone(),
        })
        .collect();
    let req = crate::federation_handlers::FedNostrPushRequest {
        actor_id: actor_hex.to_string(),
        pubkey: pubkey.to_string(),
        relay_list: relay_list.map(str::to_string),
        events,
    };
    match crate::federation_pool::originate_nostr_push(&state.federation_pool, state, peer_url, req)
        .await
    {
        Ok(reply) => {
            for rej in &reply.rejected {
                tracing::error!(
                    actor = %actor_hex,
                    event_id = %rej.id,
                    reason = %rej.reason,
                    "nostr relay push: peer rejected an event (cursor advances past it, spec R6)"
                );
            }
            // Advance the push cursor past the whole batch (rejects too).
            let conn = state.db.conn().await;
            if let Err(e) = crate::nostr::db::set_federation_push_cursor(
                &conn,
                actor_hex,
                peer_nest_id,
                last_stored_at,
                &last_id,
            ) {
                tracing::warn!(
                    "nostr relay push: persist cursor failed for actor {actor_hex}: {e}"
                );
            }
            page.len()
        }
        Err(e) => {
            tracing::warn!("nostr relay push (channel) failed for actor {actor_hex}: {e}");
            0
        }
    }
}

/// The public→head **pull** half of [`relay_actor_nostr`]. Returns the number of
/// events ingested. Advances the pull cursor to the reply's `up_to_*` only if the
/// entire page ingested without a transient error (else the cursor stays put and
/// the page re-pulls next cycle — event-id dedup makes it idempotent).
#[cfg(feature = "nostr")]
async fn relay_actor_nostr_pull(
    state: &Arc<AppState>,
    peer_url: &str,
    actor_hex: &str,
    peer_nest_id: &str,
    pubkey: &str,
    after: (i64, &str),
) -> usize {
    let req = crate::federation_handlers::FedNostrPullRequest {
        actor_id: actor_hex.to_string(),
        since_stored_at: after.0,
        since_id: after.1.to_string(),
    };
    let reply = match crate::federation_pool::originate_nostr_pull(
        &state.federation_pool,
        state,
        peer_url,
        req,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("nostr relay pull (channel) failed for actor {actor_hex}: {e}");
            return 0;
        }
    };
    if reply.events.is_empty() {
        return 0;
    }
    let mut ingested = 0;
    let mut all_ok = true;
    for ev in &reply.events {
        match crate::nostr::federation::ingest_federated_event(state, pubkey, &ev.raw_json).await {
            Ok(crate::nostr::federation::IngestOutcome::Rejected(reason)) => {
                // A terminal gate reject (out-of-scope / bad sig) — logged, but
                // not a reason to stall (mirrors the push reject-advance rule).
                tracing::warn!(
                    actor = %actor_hex,
                    reason = %reason,
                    "nostr relay pull: ingest rejected an event"
                );
                ingested += 1;
            }
            Ok(_) => ingested += 1,
            Err(e) => {
                // Transient store error — stop and leave the cursor unmoved so
                // the page re-pulls next cycle (dedup makes the retry a no-op).
                tracing::warn!(
                    "nostr relay pull: transient ingest error for actor {actor_hex} \
                     (cursor not advanced): {e}"
                );
                all_ok = false;
                break;
            }
        }
    }
    if all_ok {
        let conn = state.db.conn().await;
        if let Err(e) = crate::nostr::db::set_federation_pull_cursor(
            &conn,
            actor_hex,
            peer_nest_id,
            reply.up_to_stored_at,
            &reply.up_to_id,
        ) {
            tracing::warn!("nostr relay pull: persist cursor failed for actor {actor_hex}: {e}");
        }
    }
    ingested
}

/// Append one relayed mail record **verbatim** (Phase-3 D3,
/// `2026-07-07-phase-3-sealed-both-modes-design.md`: unseal-on-receipt is
/// retired — with sealed-at-rest universal, relayed sealed mail is stored
/// exactly as the source sealed it, in both storage modes; the AUTH'd MDA /
/// the recipient's client opens it on read). Never drops the record: the
/// relay's contiguous-ack invariant depends on every pulled record either
/// storing or freezing the cursor.
async fn append_relayed_record(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    envelope_bytes: &[u8],
    floor: fauna_mail::segments::MailFloorMetadata,
) -> anyhow::Result<crate::segments::mail::AppendOutcome> {
    crate::segments::mail::append_sealed_record(
        &state.mail_segments,
        &state.db,
        actor_id,
        envelope_bytes,
        floor,
    )
    .await
}

/// The mailbox + initial flags a relayed record lands in, derived from its
/// floor — the relay's mirror of the MTA-ingest placement choice in
/// `bridge_routing_handlers::persist_decoded_inbound_mail` (own-submission →
/// `Sent`/`\Seen`; otherwise the spam disposition → `INBOX` / `Junk`). An
/// unrecognized disposition falls back to `INBOX` (the source nest already
/// accepted + sealed it).
fn relayed_mailbox_and_flags(
    floor: &fauna_mail::segments::MailFloorMetadata,
) -> (&'static str, &'static str) {
    if floor.is_own_submission {
        ("Sent", "\\Seen")
    } else {
        match floor.spam_disposition.as_str() {
            "accept_to_spam_folder" | "policy_junk" => ("Junk", ""),
            _ => ("INBOX", ""),
        }
    }
}

/// Place a freshly-relayed mail record into `mailbox` so the home box's MDA
/// serves it over IMAP (and the client inbox-fetch finds it) — the relay-side
/// analogue of `persist_decoded_inbound_mail`'s placement step. The record's
/// segment is already durably stored; this assigns the home box's **own** local
/// UID (it does not inherit the source's) and writes the `bridge_imap_messages`
/// placement row the IMAP fetch path joins, then appends the `Append` journal
/// record (after ensuring the standard mailboxes + their `Create` records, spec
/// § D2 ordering).
///
/// Best-effort: a placement failure logs but does NOT fail the relay — the
/// segment is durably stored and the relay still acks, so the public nest can
/// purge; IMAP visibility is recoverable by a re-place / divergence repair (the
/// same crash-window deferral the ingest path documents). Only ever called for a
/// record newly appended this cycle (the caller's dedup skips records already
/// present, which a prior cycle already placed), so `content_was_new = true`.
///
/// `pub` so a tier_3 harness that needs **filed** mail files it through this
/// function rather than through a lookalike of it: a backup corpus's journal is
/// only worth restoring if it is the journal production writes
/// (`tests/nest_backup_coordinator.rs`).
pub async fn place_relayed_record(
    state: &Arc<AppState>,
    actor_id: &[u8; 32],
    record_id: &[u8],
    mailbox: &str,
    internal_date: i64,
    initial_flags: &str,
    sender_domain: &str,
) {
    use fauna_mail::segments::placement::MailPlacementRecord;

    let rid: [u8; 32] = match record_id.try_into() {
        Ok(r) => r,
        Err(_) => {
            tracing::warn!("mail relay: record_id wrong length for placement; skipping");
            return;
        }
    };

    // Ensure the six standard mailboxes exist (emitting their `Create` journal
    // records) before the `Append` — idempotent; a no-op once seeded.
    match state.db.ensure_bridge_imap_mailboxes(actor_id).await {
        Ok(newly_seeded) => {
            for seeded in newly_seeded {
                let create = MailPlacementRecord::Create {
                    mailbox: seeded.name,
                    uid_validity: seeded.uid_validity,
                    attrs: seeded.attrs,
                };
                if let Err(e) = state.mail_placement.append_event(actor_id, &create).await {
                    tracing::warn!("mail relay: placement Create append failed: {e}");
                }
            }
        }
        Err(e) => {
            tracing::warn!("mail relay: ensure_bridge_imap_mailboxes failed: {e}");
            return;
        }
    }

    match state
        .db
        .place_inbound_mail(
            actor_id,
            &rid,
            mailbox,
            internal_date,
            initial_flags,
            sender_domain,
            true,
        )
        .await
    {
        Ok(Some((uid, modseq))) => {
            let flags: Vec<String> = initial_flags.split_whitespace().map(String::from).collect();
            let append = MailPlacementRecord::Append {
                mailbox: mailbox.to_string(),
                uid,
                modseq: modseq as u64,
                flags,
                content_record_id: record_id.to_vec(),
                internal_date,
            };
            if let Err(e) = state.mail_placement.append_event(actor_id, &append).await {
                tracing::warn!("mail relay: placement Append append failed: {e}");
            }
            // Wake the recipient's client (IMAP IDLE / inbox-fetch) — the record
            // is now both segment-stored AND placed, so the fetch finds it.
            crate::segments::notify_mail_received(&state.ws, actor_id);
        }
        Ok(None) => {
            // `content_was_new = true` ⇒ a fresh placement is expected; `None`
            // only happens for a duplicate (content_was_new=false), which the
            // caller's dedup already excludes. Log if it somehow occurs.
            tracing::debug!(
                "mail relay: place_inbound_mail returned None for a newly stored record"
            );
        }
        Err(e) => {
            tracing::warn!("mail relay: place_inbound_mail failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: &str = "https://relay.example";

    fn row(id: u8, url: Option<&str>) -> (Vec<u8>, Option<String>) {
        (vec![id; 32], url.map(str::to_string))
    }

    fn target(
        actor: u8,
        peer: u8,
        url: Option<&str>,
        admin: bool,
        expired: bool,
    ) -> PairingTargetRow {
        PairingTargetRow {
            actor_id: vec![actor; 32],
            peer_nest_id: vec![peer; 32],
            nest_url: url.map(str::to_string),
            capabilities: vec![],
            expired,
            actor_is_admin: admin,
        }
    }

    /// A URL's pin is the one nest the rows recorded at it, trailing slash
    /// aside; no row there is unseeded; several ids stay unpinned (never a
    /// fault).
    #[test]
    fn select_pair_pin_cases() {
        assert_eq!(select_pair_pin(RELAY, &[]), PairPin::Unseeded);
        // Rows recorded elsewhere say nothing about this URL.
        assert_eq!(
            select_pair_pin(RELAY, &[row(1, None), row(1, Some("https://other"))]),
            PairPin::Unseeded
        );
        // The row recorded at the URL decides, trailing slash aside.
        assert_eq!(
            select_pair_pin(
                "https://relay.example/",
                &[row(1, Some("https://b.example")), row(2, Some(RELAY))]
            ),
            PairPin::Pinned([2; 32])
        );
        // Two ids both recorded at the URL: ambiguous.
        assert_eq!(
            select_pair_pin(RELAY, &[row(1, Some(RELAY)), row(2, Some(RELAY))]),
            PairPin::Ambiguous(2)
        );
        // A malformed id is skipped.
        assert_eq!(
            select_pair_pin(
                RELAY,
                &[(vec![1; 5], Some(RELAY.into())), row(3, Some(RELAY))]
            ),
            PairPin::Pinned([3; 32])
        );
    }

    /// Every recorded URL is pinned; only a live admin row's URL is exempt —
    /// an expired admin row, or any number of non-admin rows, exempts
    /// nothing; a row with no URL is not a target.
    #[test]
    fn pairing_target_table_exempts_only_a_live_admins_row() {
        let table = pairing_target_table(&[
            target(1, 9, Some("https://relay.example/"), true, false),
            target(2, 9, Some(RELAY), false, false),
            target(3, 8, Some("https://friend.example"), false, false),
            target(4, 7, Some("https://old.example"), true, true),
            target(5, 6, None, true, false),
        ]);
        assert_eq!(table.len(), 3);
        assert_eq!(
            table[RELAY],
            PairingTargetTrust {
                exempt: true,
                pin: Some([9; 32])
            }
        );
        assert_eq!(
            table["https://friend.example"],
            PairingTargetTrust {
                exempt: false,
                pin: Some([8; 32])
            }
        );
        assert_eq!(
            table["https://old.example"],
            PairingTargetTrust {
                exempt: false,
                pin: Some([7; 32])
            },
            "an expired admin row still pins, but exempts nothing"
        );
    }

    /// An exempt URL's pin is chosen from the live admin rows alone: a
    /// non-admin's row naming it with another id cannot make it ambiguous and
    /// so unpin it. Several ids among the admins' own rows stay unpinned; a
    /// URL no live admin row names keeps the all-rows rule.
    #[test]
    fn an_exempt_urls_pin_is_chosen_from_the_admin_rows_alone() {
        let table = pairing_target_table(&[
            target(1, 0x0A, Some(RELAY), true, false),
            target(2, 0x0B, Some("https://relay.example/"), false, false),
            target(3, 8, Some("https://friend.example"), false, false),
            target(4, 7, Some("https://friend.example"), false, false),
            target(5, 6, Some("https://two.example"), true, false),
            target(6, 5, Some("https://two.example"), true, false),
            target(7, 4, Some("https://lapsed.example"), true, true),
            target(8, 3, Some("https://lapsed.example"), false, false),
        ]);
        assert_eq!(
            table[RELAY],
            PairingTargetTrust {
                exempt: true,
                pin: Some([0x0A; 32])
            },
            "a non-admin's row at an admin's URL must not unpin it"
        );
        assert_eq!(
            table["https://friend.example"],
            PairingTargetTrust {
                exempt: false,
                pin: None
            },
            "a URL no admin row names keeps the all-rows rule"
        );
        assert_eq!(
            table["https://two.example"],
            PairingTargetTrust {
                exempt: true,
                pin: None
            },
            "two ids among the admins' own rows stay unpinned"
        );
        assert_eq!(
            table["https://lapsed.example"],
            PairingTargetTrust {
                exempt: false,
                pin: None
            },
            "an expired admin row exempts nothing, so every row decides the pin"
        );
    }
}
