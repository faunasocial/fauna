//! The preference surfaces over the account store — the shared layer every
//! app's muted-words, hidden-content, sync-prefs, trained-topics and
//! task-delegation pages read and write through.
//!
//! Authority: `docs/goal/architecture/config-dissolution.md` § The `__config`
//! dissolution schedule (the E1 cluster and *The closure order*, steps (1),
//! (2) and (5)). The four delegable preference kinds live on the account-state
//! plane and nowhere else: a read is answered from the replica's own store,
//! and a save is [`AccountStoreHandle::put_preference`]. The page state, its
//! loaded-bit discipline and the normalization are one implementation for all
//! seven apps (priority #2: the surface logic lives here, not in seven per-app
//! copies of decode + normalize + project).
//!
//! # What this module is, exactly
//!
//! The **plane ↔ sub-record boundary**, and nothing else. A plane entry's value
//! for an admitted preference kind *is* the cluster's sub-record in canonical
//! dag-cbor (the admitted table in [`crate::preference_put`]), so everything
//! here is one of two moves: decode an entry into its sub-record, or mutate a
//! sub-record and put it back. The **mutation meaning** is not here — it lives
//! once in `fauna_client_config::preference_records`. The **composition** of a
//! sub-record with nest-side facts — a delegation row's live lease, a trained
//! factor's example count — is not here either; it stays in the surface's own
//! shared crate, taking the sub-record as an argument.
//!
//! # Reaching the store
//!
//! Every surface takes an [`AccountStoreAccess`]: a live handle, or the seat's
//! `AccountHandleSource`, which is what an app's page hands it. A page can be
//! opened in the first seconds after sign-in, before the runtime has
//! assembled; the gesture then **waits** for the runtime (bounded —
//! `account_driver::wait_for_account_handle`) and fails if none comes. It
//! never reads or writes the sealed `__config` blob: until closure step (5)
//! each surface had a second arm that did, and the CAS-blob bridge carried
//! the value across (`config-dissolution.md` § *What replaces the bridge's two
//! carriages*, case (a)).
//!
//! # Read semantics
//!
//! **Local-first**: the handle answers from the replica's
//! own store, offline included; convergence with the fleet arrives through
//! the runtime's pump (nudge / backstop / reconnect) rather than inside the
//! read. The one exception is the **first-listing gate**
//! ([`crate::account_driver::first_listing_gate`]): a replica that has never
//! listed the delegable scope from its bound nest — a fresh device, a
//! successor's first launch, a re-created store — answers no read of it. The
//! read waits, bounded, for that first listing, which lands the fleet's rows
//! and carries a predecessor identity's, and is refused as not ready
//! ([`crate::account_driver::ScopeNotReady`]) if it does not come; it never
//! answers the empty store as the account's value. On a listed replica a
//! `None` entry is a genuine answer ("this replica holds no record"), so it
//! maps to the sub-record's `Default` — a *loaded* empty page.

use anyhow::{Context, Result};
use fauna_client_config::{MutedWordsSnapshot, preference_records};
use fauna_client_delegation::{TaskDelegationError, TaskDelegationView};
use fauna_client_personalization::topics::{TrainedTopicRow, TrainedTopics, TrainedTopicsError};
// `ModerationConfig` / `SyncPrefsConfig` are deliberately NOT imported: since
// the plane doors take their payload type FROM the typed
// kind constant (`records::*`), so no call site here names them. The doc links
// below carry their full path for the same reason.
use fauna_core::data::{DelegationConfig, PersonalizationConfig};
use fauna_core::delegation::{PinOption, TaskDelegationRow};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_i18n::strings::common::{NEEDS_NEST, NEEDS_OTHER_DEVICE};
use fauna_protocol::account_state::ACCOUNT_STATE_SCOPE;
use fauna_protocol::merge_policy::{RecordKind, records};
use fauna_protocol::personalization::TRAINED_FACTORS_MAX;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::account_driver::{AccountStoreAccess, AccountStoreHandle};

// ── The generic core (private — surfaces are reached by name, never by kind) ──

/// The store a gesture on `kind` runs against — the access's handle, waited
/// for when the access is the seat's source.
async fn store_for<T>(
    store: &impl AccountStoreAccess,
    kind: &RecordKind<T>,
) -> Result<AccountStoreHandle> {
    store
        .account_store(&format!("the {} record's store", kind.name))
        .await
}

/// Decode the replica's local entry for `kind` into its sub-record; a missing
/// entry reads as `Default` (see the module header's read semantics).
///
/// The payload type comes FROM the typed kind constant
/// (`fauna_protocol::merge_policy::records`) — never beside it — so a call
/// site cannot pair a kind with a foreign type (the kind→type binding
/// is the constant's, checked against the Secret-free pin table's const bind
/// block at compile time).
async fn read_record<T: DeserializeOwned + Default>(
    handle: &AccountStoreHandle,
    kind: &RecordKind<T>,
) -> Result<T> {
    let kind = kind.name;
    // The first-listing gate, on the delegable scope: a replica that has
    // never listed it answers no read. Its first listing lands the fleet's
    // rows and carries a predecessor identity's. Every write here is
    // [`update_record`]'s read-modify-write, so this read is also what keeps
    // a first gesture from being applied to an empty list and then outranking
    // the account's under latest-wins (`account-client-lifecycle.md` § The
    // client-side lifecycle → *The first listing*).
    crate::account_driver::first_listing_gate(handle, ACCOUNT_STATE_SCOPE).await?;
    match handle.get_preference(kind).await? {
        Some(entry) => canonical_decode::<T>(&entry.value)
            .with_context(|| format!("account store: the {kind} entry does not decode")),
        None => Ok(T::default()),
    }
}

/// What a plane-rail failure reads as on a surface: for the read gate's
/// refusal, the reason it waits for — the one every nest-requiring
/// affordance already carries (`common.needs_nest`) when the read was refused
/// because this replica has not listed the account's records from its nest
/// yet, or the other-device reason where only a sibling can end an unkeyed
/// hold (`account-client-lifecycle.md` § The client-side lifecycle → *The
/// first listing*, clauses (4) and (5)); the gesture can be retried once it
/// has come — and the error's own text for anything else.
pub fn plane_failure(e: anyhow::Error) -> String {
    crate::account_driver::not_ready_reason(&e).map_or_else(|| e.to_string(), str::to_string)
}

/// Whether a twin's failure text is the read gate's refusal
/// ([`plane_failure`]) — what a surface that logs a background read's failure
/// reads to show this one instead: a page that has not loaded because the
/// replica is not ready must say why.
pub fn refused_as_not_ready(reason: &str) -> bool {
    reason == NEEDS_NEST || reason == NEEDS_OTHER_DEVICE
}

/// [`read_record`] over the access: reach the store, then read.
async fn load_record<T: DeserializeOwned + Default>(
    store: &impl AccountStoreAccess,
    kind: &RecordKind<T>,
) -> Result<T> {
    read_record(&store_for(store, kind).await?, kind).await
}

/// Read-modify-write one sub-record through the plane, returning the stored
/// record and whatever `mutate` returned.
///
/// A mutation that leaves the encoded bytes unchanged **skips the put**: the
/// decision is made on the bytes rather than on a caller-reported `bool`, so a
/// mutator that reports "changed" while producing an identical record still
/// mints no stamp. That matters beyond saved work: every put is a new
/// [`fauna_protocol::merge_policy::LwwStamp`], and a no-op write that outranks a
/// concurrent sibling's real edit would lose it.
async fn update_record<T, U>(
    store: &impl AccountStoreAccess,
    kind: &RecordKind<T>,
    mutate: impl FnOnce(&mut T) -> U,
) -> Result<(T, U)>
where
    T: DeserializeOwned + Default + Serialize,
{
    let handle = store_for(store, kind).await?;
    let mut record: T = read_record(&handle, kind).await?;
    let kind = kind.name;
    let before = canonical_encode(&record).with_context(|| format!("encode {kind}"))?;
    let out = mutate(&mut record);
    let after = canonical_encode(&record).with_context(|| format!("encode {kind}"))?;
    if after != before {
        handle.put_preference(kind, after).await?;
    }
    Ok((record, out))
}

// ── moderation: the muted-keywords list ──

/// Read the muted-keywords list off the account store.
pub async fn load_muted_words(store: &impl AccountStoreAccess) -> Result<MutedWordsSnapshot> {
    let moderation = load_record(store, &records::MODERATION).await?;
    // A completed store read is a completed round trip to the replica's
    // store, so the page is `loaded`.
    Ok(MutedWordsSnapshot {
        keywords: moderation.muted_keywords,
        loaded: true,
    })
}

/// Replace the muted-keywords list.
///
/// Read-modify-write over the moderation record (so a sibling field on
/// [`fauna_core::data::ModerationConfig`] — the hidden-content list — survives
/// this facet's save), normalized by the shared
/// [`preference_records::set_muted_keywords`]: the returned rows are what was
/// stored, never what was typed.
pub async fn save_muted_words(
    store: &impl AccountStoreAccess,
    keywords: Vec<fauna_core::data::MutedKeyword>,
) -> Result<MutedWordsSnapshot> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::set_muted_keywords(moderation, keywords)
    })
    .await?;
    Ok(MutedWordsSnapshot {
        keywords: moderation.muted_keywords,
        loaded: true,
    })
}

/// Add one term.
///
/// The delta (`preference_records::add_muted_keyword`) applies against the
/// moderation record as `update_record` reads it, never against a page's
/// stale copy — so a term another device stored since this page loaded
/// survives, where a whole-list [`save_muted_words`] from that stale copy
/// would clobber it.
pub async fn add_muted_word(
    store: &impl AccountStoreAccess,
    word: &str,
) -> Result<MutedWordsSnapshot> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::add_muted_keyword(moderation, word)
    })
    .await?;
    Ok(MutedWordsSnapshot {
        keywords: moderation.muted_keywords,
        loaded: true,
    })
}

/// Remove one term — [`add_muted_word`]'s inverse, same shared delta
/// (`preference_records::remove_muted_keyword`): exact stored spelling,
/// absent-term removal is a success no-op.
pub async fn remove_muted_word(
    store: &impl AccountStoreAccess,
    word: &str,
) -> Result<MutedWordsSnapshot> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::remove_muted_keyword(moderation, word)
    })
    .await?;
    Ok(MutedWordsSnapshot {
        keywords: moderation.muted_keywords,
        loaded: true,
    })
}

/// Set one term's level — how hard it mutes
/// (`fauna_core::scoring::MutedKeywordLevel`, the muted-words page's level
/// picker; the weights behind the two levels are the one place that maps
/// them). The shared delta `preference_records::set_muted_keyword_weight` at
/// the level's weight, applied as `update_record` reads the record like
/// [`add_muted_word`]: exact stored spelling, and a term the list does not
/// hold is a success no-op (another device removed it first — convergence,
/// not an error).
pub async fn set_muted_word_level(
    store: &impl AccountStoreAccess,
    word: &str,
    level: fauna_core::scoring::MutedKeywordLevel,
) -> Result<MutedWordsSnapshot> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::set_muted_keyword_weight(moderation, word, level.weight())
    })
    .await?;
    Ok(MutedWordsSnapshot {
        keywords: moderation.muted_keywords,
        loaded: true,
    })
}

// ── moderation: the reporter-side hide ──

/// The ids the user hid by reporting them (`moderation.md` § Corollary — block
/// also hides).
pub async fn load_hidden_content(store: &impl AccountStoreAccess) -> Result<Vec<String>> {
    let moderation: fauna_core::data::ModerationConfig =
        load_record(store, &records::MODERATION).await?;
    Ok(moderation.hidden_content)
}

/// Hide a reported subject: the shared delta
/// (`preference_records::hide_reported_content`) applied as `update_record`
/// reads the record, so the muted words beside it and an entry another device
/// hid meanwhile both survive. What the report sheet runs after a submit
/// lands; returns the stored list the app's render verdict then reads.
pub async fn hide_reported(store: &impl AccountStoreAccess, id: &str) -> Result<Vec<String>> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::hide_reported_content(moderation, id)
    })
    .await?;
    Ok(moderation.hidden_content)
}

/// Show a hidden subject again — [`hide_reported`]'s inverse; returns the
/// stored list.
pub async fn unhide_reported(store: &impl AccountStoreAccess, id: &str) -> Result<Vec<String>> {
    let (moderation, ()) = update_record(store, &records::MODERATION, |moderation| {
        preference_records::unhide_reported_content(moderation, id)
    })
    .await?;
    Ok(moderation.hidden_content)
}

// ── sync prefs: the default conflict policy for new folders ──

/// Read the owner's default conflict policy for new folders: the canonical
/// wire string, or `None` = no preference recorded, so new sets take the nest
/// column default.
pub async fn load_sync_prefs(store: &impl AccountStoreAccess) -> Result<Option<String>> {
    let prefs = load_record(store, &records::SYNC_PREFS).await?;
    Ok(prefs.default_conflict_policy)
}

/// Set (or clear, with `None`) the default conflict policy, returning the
/// **stored** value so what a select shows is always what was persisted.
///
/// Existing sets are untouched — each set's nest row stays authoritative;
/// this is the stamp for *new* sets only.
pub async fn save_sync_prefs(
    store: &impl AccountStoreAccess,
    policy: Option<&str>,
) -> Result<Option<String>> {
    let (prefs, ()) = update_record(store, &records::SYNC_PREFS, |prefs| {
        preference_records::set_default_conflict_policy(prefs, policy)
    })
    .await?;
    Ok(prefs.default_conflict_policy)
}

// ── personalization: the sealed trained-topic registry ──
//
// The registry alone — the model blob is a separate plane
// (`fauna_client_personalization::PersonalizationClient`) and stays on the nest.
// A caller composing rows feeds this sub-record to `TrainedTopics::rows_from`.

/// Read the sealed trained-topic registry off the account store.
pub async fn load_personalization(
    store: &impl AccountStoreAccess,
) -> Result<PersonalizationConfig> {
    load_record(store, &records::PERSONALIZATION).await
}

/// Read-modify-write the trained-topic registry.
///
/// `mutate` is expected to be one of the shared
/// `fauna_client_config::preference_records` registry functions, which own the
/// cap, the trimming and the unknown-id no-ops. Its return value comes back
/// with the stored record (the minted entry, the applied-or-not flag, the
/// removed entry), and a mutation that changed nothing writes nothing.
pub async fn update_personalization<U>(
    store: &impl AccountStoreAccess,
    mutate: impl FnOnce(&mut PersonalizationConfig) -> U,
) -> Result<(PersonalizationConfig, U)> {
    update_record(store, &records::PERSONALIZATION, mutate).await
}

// The four trained-topic gestures. They live here rather than at each app's
// call site because a gesture is a *sequence* (refuse, mutate the registry,
// pair the model leg, re-compose), and seven copies of a sequence drift. They
// keep the typed `TrainedTopicsError`: the surface renders `Cap` and
// `BlankName` as their own messages (`TrainedTopicsError::localized`), and the
// `fauna-ffi` seat maps them to distinct foreign variants.

/// Mint a factor.
pub async fn create_trained_topic<R>(
    store: &impl AccountStoreAccess,
    svc: &TrainedTopics<R>,
    name: &str,
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    // Checked before touching the store, and separately from the cap — folding
    // them together would tell someone who left the name box empty that they
    // had run out of topics.
    if name.trim().is_empty() {
        return Err(TrainedTopicsError::BlankName);
    }
    let (registry, minted) = update_personalization(store, |registry| {
        preference_records::add_trained_factor(registry, name)
    })
    .await
    .map_err(store_failure)?;
    if minted.is_none() {
        return Err(TrainedTopicsError::Cap(TRAINED_FACTORS_MAX));
    }
    svc.rows_from(&registry).await
}

/// Rename a factor. An unknown id is a no-op (the row was deleted on another
/// device between render and commit); the returned rows show the current
/// truth.
pub async fn rename_trained_topic<R>(
    store: &impl AccountStoreAccess,
    svc: &TrainedTopics<R>,
    id: &[u8],
    name: &str,
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let (registry, _applied) = update_personalization(store, |registry| {
        preference_records::rename_trained_factor(registry, id, name)
    })
    .await
    .map_err(store_failure)?;
    svc.rows_from(&registry).await
}

/// Flip a factor's Layer-A engagement opt-in. Registry-only: the model row is
/// untouched, so turning the flag off stops *future* weak training but never
/// rewrites what engagement already taught.
pub async fn set_trained_topic_engagement<R>(
    store: &impl AccountStoreAccess,
    svc: &TrainedTopics<R>,
    id: &[u8],
    on: bool,
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let (registry, _applied) = update_personalization(store, |registry| {
        preference_records::set_learn_from_engagement(registry, id, on)
    })
    .await
    .map_err(store_failure)?;
    svc.rows_from(&registry).await
}

/// Delete a factor: the registry removal first, then the paired model delete
/// off the removed entry's derived key.
pub async fn delete_trained_topic<R>(
    store: &impl AccountStoreAccess,
    svc: &TrainedTopics<R>,
    id: &[u8],
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let (registry, removed) = update_personalization(store, |registry| {
        preference_records::remove_trained_factor(registry, id)
    })
    .await
    .map_err(store_failure)?;
    svc.delete_model(removed.and_then(|meta| meta.factor_key()))
        .await?;
    svc.rows_from(&registry).await
}

/// The facet's rows: the registry off the replica's own store, composed by
/// the shared [`TrainedTopics::rows_from`].
pub async fn list_trained_topics<R>(
    store: &impl AccountStoreAccess,
    svc: &TrainedTopics<R>,
) -> Result<Vec<TrainedTopicRow>, TrainedTopicsError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let registry = load_personalization(store).await.map_err(store_failure)?;
    svc.rows_from(&registry).await
}

/// A store read/write failure, in the surface's own error vocabulary — the
/// record-couldn't-be-read-or-written arm.
fn store_failure(e: anyhow::Error) -> TrainedTopicsError {
    TrainedTopicsError::Config(plane_failure(e))
}

// ── delegation: the per-task-kind pins ──

/// Read the user's task-delegation pins off the account store.
///
/// Pins alone: a *row* is this sub-record composed with the live per-kind
/// leases the nest reports, which is `TaskDelegationView::rows_from`'s job.
pub async fn load_delegation(store: &impl AccountStoreAccess) -> Result<DelegationConfig> {
    load_record(store, &records::DELEGATION).await
}

/// Read-modify-write the delegation pins.
///
/// The *pinnability* refusal is not here: `resolve_pin` needs the caller's
/// participant identity and runner capability, so [`set_task_assignment`]
/// decides it in `TaskDelegationView` before reaching this write.
pub async fn update_delegation<U>(
    store: &impl AccountStoreAccess,
    mutate: impl FnOnce(&mut DelegationConfig) -> U,
) -> Result<(DelegationConfig, U)> {
    update_record(store, &records::DELEGATION, mutate).await
}

/// Read the Task-delegation rows: the pins come off the replica's own store,
/// the live leases come off the nest, and the row composition is the view's
/// own [`TaskDelegationView::rows_from`].
pub async fn load_task_delegation_rows<R>(
    store: &impl AccountStoreAccess,
    view: &TaskDelegationView<R>,
) -> Result<Vec<TaskDelegationRow>, TaskDelegationError<R::Error>>
where
    R: RpcRequester,
{
    let delegation = load_delegation(store)
        .await
        .map_err(|e| TaskDelegationError::Store(plane_failure(e)))?;
    view.rows_from(&delegation).await
}

/// Write one kind's assignment.
///
/// The pinnability refusal runs first ([`TaskDelegationView::resolve`]): a
/// self-pin this client can never run never reaches the write, so the kind
/// can never be left waiting forever.
pub async fn set_task_assignment<R>(
    store: &impl AccountStoreAccess,
    view: &TaskDelegationView<R>,
    task_kind: &str,
    option: &PinOption,
) -> Result<(), TaskDelegationError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let pin = view.resolve(task_kind, option)?;
    update_delegation(store, |delegation| delegation.set_pin(task_kind, pin))
        .await
        .map_err(|e| TaskDelegationError::Store(plane_failure(e)))?;
    Ok(())
}

/// A task-delegation failure as the page-level string a surface shows: the first-listing gate's refusal as its bare reason
/// ([`plane_failure`]), anything else as the error's own text.
pub fn delegation_failure<E: std::fmt::Display>(e: TaskDelegationError<E>) -> String {
    match e {
        TaskDelegationError::Store(reason) if refused_as_not_ready(&reason) => reason,
        other => other.to_string(),
    }
}

// ── The read seam for consumers that are not a preference page ──

/// A read failure on the seam: the read gate's refusal as the not-ready class
/// (`StoreError::is_not_ready`), anything else as a load failure carrying its
/// own text.
fn seam_failure(e: anyhow::Error) -> fauna_client_config::StoreError {
    match e.downcast::<crate::account_driver::ScopeNotReady>() {
        Ok(refusal) => refusal.into_load(),
        Err(e) => fauna_client_config::StoreError::Load(format!("{e:#}")),
    }
}

/// [`fauna_client_config::PreferenceStore`] over an [`AccountStoreAccess`] —
/// the feed's muted-words scorer and engagement opt-ins, the `index` lease
/// loop's pins. One body for both implementors below.
macro_rules! impl_preference_store {
    ($ty:ty) => {
        #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
        #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
        impl fauna_client_config::PreferenceStore for $ty {
            async fn moderation(
                &self,
            ) -> Result<fauna_core::data::ModerationConfig, fauna_client_config::StoreError> {
                load_record(self, &records::MODERATION)
                    .await
                    .map_err(seam_failure)
            }

            async fn personalization(
                &self,
            ) -> Result<PersonalizationConfig, fauna_client_config::StoreError> {
                load_personalization(self).await.map_err(seam_failure)
            }

            async fn delegation(
                &self,
            ) -> Result<DelegationConfig, fauna_client_config::StoreError> {
                load_delegation(self).await.map_err(seam_failure)
            }
        }
    };
}

impl_preference_store!(AccountStoreHandle);
impl_preference_store!(crate::account_driver::SeatAccountStore);
