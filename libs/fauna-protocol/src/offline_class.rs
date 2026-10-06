//! Per-kind offline classification — the machine-readable half of the
//! offline-mutation contract.
//!
//! Authority for the criteria:
//! `docs/goal/architecture/account-data-plane.md` § The offline-mutation
//! contract (W4 (account-data-plane.md § Workstreams)). This module is the *inventory* those criteria produce; it
//! does not restate them. Read the charter for why a class exists, this file
//! for which class a kind is in.
//!
//! # Why this is Rust and not a doc table
//!
//! The W4 outbox has to ask, at runtime, "may I apply this mutation locally
//! and sync it as data, must I queue a replayable intent, or must I refuse
//! while offline?" — so the classification is a lookup a client performs, not
//! prose a human consults. Keeping it in the protocol crate also means a new
//! kind cannot be added without answering the question: the bijection test
//! below fails on any registered kind with no class, and on any class entry
//! naming a kind nobody registers.
//!
//! # Relationship to `RpcKindMeta::forbid_replay`
//!
//! `forbid_replay` (see [`crate::kind`]) answers a *nest-side* question:
//! is a blind auto-retry of this request across a reconnect safe? The audit
//! trail that established each value is in `kind.rs` at the declaration
//! sites, and is **cited, never duplicated, here**.
//!
//! The two fields are different questions, but they are not independent, and
//! one combination is a contradiction that [`forbid_replay_is_consistent`]
//! rejects mechanically:
//!
//! * `forbid_replay = true` means a second delivery of the same request is
//!   *not* a no-op. A kind classified [`OfflineClass::OfflineSafe`] claims the
//!   opposite (idempotent or commutative, applied locally and synced as data),
//!   and a kind classified [`OfflineClass::Read`] claims it does not mutate at
//!   all. Both claims are refuted by `forbid_replay = true`.
//! * So `forbid_replay = true` implies the class is
//!   [`OfflineClass::OfflineQueued`] or [`OfflineClass::OnlineOnly`] — the two
//!   classes that do not assume replay safety. `OfflineQueued` is the one that
//!   *buys* it, with a client-generated intent id plus the nest's durable
//!   idempotency table (charter § Nest-side requirements); it is a statement
//!   about the target state, so it is legitimate for a `forbid_replay = true`
//!   kind, and the intent id is exactly what makes it so.
//!
//! The converse is deliberately not asserted: `forbid_replay = false` is
//! compatible with every class. Plenty of audited-idempotent kinds are still
//! `OnlineOnly` for reasons that have nothing to do with replay (an auth
//! ceremony is idempotent and still needs a live nest).
//!
//! # The `Read` value is not a fourth mutation class
//!
//! The charter's partition — offline-safe / offline-queued / online-only — is
//! exhaustive over *mutations*. The wire surface is not all mutations: most of
//! it is queries. [`OfflineClass::Read`] marks those, so the three ratified
//! classes keep meaning exactly what the charter says they mean. Whether a
//! given read is *answerable* offline is a projection question (W3), not a
//! mutation-contract question, and is deliberately not encoded here.
//!
//! Classifying by verb name is the trap this file must not fall into:
//! `fauna.bridges.check_submission_quota` reads like a query and is a
//! consuming debit (`kind.rs`, `register_bridge_kinds`). Every `Read` here is
//! a claim about the handler, and `forbid_replay_is_consistent` is the
//! mechanical backstop on it.
//!
//! # Verification status — what the classifications rest on
//!
//! Stated plainly so the next session knows what it is inheriting, rather
//! than discovering the basis by reading handlers itself.
//!
//! * **Mechanically enforced:** completeness and bijection with the registry,
//!   and the `forbid_replay` consistency rule above. These are tests; they
//!   cannot rot.
//! * **Derived from the registry's own audit trail:** the classes for the
//!   families whose `kind.rs` declaration sites carry an explicit read-vs-
//!   mutation and idempotency rationale — posts, profile, notifications,
//!   contacts/knocks, feed, personalization, share, sync, content_index, and
//!   the bridge/mail clusters. Those doc comments are the evidence; this file
//!   cites them per family rather than restating them.
//! * **Derived by construction from the charter:** the admin plane, the auth
//!   ceremonies, payments, and the bridge service-user plane — the charter
//!   assigns these outright, so no per-kind latitude exists.
//! * **Handler-verified per kind (W4 phase 2, 2026-08-13):** the
//!   `OfflineSafe` / `OfflineQueued` split on the *user-app* mutation
//!   surface — all 100 kinds then in the two classes were traced to their
//!   handlers (nest `RpcRouter` registrations; the Go bridge where a kind
//!   lands there), grading each on identity assignment (client vs nest),
//!   replay effect, and per-request side effects. 16 entries moved (11
//!   queued→safe, 2 safe→queued, 1 safe→online-only, 2 safe→read); each
//!   carries its evidence at the declaration site, as do the contested
//!   keeps. Three criteria rulings made during the pass, applied uniformly
//!   and binding on future classifications:
//!   1. A **convergent, value-derived external publish** (Nostr replaceable
//!      events re-published from a settings value) does not disqualify
//!      `OfflineSafe`; a per-request, non-convergent external effect (a
//!      fresh-id ActivityPub `Follow`, a second PDS record) does.
//!   2. **Nest-side refusals never drive class** — quota, authz, or
//!      precondition rejections are the "corrected optimistic apply" the
//!      classes already price in; only identity and effect shape decide.
//!   3. **Nest-arbitrated namespaces queue** — first-binder-wins claims and
//!      nest-wide catalogs (labeler publish, folder channel claim) are
//!      resolve-on-ack even when replay is inert, because there is nothing
//!      to apply locally and sync as data.

#[cfg(test)]
use crate::kind::KindRegistry;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// What a client may do with a kind while it has no nest connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OfflineClass {
    /// Not a mutation. Offline *answerability* is a projection question (W3),
    /// not a mutation-contract one — see the module docs.
    Read,
    /// Content-addressed or client-assigned id, commutative or CAS-retry
    /// mergeable effect: apply locally and sync as data.
    OfflineSafe,
    /// Nest-assigned effects or side effects: apply optimistically where safe,
    /// queue a replayable intent with a client-generated idempotency key,
    /// resolve on ack.
    OfflineQueued,
    /// Genuinely connection-requiring. The UI desensitizes these offline.
    OnlineOnly,
}

/// The class of `kind`, or `None` if `kind` is not a registered kind.
pub fn offline_class(kind: &str) -> Option<OfflineClass> {
    table().get(kind).copied()
}

/// The class of `kind` **and its canonical `&'static str` name** — the
/// classification table's own key. For a kind string held as *data* (an
/// outbox row's `kind` column), this is how the drain re-enters the
/// `&'static str`-typed request seam ([`crate::RpcRequester`]) without
/// leaking: the one classification lookup it performs anyway also recovers
/// the static name.
pub fn offline_class_keyed(kind: &str) -> Option<(&'static str, OfflineClass)> {
    table().get_key_value(kind).map(|(k, v)| (*k, *v))
}

fn table() -> &'static BTreeMap<&'static str, OfflineClass> {
    static TABLE: OnceLock<BTreeMap<&'static str, OfflineClass>> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

fn build_table() -> BTreeMap<&'static str, OfflineClass> {
    use OfflineClass::*;
    let mut m = BTreeMap::new();

    // ── register_bridges_mail_kinds ─────────────────────────────────
    // The mail / MDA / mail-admin operational cluster: the Go bridge's own call
    // surface plus the admin-app mail-settings pages. Both halves are off the
    // offline path by construction — a bridge is a nest-side service process
    // with no account store, no outbox and no replica (its outbound queue lives
    // in the *nest*: `enqueue_outbound_mail` / `fetch_outbound_due` /
    // `mark_outbound_*`), and the admin half is admin/provisioning, which the
    // charter names online-only outright. So: queries are Read, mutations are
    // OnlineOnly, uniformly.
    //
    // One sub-surface is user-facing rather than admin — the account-alias kinds
    // (`*_account_alias`, `generate_disposable_alias`). They stay OnlineOnly
    // here: an alias is a nest-arbitrated unique name on a domain, so minting
    // one offline cannot be honoured, and the rest of the alias surface has no
    // user-side replica to apply against yet. Refining these once W4 builds the
    // mail replica is deliberate future work, recorded in the module's
    // verification note.
    m.insert("fauna.bridges.abort_primary_domain_rename", OnlineOnly);
    m.insert("fauna.bridges.add_list_member", OnlineOnly);
    m.insert("fauna.bridges.add_local_domain", OnlineOnly);
    m.insert("fauna.bridges.approve_pending_bridge", OnlineOnly);
    m.insert("fauna.bridges.atproto.delete_presence", OnlineOnly);
    m.insert("fauna.bridges.atproto.fetch_identities", Read);
    m.insert("fauna.bridges.atproto.fetch_identity_key_blob", Read);
    m.insert("fauna.bridges.atproto.fetch_issuer_jwks", Read);
    m.insert("fauna.bridges.atproto.fetch_preferences", Read);
    m.insert("fauna.bridges.atproto.fetch_profile", Read);
    m.insert("fauna.bridges.atproto.fetch_public_posts", Read);
    m.insert("fauna.bridges.atproto.fetch_session_secret_blob", Read);
    m.insert("fauna.bridges.atproto.record_blob", OnlineOnly);
    m.insert("fauna.bridges.atproto.record_minted_identity", OnlineOnly);
    m.insert("fauna.bridges.atproto.record_tombstone", OnlineOnly);
    m.insert("fauna.bridges.atproto.request_tombstone", OnlineOnly);
    m.insert("fauna.bridges.atproto.store_preferences", OnlineOnly);
    m.insert("fauna.bridges.batch_import_list_members", OnlineOnly);
    m.insert("fauna.bridges.blocklist_self_check_run", OnlineOnly);
    m.insert("fauna.bridges.check_greylist", Read);
    m.insert("fauna.bridges.complete_primary_domain_rename", OnlineOnly);
    m.insert("fauna.bridges.create_account_alias", OnlineOnly);
    m.insert("fauna.bridges.create_account_list", OnlineOnly);
    m.insert("fauna.bridges.create_forwarder", OnlineOnly);
    m.insert("fauna.bridges.create_mailbox", OnlineOnly);
    m.insert("fauna.bridges.decode_srs_bounce", Read);
    m.insert("fauna.bridges.delete_account_alias", OnlineOnly);
    m.insert("fauna.bridges.delete_account_list", OnlineOnly);
    m.insert("fauna.bridges.delete_forwarder", OnlineOnly);
    m.insert("fauna.bridges.delete_mailbox", OnlineOnly);
    m.insert("fauna.bridges.deliver_sealed_scheduling", OnlineOnly);
    m.insert("fauna.bridges.enable_account_alias", OnlineOnly);
    m.insert("fauna.bridges.enqueue_outbound_mail", OnlineOnly);
    m.insert(
        "fauna.bridges.extend_primary_domain_rename_grace",
        OnlineOnly,
    );
    m.insert("fauna.bridges.fetch_mta_sts_policy", Read);
    m.insert("fauna.bridges.fetch_outbound_due", Read);
    m.insert("fauna.bridges.fetch_recipient_filters", Read);
    m.insert("fauna.bridges.fetch_recipient_forward_config", Read);
    m.insert("fauna.bridges.fetch_recipient_index_key", Read);
    m.insert("fauna.bridges.fetch_spam_model", Read);
    m.insert("fauna.bridges.fetch_tlsa", Read);
    m.insert("fauna.bridges.resolve_mx", Read);
    m.insert("fauna.bridges.force_rotate_dkim", OnlineOnly);
    m.insert("fauna.bridges.forward_message", OnlineOnly);
    m.insert("fauna.bridges.generate_disposable_alias", OnlineOnly);
    m.insert("fauna.bridges.get_alias_policy", Read);
    m.insert("fauna.bridges.get_caldav_port", Read);
    m.insert("fauna.bridges.get_forward_all_to", Read);
    m.insert("fauna.bridges.get_forward_per_hour", Read);
    m.insert("fauna.bridges.get_mail_config", Read);
    m.insert("fauna.bridges.get_mail_serving_enabled", Read);
    m.insert("fauna.bridges.get_primary_domain_rename_status", Read);
    m.insert("fauna.bridges.get_quota", Read);
    m.insert("fauna.bridges.get_spam_baseline_state", Read);
    m.insert("fauna.bridges.get_spam_scoring_policy", Read);
    m.insert("fauna.bridges.get_spam_threshold_override", Read);
    m.insert("fauna.bridges.import_account_aliases", OnlineOnly);
    m.insert("fauna.bridges.list_account_alias_hits", Read);
    m.insert("fauna.bridges.list_account_aliases", Read);
    m.insert("fauna.bridges.list_account_lists", Read);
    m.insert("fauna.bridges.list_blocklist_self_check_history", Read);
    m.insert("fauna.bridges.list_deliverability_diagnostic_runs", Read);
    m.insert("fauna.bridges.list_forwarders", Read);
    m.insert("fauna.bridges.list_list_members", Read);
    m.insert("fauna.bridges.list_list_send_history", Read);
    m.insert("fauna.bridges.list_local_domains", Read);
    m.insert("fauna.bridges.list_pending_bridges", Read);
    m.insert("fauna.bridges.list_primary_domain_renames", Read);
    m.insert("fauna.bridges.list_spam_training_history", Read);
    m.insert("fauna.bridges.mail_health", Read);
    m.insert("fauna.bridges.mark_outbound_bounced", OnlineOnly);
    m.insert("fauna.bridges.mark_outbound_delivered", OnlineOnly);
    m.insert("fauna.bridges.mark_outbound_failed", OnlineOnly);
    m.insert("fauna.bridges.mint_bulk_byte_token", OnlineOnly);
    m.insert("fauna.bridges.outbound_warmup_reset", OnlineOnly);
    m.insert("fauna.bridges.outbound_warmup_status", Read);
    m.insert("fauna.bridges.provision_recipient_mls_pubkey", OnlineOnly);
    m.insert("fauna.bridges.provision_self_signed_cert", OnlineOnly);
    m.insert("fauna.bridges.publish_spam_baseline", OnlineOnly);
    m.insert("fauna.bridges.put_alias_policy", OnlineOnly);
    m.insert("fauna.bridges.put_auth_policy", OnlineOnly);
    m.insert("fauna.bridges.put_imap_policy", OnlineOnly);
    m.insert("fauna.bridges.put_outbound_policy", OnlineOnly);
    m.insert("fauna.bridges.put_spam_model", OnlineOnly);
    m.insert("fauna.bridges.put_spam_policy", OnlineOnly);
    m.insert("fauna.bridges.put_submission_policy", OnlineOnly);
    m.insert("fauna.bridges.reject_pending_bridge", OnlineOnly);
    m.insert("fauna.bridges.remove_local_domain", OnlineOnly);
    m.insert("fauna.bridges.rename_mailbox", OnlineOnly);
    m.insert("fauna.bridges.report_log_events", OnlineOnly);
    m.insert("fauna.bridges.report_rejected_scan", OnlineOnly);
    m.insert("fauna.bridges.report_tls_attempt", OnlineOnly);
    m.insert("fauna.bridges.request_enrollment", OnlineOnly);
    m.insert("fauna.bridges.reset_spam_model", OnlineOnly);
    m.insert("fauna.bridges.resolve_recipient", Read);
    m.insert("fauna.bridges.restore_local_domain", OnlineOnly);
    m.insert("fauna.bridges.restore_real_tls_cert", OnlineOnly);
    m.insert("fauna.bridges.resubscribe_list_member", OnlineOnly);
    m.insert("fauna.bridges.revoke_account_alias", OnlineOnly);
    m.insert("fauna.bridges.revoke_service_user", OnlineOnly);
    m.insert("fauna.bridges.rotate_list_unsubscribe_secret", OnlineOnly);
    m.insert("fauna.bridges.rotate_srs_secret", OnlineOnly);
    m.insert("fauna.bridges.run_deliverability_diagnostics", OnlineOnly);
    m.insert("fauna.bridges.search_messages", Read);
    m.insert("fauna.bridges.send_auto_reply", OnlineOnly);
    m.insert("fauna.bridges.send_list_message", OnlineOnly);
    m.insert(
        "fauna.bridges.set_auto_enable_mail_for_new_users",
        OnlineOnly,
    );
    m.insert("fauna.bridges.set_baseline_contribution", OnlineOnly);
    m.insert("fauna.bridges.set_caldav_enabled", OnlineOnly);
    m.insert("fauna.bridges.set_caldav_port", OnlineOnly);
    m.insert("fauna.bridges.set_carddav_enabled", OnlineOnly);
    m.insert("fauna.bridges.set_catch_all_actor", OnlineOnly);
    m.insert("fauna.bridges.set_dkim_rotation_days", OnlineOnly);
    m.insert("fauna.bridges.set_forward_all_to", OnlineOnly);
    m.insert("fauna.bridges.set_forward_per_hour", OnlineOnly);
    m.insert("fauna.bridges.set_mail_enabled", OnlineOnly);
    m.insert("fauna.bridges.set_mail_serving_enabled", OnlineOnly);
    m.insert("fauna.bridges.set_role_address", OnlineOnly);
    m.insert("fauna.bridges.set_spam_threshold_override", OnlineOnly);
    m.insert("fauna.bridges.set_webdav_enabled", OnlineOnly);
    m.insert("fauna.bridges.start_primary_domain_rename", OnlineOnly);
    m.insert("fauna.bridges.subscribe_mailbox", OnlineOnly);
    m.insert("fauna.bridges.subscribe_mailbox_state", Read);
    m.insert("fauna.bridges.unsubscribe_list_member", OnlineOnly);
    m.insert("fauna.bridges.unsubscribe_mailbox", OnlineOnly);
    m.insert("fauna.bridges.update_account_alias", OnlineOnly);
    m.insert("fauna.bridges.update_account_list", OnlineOnly);
    m.insert("fauna.bridges.update_local_domain_config", OnlineOnly);
    m.insert("fauna.bridges.webdav_admit_principal", OnlineOnly);
    m.insert("fauna.bridges.webdav_list_folders", Read);
    m.insert("fauna.bridges.webdav_list_files", Read);
    m.insert("fauna.bridges.webdav_quota", Read);
    m.insert("fauna.bridges.webdav_record_change", OnlineOnly);
    m.insert("fauna.bridges.whoami", Read);

    // ── register_subscriptions_kinds ────────────────────────────────
    // Paid-subscription control plane. The money-moving verbs (`subscribe`,
    // `unsubscribe`) are online-only by the charter's `payments` clause; tier
    // authoring is the creator's own owner-keyed data; approving/rejecting a
    // request has a side effect on the subscriber, so it queues.
    m.insert("fauna.subscriptions.delegate.upload", OnlineOnly);
    m.insert("fauna.subscriptions.key_blob.get", Read);
    // key_blob.rotate: OnlineOnly, and not because it moves money. The upload
    // is verified against LIVE nest state on both axes — `roster_mismatch`
    // refuses a blob whose entry set is not today's roster, `stale_rotation`
    // refuses a `rotated_at` that does not advance past the stored blob — so a
    // queued intent is a blob minted against a roster that has since moved,
    // which is exactly what those two refusals exist to reject. Queuing would
    // buy nothing either: the one caller (the post-succession rotation leg)
    // re-derives what is owed and re-mints from scratch on every pass, so an
    // attempt that could not reach the nest is simply re-made next time.
    m.insert("fauna.subscriptions.key_blob.rotate", OnlineOnly);
    m.insert("fauna.subscriptions.mine.list", Read);
    m.insert("fauna.subscriptions.offers.list", Read);
    m.insert("fauna.subscriptions.post_unlock.get", Read);
    m.insert("fauna.subscriptions.requests.approve", OfflineQueued);
    m.insert("fauna.subscriptions.requests.list", Read);
    // reject: handler-verified 2026-08-13 (W4 phase 2) — the entire handler is
    // an ownership-checked idempotent row delete (subscription_handlers.rs
    // `handle_reject_subscribe_request`); no key rotation, no fan-out, no
    // notification. The "side effect on the subscriber" that keeps `approve`
    // queued (key_version assignment + unlock fan-out) has no analogue here.
    m.insert("fauna.subscriptions.requests.reject", OfflineSafe);
    m.insert("fauna.subscriptions.status.get", Read);
    m.insert("fauna.subscriptions.subscribe", OnlineOnly);
    m.insert("fauna.subscriptions.subscribers.list", Read);
    m.insert("fauna.subscriptions.subscribers.remove", OfflineQueued);
    m.insert("fauna.subscriptions.tiers.clear_field", OfflineSafe);
    // tiers.create: handler-verified 2026-08-13 (W4 phase 2) — identity is the
    // client-assigned (author, name); a replay is refused by the UNIQUE check
    // (`tier_already_exists`, state unchanged); the birth KeyBlob is
    // client-minted. The `unlocks_post` arm's subscriber fan-out is derived
    // from the created value (the site-re-render class of effect), not a
    // per-request side effect. Joins its update/delete/clear_field siblings.
    m.insert("fauna.subscriptions.tiers.create", OfflineSafe);
    m.insert("fauna.subscriptions.tiers.delete", OfflineSafe);
    m.insert("fauna.subscriptions.tiers.list", Read);
    m.insert("fauna.subscriptions.tiers.update", OfflineSafe);
    m.insert("fauna.subscriptions.unsubscribe", OnlineOnly);

    // ── register_filesync_kinds ─────────────────────────────────────
    // Snapshot plane. Creating a snapshot takes a nest-assigned generation, so
    // it queues; the destructive verbs (`delete_immediate`, `prune`,
    // `restore_message_kind`) are irreversible bulk state changes and stay
    // online. Policy and label edits are owner-keyed idempotent overwrites.
    m.insert("fauna.filesync.snapshot.check", Read);
    m.insert("fauna.filesync.snapshot.create_folder", OfflineQueued);
    m.insert("fauna.filesync.snapshot.create_message_kind", OfflineQueued);
    // delete: handler-verified 2026-08-13 (W4 phase 2) — it does not delete:
    // it INSERTS a pending_actions row (autoincrement id, hash-chained, no
    // dedup on (type, target)) and replies with the nest-assigned
    // pending_action_id + a nest-clock execute_after; a replay files a second
    // pending action. That is the queued-intent shape, not an overwrite.
    m.insert("fauna.filesync.snapshot.delete", OfflineQueued);
    m.insert("fauna.filesync.snapshot.delete_immediate", OnlineOnly);
    m.insert("fauna.filesync.snapshot.diff", Read);
    m.insert("fauna.filesync.snapshot.get", Read);
    m.insert("fauna.filesync.snapshot.list", Read);
    m.insert("fauna.filesync.snapshot.stamp_labels", OfflineSafe);
    m.insert("fauna.filesync.snapshot.list_restore_divergence", Read);
    m.insert("fauna.filesync.snapshot.list_restore_history", Read);
    m.insert("fauna.filesync.snapshot.prune", OnlineOnly);
    // prune_set_policy: handler-verified 2026-08-13 (W4 phase 2) — not a
    // policy edit: it reads the resting policy and EXECUTES retention
    // (`evaluate_folder_retention_at(…, now_epoch_secs())` + a soft-delete
    // loop), so the destroyed set is computed nest-side from live state and
    // wall-clock — nothing a client can apply locally, and a delayed replay
    // prunes a different (larger) set. Joins its twin `prune`, which runs the
    // same loop and was OnlineOnly from the start.
    m.insert("fauna.filesync.snapshot.prune_set_policy", OnlineOnly);
    m.insert("fauna.filesync.snapshot.restore_message_kind", OnlineOnly);
    m.insert("fauna.filesync.snapshot.undelete", OfflineSafe);

    // ── register_payments_kinds ─────────────────────────────────────
    // `payments` cargo feature. The charter names payments online-only outright;
    // `claims.mint` additionally carries `forbid_replay` (minting is not
    // idempotent).
    #[cfg(feature = "payments")]
    {
        m.insert("fauna.payments.claims.list", Read);
        m.insert("fauna.payments.claims.mint", OnlineOnly);
        m.insert("fauna.payments.claims.redeem", OnlineOnly);
        m.insert("fauna.payments.providers.list", Read);
        m.insert("fauna.payments.providers.remove", OnlineOnly);
        m.insert("fauna.payments.providers.set", OnlineOnly);
    }

    // ── register_tips_kinds ─────────────────────────────────────────
    // `payments` cargo feature. A bounded aggregate point read.
    #[cfg(feature = "payments")]
    {
        m.insert("fauna.tips.list", Read);
    }

    // ── register_features_kinds ─────────────────────────────────────
    // The feature plane's transparency read. NOT `payments`-gated: the plane
    // gates three registry members, so its read answers for whichever a build
    // ships.
    m.insert("fauna.features.status", Read);
    // The policy writes. `OnlineOnly` rather than a queued mutation: a policy
    // document is authored *against* what the other tiers currently say, and the
    // meet it lands in is recomputed per evaluation — so a write replayed from an
    // offline queue hours later would apply a decision made about a policy state
    // that no longer exists. Restrictions are also exactly the class of change
    // that must not land silently late.
    m.insert("fauna.features.policy.update", OnlineOnly);
    m.insert("fauna.features.self_limits.update", OnlineOnly);
    // The authored-document reads the editors seed from — plain reads.
    m.insert("fauna.features.policy.get", Read);
    m.insert("fauna.features.self_limits.get", Read);

    // ── register_dns_kinds ──────────────────────────────────────────
    // Admin DNS provisioning; `verify_records` drives a live external probe.
    m.insert("fauna.dns.list_records", Read);
    m.insert("fauna.dns.set_host_address", OnlineOnly);
    m.insert("fauna.dns.verify_records", OnlineOnly);
    // The propagation gate's live readiness probe — a cached answer is worse
    // than useless here (the whole point is "has it published *yet*").
    m.insert("fauna.dns.probe_txt_visible", OnlineOnly);

    // ── register_domain_kinds ───────────────────────────────────────
    // The domain-expiry watch's read. `Read`, not `OnlineOnly`: it serves a
    // *stored* record the nest's own slow watch wrote, so a cached answer is
    // exactly as good as a live one — and a registration's state moves on the
    // scale of days, which is the cadence the watch itself runs at.
    m.insert("fauna.domain.expiry.get", Read);

    // ── register_pair_kinds ─────────────────────────────────────────
    // Device pairing — an auth ceremony, plus its roster read.
    m.insert("fauna.pair.add", OnlineOnly);
    m.insert("fauna.pair.list", Read);
    m.insert("fauna.pair.revoke", OnlineOnly);
    m.insert("fauna.pair.forward_retry", OnlineOnly);
    m.insert("fauna.pair.forward_discard", OnlineOnly);

    // ── register_drafts_kinds ───────────────────────────────────────
    // The `__drafts` reserved rail. A draft is the user's own content, written
    // at a client-held path — offline editing is the whole point of the rail.
    m.insert("fauna.drafts.get", Read);
    m.insert("fauna.drafts.put", OfflineSafe);

    // ── register_mls_kinds ──────────────────────────────────────────
    // The device's MLS state blob. `put` is online-only despite being an
    // idempotent overwrite: it carries epoch state, and the device-owned-epoch
    // invariant (devices.md § Offline compose) makes a fork the hazard, not a
    // lost write. Application-message sends queue; epoch state does not.
    m.insert("fauna.mls.get", Read);
    m.insert("fauna.mls.put", OnlineOnly);

    // ── register_oauth_kinds ────────────────────────────────────────
    // The nest-held OAuth issuer key set. Rotation mints deployment crypto
    // material, so it is never applied from a queue: an admin rotating in
    // response to a compromise needs the nest's own answer, not a local
    // optimism that a reconnect might replay twice.
    m.insert("fauna.oauth.issuer_key_status", Read);
    m.insert("fauna.oauth.rotate_issuer_key", OnlineOnly);
    // The forced arm even more so: it is the compromise response, and an admin
    // needs to know the leaked key is GONE, not queued to go.
    m.insert("fauna.oauth.force_rotate_issuer_key", OnlineOnly);
    // Its sibling for the second signer, for the same reason: every
    // outstanding refresh token must be DEAD when the reply lands.
    m.insert("fauna.oauth.force_rotate_session_secret", OnlineOnly);
    // The consent starts' user half. Typing a code claims a live, minutes-long
    // request on the nest — there is nothing to apply locally and nothing a
    // queue could usefully replay after the request expired. The block is a
    // small user setting whose effect only exists nest-side, so it too is
    // answered by the nest rather than optimistically.
    m.insert("fauna.oauth.consent.lookup_code", OnlineOnly);
    // Opening a handoff spends a pushed request that lives 90 seconds — the
    // same nothing-to-replay shape as typing a code.
    m.insert("fauna.oauth.consent.open_handoff", OnlineOnly);
    m.insert("fauna.oauth.consent.block_client", OnlineOnly);
    m.insert("fauna.oauth.consent.list_blocked_clients", Read);

    // ── register_principals_kinds ───────────────────────────────────
    // The third-party principal roster. Revoke is a security act — the user
    // needs the nest's answer that the connection is GONE, never a queued
    // optimism.
    m.insert("fauna.principals.list", Read);
    m.insert("fauna.principals.revoke", OnlineOnly);
    // The events doors' poll (`transport.md` § Push events → *Third-party
    // event doors*): a pure read, issued by a principal, never by an app.
    m.insert("fauna.events.poll", Read);

    // ── register_plugins_kinds ──────────────────────────────────────
    // The admin's hosted-plugin surface. Install and uninstall are acts the
    // admin needs the nest's answer to — a card to approve, a plugin gone.
    m.insert("fauna.plugins.install", OnlineOnly);
    m.insert("fauna.plugins.list", Read);
    m.insert("fauna.plugins.uninstall", OnlineOnly);

    // ── register_tls_kinds ──────────────────────────────────────────
    // Certificate provisioning.
    m.insert("fauna.tls.cert_status", Read);
    m.insert("fauna.tls.publish_cert", OnlineOnly);

    // ── register_transport_kinds ────────────────────────────────────
    // Admin transport policy.
    m.insert("fauna.transport.get_policy", Read);
    m.insert("fauna.transport.put_policy", OnlineOnly);

    // ── register_bridge_kinds ───────────────────────────────────────
    // The bridge service-user plane proper — wrapped-blob custody, the IMAP
    // metadata/body surface, CalDAV/CardDAV collections, mailbox import, and the
    // atproto app-credential surface. Same construction as the mail cluster
    // above: the caller is a nest-side service process with no replica, so
    // nothing here can be applied locally and synced later.
    //
    // Two entries are the reason this file distrusts verb names:
    // `check_submission_quota` reads like a query and is a consuming debit, and
    // `fauna.conversations.keypackage.fetch` (below) consumes a one-time key
    // package. Both carry `forbid_replay`, so `forbid_replay_is_consistent`
    // would reject a `Read` on either.
    m.insert("fauna.bridges.fetch_wrapped_mls_blob", Read);
    m.insert("fauna.bridges.fetch_mls_snapshot_blob", Read);
    m.insert("fauna.bridges.fetch_webdav_keys_blob", Read);
    m.insert("fauna.bridges.fetch_wrapped_submission_token", Read);
    m.insert("fauna.bridges.fetch_tls_cert_blob", Read);
    m.insert("fauna.bridges.fetch_bridge_pubkey", Read);
    m.insert("fauna.bridges.provision_wrapped_mls_blob", OnlineOnly);
    m.insert("fauna.bridges.provision_mls_snapshot_blob", OnlineOnly);
    m.insert("fauna.bridges.provision_webdav_keys_blob", OnlineOnly);
    m.insert(
        "fauna.bridges.provision_wrapped_submission_token",
        OnlineOnly,
    );
    m.insert("fauna.bridges.provision_tls_cert_blob", OnlineOnly);
    m.insert("fauna.bridges.list_dkim_selectors", Read);
    m.insert("fauna.bridges.revoke_dkim_blob", OnlineOnly);
    m.insert("fauna.bridges.list_service_users", Read);
    m.insert("fauna.bridges.revoke_wrapped_mls_blob", OnlineOnly);
    m.insert("fauna.bridges.revoke_wrapped_submission_token", OnlineOnly);
    m.insert("fauna.bridges.register_service_user", OnlineOnly);
    m.insert("fauna.bridges.report_auth_event", OnlineOnly);
    m.insert("fauna.bridges.validate_recipient", Read);
    m.insert("fauna.bridges.fetch_recipient_mls_pubkey", Read);
    m.insert("fauna.bridges.fetch_config", Read);
    m.insert("fauna.bridges.report_session_close", OnlineOnly);
    m.insert("fauna.bridges.check_submission_quota", OnlineOnly);
    m.insert("fauna.bridges.ingest_inbound_mail", OnlineOnly);
    m.insert("fauna.bridges.submit_inbound_mail", OnlineOnly);
    m.insert("fauna.bridges.list_mailboxes", Read);
    m.insert("fauna.bridges.select_mailbox", Read);
    m.insert("fauna.bridges.list_messages", Read);
    m.insert("fauna.bridges.fetch_message_metadata", Read);
    m.insert("fauna.bridges.fetch_message_ciphertext", Read);
    m.insert("fauna.bridges.fetch_index_segments_since", Read);
    m.insert("fauna.bridges.store_flags", OnlineOnly);
    m.insert("fauna.bridges.expunge", OnlineOnly);
    m.insert("fauna.bridges.copy", OnlineOnly);
    m.insert("fauna.bridges.move", OnlineOnly);
    m.insert("fauna.bridges.append", OnlineOnly);
    m.insert("fauna.bridges.provision_calendar", OnlineOnly);
    m.insert("fauna.bridges.list_calendars", Read);
    m.insert("fauna.bridges.query_events", Read);
    m.insert("fauna.bridges.put_event_ciphertext", OnlineOnly);
    m.insert("fauna.bridges.delete_event", OnlineOnly);
    m.insert("fauna.bridges.sync_calendar_since", Read);
    m.insert("fauna.bridges.place_inbound_invite", OnlineOnly);
    m.insert("fauna.bridges.provision_addressbook", OnlineOnly);
    m.insert("fauna.bridges.list_addressbooks", Read);
    m.insert("fauna.bridges.query_cards", Read);
    m.insert("fauna.bridges.put_card_ciphertext", OnlineOnly);
    m.insert("fauna.bridges.delete_card", OnlineOnly);
    m.insert("fauna.bridges.sync_addressbook_since", Read);
    m.insert("fauna.bridges.delete_addressbook", OnlineOnly);
    m.insert("fauna.bridges.start_import_session", OnlineOnly);
    m.insert("fauna.bridges.import_message", OnlineOnly);
    m.insert("fauna.bridges.import_message_batch", OnlineOnly);
    m.insert("fauna.bridges.list_import_sessions", Read);
    m.insert("fauna.bridges.pause_import_session", OnlineOnly);
    m.insert("fauna.bridges.resume_import_session", OnlineOnly);
    m.insert("fauna.bridges.cancel_import_session", OnlineOnly);
    m.insert("fauna.bridges.finalize_import_session", OnlineOnly);
    m.insert("fauna.bridges.fail_import_session", OnlineOnly);
    m.insert("fauna.bridges.list_own_mailboxes", Read);
    m.insert("fauna.bridges.start_export_session", OnlineOnly);
    m.insert("fauna.bridges.list_export_sessions", Read);
    m.insert("fauna.bridges.fetch_export_chunk_ciphertext", Read);
    m.insert("fauna.bridges.upload_export_chunk", OnlineOnly);
    m.insert("fauna.bridges.pause_export_session", OnlineOnly);
    m.insert("fauna.bridges.resume_export_session", OnlineOnly);
    m.insert("fauna.bridges.restart_export_session", OnlineOnly);
    m.insert("fauna.bridges.cancel_export_session", OnlineOnly);
    m.insert("fauna.bridges.finalize_export_session", OnlineOnly);
    m.insert("fauna.bridges.fail_export_session", OnlineOnly);
    m.insert("fauna.bridges.discard_export_blob", OnlineOnly);
    m.insert("fauna.bridges.atproto.fetch_app_credential_verifiers", Read);
    m.insert("fauna.bridges.atproto.record_session", OnlineOnly);
    m.insert("fauna.bridges.atproto.refresh_session", OnlineOnly);
    m.insert("fauna.bridges.atproto.end_session", OnlineOnly);
    m.insert("fauna.bridges.atproto.provision_app_credential", OnlineOnly);
    m.insert("fauna.bridges.atproto.list_app_credentials", Read);
    m.insert("fauna.bridges.atproto.revoke_app_credential", OnlineOnly);
    m.insert("fauna.bridges.atproto.list_sessions", Read);
    m.insert("fauna.bridges.atproto.list_grants", Read);
    m.insert("fauna.bridges.atproto.revoke_session", OnlineOnly);
    m.insert(
        "fauna.bridges.atproto.set_external_apps_enabled",
        OnlineOnly,
    );
    m.insert("fauna.bridges.atproto.list_pending_consents", Read);
    m.insert("fauna.bridges.atproto.resolve_consent", OnlineOnly);
    m.insert("fauna.bridges.atproto.deliver_permission_set", OnlineOnly);
    m.insert("fauna.bridges.atproto.get_integration_status", Read);
    m.insert("fauna.bridges.atproto.set_integration_level", OnlineOnly);
    m.insert("fauna.bridges.atproto.fetch_authoring_key", Read);
    m.insert("fauna.bridges.atproto.fetch_authoring_delegation", Read);
    m.insert(
        "fauna.bridges.atproto.provision_authoring_delegation",
        OnlineOnly,
    );
    m.insert(
        "fauna.bridges.atproto.revoke_authoring_delegation",
        OnlineOnly,
    );
    m.insert("fauna.bridges.atproto.ingest_external_write", OnlineOnly);

    // ── register_backup_kinds ───────────────────────────────────────
    // Backup custody + destination provisioning — admin/provisioning throughout.
    m.insert("fauna.backup.nest_key.grant", OnlineOnly);
    m.insert("fauna.backup.nest_key.revoke", OnlineOnly);
    m.insert("fauna.backup.status", Read);
    m.insert("fauna.backup.custody.list", Read);
    m.insert("fauna.backup.custodian.checkin", OnlineOnly);
    m.insert("fauna.backup.destination.list", Read);
    m.insert("fauna.backup.destination.register", OnlineOnly);
    m.insert("fauna.backup.destination.remove", OnlineOnly);
    m.insert("fauna.backup.destination.attach_folder", OnlineOnly);
    m.insert("fauna.backup.destination.detach_folder", OnlineOnly);
    m.insert("fauna.backup.generation.list", Read);
    m.insert("fauna.backup.generation.restore", OnlineOnly);
    m.insert("fauna.backup.custody.materialize", OnlineOnly);
    m.insert("fauna.backup.custody.recover", OnlineOnly);

    // ── register_generation_escrow_kinds ────────────────────────────
    // The generation escrow doors (account-data-plane.md § The generation
    // machinery → *The escrow doors*). All nest-held state with no local
    // mirror: `put`'s deliverable is the holder-**signed** receipt (only the
    // nest's deployment key can produce it — a client cannot apply this
    // locally and sync as data), and `delete` removes rows the client never
    // holds a copy of. Both queue per criteria ruling 3 (nest-arbitrated,
    // nothing to apply locally). `get` is a plain read of those rows.
    m.insert("fauna.generation.escrow.put", OfflineQueued);
    m.insert("fauna.generation.escrow.get", Read);
    m.insert("fauna.generation.escrow.delete", OfflineQueued);
    m.insert("fauna.backup.writer_grant.list", Read);
    m.insert("fauna.backup.writer_grant.register", OnlineOnly);
    m.insert("fauna.backup.writer_grant.revoke", OnlineOnly);

    // ── register_recovery_kinds ─────────────────────────────────────
    // Account-recovery ceremonies — challenge/response against a live nest, so
    // every verb is online-only; only the status/lookup reads are answerable
    // from a replica.
    m.insert("fauna.recovery.registration.submit", OnlineOnly);
    m.insert("fauna.recovery.registration.chain", Read);
    m.insert("fauna.recovery.escrow.put", OnlineOnly);
    m.insert("fauna.recovery.escrow.challenge", OnlineOnly);
    m.insert("fauna.recovery.escrow.fetch", OnlineOnly);
    m.insert("fauna.recovery.escrow.status", Read);
    m.insert("fauna.recovery.replacement.request", OnlineOnly);
    m.insert("fauna.recovery.replacement.challenge", OnlineOnly);
    m.insert("fauna.recovery.replacement.veto", OnlineOnly);
    m.insert("fauna.recovery.replacement.status", Read);
    m.insert("fauna.recovery.succession.submit", OnlineOnly);
    m.insert("fauna.recovery.succession.lookup", Read);
    m.insert(crate::recovery::SUCCESSION_STATUS_KIND, Read);
    m.insert(crate::recovery::SUCCESSION_OWED_SETTLE_KIND, OnlineOnly);

    // ── register_capability_kinds ───────────────────────────────────
    // Capability grants — the user-minted, time-bounded, revocable primitive
    // (encryption-at-rest.md § Capability tiering). Minting, renewing and
    // revoking authority are auth ceremonies; the worklist/submit pair is grant-
    // gated worker traffic that only means anything against a live nest.
    m.insert("fauna.capabilities.mint", OnlineOnly);
    m.insert("fauna.capabilities.fetch", Read);
    m.insert("fauna.capabilities.renew", OnlineOnly);
    m.insert("fauna.capabilities.revoke", OnlineOnly);
    m.insert("fauna.capabilities.reconcile", Read);
    m.insert("fauna.capabilities.rescore_worklist", Read);
    m.insert("fauna.capabilities.submit_scores", OnlineOnly);
    m.insert("fauna.capabilities.spam_baseline_worklist", Read);
    m.insert("fauna.capabilities.submit_spam_baseline", OnlineOnly);

    // The custodian-nest hosting pair (`account-data-plane.md` § Replica
    // posture → The custody grant + ceremony, item 6 stage (b)). `register`
    // is an idempotent LWW upsert, but it is the deposit half of a
    // record-then-deposit ceremony leg with its own redrive marker — the
    // ceremony driver re-fires it until acked, so queueing it would double
    // the redrive machinery for no offline win.
    m.insert("fauna.custody.hosting.register", OnlineOnly);
    m.insert("fauna.custody.hosting.list", Read);
    // The reclaim: a destructive act whose confirm copy promises the
    // bytes are freed — queueing it offline would show "removed" while the
    // store still holds them, so the affordance stays online-only and honest.
    m.insert("fauna.custody.hosting.remove", OnlineOnly);
    // Stage (c)'s deposit is spoken only by the custodian NEST's pump (never
    // an app), so the offline question does not arise — and the pump has its
    // own redrive (a failed deposit leaves the receipt "due" next pass).
    m.insert("fauna.custody.receipt.deposit", OnlineOnly);
    m.insert("fauna.custody.receipt.list", Read);
    // The admin hosting door. `list` is a pure read. `remove` tears
    // down a hosting row and, with the pair's last row, the custodied store —
    // an admin acting on what THIS nest holds right now, against a list they
    // just read; queueing it offline would let a stale decision fire against
    // state that has since changed, and the destructive half deserves the
    // online round trip. Same reasoning as the other admin teardown doors.
    m.insert("fauna.admin.custody_hosting.list", Read);
    m.insert("fauna.admin.custody_hosting.remove", OnlineOnly);

    // ── register_labeler_kinds ──────────────────────────────────────
    // Labeler publication + subscription. A subscription set is the user's own
    // idempotent membership list; publishing a labeler is externally visible, so
    // it queues.
    // publish: handler-verified 2026-08-13 (W4 phase 2), KEPT queued. Replay is
    // inert (monotonic version gate → StaleLabelerVersion), but the target is
    // the NEST-WIDE labeler catalog with cross-subscriber score
    // materialization — not account data; there is nothing to "apply locally
    // and sync as data", so OfflineSafe is conceptually inapplicable.
    m.insert("fauna.labelers.publish", OfflineQueued);
    m.insert("fauna.labelers.inspect", Read);
    m.insert("fauna.labelers.list", Read);
    m.insert("fauna.labelers.subscribe", OfflineSafe);
    m.insert("fauna.labelers.unsubscribe", OfflineSafe);

    // ── register_bridges_ui_kinds ───────────────────────────────────
    // The user-facing feed-side bridge control plane (bridges.md). `link` is an
    // OAuth/credential ceremony against an external network and `unlink` revokes
    // one — both online-only. Follows are externally visible, so they queue;
    // per-bridge settings are the user's own idempotent data.
    m.insert("fauna.bridges.list", Read);
    // set_settings: handler-verified 2026-08-13 (W4 phase 2), KEPT safe. The
    // AP/Bluesky arms are idempotent overwrites; the Nostr arm republishes
    // NIP-65/NIP-17 (kinds 10002/10050) to external relays per call carrying
    // `relay_list` — but those are Nostr REPLACEABLE events (relays keep only
    // the newest), so the external state converges on replay and the publish
    // is derived from the settings value. Ruling (phase 2): a convergent,
    // value-derived external publish does not disqualify OfflineSafe; a
    // per-request non-convergent external effect (add_follow's fresh
    // activity-id Follow) does.
    m.insert("fauna.bridges.set_settings", OfflineSafe);
    m.insert("fauna.bridges.list_follows", Read);
    m.insert("fauna.bridges.link", OnlineOnly);
    // A nonce is minted for the signer to sign right now; queuing it offline
    // would hand the app a challenge that expired before it could be signed.
    m.insert("fauna.bridges.link_challenge", OnlineOnly);
    m.insert("fauna.bridges.unlink", OnlineOnly);
    m.insert("fauna.bridges.add_follow", OfflineQueued);
    m.insert("fauna.bridges.remove_follow", OfflineQueued);
    m.insert("fauna.bridges.list_follow_requests", Read);
    // Like remove_follow: the answer is non-optimistic (the card re-reads the
    // list before painting) and sends one activity to the requester's server.
    m.insert("fauna.bridges.resolve_follow_request", OfflineQueued);
    m.insert("fauna.bridges.feeds.list", Read);
    m.insert("fauna.bridges.feeds.create", OfflineQueued);
    // feeds.delete: handler-verified 2026-08-13 (W4 phase 2) — a pure
    // owner-scoped DB delete of a row the client already holds from
    // feeds.list; no provider call, no external send, no grant consumption
    // (cleanup ops are deliberately ungated); replay → feed_not_found, same
    // state. `create` stays queued for its nest-assigned id + single-use
    // guardian-grant burn; none of that exists here.
    m.insert("fauna.bridges.feeds.delete", OfflineSafe);

    // ── register_email_kinds ────────────────────────────────────────
    // The user's own mail surface. `send` is **offline-queued, not online-
    // only**, and the distinction is worth stating: the charter's online-only
    // example is "mail submission to foreign MX" — that is the *nest → foreign
    // MX* leg, which is nest-side, already at-least-once, and not a client kind
    // at all. What a client issues here is a submission to *its own nest*, which
    // owns the durable outbound queue (`enqueue_outbound_mail` /
    // `fetch_outbound_due`). That is the textbook queued intent, and its
    // `forbid_replay` is exactly why it needs the contract's client-generated
    // intent id.
    m.insert("fauna.email.filters.list", Read);
    m.insert("fauna.email.filters.create", OfflineQueued);
    m.insert("fauna.email.filters.get", Read);
    m.insert("fauna.email.filters.update", OfflineSafe);
    m.insert("fauna.email.filters.delete", OfflineSafe);
    m.insert("fauna.email.send", OfflineQueued);
    m.insert("fauna.email.inbox.fetch", Read);
    m.insert("fauna.email.apply_spam_disposition", OfflineSafe);
    // mark_seen: an idempotent own-side mark (adds `\Seen`, a repeat is a
    // no-op); flag_changes: a pure read of the INBOX flag delta.
    m.insert("fauna.email.inbox.mark_seen", OfflineSafe);
    m.insert("fauna.email.inbox.flag_changes", Read);
    m.insert("fauna.email.sent.fetch", Read);

    // ── register_inbox_kinds ────────────────────────────────────────
    // Cross-actor inbox. A send is an intent; an ack is an idempotent own-side
    // mark.
    m.insert("fauna.inbox.fetch", Read);
    m.insert("fauna.inbox.ack", OfflineSafe);
    m.insert("fauna.inbox.send", OfflineQueued);

    // ── register_bluesky_kinds ──────────────────────────────────────
    // Consume-side thread fetch — a pure read.
    m.insert("bluesky.feed.thread", Read);

    // ── register_nostr_bunker_kinds ─────────────────────────────────
    // NIP-46 bunker credentials — minting and revoking a signer credential.
    m.insert("fauna.nostr.bunker.create_invite", OnlineOnly);
    m.insert("fauna.nostr.bunker.list", Read);
    m.insert("fauna.nostr.bunker.revoke", OnlineOnly);
    m.insert("fauna.nostr.bunker.set_label", OfflineSafe);
    // The oracle's door — a third-party principal binding its client key;
    // the answer (the connect string) is the point of the call.
    m.insert("fauna.nostr.bunker.bind", OnlineOnly);

    // ── register_bridged_conversation_kinds ─────────────────────────
    // The bridged-conversation family (Phase G). The bridge's six are a
    // principal's, all online; the user's reads are reads, `rooms.open`
    // matches the bridge's grammar nest-side, and `send` has no
    // pending-action executor, so both need a nest.
    m.insert("fauna.bridges.conversation.deposit", OnlineOnly);
    m.insert("fauna.bridges.conversation.outbox.fetch", OnlineOnly);
    m.insert("fauna.bridges.conversation.outbox.ack", OnlineOnly);
    m.insert("fauna.bridges.conversation.room.upsert", OnlineOnly);
    m.insert("fauna.bridges.conversation.room.members", OnlineOnly);
    m.insert("fauna.bridges.conversation.receipt", OnlineOnly);
    m.insert("fauna.bridges.conversation.rooms.list", Read);
    m.insert("fauna.bridges.conversation.rooms.open", OnlineOnly);
    m.insert("fauna.bridges.conversation.inbox.fetch", Read);
    m.insert("fauna.bridges.conversation.send", OnlineOnly);

    // ── register_nostr_zap_signer_kinds ─────────────────────────────
    // `zaps` cargo feature. Authorized zap signers — an authorization roster.
    // Gated in lockstep with the registry side: this table's keys are kind
    // STRINGS, so an ungated entry would keep `fauna.nostr.zap_signers.*` in an
    // excised artifact, and `every_registered_kind_is_classified` would
    // additionally see a classified kind the registry no longer declares.
    #[cfg(feature = "zaps")]
    {
        m.insert("fauna.nostr.zap_signers.list", Read);
        m.insert("fauna.nostr.zap_signers.add", OnlineOnly);
        m.insert("fauna.nostr.zap_signers.remove", OnlineOnly);
    }

    // ── register_nostr_content_kinds ────────────────────────────────
    // Nostr content. `publish_signed` relay-enqueues a one-shot event via the
    // user's nest. Only `zaps.total` is a `zaps` registry surface.
    #[cfg(feature = "zaps")]
    m.insert("nostr.zaps.total", Read);
    m.insert("nostr.badges.list", Read);
    m.insert("nostr.events.publish_signed", OfflineQueued);

    // ── register_conversations_channel_kinds ────────────────────────
    // MLS channels. The boundary is ratified in devices.md § Offline compose:
    // application-message sends are offline-queued intents (queued as
    // *plaintext*, encrypted at send — never pre-built ciphertext for a future
    // epoch).
    m.insert("fauna.conversations.channel.send", OfflineQueued);
    m.insert("fauna.conversations.channel.send_remote", OfflineQueued);
    // A token mint is a read-shaped, online-only step of an attachment send:
    // the queued intent is the send itself (above); the token is minted when
    // the send actually runs.
    m.insert("fauna.conversations.blob.write_token.get", Read);
    m.insert("fauna.conversations.channel.fetch", Read);
    m.insert("fauna.conversations.channel.list_for_actor", Read);
    m.insert("fauna.conversations.channel.actors", Read);
    m.insert("fauna.conversations.channel.actors_remote", Read);

    // ── register_conversations_keypackage_kinds ─────────────────────
    // MLS key packages. **`fetch` is not a read** — a key package is one-time
    // use, so fetching one consumes it; its `forbid_replay` says so, and the
    // consistency test enforces the consequence.
    m.insert("fauna.conversations.keypackage.upload", OnlineOnly);
    m.insert("fauna.conversations.keypackage.fetch", OnlineOnly);
    m.insert("fauna.conversations.keypackage.count", Read);

    // ── register_conversations_welcome_kinds ────────────────────────
    // MLS Welcome delivery — a membership op, online-only per devices.md.
    m.insert("fauna.conversations.welcome.deliver", OnlineOnly);

    // ── register_conversations_room_kinds ───────────────────────────
    // The room plane's floor roster. `roster_report` is a MEMBERSHIP op and
    // online-only, for a sharp reason: the report is a **mirror of a
    // membership authority the nest cannot read**
    // (`conversation-rooms.md` § The floor roster → *End-to-end rooms*).
    // Queueing one would let a device replay a roster the MLS group has
    // since moved past, and the nest has no way to notice — a stale mirror
    // re-asserted as current. Every report is produced by a commit the
    // device just authored while online with its own delivery service, so
    // the offline case does not arise honestly.
    m.insert("fauna.conversations.room.roster_report", OnlineOnly);
    // `create` is the community class's birth CEREMONY, and a ceremony is a
    // membership op: it seats the owner and the home nest on the floor. It
    // is also the one op whose result the caller cannot predict offline in
    // any useful way — a queued birth would hand the app a room id for a
    // room no nest has founded, and every later op on it would fail until
    // the queue drained.
    m.insert("fauna.conversations.room.create", OnlineOnly);
    m.insert("fauna.conversations.room.list_roster", Read);
    // The relayed twin is a **relay**, so serving it needs not just this nest
    // but a live nest↔nest leg to the room's home — `OnlineOnly` rather than
    // `Read`, the `generations_remote` grading verbatim. There is no local
    // answer to fall back on: this nest holds no floor roster for a room homed
    // elsewhere, and a cached one would name a membership that has moved on.
    m.insert("fauna.conversations.room.list_roster_remote", OnlineOnly);
    // The report's relayed twin: a MEMBERSHIP op (the `roster_report`
    // reasoning verbatim — a queued mirror is a stale mirror) AND a relay
    // (the `list_roster_remote` reasoning — it needs a live nest↔nest leg),
    // so `OnlineOnly` on both counts.
    m.insert("fauna.conversations.room.roster_report_remote", OnlineOnly);
    // The departure's relayed twin: a MEMBERSHIP op (the `room.leave`
    // reasoning verbatim) AND a relay (the `list_roster_remote` reasoning —
    // it needs a live nest↔nest leg to the room's home), so `OnlineOnly` on
    // both counts. There is emphatically no local fallback: this nest holds
    // no floor for a room homed elsewhere, so a "queued" leave would leave
    // the member seated on the only floor that counts.
    m.insert("fauna.conversations.room.leave_remote", OnlineOnly);
    // The acceptance's relayed twin: a MEMBERSHIP op (the `accept_invite`
    // reasoning below) AND a relay (it needs a live nest↔nest leg to the
    // room's home), so `OnlineOnly` on both counts — a queued accept would
    // seat the member on an offer the room may have withdrawn meanwhile.
    m.insert("fauna.conversations.room.accept_invite_remote", OnlineOnly);
    // The invitation's relayed twin, for an inviter homed on another nest: a
    // MEMBERSHIP op (the `room.invite` reasoning below) AND a relay (it needs
    // a live nest↔nest leg to the room's home, which alone judges the join
    // rule), so `OnlineOnly` on both counts.
    m.insert("fauna.conversations.room.invite_remote", OnlineOnly);
    // The membership ops: each changes who is in a room, and a queued one would apply a
    // membership decision the room may have moved past while the device was
    // away — an invitation to a room since renamed or left, an accept of an
    // invitation since withdrawn, a removal of someone since departed. The
    // nest refuses each of those on its own merits when it finally arrives,
    // so queueing buys a guaranteed-stale attempt and nothing else.
    m.insert("fauna.conversations.room.invite", OnlineOnly);
    m.insert("fauna.conversations.room.accept_invite", OnlineOnly);
    m.insert("fauna.conversations.room.remove", OnlineOnly);
    m.insert("fauna.conversations.room.leave", OnlineOnly);
    // The pending-invitation pair. The list carries a judgement made against
    // the floor as it stands ("would this still be accepted"), so a stale
    // copy is a wrong answer rather than an old one; the withdrawal is a
    // membership door like the four above.
    m.insert("fauna.conversations.room.list_invites", OnlineOnly);
    m.insert("fauna.conversations.room.revoke_invite", OnlineOnly);
    // A queued policy change would be applied against a version the
    // room has since moved past, which the strict ratchet refuses — a
    // guaranteed-stale attempt and nothing else.
    m.insert("fauna.conversations.room.set_policy", OnlineOnly);
    // The labeler set rides the same strict ratchet, for the same reason.
    m.insert("fauna.conversations.room.set_labelers", OnlineOnly);
    m.insert("fauna.conversations.room.transfer_ownership", OnlineOnly);
    // The sealing plane's two KEYING acts, online-only for a reason the
    // membership ops only approximate: both are judged against the room's
    // **current** state at the nest, and a queued one is not merely stale but
    // refused by construction. A mint must name the room's live tip as its
    // parent (a strict ratchet — this plane has no arbiter for a fork), so a
    // mint built offline is invalid the moment anyone else rotates. A
    // backfill's wraps are bound to the target's **current** roster entry,
    // and a re-admission mints a fresh one — so a queued backfill would
    // arrive naming a seat its target no longer holds. Both also wrap to a
    // roster read at build time, which offline is by definition old.
    m.insert("fauna.conversations.room.publish_generation", OnlineOnly);
    m.insert("fauna.conversations.room.backfill_generations", OnlineOnly);
    // A seat's own wrap target, supplied or rotated after seating. Online-
    // only for the rotation's sake: a set queued offline and landing after a
    // later rotation would roll the seat back to a key its owner retired,
    // and the reply names a tip that is only meaningful now.
    m.insert("fauna.conversations.room.set_reception_key", OnlineOnly);
    // The two reads. `generations` is an ordinary read of the caller's own
    // wraps. `generations_remote` is the same read for a room homed on
    // another nest, and is online-only rather than `Read`: it is a **relay**,
    // so serving it needs not just this nest but a live nest↔nest leg to the
    // room's home (`conversation-rooms.md` § The home nest). There is no
    // local answer to fall back on — a cached one would be the client's own
    // key material, which the client already holds by other means.
    m.insert("fauna.conversations.room.generations", Read);
    m.insert("fauna.conversations.room.generations_remote", OnlineOnly);
    // The room search door. `Read` like `generations`: it answers from a
    // derived view this nest already holds, so a queued call replays against
    // whatever the corpus says when it runs — which is the same answer a
    // fresh call would get, only later. Nothing about it is judged against
    // live nest state the way the keying acts are.
    m.insert("fauna.conversations.room.search", Read);

    // ── register_delegation_kinds ───────────────────────────────────
    // Delegation liveness — a heartbeat has no meaning to replay after the fact.
    m.insert("fauna.delegation.heartbeat", OnlineOnly);
    m.insert("fauna.delegation.observe", Read);

    // ── register_spam_kinds ─────────────────────────────────────────
    // The user's own spam preferences — an idempotent overwrite.
    m.insert("fauna.spam.get_preferences", Read);
    m.insert("fauna.spam.set_preferences", OfflineSafe);

    // ── register_moderation_kinds ───────────────────────────────────
    // Moderation. `train` is online-only on audited evidence, not on its name:
    // the shared-accumulator predicate caught it (it carries `forbid_replay`).
    // `legal_takedown` is irreversible. Share-policy toggles are own-data.
    m.insert("fauna.moderation.stats", Read);
    m.insert("fauna.moderation.actions", Read);
    m.insert("fauna.moderation.appeal", OfflineQueued);
    m.insert("fauna.moderation.train", OnlineOnly);
    m.insert("fauna.moderation.legal_takedown", OnlineOnly);
    m.insert("fauna.moderation.report_share.set", OfflineSafe);
    m.insert("fauna.moderation.report_share.status", Read);
    m.insert("fauna.moderation.signal_share.set", OfflineSafe);
    m.insert("fauna.moderation.signal_share.status", Read);
    // signal_contribute: handler-verified 2026-08-13 (W4 phase 2) — identity
    // is (contributor, 32-byte content hash, factor); `capture_signal` is a
    // per-factor last-wins verdict set (insert/delete keyed rows), so a
    // replayed identical verdict changes nothing, and the only side effect is
    // the same debounced `notify_exchange_transition` nudge its OfflineSafe
    // siblings `report_share.set`/`signal_share.set` already fire. The
    // `content_is_public` precondition is a nest-side refusal, and refusals
    // do not drive class (a corrected optimistic apply, per the module
    // header).
    m.insert("fauna.moderation.signal_contribute", OfflineSafe);
    // User-initiated reporting. `submit`/`withdraw` have effects on other
    // parties (an admin's doorbell, a forwarded copy on a peer nest), so they
    // queue — like `appeal`. `resolve` is an admin's final record that writes
    // the reporter's notification and may cross to a peer nest: online-only,
    // like `legal_takedown`.
    m.insert("fauna.moderation.abuse_report.submit", OfflineQueued);
    m.insert("fauna.moderation.abuse_report.mine", Read);
    m.insert("fauna.moderation.abuse_report.withdraw", OfflineQueued);
    m.insert("fauna.moderation.abuse_report.queue", Read);
    m.insert("fauna.moderation.abuse_report.resolve", OnlineOnly);

    // ── register_family_kinds ───────────────────────────────────────
    // Family/guardian plane. Custody transfer and graduation are irreversible
    // lifecycle ceremonies; approvals and reports have effects on the other
    // party, so they queue; policy and device marks are the guardian's own data.
    m.insert("fauna.family.status", Read);
    m.insert("fauna.family.policy.update", OfflineSafe);
    m.insert("fauna.family.graduate", OnlineOnly);
    m.insert("fauna.family.transfer", OnlineOnly);
    m.insert("fauna.family.transfer.accept", OnlineOnly);
    m.insert("fauna.family.transfer.decline", OnlineOnly);
    m.insert("fauna.family.transfer.cancel", OnlineOnly);
    m.insert("fauna.family.approvals.list", Read);
    m.insert("fauna.family.approvals.decide", OfflineQueued);
    m.insert("fauna.family.contact.add", OfflineQueued);
    m.insert("fauna.family.contact.request", OfflineQueued);
    m.insert("fauna.family.feed_source.request", OfflineQueued);
    m.insert("fauna.family.notify_report", OfflineQueued);
    m.insert("fauna.family.usage_report", OfflineQueued);
    m.insert("fauna.family.device.mark", OfflineSafe);

    // ── register_auth_kinds ─────────────────────────────────────────
    // Auth ceremonies — the charter's first online-only example.
    m.insert("fauna.auth.handshake", OnlineOnly);
    m.insert("fauna.auth.challenge", OnlineOnly);
    m.insert("fauna.auth.verify", OnlineOnly);
    m.insert("fauna.auth.nest_handshake", OnlineOnly);
    m.insert("fauna.auth.device_handshake", OnlineOnly);
    m.insert("fauna.auth.custody_handshake", OnlineOnly);
    // ⚠ The one member of this family that is NOT an auth ceremony, and the
    // block comment above must not be read onto it. `rotation_chain` mints
    // nothing and authenticates nobody — it is a pure read of the box's
    // append-only, public-by-construction rotation log, so it is classed like
    // its exact structural twin `fauna.recovery.registration.chain` (a public
    // chain read a verifier walks), not like the ceremonies it sits beside.
    m.insert("fauna.auth.rotation_chain", Read);

    // ── register_discovery_kinds ────────────────────────────────────
    // Pre-auth discovery — all pure reads of nest-published facts.
    m.insert("fauna.nest.info", Read);
    m.insert("fauna.handle.available", Read);
    m.insert("fauna.nest.resolve", Read);
    m.insert("fauna.actor.by_handle", Read);
    m.insert("fauna.setup.status", Read);

    // ── register_account_register_kind ──────────────────────────────
    // Account creation — provisioning.
    m.insert("fauna.account.register", OnlineOnly);

    // ── register_account_age_nonce_kind ─────────────────────────────
    // A short-TTL single-use mint; an offline-queued nonce would be expired
    // by the time it flushed.
    m.insert("fauna.account.age_nonce", OnlineOnly);

    // ── register_account_lockout_kind ───────────────────────────────
    // Lockout — a security ceremony.
    m.insert("fauna.account.lockout", OnlineOnly);

    // ── register_claim_admin_kind ───────────────────────────────────
    // Admin claim — provisioning, and the one-shot that must not half-apply.
    m.insert("fauna.auth.claim_admin", OnlineOnly);

    // ── register_account_kinds ──────────────────────────────────────
    // Account surface. A handle is a globally unique name the nest arbitrates,
    // so changing it cannot be settled offline; delete and upgrade are
    // irreversible/billing.
    m.insert("fauna.account.get", Read);
    m.insert("fauna.quota.get", Read);
    m.insert("fauna.account.am_i_admin", Read);
    m.insert("fauna.profile.handle.change", OnlineOnly);
    m.insert("fauna.account.delete", OnlineOnly);
    m.insert("fauna.account.upgrade", OnlineOnly);

    // ── register_pending_actions_kinds ──────────────────────────────
    // Pending-action review — a decision with an effect on the queued action.
    m.insert("fauna.pending_actions.list", Read);
    m.insert("fauna.pending_actions.get", Read);
    m.insert("fauna.pending_actions.cancel", OfflineQueued);
    m.insert("fauna.pending_actions.approve", OfflineQueued);

    // ── register_stats_kinds ────────────────────────────────────────
    // A pure aggregate read.
    m.insert("fauna.stats.get", Read);

    // ── register_linkpreview_kinds ──────────────────────────────────
    // Nest-side fetch of a remote URL. Non-mutating, so `Read` — whether it can
    // be *answered* offline is the projection question this file does not
    // encode.
    m.insert("fauna.linkpreview.resolve", Read);

    // ── register_files_versions_kinds ───────────────────────────────
    // Version history — pure reads, plus the retention pipeline's recovery
    // verb: an idempotent single-row restore keyed on the client-known
    // (path_hash, version_num), no nest-minted id — replaying it re-asserts
    // "keep this version", so it queues safely offline.
    m.insert("fauna.files.versions.list", Read);
    m.insert("fauna.files.versions.get", Read);
    m.insert("fauna.files.versions.undelete", OfflineSafe);

    // ── register_folders_kinds ────────────────────────────────────
    // Folders. `create` inserts with a fresh nest-side id (the same shape as
    // `sync.changes.record`), so it needs a client intent id — queued. Eviction
    // rotates keys and a lease is live coordination, so both stay online. The
    // member/access/schedule edits are owner-keyed idempotent overwrites.
    m.insert("fauna.folders.create", OfflineQueued);
    m.insert("fauna.folders.list", Read);
    m.insert("fauna.folders.update", OfflineSafe);
    m.insert("fauna.folders.delete", OfflineSafe);
    m.insert("fauna.folders.devices", Read);
    // deposit: a third-party principal's ingress (`file-sync.md` § Third-party
    // deposit ingress), not replay-safe and issued by no app — never queued,
    // the `fauna.bridges.conversation.deposit` grade.
    m.insert("fauna.folders.deposit", OnlineOnly);
    // The owner's half of the inbox — issued by the sync engine's adoption
    // pass, which runs only while connected: a read, and an idempotent
    // retire keyed by the item's id.
    m.insert("fauna.folders.deposits.list", Read);
    m.insert("fauna.folders.deposits.retire", OfflineSafe);
    // public.fetch: the phase-4 public folder read plane — a pure read of
    // another account's `public`-audience folder (same grade as
    // `sync.changes.list`, which it is the cross-account form of). Nothing is
    // written on either nest; the relay arm only forwards the same read.
    m.insert("fauna.folders.public.fetch", Read);
    m.insert("fauna.folders.members.list", Read);
    m.insert("fauna.folders.members.list_actors", Read);
    // The cross-nest relay twin (`federation.md` § Cross-nest…, *The cross-nest
    // writer roster read*) — a pure read like the door it relays to.
    m.insert("fauna.folders.members.list_actors_remote", Read);
    m.insert("fauna.folders.members.set_access", OfflineSafe);
    m.insert("fauna.folders.members.remove", OfflineSafe);
    m.insert("fauna.folders.members.evict", OnlineOnly);
    // places.set: the one add/edit door onto a device place (folders re-model
    // § Places) — keyed on the client-supplied (name/name_hash, device_id),
    // the write is INSERT OR REPLACE of one roster row, no nest-minted id, no
    // notification. It IS one of the "member/access/schedule edits are
    // owner-keyed idempotent overwrites" this block's own comment describes.
    m.insert("fauna.folders.places.set", OfflineSafe);
    m.insert("fauna.folders.leave", OfflineQueued);
    // share: handler-verified 2026-08-13 (W4 phase 2), KEPT queued. The
    // channel id is client-minted and re-binding by the same claimant is
    // idempotent — but `claim_folder_channel` is first-binder-wins
    // arbitration of a nest-held namespace (terminal already_claimed /
    // already_bound refusals), the profile.handle.change shape: the caller
    // needs the nest's arbitration answer to proceed, which is
    // resolve-on-ack, not apply-locally.
    m.insert("fauna.folders.share", OfflineQueued);
    m.insert("fauna.folders.content_key.put", OfflineSafe);
    m.insert("fauna.folders.content_key.get", Read);
    m.insert("fauna.folders.lease.acquire", OnlineOnly);
    m.insert("fauna.folders.lease.release", OnlineOnly);
    m.insert("fauna.sync.conflicts.list", Read);
    m.insert("fauna.sync.conflicts.report", OfflineQueued);
    m.insert("fauna.sync.conflicts.resolve", OfflineQueued);
    // set_web_paywall: handler-verified 2026-08-13 (W4 phase 2) — the column
    // write is last-wins, but the `tier: Some(_)` arm first spends a
    // `feature_gate` usage unit (`try_spend_feature_usage` — `op_delta`
    // returns 1 for Operations unconditionally), permanently incrementing the
    // actor's day/week/month counters per allowed call with no dedup key.
    // The shared-accumulator predicate that made `moderation.train`
    // online-only applies; here the spend is per-designate, so it queues
    // (the intent id is what a future dedup can key on).
    m.insert("fauna.folders.set_web_paywall", OfflineQueued);
    m.insert("fauna.folders.write_token.get", Read);
    m.insert("fauna.folders.read_token.get", Read);
    // served_rows.adopt (`writer-signed-change-records.md` ruling (7)(b)):
    // fills a row's signature columns only where they are NULL, under a
    // signature the nest verifies over the stored row — idempotent and
    // order-free, no nest-assigned effect.
    m.insert("fauna.folders.served_rows.adopt", OfflineSafe);

    // ── register_share_kinds ────────────────────────────────────────
    // Share links: `create` and `revoke` are ONLINE-ONLY by the feature's own
    // ruling (`share-links.md` § Flows → Create, reclassified 2026-09-28 with
    // the first app caller). The effect alone would pass as offline-safe (a
    // client-minted token, an `INSERT OR IGNORE` on its derived id; an
    // idempotent revoke UPDATE), but the flow forbids queueing: a link's URL
    // is revealed only after registration succeeds, so a queued create would
    // hold an unrevealed, unregistered token across restarts for no user
    // benefit, and a queued revoke would show a link dead that still serves.
    m.insert("fauna.share.create", OnlineOnly);
    m.insert("fauna.share.list", Read);
    m.insert("fauna.share.revoke", OnlineOnly);

    // ── register_sync_kinds ─────────────────────────────────────────
    // Device-sync control plane. `changes.supersede` is a head-bound
    // idempotent mark.
    // register: handler-verified 2026-08-13 (W4 phase 2) — the device_id is
    // client-supplied hex and the write is an explicit
    // `INSERT … ON CONFLICT (actor_id, device_id) DO UPDATE`; the reply
    // echoes the request's own id. Nothing nest-assigned: the offline-safe
    // shape.
    // changes.record: handler-verified 2026-08-13 (W4 phase 2) — this file's
    // previous note ("a non-idempotent INSERT … covered only by the
    // per-connection cache") is contradicted by the handler's own code:
    // `record_sync_change_metered` carries a DURABLE exactly-once check that
    // returns the existing head `seq` and charges nothing for a repeat of
    // (actor, path_hash, manifest_hash, change_type, ckv), plus a
    // repeated-delete guard — its own comment says the durable content check
    // "is the only exactly-once guarantee". Identity is content-derived
    // (blake3 path + manifest hashes); quota/content-key refusals are
    // nest-side refusals, which do not drive class.
    m.insert("fauna.sync.register", OfflineSafe);
    m.insert("fauna.sync.device_grant.register", OnlineOnly);
    m.insert("fauna.sync.device_grant.revoke", OnlineOnly);
    // The participation report/brake is nest-state-bound like its three
    // siblings: the self arm is a proof of possession verified against the
    // row's principal, and the owner arm's whole effect is a nest-side flag
    // another device reads back.
    m.insert("fauna.sync.devices.p2p_participation.set", OnlineOnly);
    // Relay serving's announce tags the live connection it rides — meaningless
    // once replayed later (the `fauna.push.presence` reasoning).
    m.insert("fauna.sync.serve.announce", OnlineOnly);
    m.insert("fauna.sync.changes.list", Read);
    m.insert("fauna.sync.changes.record", OfflineSafe);
    m.insert("fauna.sync.changes.supersede", OfflineSafe);
    m.insert("fauna.sync.status", Read);
    m.insert("fauna.sync.files", Read);
    m.insert("fauna.sync.backup_status", Read);
    m.insert("fauna.sync.devices.list", Read);
    m.insert("fauna.sync.devices.delete", OfflineSafe);
    // W2.3's class-2 write leg is the charter's own worked example of
    // `OfflineSafe`: the effect is a client-assigned-id, state-based mergeable
    // entry (`account-data-plane.md` § The class-2 entry form), so a replica
    // applies it to its own store immediately and syncs it as data. It is not
    // `OfflineQueued` because nothing about the effect is nest-assigned — the
    // nest allocates a feed `seq` for relay ordering, but the entry's identity
    // is its `(writer_id, writer_seq, item_key)` coordinates, which the writer
    // owns.
    m.insert(crate::account_state::KIND_STATE_PUT, OfflineSafe);
    // `state.put`'s retire counterpart (`account-data-taxonomy.md` § The
    // generation machinery → *Fleet-scope reclamation*): also
    // client-assigned identity (the caller's own cleartext coordinates,
    // never a nest-allocated one) and a converging effect — the retire
    // probe early-returns once the row is already superseded, so a
    // re-apply is a no-op, the same shape as the sibling
    // `changes.supersede` entry above. The nest's `not_yet_stable` /
    // `generation_in_use` refusals are preconditions, not identity or
    // effect-shape facts, so per rule 2 above they do not drive class.
    m.insert(crate::account_state::KIND_STATE_RETIRE, OfflineSafe);

    // ── register_media_kinds ────────────────────────────────────────
    // A cross-set aggregating read.
    m.insert("fauna.media.list", Read);
    // Mints a URL credential and writes nothing — a read of the nest's signer.
    m.insert(crate::media_ticket::KIND_MEDIA_PLAYBACK_TICKET, Read);

    // ── register_web_kinds ──────────────────────────────────────────
    // Web publishing. Domain and apex/subdomain wiring is provisioning; the
    // publish set is the user's own idempotent mapping; minting a paywall token
    // is a one-shot.
    m.insert("fauna.web.publish.set", OfflineSafe);
    m.insert("fauna.web.publish.unset", OfflineSafe);
    m.insert("fauna.web.publish.list", Read);
    m.insert("fauna.web.domain.set", OnlineOnly);
    m.insert("fauna.web.domain.get", Read);
    m.insert("fauna.web.domain.delete", OnlineOnly);
    m.insert("fauna.web.set_apex_actor", OnlineOnly);
    m.insert("fauna.web.get_apex_actor", Read);
    m.insert("fauna.web.set_subdomain_enabled", OnlineOnly);
    m.insert("fauna.web.get_subdomain_enabled", Read);
    m.insert("fauna.web.paywall.mint_token", OnlineOnly);
    // The sealed-projection complete-set declaration is only meaningful in the
    // same live session as the fully successful re-record walk it follows
    // (`web.rs`, `WebFilesPruneSealedRequest`) — a deferred or replayed-stale
    // set deletes live content, so it is never applied locally or queued.
    m.insert("fauna.web.files.prune_sealed", OnlineOnly);

    // ── register_labels_kinds ───────────────────────────────────────
    // Label attachment — an idempotent keyed write.
    m.insert("fauna.labels.attach", OfflineSafe);
    m.insert("fauna.labels.list", Read);

    // ── register_admin_kinds ────────────────────────────────────────
    // The admin plane. The charter names admin/provisioning online-only
    // outright, so every mutation here is `OnlineOnly` and every query is `Read`
    // — no per-kind latitude, which is also why this block needs no per-kind
    // rationale.
    m.insert("fauna.admin.users.list", Read);
    m.insert("fauna.admin.users.get", Read);
    m.insert("fauna.admin.users.create", OnlineOnly);
    m.insert("fauna.admin.users.update", OnlineOnly);
    m.insert("fauna.admin.users.delete", OnlineOnly);
    m.insert("fauna.admin.users.clear_handle", OnlineOnly);
    m.insert("fauna.admin.users.evict", OnlineOnly);
    m.insert("fauna.admin.users.cancel_eviction", OnlineOnly);
    m.insert("fauna.admin.users.suspend", OnlineOnly);
    m.insert("fauna.admin.evictions.list", Read);
    m.insert("fauna.admin.tiers.list", Read);
    m.insert("fauna.admin.tiers.create", OnlineOnly);
    m.insert("fauna.admin.tiers.update", OnlineOnly);
    m.insert("fauna.admin.membership_tiers.list", Read);
    m.insert("fauna.admin.membership_tiers.set", OnlineOnly);
    m.insert("fauna.admin.membership_tiers.clear", OnlineOnly);
    m.insert("fauna.admin.invite_codes.list", Read);
    m.insert("fauna.admin.invite_codes.create", OnlineOnly);
    m.insert("fauna.admin.invite_codes.delete", OnlineOnly);
    m.insert("fauna.admin.invite_requests.list", Read);
    m.insert("fauna.admin.invite_requests.approve", OnlineOnly);
    m.insert("fauna.admin.invite_requests.deny", OnlineOnly);
    m.insert("fauna.admin.admins.list", Read);
    m.insert("fauna.admin.admins.add", OnlineOnly);
    m.insert("fauna.admin.admins.remove", OnlineOnly);
    // The co-admin seed hand-off is `OnlineOnly`, not `Read`, despite being a
    // pure read: `Read`'s offline-answerability is a projection question (a
    // client-side materialized view), and there is no legitimate materialized
    // copy of a live deployment secret to answer from — the self-healing
    // capture that consumes this kind IS the only place a copy is meant to
    // exist, and only after this round trip succeeds.
    m.insert("fauna.admin.deployment_seed.get", OnlineOnly);
    // Rotation is `OnlineOnly` for a reason stronger than "it is a mutation":
    // the transaction re-keys secrets that exist only on the box, so a queued
    // offline copy could never be replayed correctly against a box that has
    // moved on. The ceremony is also ordering-sensitive (rotate only into a
    // clean roster) — a decision no offline queue can re-check at drain time.
    m.insert("fauna.admin.deployment_seed.rotate", OnlineOnly);
    m.insert("fauna.admin.stats", Read);
    m.insert("fauna.admin.status", Read);
    m.insert("fauna.admin.audit.list", Read);
    m.insert("fauna.admin.audit.integrity", Read);
    m.insert("fauna.admin.cluster.status", Read);
    m.insert("fauna.admin.gc", OnlineOnly);
    m.insert("fauna.admin.worker.status", Read);
    m.insert("fauna.admin.pending_actions.list", Read);
    // The declared region. `get` is an ordinary admin read. `set` is
    // `OnlineOnly` for the same reason the feature-policy update kinds are: a
    // declaration is made against what the tier currently holds, and one
    // replayed out of an offline queue would re-assert a situs the admin has
    // since changed — including re-declaring a region they deliberately
    // withdrew.
    m.insert("fauna.admin.region.get", Read);
    m.insert("fauna.admin.region.set", OnlineOnly);
    // The web-app origin choice: an ordinary admin read, and a `set` that is
    // `OnlineOnly` like the region's — a choice replayed out of an offline queue
    // would re-assert what `/app` answers after the admin had moved it back.
    m.insert("fauna.admin.web_app_origin.get", Read);
    m.insert("fauna.admin.web_app_origin.set", OnlineOnly);
    // The app's region relay: a read of the nest's own cache (the log fetch
    // happens off the request path) — the `list_roster` grading, not a
    // nest-to-nest relay's `OnlineOnly`.
    m.insert("fauna.region.artifact.get", Read);
    m.insert("fauna.admin.folders.create", OnlineOnly);
    m.insert("fauna.admin.folders.get", Read);
    m.insert("fauna.admin.folders.add_member", OnlineOnly);
    m.insert("fauna.admin.services.list", Read);
    m.insert("fauna.admin.services.update", OnlineOnly);
    m.insert("fauna.admin.factory_reset", OnlineOnly);
    m.insert("fauna.admin.set_registration_mode", OnlineOnly);
    m.insert("fauna.admin.set_subhandles", OnlineOnly);
    m.insert("fauna.admin.set_age_verification_required", OnlineOnly);
    m.insert("fauna.admin.set_max_storage_bytes", OnlineOnly);
    m.insert("fauna.admin.set_cors_origins", OnlineOnly);
    m.insert("fauna.admin.set_serving_port", OnlineOnly);
    m.insert("fauna.admin.request_host_restart", OnlineOnly);
    m.insert("fauna.admin.logs", Read);

    // ── register_push_kinds ─────────────────────────────────────────
    // Push registration — a live endpoint the nest must reach.
    m.insert("fauna.push.vapid_key", Read);
    m.insert("fauna.push.subscribe", OnlineOnly);
    m.insert("fauna.push.unsubscribe", OnlineOnly);
    // Tags the live connection it rides — meaningless once replayed later.
    m.insert("fauna.push.presence", OnlineOnly);

    // ── register_sessions_kinds ─────────────────────────────────────
    // Session revocation — a security ceremony that must take effect now.
    m.insert("fauna.sessions.list", Read);
    m.insert("fauna.sessions.revoke", OnlineOnly);
    m.insert("fauna.sessions.revoke_all", OnlineOnly);
    m.insert("fauna.sessions.lockout", OnlineOnly);

    // ── register_invite_kinds ───────────────────────────────────────
    // Invite requests + code verification — provisioning against a live nest.
    m.insert("fauna.account.invite_request.submit", OnlineOnly);
    m.insert("fauna.account.invite_request.status", Read);
    m.insert("fauna.account.invite_request.cancel", OnlineOnly);
    m.insert("fauna.account.invite_code.verify", OnlineOnly);

    // ── register_nat_mode_kind ──────────────────────────────────────
    // Setup-time provisioning.
    m.insert("fauna.setup.nat_mode", OnlineOnly);

    // ── register_posts_kinds ────────────────────────────────────────
    // Posts — the charter's own worked example of offline-safe: `post_id =
    // blake3(body)`, so a local apply and a later sync converge. `interact` is
    // the counter-example the registry itself names: `like` increments a non-
    // content-addressed score, which is why it carries `forbid_replay` and why
    // it queues rather than applying locally.
    // create: handler-verified 2026-08-13 (W4 phase 2) — the DB legs are
    // idempotent as the charter says, but the Bluesky write-through fan-out
    // (`spawn_write_through`) calls create_record with rkey: None and no
    // dedupe against the existing crosspost mapping, so a replay publishes a
    // SECOND external PDS record (AP is fine — its activity_id derives from
    // post_id). That is a handler BUG, not a classification fact: the class
    // stays on the charter's exemplar and the dedupe fix is
    // nest-captured.
    m.insert("fauna.posts.get", Read);
    m.insert("fauna.posts.list", Read);
    // A community room's verdicts for room-restricted posts — a read of a
    // derived view the nest already holds, `room.search`'s class.
    m.insert("fauna.posts.room_labels", Read);
    // The relayed twin is a **relay**, so serving it needs not just this nest
    // but a live nest↔nest leg to the room's home — `OnlineOnly` rather than
    // `Read`, the `room.list_roster_remote` grading verbatim. There is no
    // local answer to fall back on: this nest indexes no post of a room it
    // does not home, so its own reception-pass map resolves nothing.
    m.insert("fauna.posts.room_labels_remote", OnlineOnly);
    m.insert("fauna.posts.create", OfflineSafe);
    m.insert("fauna.posts.delete", OfflineSafe);
    m.insert("fauna.posts.interact", OfflineQueued);

    // ── register_profile_kinds ──────────────────────────────────────
    // `set` is a client-signed own-write whose content row id is `blake3(body)`
    // under an `INSERT OR REPLACE` — offline-safe on the registry's own note.
    m.insert("fauna.profile.get", Read);
    m.insert("fauna.profile.set", OfflineSafe);

    // ── register_notifications_kinds ────────────────────────────────
    // `mark_read` is an idempotent upsert that converges on replay — the
    // registry contrasts it with `posts.interact` explicitly.
    m.insert("fauna.notifications.list", Read);
    m.insert("fauna.notifications.mark_read", OfflineSafe);
    m.insert("fauna.notifications.count", Read);
    // `dismiss` / `clear` delete by an id the client already holds, or up to
    // a timestamp it chose — idempotent, commutative with everything else on
    // the row, and converging on replay exactly like `mark_read`.
    m.insert("fauna.notifications.dismiss", OfflineSafe);
    m.insert("fauna.notifications.clear", OfflineSafe);

    // ── register_contacts_kinds ─────────────────────────────────────
    // Connection management. `accept` stays queued on a real per-request
    // effect — it releases the held knock payload into the inbox
    // (`deliver_held_knock_payload`). `block` was queued on a per-call
    // training accumulator that is gone (`mail-spam.md` § Implicit signals
    // are forbidden). Handler-verified 2026-10-03: `block_contact_core` is
    // three own-row writes keyed by `(actor, peer)` — the contact upsert to
    // `blocked`, the knock delete and its doorbell delete — with no
    // notification, no federation send and nothing the peer observes, so a
    // replay converges and it is `OfflineSafe` beside `unblock`.
    // Handler-verified 2026-08-13
    // (W4 phase 2): `unblock` is one guarded own-row DELETE and `confirm` one
    // guarded own-row UPDATE — no notification, no federation send, no
    // effect on the peer anywhere in either handler — so the old family-wide
    // "changes what the other party sees" rationale does not hold for them at
    // code level; both are client-keyed idempotent own-row writes. `dismiss`
    // and the inbox mode are own-side only.
    m.insert("fauna.knocks.list", Read);
    m.insert("fauna.knocks.accept", OfflineQueued);
    m.insert("fauna.knocks.block", OfflineSafe);
    m.insert("fauna.knocks.unblock", OfflineSafe);
    m.insert("fauna.knocks.dismiss", OfflineSafe);
    m.insert("fauna.contacts.list", Read);
    m.insert("fauna.contacts.status", Read);
    m.insert("fauna.contacts.confirm", OfflineSafe);
    m.insert("fauna.inbox.mode.get", Read);
    m.insert("fauna.inbox.mode.set", OfflineSafe);

    // ── register_feed_kinds ─────────────────────────────────────────
    // Feeds. `create` inserts a row with a fresh random `feed_id` — the registry
    // notes a replay would duplicate it — so it needs a client-generated id and
    // queues. The rest are owner-keyed idempotent overwrites/removals.
    m.insert("fauna.feed.list", Read);
    m.insert("fauna.feed.get", Read);
    m.insert("fauna.feed.posts", Read);
    m.insert("fauna.feed.local.posts", Read);
    m.insert("fauna.feed.trending.posts", Read);
    m.insert("fauna.feed.contributors.list", Read);
    m.insert("fauna.feed.create", OfflineQueued);
    m.insert("fauna.feed.update", OfflineSafe);
    m.insert("fauna.feed.delete", OfflineSafe);
    m.insert("fauna.feed.contributors.grant", OfflineSafe);
    m.insert("fauna.feed.contributors.revoke", OfflineSafe);
    m.insert("fauna.feed.factors.get", Read);
    m.insert("fauna.feed.factors.set", OfflineSafe);

    // ── register_personalization_kinds ──────────────────────────────
    // The sealed personalization model — an idempotent whole-row overwrite keyed
    // on the caller's own `(actor, factor)`. Purely local data.
    m.insert("fauna.personalization.model.fetch", Read);
    m.insert("fauna.personalization.model.put", OfflineSafe);
    m.insert("fauna.personalization.model.delete", OfflineSafe);

    // ── register_search_kinds ───────────────────────────────────────
    // A pure read.
    m.insert("fauna.search.query", Read);

    // ── register_segments_kinds ─────────────────────────────────────
    // Segment listing + the changed push event; `compact` is nest-side
    // maintenance.
    m.insert("fauna.segments.list", Read);
    m.insert("fauna.segments.changed", Read);
    m.insert("fauna.segments.compact", OnlineOnly);
    m.insert("fauna.segments.counter_floor", OnlineOnly);

    // ── register_content_index_kinds ────────────────────────────────
    // The `__index` rail. `record` converges on the same live row for a repeated
    // `(path, blob_hash)` — content-addressed, so offline-safe.
    // Precision (phase 2, 2026-08-13): it calls the UNMETERED
    // `record_sync_change`, which has no content dedup, so a replay appends a
    // duplicate feed row; the READABLE state (MAX(seq) per path) still
    // converges, which is the test — but the rail is inconsistent with its
    // metered twin (`sync.changes.record`, durably deduped) and could adopt
    // the same check.
    m.insert("fauna.index.record", OfflineSafe);
    m.insert("fauna.index.list", Read);
    m.insert("fauna.bridges.index_list", Read);

    // ── protocol ────────────────────────────────────────────────────
    // The protocol's own liveness probe (registered by
    // `default_with_protocol_kinds`).
    m.insert("fauna.protocol.echo", Read);

    m
}

// ─────────────────────────────────────────────────────────────────────────────
// The UI-desensitizing rule (W4 phase 4)
// ─────────────────────────────────────────────────────────────────────────────

/// Whether a surface may offer an affordance **right now**.
///
/// This is the *decision* half of the charter's class-3 sentence — "UI
/// desensitizes these offline" (`account-data-plane.md` § The offline-mutation
/// contract) — and it lives here, next to the classification it reads, for the
/// same reason the classification is Rust and not a doc table: the surface has
/// to *ask*. Every app already depends on this crate, so all seven consume one
/// rule; none re-derives `class == OnlineOnly` for itself, and none keeps a
/// hand-list of widgets-to-grey (priority #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affordance {
    /// Offer it: the kind's class survives the app's current reach.
    Available,
    /// Desensitize it, and say why: the kind is [`OfflineClass::OnlineOnly`]
    /// and there is no live nest to arbitrate it.
    ///
    /// The reason is per-affordance on purpose. The charter forbids expressing
    /// this as a global "offline mode" banner: "Features declare their nest
    /// dependency per kind (the `offline_class` precedent), never as a global
    /// offline mode" (`account-data-plane.md` § R11), and the same sentence is
    /// what makes a nest-*less* account work — a nest-arbitrated kind "stays
    /// desensitized exactly as it does offline", so this one rule covers both
    /// situations with no second concept.
    NeedsNest,
}

impl Affordance {
    /// `true` when the affordance may be offered — the boolean an app hands
    /// its widget's own sensitivity flag.
    pub fn is_available(self) -> bool {
        matches!(self, Affordance::Available)
    }

    /// The localized reason to show beside a desensitized affordance, or
    /// `None` when it is available.
    ///
    /// A [`fauna_core::localized::LocalizedText`] rather than a resolved
    /// string, like every other shared render decision
    /// ([`fauna_core::format`]): the key→text lookup is each app's own i18n
    /// pipeline.
    pub fn reason(self) -> Option<fauna_core::localized::LocalizedText> {
        match self {
            Affordance::Available => None,
            Affordance::NeedsNest => Some(fauna_core::localized::LocalizedText::key(
                "common.needs_nest",
            )),
        }
    }
}

/// The connection-state words that mean **no live nest**.
///
/// The same lowercase wire vocabulary
/// [`fauna_core::format::connection_state_label`] takes, for the same reason it
/// takes strings rather than the transport enum: this crate does not depend on
/// the native transport crate, and the word is what every app family already
/// carries (web's string, the FFI's, FaunaKit's).
const OFFLINE_STATE_WORDS: [&str; 3] = ["connecting", "disconnected", "unreachable"];

/// Does `connection_state` mean the app **has** a live nest?
///
/// The one owner of [`affordance`]'s ruling 3 polarity: a word is online unless
/// it is one of the *known* offline words, so an older peer meeting a future
/// state word reads as online rather than dead. Callers must never re-derive
/// this by comparing against `"connected"` — that inverts the ruling and turns
/// every future word into a hang (measured: the e2e connection barrier's first
/// design did exactly that).
///
/// Split out of [`affordance`] because the e2e state providers need the verdict
/// with no `kind` in hand: every app publishes it beside the word so the harness
/// waits on shared Rust's answer instead of shipping a second copy of
/// `OFFLINE_STATE_WORDS` in Python.
pub fn is_online(connection_state: &str) -> bool {
    !OFFLINE_STATE_WORDS.contains(&connection_state)
}

/// May a surface offer an affordance that issues `kind`, given the app's
/// current `connection_state`?
///
/// `connection_state` is the lowercase transport word
/// ([`fauna_core::format::connection_state_label`]'s input): `"connected"`,
/// `"connecting"`, `"disconnected"`, `"unreachable"`.
///
/// # The three rulings this function makes
///
/// 1. **Only [`OfflineClass::OnlineOnly`] desensitizes.** `OfflineSafe` and
///    `OfflineQueued` are precisely the classes that *work* without a nest —
///    greying them would contradict the outbox this table was built for. And
///    [`OfflineClass::Read`] is not a mutation at all: whether a given read is
///    *answerable* offline is a projection question (W3) the module docs
///    deliberately decline to encode, so desensitizing a read here would assert
///    something the classification does not claim.
///
/// 2. **An unregistered kind is [`Affordance::Available`].** An unknown string
///    is not a licence to grey out a working control; the bijection test is
///    what keeps kinds registered, and a typo must surface as a failing test,
///    never as a dead button in a user's hands.
///
/// 3. **Only the *known* offline words count as offline.** Note the polarity is
///    opposite to [`fauna_core::format::connection_state_label`], which reads an
///    unrecognised word as `disconnected` — and it is opposite for the same
///    reason: never overclaim. For a *display*, the honest weaker claim is
///    "disconnected"; for a *gate*, the honest weaker claim is "don't block the
///    user". So an older app meeting a future state word keeps its controls
///    live: the worst case is a request that fails with the error it would have
///    shown anyway, instead of a working button the user cannot press.
pub fn affordance(kind: &str, connection_state: &str) -> Affordance {
    if is_online(connection_state) {
        return Affordance::Available;
    }
    match offline_class(kind) {
        Some(OfflineClass::OnlineOnly) => Affordance::NeedsNest,
        // Ruling 1 (safe/queued/read) and ruling 2 (`None`) land here together.
        _ => Affordance::Available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One registered kind of each class, taken from the table itself rather
    /// than hard-coded: a later reclassification (phase 2 moved 16 entries)
    /// must not turn these gate tests red for the wrong reason.
    fn a_kind_of(class: OfflineClass) -> &'static str {
        table()
            .iter()
            .find(|(_, c)| **c == class)
            .map(|(k, _)| *k)
            .unwrap_or_else(|| panic!("no registered kind is classified {class:?}"))
    }

    /// The whole rule, in both directions, over every class.
    ///
    /// Online: nothing is ever desensitized. Offline: `OnlineOnly` and only
    /// `OnlineOnly` is — ruling 1 of [`affordance`].
    #[test]
    fn only_online_only_kinds_desensitize_and_only_while_offline() {
        for class in [
            OfflineClass::Read,
            OfflineClass::OfflineSafe,
            OfflineClass::OfflineQueued,
            OfflineClass::OnlineOnly,
        ] {
            let kind = a_kind_of(class);
            assert_eq!(
                affordance(kind, "connected"),
                Affordance::Available,
                "{kind} ({class:?}) must stay live while connected"
            );
            let offline = affordance(kind, "disconnected");
            if class == OfflineClass::OnlineOnly {
                assert_eq!(
                    offline,
                    Affordance::NeedsNest,
                    "{kind} is OnlineOnly — it must desensitize offline"
                );
            } else {
                assert_eq!(
                    offline,
                    Affordance::Available,
                    "{kind} ({class:?}) works without a nest — greying it \
                     would contradict the class"
                );
            }
        }
    }

    /// Every non-connected transport word desensitizes an `OnlineOnly` kind —
    /// `connecting` and `unreachable` are as nest-less as `disconnected`.
    #[test]
    fn every_offline_word_desensitizes() {
        let kind = a_kind_of(OfflineClass::OnlineOnly);
        for word in OFFLINE_STATE_WORDS {
            assert_eq!(
                affordance(kind, word),
                Affordance::NeedsNest,
                "{word:?} means no live nest"
            );
        }
    }

    /// Ruling 2: an unregistered kind never greys a control out.
    #[test]
    fn an_unregistered_kind_stays_available() {
        assert_eq!(
            affordance("fauna.not.a.registered.kind", "disconnected"),
            Affordance::Available
        );
    }

    /// Ruling 3, and the polarity that separates this from
    /// `connection_state_label`: a state word an older app has never seen
    /// leaves its controls live rather than blocking the user.
    #[test]
    fn an_unknown_state_word_reads_as_online() {
        let kind = a_kind_of(OfflineClass::OnlineOnly);
        assert_eq!(affordance(kind, "connected"), Affordance::Available);
        assert_eq!(
            affordance(kind, "some-future-state"),
            Affordance::Available,
            "an unrecognised word must not desensitize — the gate's honest \
             weaker claim is 'do not block'"
        );
    }

    /// The desensitized arm carries a reason, and the available arm does not —
    /// the charter's per-feature "needs a nest", never a global banner.
    #[test]
    fn only_the_desensitized_arm_carries_a_reason() {
        assert_eq!(Affordance::Available.reason(), None);
        assert!(Affordance::Available.is_available());
        let reason = Affordance::NeedsNest.reason().expect("a reason to show");
        assert_eq!(reason.key, "common.needs_nest");
        assert!(!Affordance::NeedsNest.is_available());
    }

    /// Dump every registered kind with its `forbid_replay` value, so the table
    /// above can be authored against the compiled registry rather than a grep
    /// of the source (kinds registered via constants are invisible to a grep).
    #[test]
    #[ignore = "authoring aid, not a gate"]
    fn dump_registered_kinds() {
        let reg = KindRegistry::full();
        let mut rows: Vec<_> = reg.iter().collect();
        rows.sort_by_key(|(k, _)| *k);
        let mut counts = std::collections::BTreeMap::new();
        for (k, meta) in &rows {
            let class = offline_class(k);
            println!("{}\t{}\t{:?}", k, meta.forbid_replay, class);
            *counts.entry(format!("{class:?}")).or_insert(0usize) += 1;
        }
        println!("TOTAL {} — per class {:?}", rows.len(), counts);
    }

    /// Every registered kind carries exactly one class, and every class entry
    /// names a registered kind. This is what makes adding a kind without
    /// answering the offline question impossible.
    #[test]
    fn every_registered_kind_is_classified() {
        let reg = KindRegistry::full();
        let table = table();

        let unclassified: Vec<&str> = reg
            .iter()
            .map(|(k, _)| k)
            .filter(|k| !table.contains_key(k))
            .collect();
        let unregistered: Vec<&str> = table.keys().copied().filter(|k| !reg.contains(k)).collect();

        assert!(
            unclassified.is_empty() && unregistered.is_empty(),
            "offline-class table is out of sync with the kind registry.\n\
             {} registered kind(s) with no class: {:#?}\n\
             {} class entr(ies) naming an unregistered kind: {:#?}",
            unclassified.len(),
            unclassified,
            unregistered.len(),
            unregistered,
        );
    }

    /// `forbid_replay = true` refutes both "this does not mutate" (`Read`) and
    /// "a second delivery is a no-op" (`OfflineSafe`). See the module docs.
    #[test]
    fn forbid_replay_is_consistent() {
        let reg = KindRegistry::full();
        let offenders: Vec<(&str, OfflineClass)> = reg
            .iter()
            .filter(|(_, meta)| meta.forbid_replay)
            .filter_map(|(k, _)| offline_class(k).map(|c| (k, c)))
            .filter(|(_, c)| matches!(c, OfflineClass::Read | OfflineClass::OfflineSafe))
            .collect();

        assert!(
            offenders.is_empty(),
            "these kinds are `forbid_replay = true`, so they cannot be Read \
             (they mutate) or OfflineSafe (a replay is not a no-op): {:#?}",
            offenders,
        );
    }

    /// [`is_online`] is the polarity, and a FUTURE word reads as online.
    ///
    /// The whole reason this is a named function rather than an equality check
    /// against `"connected"`: every caller that re-derived it — the e2e
    /// connection barrier most recently — got the future-word case backwards
    /// and would have blocked on a word the gate itself lets through.
    #[test]
    fn is_online_is_the_gate_polarity_not_equality_with_connected() {
        assert!(is_online("connected"));
        for word in OFFLINE_STATE_WORDS {
            assert!(!is_online(word), "{word} means no live nest");
        }
        // Ruling 3: an unknown/future word is online, never offline.
        assert!(is_online("degraded"));
        assert!(is_online(""));

        // And it agrees with the gate it was split out of, over both classes.
        let online_only = a_kind_of(OfflineClass::OnlineOnly);
        for word in ["connected", "degraded", "connecting", "unreachable"] {
            assert_eq!(
                affordance(online_only, word).is_available(),
                is_online(word),
                "the gate and its own polarity predicate disagree on {word:?}",
            );
        }
    }
}
