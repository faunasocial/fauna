//! The T16 custody facet's **act** half — the shared assembly every app's
//! Devices page drives its five custody gestures through
//! (`docs/goal/ui/devices.md` § Custody facet).
//!
//! The split, and why it is where it is: the facet's PURE half — the three
//! folds, [`fauna_client_capabilities::view_model::fold_custody_facet`], the
//! registry overlay, the budget seed texts — lives in
//! `fauna-client-capabilities`, which is wasm-clean so the web leg renders from
//! the same projection the native apps do. The acts need both an
//! `AccountStoreHandle` (`fauna-sync-engine`) and a conversations session
//! (`fauna-client-conversations`), which are native-only *and* independent
//! siblings, so they can only be assembled ABOVE both. That is this crate.
//!
//! Everything semantic is already shared — `custody_ceremony::{build_accept,
//! decline_offer, begin_offer, drive_ceremonies}` and
//! `grant_log::record_revoke`. This crate only assembles the doors those seams
//! name, and it exists so the seven app shells do not each re-assemble them:
//! two of the orderings below are subtle enough that an independent
//! re-derivation gets them wrong (see [`run_custody_act`]).
//!
//! Lifted out of `apps/fauna-tui` 2026-08-17 with no
//! behaviour change; tui was the only caller at the time.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_capabilities::custody_ceremony::CeremonyRecords;
use fauna_client_capabilities::custody_view::CustodyMintCandidateView;
use fauna_client_capabilities::view_model::CustodyFacetSnapshot;
use fauna_conversations::ConversationsSession;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_sync_engine::account_runtime::AccountStoreHandle;

/// The account-store seams — the succession ledger and the custody-ceremony
/// state — over this host's optional store handle: the handle itself, or the
/// not-ready refusal ([`fauna_client_config::LEDGER_NOT_READY`]) before the
/// store is up.
fn store_seam(
    store: Option<AccountStoreHandle>,
) -> fauna_client_config::ResolvingLedgerStore<impl Fn() -> Option<AccountStoreHandle>> {
    fauna_client_config::ResolvingLedgerStore::new(move || store.clone())
}

/// Load + fold the custody facet: the ceremony records
/// (`fauna.state.custody-ceremony`) and the grant log
/// (`fauna.state.succession-ledger`) off the account store, the shared
/// three-family fold, and the R14 (account-data-plane.md § The ratified decisions) registry-row overlay (the
/// budget in force + the stop mark).
///
/// `None` = the account store is not up yet, or the records or the grant log
/// were unreadable this pass. Callers keep their previous facet rather than
/// painting an empty one over live rows — an unreadable log is a transient,
/// not "the user has no custodians", and folding the records against an empty
/// log would read every minted custody as revoked.
pub async fn load_custody_facet(
    nest: Arc<NestClient>,
    store: Option<AccountStoreHandle>,
) -> Option<CustodyFacetSnapshot> {
    let store = store?;
    // Stage (c)'s owner-side leg rides EVERY facet refresh (the reconcile
    // sweep's cadence precedent), so the freshly-fetched receipt is in the
    // very snapshot this visit paints — the Nests page's three-state
    // freshness then never waits on a ceremony edge.
    ingest_nest_receipts(&nest, &store).await;
    let custody = store.snapshot().await.ok()?;
    let ledger = {
        use fauna_client_config::SuccessionLedgerStore;
        store.load().await.ok()?
    };
    // The trust facet's render clock (`trust_clock`): real, plus an e2e offset
    // that is zero in every real run — how a journey reaches a stale receipt.
    let now_micros = fauna_client_capabilities::trust_clock::render_now_micros(Timestamp::now().0);
    let mut facet =
        fauna_client_capabilities::view_model::fold_custody_facet(&custody, &ledger, now_micros);
    if let Ok(entries) = store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIES_HELD)
        .await
    {
        let decoded: Vec<fauna_core::custodies_held::CustodyHeld> = entries
            .iter()
            .filter_map(|e| fauna_core::encoding::canonical_decode(&e.value).ok())
            .collect();
        facet.overlay_registry_rows(&decoded);
    }
    Some(facet)
}

/// The offer flow's host options (`custody-mint-host-select`), already
/// projected onto the shared boundary row.
///
/// One call rather than three steps, because all three are places a leg could
/// drift: deriving the owner's own [`ActorId`] from its secret (the
/// self-exclusion depends on it), the candidate rules themselves
/// (`fauna_client_conversations::mint_candidates_from_channels`, pinned by its
/// own tests), and the projection to the boundary row.
///
/// An **empty** list is the answer "there is no one to ask yet" — the ceremony
/// rides an existing 1:1 conversation, so a leg must refuse to open the flow
/// and say so (`devices.custody_mint_no_contacts`) rather than presenting an
/// empty picker. tui's `CustodyMintOpen` is the reference.
pub fn mint_candidates(
    session: &ConversationsSession,
    secret: [u8; 32],
) -> Vec<CustodyMintCandidateView> {
    let own = ActorKeypair::from_secret(secret).actor_id();
    fauna_client_conversations::custody_mint_candidates(session, &own)
        .iter()
        .map(project_mint_candidate)
        .collect()
}

fn project_mint_candidate(
    c: &fauna_client_conversations::CustodyMintCandidate,
) -> CustodyMintCandidateView {
    c.view()
}

/// Everything a custody act needs off the app at dispatch time — cloned into
/// the spawned op, so the act never borrows app state across an await.
pub struct CustodyCtx {
    pub nest: Arc<NestClient>,
    pub secret: [u8; 32],
    pub store: Option<AccountStoreHandle>,
    pub session: Option<Arc<ConversationsSession>>,
}

/// One custody gesture, resolved to its grant at dispatch time.
///
/// Each act carries the **grant id, never the row index** — a facet refresh
/// re-orders rows, so an index captured at paint time can address a different
/// custody by the time the act runs.
pub enum CustodyAct {
    Accept {
        grant_id: Vec<u8>,
        /// The host-side choice (the nest-custodian identity fact): `false`
        /// binds THIS device; `true` binds the host's NEST — legal only under
        /// an advertising offer with a pinned nest identity in hand (the
        /// consent card offers the choice only then; this arm re-checks).
        on_nest: bool,
    },
    Decline {
        grant_id: Vec<u8>,
    },
    SetBudget {
        grant_id: Vec<u8>,
        cap: u64,
    },
    Stop {
        grant_id: Vec<u8>,
    },
    /// `custody-held-remove-button` — the RECLAIM: drop the hosting
    /// row and free the bytes. [`Self::Stop`] only pauses the pull and keeps
    /// them; this is the affordance that gives the space back.
    Remove {
        grant_id: Vec<u8>,
    },
    Revoke {
        grant_id: Vec<u8>,
        /// The accept-bound custodian key — the `Revoke` event's holder.
        holder: Option<[u8; 32]>,
    },
    /// Offer initiation (`custody-mint-confirm-button`): record the offer on a
    /// fresh ceremony record (Account scope, default window) and drive — the
    /// drive posts it over the chosen conversation's channel.
    Mint {
        host: fauna_core::identity::ActorId,
        channel_hex: String,
    },
}

/// Resolve this machine's DEVICE PRINCIPAL — the T10 writer key's public, which
/// IS the peer-plane NodeId (R5).
///
/// ⚠️ What a custody accept binds and what an offer's `owner_devices` discovery
/// seed names — **NEVER the roster's `device.db` id**, which is a random
/// 32-byte sync-registry id no transport can prove. A custody bound to that
/// could neither pull nor attest; it is the wiring bug the two-account journey
/// caught on 2026-08-17, and it is exactly the kind of thing a per-app
/// re-derivation of these acts would reintroduce.
///
/// Blocking I/O, so it runs on a blocking task.
async fn resolve_device_principal(secret: [u8; 32]) -> Result<[u8; 32], String> {
    let actor_hex = fauna_core::hex32::encode(&ActorKeypair::from_secret(secret).actor_id().0);
    tokio::task::spawn_blocking(move || {
        let creds = fauna_sync_engine::account_runtime::production_credential_store();
        fauna_sync_engine::account_runtime::resolve_writer_key_serialized(
            &fauna_sync_engine::account_runtime::StoreRoot::platform()?,
            &actor_hex,
            &creds,
        )
        .map(|k| k.verifying_key().to_bytes())
    })
    .await
    .map_err(|e| format!("writer-key resolve task: {e}"))
    .and_then(|r| r.map_err(|e| format!("{e:#}")))
}

/// Run one custody act, then re-fold the facet.
///
/// The error string (if any) rides the outcome onto the page's `error-message`
/// — a custody gesture must never be silently dropped (e2e convention 11).
///
/// Two orderings here are load-bearing and are the reason this is shared rather
/// than re-derived per app:
///
/// 1. **Accept binds the T10 writer key**, not the roster device id — see
///    [`resolve_device_principal`].
/// 2. **Revoke hits the nest BEFORE recording the signed event.** A recorded
///    revoke the nest never saw would leave the capability live: the log would
///    say "revoked" while the holder kept serving.
pub async fn run_custody_act(
    ctx: CustodyCtx,
    act: CustodyAct,
) -> (Option<CustodyFacetSnapshot>, Option<String>) {
    use fauna_client_capabilities::custody_ceremony as ceremony;
    let keypair = ActorKeypair::from_secret(ctx.secret);
    let records = store_seam(ctx.store.clone());
    let now = Timestamp::now();
    let mut error: Option<String> = None;
    match act {
        CustodyAct::Accept { grant_id, on_nest } => {
            if on_nest {
                // The NEST form: the bound principal is the host's PINNED
                // nest actor identity (TOFU state, never the nest's own
                // claim), the anchor its URL — both from what this app
                // already holds for its own home connection. No pin → the
                // choice is absent, and this arm answers the same way if
                // reached anyway (the escrow-holder rule's no-pin arm).
                let nest_url = ctx.nest.nest_url();
                match fauna_anon_client::trust::pinned_nest_custodian_identity(&nest_url) {
                    None => {
                        error = Some(
                            "no pinned identity for your nest — connect to it once first"
                                .to_string(),
                        );
                    }
                    Some(nest_key) => {
                        let result = records
                            .update(|cfg| {
                                ceremony::build_accept_nest(
                                    cfg,
                                    &keypair,
                                    &grant_id,
                                    nest_key,
                                    nest_url.clone(),
                                    fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP,
                                    None,
                                    now,
                                )
                            })
                            .await;
                        match result {
                            // Recorded (unposted); the drive posts the accept
                            // AND deposits the hosting row once the deliver
                            // lands.
                            Ok(Ok(_)) => spawn_drive(
                                Arc::clone(&ctx.nest),
                                ctx.secret,
                                ctx.session.clone(),
                                ctx.store.clone(),
                            ),
                            Ok(Err(e)) => error = Some(e.to_string()),
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                }
            } else {
                match resolve_device_principal(ctx.secret).await {
                    Err(e) => {
                        error = Some(format!("this device has no store principal to bind — {e}"));
                    }
                    Ok(key) => {
                        let result = records
                            .update(|cfg| {
                                ceremony::build_accept(
                                    cfg,
                                    &keypair,
                                    &grant_id,
                                    key,
                                    fauna_core::device_endpoints::DeviceEndpoints {
                                        node_id: key,
                                        // Address candidates heal over the admit
                                        // endpoint re-exchange (T13 step 4) — the
                                        // accept needs only the identity.
                                        ..Default::default()
                                    },
                                    fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP,
                                    None,
                                    now,
                                )
                            })
                            .await;
                        match result {
                            // The accept is recorded (unposted) — the drive posts it.
                            Ok(Ok(_)) => spawn_drive(
                                Arc::clone(&ctx.nest),
                                ctx.secret,
                                ctx.session.clone(),
                                ctx.store.clone(),
                            ),
                            Ok(Err(e)) => error = Some(e.to_string()),
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                }
            }
        }
        CustodyAct::Decline { grant_id } => {
            if let Err(e) = records
                .update(|cfg| {
                    ceremony::decline_offer(cfg, &grant_id, now);
                })
                .await
            {
                error = Some(e.to_string());
            }
        }
        CustodyAct::SetBudget { grant_id, cap } => {
            error = put_runtime_row_update(&ctx, &records, &grant_id, |cap_slot, stopped_slot| {
                let _ = stopped_slot;
                *cap_slot = cap;
            })
            .await;
        }
        CustodyAct::Stop { grant_id } => {
            error = put_runtime_row_update(&ctx, &records, &grant_id, |cap_slot, stopped_slot| {
                let _ = cap_slot;
                *stopped_slot = true;
            })
            .await;
        }
        CustodyAct::Remove { grant_id } => {
            error = remove_held_custody(&ctx, &records, &grant_id, now).await;
        }
        CustodyAct::Mint { host, channel_hex } => {
            // The shared assembly (`custody_acts::mint_custody`, wasm-clean so
            // web runs it too) refuses a session-less mint — the drive posts
            // the offer over the chosen conversation's channel, and recording
            // one with nothing to post it on is a silent drop (e2e convention
            // 11) — and a `(host, channel)` pair the candidate fold never
            // offered, before anything is recorded.
            let candidates = ctx
                .session
                .as_deref()
                .map(|session| mint_candidates(session, ctx.secret));
            // The offer's dial-identity discovery seed: this device's real
            // principal (addresses heal over the T13 step-4 re-exchange),
            // resolved only for a pair the shared check will accept. A failed
            // resolve degrades to an empty seed — the nest URL is the
            // always-on anchor either way.
            let offered = candidates.as_deref().is_some_and(|c| {
                fauna_client_capabilities::custody_acts::host_channel_is_a_mint_candidate(
                    c,
                    &host.0,
                    &channel_hex,
                )
            });
            let owner_devices = if offered {
                match resolve_device_principal(ctx.secret).await {
                    Ok(key) => vec![fauna_core::device_endpoints::DeviceEndpoints {
                        node_id: key,
                        ..Default::default()
                    }],
                    Err(e) => {
                        tracing::warn!(
                            "custody mint: no device principal for the offer seed ({e})"
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            error = fauna_client_capabilities::custody_acts::mint_custody(
                &records,
                &keypair,
                candidates.as_deref(),
                host,
                channel_hex,
                owner_devices,
                ctx.nest.nest_url(),
                now,
            )
            .await;
            if error.is_none() {
                // The offer is recorded (unposted) — the drive posts it.
                spawn_drive(
                    Arc::clone(&ctx.nest),
                    ctx.secret,
                    ctx.session.clone(),
                    ctx.store.clone(),
                );
            }
        }
        CustodyAct::Revoke { grant_id, holder } => {
            // The assembly — including the load-bearing nest-before-record
            // order — is shared, because it is the ONE custody act that needs
            // nothing native and so is web's act path too
            // (`fauna_client_capabilities::custody_acts`, lifted 2026-08-17).
            // A second copy here is exactly where the ordering would drift.
            error = fauna_client_capabilities::custody_acts::revoke_custody(
                Arc::clone(&ctx.nest),
                &records,
                ctx.secret,
                &grant_id,
                holder,
            )
            .await;
        }
    }
    let facet = load_custody_facet(ctx.nest, ctx.store).await;
    (facet, error)
}

/// The budget/stop write: read the custody's registry row, apply the edit, put
/// it back through the R14 door (whole-record LWW — the pump's next budget pass
/// meters against what lands). Returns the error string, if any.
/// Route a budget/stop rewrite to the custody's RUNTIME row, whichever door
/// that is (the device-or-nest bullet, item 6): a device-form custody
/// rewrites its `custodies-held` fleet row; a NEST-form custody re-registers
/// its hosting row on the host's own nest — the same verb the deposit used,
/// rebuilt from the ceremony record with only the edited knobs changed.
async fn put_runtime_row_update(
    ctx: &CustodyCtx,
    records: &impl CeremonyRecords,
    grant_id: &[u8],
    edit: impl FnOnce(&mut u64, &mut bool) + Send,
) -> Option<String> {
    use fauna_client_capabilities::custody_ceremony as ceremony;
    // The ceremony record decides the row's home.
    let custody = match records.snapshot().await {
        Ok(custody) => custody,
        Err(e) => return Some(format!("{e:#}")),
    };
    let Some(record) = custody.held.iter().find(|h| h.grant_id == grant_id) else {
        return Some("no ceremony record for this custody".to_string());
    };
    let accept = match ceremony::decode_accept_record(record) {
        Ok(a) => a,
        Err(_) => return Some("this custody has no accept recorded yet".to_string()),
    };
    // The knobs are RECORD-then-row (`HeldCustody::host_knobs`): the edit
    // starts from the record's knobs — or, before the host ever set one, the
    // runtime row's current state, so a budget edit cannot silently un-stop
    // a stopped custody — and lands on the record BEFORE the row. A fresher
    // owner deliver re-derives the row from the record, so a knob held only
    // on the row would be reset by it.
    if accept.custodian_nest_url.is_none() {
        let mut row = match load_held_row(ctx, grant_id).await {
            Ok(row) => row,
            Err(e) => return Some(e),
        };
        let (cap, stopped) = match apply_knobs(
            records,
            grant_id,
            (row.retained_bytes_cap, row.stopped),
            edit,
        )
        .await
        {
            Ok(k) => k,
            Err(e) => return Some(e),
        };
        row.retained_bytes_cap = cap;
        row.stopped = stopped;
        return put_held_row(ctx, row).await;
    }
    // NEST form: rebuild the deposit from the record (arm 6's recipe) with
    // the edited knobs.
    let deliver = match ceremony::decode_deliver_record(record) {
        Ok(d) => d,
        Err(_) => {
            return Some(
                "no witness delivered yet — the hosting row lands with the ceremony's next pass"
                    .to_string(),
            );
        }
    };
    let Some(owner_url) = deliver.owner_nest_url.clone() else {
        return Some("the deliver names no owner nest URL".to_string());
    };
    let hosting = fauna_client_capabilities::custody_hosting::CustodyHostingClient::new(
        Arc::clone(&ctx.nest),
    );
    let current = if record.host_knobs.is_some() {
        // The record's knobs win; `current` is only the pre-knob fallback.
        (accept.retained_bytes_cap, false)
    } else {
        match hosting.list().await {
            Ok(reply) => reply
                .rows
                .iter()
                .find(|r| r.grant_id.as_slice() == grant_id)
                .map(|r| (r.retained_bytes_cap, r.stopped))
                .unwrap_or((accept.retained_bytes_cap, false)),
            Err(e) => return Some(format!("{e:#}")),
        }
    };
    let (cap, stopped) = match apply_knobs(records, grant_id, current, edit).await {
        Ok(k) => k,
        Err(e) => return Some(e),
    };
    let witness = match fauna_core::encoding::canonical_encode(&deliver.witness) {
        Ok(w) => w.to_vec(),
        Err(e) => return Some(format!("{e}")),
    };
    let owner_devices = match fauna_core::encoding::canonical_encode(&deliver.owner_devices) {
        Ok(d) => d.to_vec(),
        Err(e) => return Some(format!("{e}")),
    };
    let deposit = fauna_client_capabilities::custody_hosting::HostingDeposit {
        grant_id: grant_id.to_vec(),
        owner: record.owner,
        witness,
        owner_nest_url: owner_url,
        owner_devices,
        retained_bytes_cap: cap,
        stopped,
    };
    match hosting.register(&deposit).await {
        Ok(reply) if reply.ok => None,
        Ok(_) => Some("the nest refused the hosting rewrite".to_string()),
        Err(e) => Some(format!("{e:#}")),
    }
}

/// The reclaim: tear the custody down where it actually
/// lives, then mark the ceremony record removed so the card stops rendering it.
///
/// Two homes, the same split the rest of this module keeps: a NEST-form custody
/// lives as a hosting row on the host's nest and comes down through
/// `fauna.custody.hosting.remove`; a DEVICE-form one lives as this account's own
/// `custodies-held` registry row and comes down by dropping the row's hold.
///
/// **Order is load-bearing**: the teardown runs FIRST and the mark only on its
/// success. Marking first would let a failed teardown hide a live custody from
/// the very surface that could retry it — a row still pulling, still holding
/// bytes, and no longer rendered anywhere.
async fn remove_held_custody(
    ctx: &CustodyCtx,
    records: &impl CeremonyRecords,
    grant_id: &[u8],
    now: fauna_core::data::Timestamp,
) -> Option<String> {
    use fauna_client_capabilities::custody_ceremony as ceremony;
    let custody = match records.snapshot().await {
        Ok(custody) => custody,
        Err(e) => return Some(format!("{e:#}")),
    };
    let Some(record) = custody.held.iter().find(|h| h.grant_id == grant_id) else {
        return Some("no ceremony record for this custody".to_string());
    };
    let accept = match ceremony::decode_accept_record(record) {
        Ok(a) => a,
        Err(_) => return Some("this custody has no accept recorded yet".to_string()),
    };

    if accept.custodian_nest_url.is_some() {
        let hosting = fauna_client_capabilities::custody_hosting::CustodyHostingClient::new(
            Arc::clone(&ctx.nest),
        );
        // `removed: false` is not a failure: the row was already gone (an admin
        // dropped it, or an earlier attempt landed and its mark did not). The
        // mark below is what makes the retry idempotent.
        if let Err(e) = hosting.remove(grant_id).await {
            return Some(format!("{e:#}"));
        }
    } else if let Some(error) = put_held_row_update(ctx, grant_id, |row| {
        // A device-form hold has no nest row to drop; zeroing its budget and
        // stopping it is what frees the space on the next metering pass.
        row.retained_bytes_cap = 0;
        row.stopped = true;
    })
    .await
    {
        return Some(error);
    }

    if let Err(e) = records
        .update(|cfg| {
            ceremony::mark_custody_removed(cfg, grant_id, now);
        })
        .await
    {
        return Some(e.to_string());
    }
    None
}

/// The knob write: [`ceremony::apply_host_knob_edit`] inside the records'
/// `update`, so the `(cap, stopped)` the runtime row carries comes only from
/// the call that stamped the ceremony record (record-then-row is structural).
///
/// [`ceremony::apply_host_knob_edit`]: fauna_client_capabilities::custody_ceremony::apply_host_knob_edit
async fn apply_knobs(
    records: &impl CeremonyRecords,
    grant_id: &[u8],
    current: (u64, bool),
    edit: impl FnOnce(&mut u64, &mut bool) + Send,
) -> Result<(u64, bool), String> {
    use fauna_client_capabilities::custody_ceremony as ceremony;
    let now = Timestamp::now();
    let grant_id = grant_id.to_vec();
    records
        .update(move |cfg| ceremony::apply_host_knob_edit(cfg, &grant_id, current, edit, now))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no ceremony record for this custody".to_string())
}

async fn put_held_row_update(
    ctx: &CustodyCtx,
    grant_id: &[u8],
    edit: impl FnOnce(&mut fauna_core::custodies_held::CustodyHeld),
) -> Option<String> {
    let mut row = match load_held_row(ctx, grant_id).await {
        Ok(row) => row,
        Err(e) => return Some(e),
    };
    edit(&mut row);
    put_held_row(ctx, row).await
}

/// This account's `custodies-held` row for `grant_id`, or the error sentence.
async fn load_held_row(
    ctx: &CustodyCtx,
    grant_id: &[u8],
) -> Result<fauna_core::custodies_held::CustodyHeld, String> {
    let Some(store) = ctx.store.as_ref() else {
        return Err("account store not ready — try again shortly".to_string());
    };
    let entries = store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIES_HELD)
        .await
        .map_err(|e| format!("{e:#}"))?;
    entries
        .iter()
        .filter_map(|e| fauna_core::encoding::canonical_decode(&e.value).ok())
        .find(|r: &fauna_core::custodies_held::CustodyHeld| r.grant_id == grant_id)
        .ok_or_else(|| {
            "no registry row for this custody yet — it lands with the ceremony's next pass"
                .to_string()
        })
}

async fn put_held_row(
    ctx: &CustodyCtx,
    row: fauna_core::custodies_held::CustodyHeld,
) -> Option<String> {
    let Some(store) = ctx.store.as_ref() else {
        return Some("account store not ready — try again shortly".to_string());
    };
    match store.put_custodies_held(row).await {
        Ok(_) => None,
        Err(e) => Some(format!("{e:#}")),
    }
}

/// The shared stage (c) owner-side leg
/// ([`fauna_client_capabilities::custody_ceremony::ingest_nest_receipts`], the
/// one web's `custodyFacetLoad` runs too), its skips logged.
async fn ingest_nest_receipts(nest: &Arc<NestClient>, records: &impl CeremonyRecords) {
    for skipped in
        fauna_client_capabilities::custody_ceremony::ingest_nest_receipts(Arc::clone(nest), records)
            .await
    {
        tracing::debug!("{skipped}");
    }
}

/// Run one `drive_ceremonies` pass in the background — the record-then-act
/// loop's "act" half.
///
/// Cheap when settled (a snapshot read and nothing owed), so callers fire it
/// freely on its three edges: session start (crash recovery), account-store
/// ready, and every ceremony-moved notification. A missing session or store
/// skips silently — the ceremony state lives in the account store, so there is
/// nothing to drive before it is up, and the store-ready edge fires the next
/// pass.
///
/// ⚠ **Must be called inside a tokio runtime context** — it `tokio::spawn`s,
/// which panics outside one. tui and linux call it on their own runtimes. A
/// UniFFI face must reach it from an `async_runtime = "tokio"` export, never a
/// synchronous one (`fauna-ffi`'s `custody_drive` documents the incident).
pub fn spawn_drive(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    session: Option<Arc<ConversationsSession>>,
    store: Option<AccountStoreHandle>,
) {
    let owner = ActorKeypair::from_secret(secret);
    let Some(session) = session else { return };
    let Some(store) = store else { return };
    // The pass itself — receipts first (stage (c)'s owner-side leg, so this
    // same pass writes any re-opened `custodian-endpoints` row), then the
    // drive with both deposit doors — is the shared body web's rail runs too.
    tokio::spawn(async move {
        fauna_client_conversations::drive_ceremony_pass(nest, &store, &owner, session.backend())
            .await;
    });
}

/// The owner sends again — spawn one
/// [`fauna_client_capabilities::custody_ceremony::resend_owner_ceremonies`]
/// pass over this session's channels: the shared body behind the
/// `fauna_e2e_agent::CUSTODY_OWNER_RESEND_NOW` poke, which owns the contract.
/// Fire-and-forget like [`spawn_drive`]; no session or store yet is a quiet
/// no-op. Compiled out of release artifacts (convention 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn spawn_owner_resend(
    secret: [u8; 32],
    session: Option<Arc<ConversationsSession>>,
    store: Option<AccountStoreHandle>,
) {
    let owner = ActorKeypair::from_secret(secret);
    let Some(session) = session else { return };
    let Some(store) = store else { return };
    tokio::spawn(async move {
        let poster = fauna_client_conversations::BackendCustodyPoster(session.backend());
        match fauna_client_capabilities::custody_ceremony::resend_owner_ceremonies(
            &store, &store, &owner, &poster, &store,
        )
        .await
        {
            Ok(r) => tracing::info!(
                offers = r.offers,
                delivers = r.delivers,
                failed = r.failed,
                "custody owner re-send"
            ),
            Err(e) => tracing::warn!("custody owner re-send aborted: {e}"),
        }
    });
}

/// Register the account-custody ceremony sink on `session` — the receive half
/// every app needs for the custody facet to be anything but empty: each
/// received offer / accept / deliver / A7 receipt on a conversation channel is
/// verified and durably captured into `fauna.state.custody-ceremony` in one
/// read-join through the account store, and every
/// ingest that actually moved a ceremony schedules one [`spawn_drive`] pass (the
/// "act" half). Without it the payloads wait in channel history, tallied
/// `no_sink`, and no offer ever reaches a consent card.
///
/// `store` is read at each drive, not captured once: the account store lands
/// (and is torn down) independently of the session. `on_moved` is the app's
/// repaint nudge — it runs in the poll's async context, so it must only
/// notify, never work inline; pass a no-op when the page re-reads on its own
/// load edge.
///
/// tui registers the same sink with its own UI-message observer; this is the
/// shape linux and every UniFFI app share, so none re-assembles it.
pub fn register_ceremony_sink(
    session: &Arc<ConversationsSession>,
    nest: Arc<NestClient>,
    secret: [u8; 32],
    store: fn() -> Option<AccountStoreHandle>,
    on_moved: Arc<dyn Fn() + Send + Sync>,
) {
    let keypair = ActorKeypair::from_secret(secret);
    let own_actor = keypair.actor_id();
    let observer = DriveOnCeremonyMove {
        nest: Arc::clone(&nest),
        secret,
        // Weak: the session owns the sink that owns this observer.
        session: Arc::downgrade(session),
        store,
        on_moved,
    };
    session.set_custody_ceremony_sink(Arc::new(
        fauna_client_conversations::StoreCustodyCeremonySink::new(
            fauna_client_config::ResolvingLedgerStore::new(store),
            own_actor,
        )
        .with_observer(Arc::new(observer)),
    ));
}

/// The ceremony-moved edge → one drive pass plus the app's repaint nudge.
struct DriveOnCeremonyMove {
    nest: Arc<NestClient>,
    secret: [u8; 32],
    session: std::sync::Weak<ConversationsSession>,
    store: fn() -> Option<AccountStoreHandle>,
    on_moved: Arc<dyn Fn() + Send + Sync>,
}

impl fauna_client_conversations::CustodyCeremonyObserver for DriveOnCeremonyMove {
    fn ceremony_moved(&self, _grant_id: &[u8]) {
        // A dropped session means sign-out is under way — the next session
        // start re-drives from durable state.
        let Some(session) = self.session.upgrade() else {
            return;
        };
        spawn_drive(
            Arc::clone(&self.nest),
            self.secret,
            Some(session),
            (self.store)(),
        );
        (self.on_moved)();
    }
}

#[cfg(test)]
mod mint_tests {
    use super::*;

    /// The boundary row carries the candidate's three facts intact — the host
    /// as its 32 raw bytes (a leg hands them straight back as the mint's
    /// `host`), and the channel + label verbatim.
    #[test]
    fn a_candidate_projects_onto_the_boundary_row() {
        let got = project_mint_candidate(&fauna_client_conversations::CustodyMintCandidate {
            host: fauna_core::identity::ActorId([7u8; 32]),
            channel_hex: "c0ffee".to_string(),
            label: "Bo".to_string(),
        });
        assert_eq!(
            got,
            CustodyMintCandidateView {
                host: vec![7u8; 32],
                channel_hex: "c0ffee".to_string(),
                label: "Bo".to_string(),
            }
        );
    }

    /// A mint with no session must ANSWER, not record. The offer is POSTED by
    /// the drive pass, which returns immediately without a session, so
    /// recording one would leave an offer that is never sent and never
    /// reported — a silent drop (e2e convention 11).
    ///
    /// The nest here is unreachable on purpose: the guard must refuse *before*
    /// any I/O, so the refusal is the mint's own verdict and not a connection
    /// error wearing its clothes. That is exactly what a regression would
    /// look like — delete the guard and this returns the config CAS's error
    /// instead.
    #[tokio::test]
    async fn a_session_less_mint_is_refused_rather_than_recorded() {
        let secret = [9u8; 32];
        // Port 1 refuses instantly; nothing here should reach it anyway.
        let nest = NestClient::new(
            "ws://127.0.0.1:1".to_string(),
            ActorKeypair::from_secret(secret),
        );
        let (facet, error) = run_custody_act(
            CustodyCtx {
                nest,
                secret,
                store: None,
                session: None,
            },
            CustodyAct::Mint {
                host: fauna_core::identity::ActorId([2u8; 32]),
                channel_hex: "aa".to_string(),
            },
        )
        .await;
        let error = error.expect("a session-less mint must report, never drop silently");
        assert!(
            error.contains("conversation"),
            "the refusal must name the missing channel, got: {error}"
        );
        // The refold could not read an unreachable nest — a transient, so the
        // leg keeps its previous rows.
        assert!(facet.is_none());
    }
}
