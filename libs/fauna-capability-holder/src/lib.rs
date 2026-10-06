//! The content-processing capability **holder** loop — shared Rust.
//!
//! A capability is a user-minted, scope-limited, revocable grant that lets its
//! holder (an enrolled service-user: the mail MDA, a scorer, an FTS-indexer,
//! the nest's web-serve component) unseal a specific slice of a user's content,
//! HPKE-wrapped to the holder's own X25519 pubkey. This [`Registry`] is the
//! re-acquire transport for those grants: it fetches the grants sealed to this
//! holder (via a caller-supplied fetch seam — a WS-RPC `fauna.capabilities.fetch`
//! for an external bridge, a direct DB read for an in-process nest component),
//! unseals every blob, and swaps the unwrapped set into a snapshot cache readers
//! consume lock-free.
//!
//! This is the Rust port of the Go `bins/fauna-bridges/internal/capability`
//! registry — the semantic spec (capability-mediated content-processing design
//! § 2.3). The load-bearing contract, identical on both sides:
//!
//!   - **TRANSIENT fetch error** (the fetch seam returns `Err`): the cached set
//!     is left INTACT. Revocation must not bite on a network blip.
//!   - **AUTHORITATIVE fetch** (the seam returns a — possibly empty — blob
//!     list): the cache is REPLACED with exactly what the authority served. A
//!     grant held before but absent now is dropped (revoked/expired) — it goes
//!     dark immediately for every subsequent [`Registry::current`] reader.
//!   - **Present-but-unsealable** blobs are skipped (logged) without aborting
//!     the refresh or keeping the old set — one bad grant must not block the
//!     revocation of others.
//!   - **Refresh is single-flight** (serialized on an async mutex).
//!   - [`GrantSet::key_for`] independently honors the grant's
//!     `[epoch_start, epoch_end]` window even though the authority already
//!     expiry-filters at fetch (defense-in-depth — the holder must not wield a
//!     key past the honest bound the user set).
//!
//! One deliberate divergence from the Go port: Go defers zeroization of a
//! superseded set by one refresh cycle (`retired` + `zeroizeSet`) because a GC
//! runtime has no deterministic reclaim. Rust readers hold `Arc<GrantSet>`
//! snapshots and every unwrapped key is a [`Zeroizing`] buffer, so a superseded
//! set is wiped exactly when its last reader drops it — strictly tighter than
//! the one-cycle bound, with no retired-set bookkeeping.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use fauna_mls::wrapped_blob::{
    GrantBlob, MLKEM768_DECAPS_KEY_LEN, ScopeTuple, unseal_capability, unseal_capability_hybrid,
};
use zeroize::Zeroizing;

/// One unsealed scope key inside a held grant: the scope tuple it serves plus
/// the minimal derived content key (e.g. a tier `period_key`), zeroized on drop.
pub struct UnsealedScopeKey {
    pub scope: ScopeTuple,
    /// `None` = master-key regime (every content kind today).
    pub epoch: Option<u64>,
    pub key: Zeroizing<Vec<u8>>,
}

/// One held (unsealed) capability grant.
pub struct HeldGrant {
    pub owner_actor_id: [u8; 32],
    pub grant_id: Vec<u8>,
    pub epoch_start: u64,
    pub epoch_end: u64,
    pub keys: Vec<UnsealedScopeKey>,
}

/// The immutable, snapshot-swapped cache: the unwrapped grants a successful
/// fetch last produced, keyed by `(owner, grant_id)` — the same storage key +
/// revocation handle the nest uses. Never mutated after construction; readers
/// hold a stable `Arc` snapshot.
#[derive(Default)]
pub struct GrantSet {
    by_key: HashMap<([u8; 32], Vec<u8>), HeldGrant>,
}

impl GrantSet {
    /// How many grants the holder currently holds.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// The held grants (unordered) — the audit/settings projection.
    pub fn grants(&self) -> impl Iterator<Item = &HeldGrant> {
        self.by_key.values()
    }

    /// The unwrapped content key for `(owner, class, kind, tier)` from a grant
    /// whose window contains `now_unix`, or `None` when this holder wields no
    /// such capability. `tier` must match exactly (`None` matches a tier-less
    /// scope like `content.read{mail}`; `Some` a post-tier scope). The window
    /// check independently honors `[epoch_start, epoch_end]` even though the
    /// authority already expiry-filters at fetch (defense-in-depth). The
    /// returned slice borrows the snapshot — hold the `Arc<GrantSet>` while
    /// using it.
    pub fn key_for(
        &self,
        owner: &[u8; 32],
        class: &str,
        kind: &str,
        tier: Option<&str>,
        now_unix: u64,
    ) -> Option<&[u8]> {
        self.by_key
            .values()
            .filter(|g| &g.owner_actor_id == owner)
            .filter(|g| now_unix >= g.epoch_start && now_unix <= g.epoch_end)
            .flat_map(|g| g.keys.iter())
            .find(|k| {
                k.scope.class == class
                    && k.scope.kind.as_deref() == Some(kind)
                    && k.scope.tier.as_deref() == tier
            })
            .map(|k| k.key.as_slice())
    }

    /// Candidate mail content keys for a record sealed at the wall-clock
    /// epoch its own ingest timestamp implies — the content-sealing-epochs
    /// design § 4 windowed-holder opener chain. `record_timestamp` is the
    /// record's stored ingest instant (e.g. the Go MDA's
    /// `FetchedCiphertext.InternalDate`); the target epoch is
    /// `mail_sealing_epoch_of(record_timestamp)`.
    ///
    /// Two regimes, mutually exclusive within one grant (the mint policy's
    /// XOR, § 2 — never both in one blob):
    ///
    ///   - **Master-key** (a wrap with `epoch: None`): matches any
    ///     timestamp, exactly [`Self::key_for`]'s shape — a grant whose wrap
    ///     carries no epoch (`build_grant_blob` mints them today) keeps
    ///     opening everything in its window. Returned alone (it is a superset of any bounded key this
    ///     holder might also wield from a different grant).
    ///   - **Bounded** (every wrap `epoch: Some(e)`): returns every held
    ///     epoch `e <= target`, **nearest first** (`target`, then
    ///     `target - 1` — the boundary/clock-skew tolerance — then any
    ///     older held epoch, the stale-published-schedule case, § 3 step
    ///     2). A held epoch `> target` is never a candidate — the record
    ///     cannot have been sealed under a key that didn't exist yet.
    ///
    /// The caller tries each returned key in order and lets AEAD/HPKE-open
    /// disambiguate (mirrors [`Self::keys_for_folder`]'s shape) — each miss
    /// is one cheap failed open. An empty vec is the fail-closed verdict:
    /// this holder cannot open a record from that epoch (never granted,
    /// revoked, or the record postdates the grant's held epoch set) — the
    /// content stays permanently dark to it, which for mail is the whole
    /// point of the bound (design § 8's tier_3 bar).
    ///
    /// `factor` is the bus factor the caller is about to compute, matched
    /// **exactly** against each wrap's `ScopeTuple::factor`
    /// ([`ScopeTuple::licenses_factor`]): a built-in perimeter factor
    /// (`None`) opens only under a factor-less wrap, and a community
    /// labeler's `labeler:<hex>` factor only under a wrap the owner minted
    /// for that labeler — the per-labeler grant. A
    /// grant that licenses no labeler therefore yields **no** key for one,
    /// whatever the worklist claims: the store's word never widens a wrap.
    pub fn keys_for_mail_epoch(
        &self,
        owner: &[u8; 32],
        factor: Option<&str>,
        record_timestamp: u64,
        now_unix: u64,
    ) -> Vec<&[u8]> {
        let target = fauna_mls::wrapped_blob::mail_sealing_epoch_of(record_timestamp);
        let mut master: Option<&[u8]> = None;
        let mut bounded: Vec<(u64, &[u8])> = Vec::new();
        for g in self
            .by_key
            .values()
            .filter(|g| &g.owner_actor_id == owner)
            .filter(|g| now_unix >= g.epoch_start && now_unix <= g.epoch_end)
        {
            for k in &g.keys {
                if k.scope.class != ScopeTuple::CLASS_CONTENT_READ
                    || k.scope.kind.as_deref() != Some(ScopeTuple::KIND_MAIL)
                    || !k.scope.licenses_factor(factor)
                {
                    continue;
                }
                match k.epoch {
                    None => master = Some(k.key.as_slice()),
                    Some(e) if e <= target => bounded.push((e, k.key.as_slice())),
                    Some(_) => {}
                }
            }
        }
        if let Some(m) = master {
            return vec![m];
        }
        bounded.sort_by_key(|b| std::cmp::Reverse(b.0));
        bounded.into_iter().map(|(_, k)| k).collect()
    }

    /// Every unwrapped **folder content key** this holder wields for
    /// `(owner, set)` at content-key generation `version` — the
    /// `content.read{folder:set}` scope class, the set named by its
    /// `set_name_hash` ([`ScopeTuple::folder_set_qualifier`]), whose wraps carry the
    /// generation `version` in the `epoch` slot (`key-material-hierarchy.md`
    /// § M2 → distribution channels; `monetization.md` § Pillar 2).
    ///
    /// Returns **all** matching candidates rather than one, because a
    /// concurrent-rotation CRDT merge can leave two distinct keys sharing one
    /// `version` (KMH § M2 *Generations*). The caller tries each and lets the
    /// AEAD tag disambiguate — the same shape as the sync engine's
    /// `content_open_roots`. An empty vec is the fail-closed verdict: this
    /// holder cannot open that generation (never granted, revoked, or the
    /// grant's window has passed), so the content stays dark.
    ///
    /// The `set` qualifier must match exactly — a grant for set A never opens
    /// set B, and a version-`v` wrap never opens version `v+1` (the epoch is
    /// AAD-bound, so a substituted wrap fails to unseal in the first place).
    pub fn keys_for_folder(
        &self,
        owner: &[u8; 32],
        set_name_hash: &[u8],
        version: u64,
        now_unix: u64,
    ) -> Vec<&[u8]> {
        let set = ScopeTuple::folder_set_qualifier(set_name_hash);
        self.by_key
            .values()
            .filter(|g| &g.owner_actor_id == owner)
            .filter(|g| now_unix >= g.epoch_start && now_unix <= g.epoch_end)
            .flat_map(|g| g.keys.iter())
            .filter(|k| {
                k.scope.class == ScopeTuple::CLASS_CONTENT_READ
                    && k.scope.kind.as_deref() == Some(ScopeTuple::KIND_FOLDER)
                    && k.scope.set.as_deref() == Some(set.as_str())
                    && k.epoch == Some(version)
            })
            .map(|k| k.key.as_slice())
            .collect()
    }
}

/// Errors surfaced by [`Registry::refresh`].
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The fetch seam failed — the TRANSIENT verdict; the cached set was kept.
    #[error("fetch capability grants: {0}")]
    Fetch(String),
}

/// The async fetch seam: returns the canonical-CBOR `GrantBlob`s currently
/// sealed to this holder. `Err` is the TRANSIENT verdict (keep cached);
/// `Ok(blobs)` — possibly empty — is the AUTHORITATIVE verdict (replace).
pub type FetchFn = Box<
    dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<Vec<u8>>, String>> + Send>>
        + Send
        + Sync,
>;

/// Holds the current set of unsealed capability grants for one holder identity
/// and re-fetches them on demand. Reads are lock-free snapshots via
/// [`Registry::current`]; refreshes are single-flight.
pub struct Registry {
    x25519_secret: Zeroizing<[u8; 32]>,
    /// The holder's ML-KEM-768 decapsulation key (PQ hybrid), `None` for a
    /// classical-only holder. `Some` opens both classical and X-Wing wraps.
    mlkem_dk: Option<Zeroizing<[u8; MLKEM768_DECAPS_KEY_LEN]>>,
    fetch: FetchFn,
    current: RwLock<Arc<GrantSet>>,
    /// Single-flights [`Registry::refresh`] — the swap must be serialized so
    /// two concurrent authoritative fetches cannot interleave replace order.
    refresh_lock: tokio::sync::Mutex<()>,
}

impl Registry {
    /// Construct a registry for one holder identity. Performs no I/O; the
    /// caller drives the first [`Registry::refresh`].
    pub fn new(
        x25519_secret: [u8; 32],
        mlkem_dk: Option<[u8; MLKEM768_DECAPS_KEY_LEN]>,
        fetch: FetchFn,
    ) -> Self {
        Self {
            x25519_secret: Zeroizing::new(x25519_secret),
            mlkem_dk: mlkem_dk.map(Zeroizing::new),
            fetch,
            current: RwLock::new(Arc::new(GrantSet::default())),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The live grant-set snapshot. A holder with no successful fetch yet reads
    /// as an empty set.
    pub fn current(&self) -> Arc<GrantSet> {
        self.current
            .read()
            .expect("grant-set lock poisoned")
            .clone()
    }

    /// Re-fetch the grants sealed to this holder and rebuild the cache under
    /// the transient/authoritative contract (module docs). Single-flight.
    pub async fn refresh(&self) -> Result<(), RefreshError> {
        let _flight = self.refresh_lock.lock().await;
        let blobs = (self.fetch)().await.map_err(RefreshError::Fetch)?;

        let mut by_key = HashMap::with_capacity(blobs.len());
        for blob_bytes in &blobs {
            match self.unseal_blob(blob_bytes) {
                Ok(grant) => {
                    by_key.insert((grant.owner_actor_id, grant.grant_id.clone()), grant);
                }
                Err(err) => {
                    // Present-but-unsealable: not a revoke, not a transport
                    // error — a malformed/foreign grant. Omit it (go dark for
                    // its scope) without aborting the refresh.
                    tracing::warn!(target: "capability_holder", %err, "skipping unsealable grant");
                }
            }
        }
        let fetched = blobs.len();
        let held = by_key.len();
        let new_set = Arc::new(GrantSet { by_key });
        *self.current.write().expect("grant-set lock poisoned") = new_set;
        tracing::info!(
            target: "capability_holder",
            grants = held,
            fetched_blobs = fetched,
            "capability grant set updated"
        );
        Ok(())
    }

    /// Unseal one canonical-CBOR `GrantBlob` with the holder secrets. All-or-
    /// nothing per grant, mirroring the FFI `unseal_capability_grant`.
    fn unseal_blob(&self, blob_bytes: &[u8]) -> Result<HeldGrant, String> {
        let blob = GrantBlob::from_canonical_bytes(blob_bytes)
            .map_err(|e| format!("decode grant blob: {e}"))?;
        let owner: [u8; 32] = blob
            .index
            .0
            .as_slice()
            .try_into()
            .map_err(|_| format!("grant owner must be 32 bytes, got {}", blob.index.0.len()))?;
        let mut keys = Vec::with_capacity(blob.wrapped_keys.len());
        for wk in &blob.wrapped_keys {
            let key = match &self.mlkem_dk {
                Some(dk) => unseal_capability_hybrid(wk, &owner, &self.x25519_secret, dk),
                None => unseal_capability(wk, &owner, &self.x25519_secret),
            }
            .map_err(|e| format!("unseal scope key: {e}"))?;
            keys.push(UnsealedScopeKey {
                scope: wk.scope.clone(),
                epoch: wk.epoch,
                key: Zeroizing::new(key),
            });
        }
        Ok(HeldGrant {
            owner_actor_id: owner,
            grant_id: blob.index.1.clone(),
            epoch_start: blob.window.0,
            epoch_end: blob.window.1,
            keys,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fauna_mls::wrapped_blob::{GrantWindow, build_grant_blob, derive_x25519_keypair_from_ikm};

    use super::*;

    fn holder_keys() -> ([u8; 32], [u8; 32]) {
        derive_x25519_keypair_from_ikm(b"capability-holder-test-ikm")
    }

    fn post_scope(tier: &str) -> ScopeTuple {
        ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
            kind: Some(ScopeTuple::KIND_POST.to_string()),
            tier: Some(tier.to_string()),
            set: None,
            factor: None,
        }
    }

    fn mail_scope() -> ScopeTuple {
        ScopeTuple::mail()
    }

    fn mint_blob(
        owner: &[u8; 32],
        grant_id: &[u8; 16],
        holder_pub: &[u8; 32],
        tier: &str,
        period_key: [u8; 32],
        window: (u64, u64),
    ) -> Vec<u8> {
        build_grant_blob(
            owner,
            grant_id,
            holder_pub,
            None,
            GrantWindow(window.0, window.1),
            &[(post_scope(tier), Some(period_key.to_vec()))],
        )
        .expect("mint grant blob")
        .to_canonical_bytes()
        .expect("encode grant blob")
    }

    /// A shared, mutable script of fetch verdicts: each entry is one call's
    /// answer — either the blobs it serves or the error it raises.
    type VerdictScript = Arc<Mutex<Vec<Result<Vec<Vec<u8>>, String>>>>;

    /// A fetch seam backed by a mutable script of verdicts.
    fn scripted_fetch(script: VerdictScript) -> FetchFn {
        Box::new(move || {
            let script = script.clone();
            Box::pin(async move { script.lock().expect("script lock").remove(0) })
        })
    }

    /// The folder lookup is exact on **set** and **generation**: a grant for
    /// set A never opens set B, and a version-`v` wrap never opens `v+1`.
    /// Same-version duplicates (the CRDT-merge edge, KMH § M2) all come back so
    /// the caller can let the AEAD disambiguate.
    /// Stand-in `set_name_hash`es: the holder only compares bytes.
    const MEMBERS_SITE: &[u8] = &[0x51; 32];
    const OTHER_SET: &[u8] = &[0x52; 32];

    #[tokio::test]
    async fn keys_for_folder_matches_set_and_generation_exactly() {
        use fauna_mls::wrapped_blob::build_grant_blob_with_epochs;

        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let (g1, g2a, g2b) = ([0xA1u8; 32], [0xA2u8; 32], [0xA9u8; 32]);
        let scope = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
            kind: Some(ScopeTuple::KIND_FOLDER.to_string()),
            tier: None,
            set: Some(ScopeTuple::folder_set_qualifier(MEMBERS_SITE)),
            factor: None,
        };
        let blob = build_grant_blob_with_epochs(
            &owner,
            &[1u8; 16],
            &holder_pub,
            None,
            GrantWindow(0, u64::MAX),
            &[(
                scope,
                vec![
                    (Some(1), g1.to_vec()),
                    // Two distinct keys at ONE version — the merge edge.
                    (Some(2), g2a.to_vec()),
                    (Some(2), g2b.to_vec()),
                ],
            )],
        )
        .expect("mint folder grant")
        .to_canonical_bytes()
        .expect("encode");

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob])]));
        let registry = Registry::new(holder_sec, None, scripted_fetch(script));
        registry.refresh().await.expect("refresh");
        let set = registry.current();

        assert_eq!(
            set.keys_for_folder(&owner, MEMBERS_SITE, 1, 100),
            vec![&g1[..]],
            "generation 1 opens with its own key"
        );
        let v2 = set.keys_for_folder(&owner, MEMBERS_SITE, 2, 100);
        assert_eq!(v2.len(), 2, "both same-version generations are candidates");
        assert!(v2.contains(&&g2a[..]) && v2.contains(&&g2b[..]));

        assert!(
            set.keys_for_folder(&owner, MEMBERS_SITE, 3, 100).is_empty(),
            "a generation never granted must be dark (no fallback to another)"
        );
        assert!(
            set.keys_for_folder(&owner, OTHER_SET, 1, 100).is_empty(),
            "a grant for one set must never open another"
        );
        assert!(
            set.keys_for_folder(&[9u8; 32], MEMBERS_SITE, 1, 100)
                .is_empty(),
            "a grant from one owner must never open another's set"
        );
    }

    /// A bounded (epoch-wrapped) mail grant: exact-epoch match wins outright;
    /// missing the exact epoch degrades to the nearest held epoch at or
    /// before the target (never a held epoch *after* it — the design § 4
    /// windowed-holder chain); a record older than every held epoch is dark.
    #[tokio::test]
    async fn keys_for_mail_epoch_bounded_grant_nearest_first_and_never_future() {
        use fauna_mls::wrapped_blob::{MAIL_SEALING_EPOCH_SECS, build_grant_blob_with_epochs};

        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let (k100, k101, k102) = ([0x10u8; 32], [0x11u8; 32], [0x12u8; 32]);
        let blob = build_grant_blob_with_epochs(
            &owner,
            &[1u8; 16],
            &holder_pub,
            None,
            GrantWindow(0, u64::MAX),
            &[(
                mail_scope(),
                vec![
                    (Some(100), k100.to_vec()),
                    (Some(101), k101.to_vec()),
                    (Some(102), k102.to_vec()),
                ],
            )],
        )
        .expect("mint bounded mail grant")
        .to_canonical_bytes()
        .expect("encode");

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob])]));
        let registry = Registry::new(holder_sec, None, scripted_fetch(script));
        registry.refresh().await.expect("refresh");
        let set = registry.current();

        let ts_for = |e: u64| e * MAIL_SEALING_EPOCH_SECS + 10;

        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, ts_for(101), 1),
            vec![&k101[..], &k100[..]],
            "exact epoch first, then every OLDER held epoch as a fallback \
             candidate (the stale-published-schedule case) — never epoch 102, \
             which postdates the target"
        );
        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, ts_for(103), 1),
            vec![&k102[..], &k101[..], &k100[..]],
            "epoch 103 not held: every held epoch <= target is a candidate, \
             nearest first"
        );
        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, ts_for(500), 1),
            vec![&k102[..], &k101[..], &k100[..]],
            "way past the held set: still degrades to the held epochs, nearest first"
        );
        assert!(
            set.keys_for_mail_epoch(&owner, None, ts_for(50), 1)
                .is_empty(),
            "before every held epoch: the record predates this grant's schedule, never openable"
        );
        assert!(
            set.keys_for_mail_epoch(&[9u8; 32], None, ts_for(101), 1)
                .is_empty(),
            "a grant from one owner must never open another's mail"
        );
    }

    /// A master-key (standing) mail grant — one `epoch: None` wrap — opens a
    /// record from any timestamp, exactly like the pre-epoch [`GrantSet::key_for`]
    /// shape; the mint policy's XOR (design § 2) means this never mixes with
    /// per-epoch wraps in the same grant.
    #[tokio::test]
    async fn keys_for_mail_epoch_master_key_grant_matches_any_timestamp() {
        use fauna_mls::wrapped_blob::MAIL_SEALING_EPOCH_SECS;

        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let standing_key = [0x42u8; 32];
        let blob = build_grant_blob(
            &owner,
            &[1u8; 16],
            &holder_pub,
            None,
            GrantWindow(0, u64::MAX),
            &[(mail_scope(), Some(standing_key.to_vec()))],
        )
        .expect("mint master-key mail grant")
        .to_canonical_bytes()
        .expect("encode");

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob])]));
        let registry = Registry::new(holder_sec, None, scripted_fetch(script));
        registry.refresh().await.expect("refresh");
        let set = registry.current();

        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, 0, 1),
            vec![&standing_key[..]]
        );
        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, 999 * MAIL_SEALING_EPOCH_SECS, 1),
            vec![&standing_key[..]],
            "the standing wrap opens any epoch — no forward/backward bound"
        );
    }

    /// The grant's own advisory `[epoch_start, epoch_end]` window is honored
    /// independently of the epoch match, exactly like [`GrantSet::key_for`].
    #[tokio::test]
    async fn keys_for_mail_epoch_honors_grant_window() {
        use fauna_mls::wrapped_blob::{MAIL_SEALING_EPOCH_SECS, build_grant_blob_with_epochs};

        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let key101 = [0x11u8; 32];
        let blob = build_grant_blob_with_epochs(
            &owner,
            &[1u8; 16],
            &holder_pub,
            None,
            GrantWindow(100, 200),
            &[(mail_scope(), vec![(Some(101), key101.to_vec())])],
        )
        .expect("mint")
        .to_canonical_bytes()
        .expect("encode");

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob])]));
        let registry = Registry::new(holder_sec, None, scripted_fetch(script));
        registry.refresh().await.expect("refresh");
        let set = registry.current();

        let ts = 101 * MAIL_SEALING_EPOCH_SECS + 10;
        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, ts, 150),
            vec![&key101[..]]
        );
        assert!(
            set.keys_for_mail_epoch(&owner, None, ts, 99).is_empty(),
            "before the grant's own window: deny even though the epoch matches"
        );
        assert!(
            set.keys_for_mail_epoch(&owner, None, ts, 201).is_empty(),
            "after the grant's own window: deny even though the epoch matches"
        );
    }

    /// The per-labeler license: a wrap serves exactly
    /// the factor it was minted for. The composed MDA grant (factor-less)
    /// yields no key for a community labeler however the worklist names it,
    /// labeler A's grant yields none for labeler B or for a built-in factor,
    /// and each opens only its own.
    #[tokio::test]
    async fn keys_for_mail_epoch_is_confined_to_the_wraps_factor() {
        use fauna_mls::wrapped_blob::{MAIL_SEALING_EPOCH_SECS, build_grant_blob_with_epochs};

        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let base_key = [0x10u8; 32];
        let a_key = [0xA1u8; 32];
        let mint = |grant_id: u8, scope: ScopeTuple, key: [u8; 32]| {
            build_grant_blob_with_epochs(
                &owner,
                &[grant_id; 16],
                &holder_pub,
                None,
                GrantWindow(0, u64::MAX),
                &[(scope, vec![(Some(101), key.to_vec())])],
            )
            .expect("mint")
            .to_canonical_bytes()
            .expect("encode")
        };
        let base = mint(1, ScopeTuple::mail(), base_key);
        let labeler_a = mint(2, ScopeTuple::mail_for_factor("labeler:aa"), a_key);

        let script = Arc::new(Mutex::new(vec![Ok(vec![base, labeler_a])]));
        let registry = Registry::new(holder_sec, None, scripted_fetch(script));
        registry.refresh().await.expect("refresh");
        let set = registry.current();
        let ts = 101 * MAIL_SEALING_EPOCH_SECS + 10;

        assert_eq!(
            set.keys_for_mail_epoch(&owner, None, ts, 1),
            vec![&base_key[..]],
            "a built-in factor opens under the composed grant only"
        );
        assert_eq!(
            set.keys_for_mail_epoch(&owner, Some("labeler:aa"), ts, 1),
            vec![&a_key[..]],
            "labeler A opens under its own grant only — never the composed one"
        );
        assert!(
            set.keys_for_mail_epoch(&owner, Some("labeler:bb"), ts, 1)
                .is_empty(),
            "a labeler the owner never granted stays dark, whatever the worklist names"
        );
    }

    #[tokio::test]
    async fn authoritative_fetch_populates_and_key_for_matches_scope() {
        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let period_key = [42u8; 32];
        let blob = mint_blob(
            &owner,
            &[1u8; 16],
            &holder_pub,
            "gold",
            period_key,
            (100, 200),
        );

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob])]));
        let reg = Registry::new(holder_sec, None, scripted_fetch(script));
        reg.refresh().await.expect("refresh");

        let set = reg.current();
        assert_eq!(set.len(), 1);
        // Exact scope match returns the wrapped period key.
        assert_eq!(
            set.key_for(&owner, "content.read", "post", Some("gold"), 150),
            Some(&period_key[..])
        );
        // Wrong tier, wrong kind, wrong owner, tier-less query: all deny.
        assert!(
            set.key_for(&owner, "content.read", "post", Some("silver"), 150)
                .is_none()
        );
        assert!(
            set.key_for(&owner, "content.read", "mail", None, 150)
                .is_none()
        );
        assert!(
            set.key_for(&[8u8; 32], "content.read", "post", Some("gold"), 150)
                .is_none()
        );
        assert!(
            set.key_for(&owner, "content.read", "post", None, 150)
                .is_none()
        );
        // Outside the window: deny even though the authority served it.
        assert!(
            set.key_for(&owner, "content.read", "post", Some("gold"), 99)
                .is_none()
        );
        assert!(
            set.key_for(&owner, "content.read", "post", Some("gold"), 201)
                .is_none()
        );
    }

    #[tokio::test]
    async fn transient_error_keeps_cached_set_and_authoritative_absent_drops() {
        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let blob = mint_blob(
            &owner,
            &[1u8; 16],
            &holder_pub,
            "gold",
            [42u8; 32],
            (0, u64::MAX),
        );

        let script = Arc::new(Mutex::new(vec![
            Ok(vec![blob]),                  // 1: populate
            Err("network blip".to_string()), // 2: transient → keep
            Ok(vec![]),                      // 3: authoritative empty → drop
        ]));
        let reg = Registry::new(holder_sec, None, scripted_fetch(script));

        reg.refresh().await.expect("populate");
        assert_eq!(reg.current().len(), 1);

        // Transient: refresh errors, the cached set survives (revocation must
        // not bite on a network blip).
        assert!(matches!(reg.refresh().await, Err(RefreshError::Fetch(_))));
        assert_eq!(
            reg.current().len(),
            1,
            "transient error must keep the cached set"
        );

        // Authoritative empty: the grant goes dark immediately.
        reg.refresh().await.expect("authoritative empty");
        assert!(
            reg.current().is_empty(),
            "authoritative absent grant must drop"
        );
    }

    #[tokio::test]
    async fn unsealable_blob_is_skipped_without_aborting_refresh() {
        let (holder_sec, holder_pub) = holder_keys();
        // A blob sealed to a DIFFERENT holder — present but unsealable by us.
        let (_, foreign_pub) = derive_x25519_keypair_from_ikm(b"foreign-holder-ikm");
        let owner = [7u8; 32];
        let good = mint_blob(
            &owner,
            &[1u8; 16],
            &holder_pub,
            "gold",
            [42u8; 32],
            (0, u64::MAX),
        );
        let foreign = mint_blob(
            &owner,
            &[2u8; 16],
            &foreign_pub,
            "gold",
            [43u8; 32],
            (0, u64::MAX),
        );

        let script = Arc::new(Mutex::new(vec![Ok(vec![good, foreign])]));
        let reg = Registry::new(holder_sec, None, scripted_fetch(script));
        reg.refresh()
            .await
            .expect("refresh must not abort on one bad grant");

        let set = reg.current();
        assert_eq!(
            set.len(),
            1,
            "the unsealable grant is omitted, the good one held"
        );
        assert!(
            set.key_for(&owner, "content.read", "post", Some("gold"), 1)
                .is_some()
        );
    }

    #[tokio::test]
    async fn snapshot_survives_replacement_until_dropped() {
        let (holder_sec, holder_pub) = holder_keys();
        let owner = [7u8; 32];
        let blob = mint_blob(
            &owner,
            &[1u8; 16],
            &holder_pub,
            "gold",
            [42u8; 32],
            (0, u64::MAX),
        );

        let script = Arc::new(Mutex::new(vec![Ok(vec![blob]), Ok(vec![])]));
        let reg = Registry::new(holder_sec, None, scripted_fetch(script));
        reg.refresh().await.expect("populate");

        // A reader captures the snapshot, then a revoke lands.
        let snapshot = reg.current();
        reg.refresh().await.expect("authoritative empty");
        // New readers see the revoke immediately; the captured snapshot stays
        // valid (its keys are zeroized when the last holder drops it).
        assert!(reg.current().is_empty());
        assert_eq!(snapshot.len(), 1);
        assert!(
            snapshot
                .key_for(&owner, "content.read", "post", Some("gold"), 1)
                .is_some()
        );
    }
}
