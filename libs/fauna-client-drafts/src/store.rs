//! WS-RPC transport for the client-side `__drafts` store: [`DraftsClient`] is
//! the typed `fauna.drafts.{get,put}` call surface that seals on `save` and
//! unseals on `load`, generic over the `fauna_protocol::RpcRequester` seam (so
//! tests inject a fake nest and web/native pass their real transport).

use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcRequester;
use fauna_protocol::drafts::{
    GetDraftsReply, GetDraftsRequest, KIND_GET, KIND_PUT, PutDraftsReply, PutDraftsRequest,
};
use serde_bytes::ByteBuf;

use crate::seal::{
    DraftSealError, backup_key_from_seed, rekey_drafts_blob, seal_drafts, unseal_drafts,
};

/// What [`DraftsClient::rekey_rail_from_predecessors`] found on one rail.
///
/// Four values rather than a boolean: the aftermath's progress surface must
/// tell "done" from "nothing to do" from "still owed by some other device". Collapsing them would let a client report
/// the corpus re-sealed while a rail stays sealed to a retired key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftsRekeyOutcome {
    /// No blob at rest on this rail — a predecessor who never composed here.
    /// Nothing is owed, by this device or any other.
    NothingStored,
    /// Already sealed under the successor's own key. The idempotent arm: a pass
    /// that already completed, a second device arriving after the first, or a
    /// resumed pass. Costs one `get` and **writes nothing** — which matters
    /// here because the drafts plane has no CAS, so a
    /// needless re-write would race whatever the user has typed since.
    AlreadyCurrent,
    /// Opened under a predecessor's key and re-sealed under the successor's.
    Rekeyed,
    /// Present, and no offered key opens it. **Not a failure and not
    /// necessarily corruption** — the ordinary state on a device that never
    /// held the predecessor's seed (the user succeeded elsewhere), where the
    /// pass is still owed by a device that did. AEAD cannot distinguish a wrong
    /// key from damaged ciphertext, so genuine corruption also lands here; what
    /// both share is the only safe response, which is to leave the bytes alone.
    NoKeyOpensIt,
}

/// Failure from a [`DraftsClient`] `load`/`save`. Distinguishes a transport
/// failure (the WS-RPC call itself) from a seal/unseal failure, so per-app
/// glue can render an actionable message.
#[derive(Debug)]
pub enum DraftsClientError<E> {
    /// The `fauna.drafts.{get,put}` WS-RPC call failed (disconnect, deadline,
    /// server rejection, …). Carries the transport's own error.
    Transport(E),
    /// Sealing/unsealing the blob failed (wrong `BackupKey`, tampered/truncated
    /// bytes, corrupt zstd).
    Seal(DraftSealError),
}

impl<E: core::fmt::Display> core::fmt::Display for DraftsClientError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "drafts transport: {e}"),
            Self::Seal(e) => write!(f, "drafts seal/unseal: {e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for DraftsClientError<E> {}

/// Typed `fauna.drafts.{get,put}` call surface that seals/unseals a rail's
/// draft blob under the owner's `BackupKey`. One instance per actor on each
/// Fauna app; cheap to hold (a transport handle + the derived key).
///
/// Drafts are owner-only (no signing), so this does
/// not retain the identity keypair: it derives the at-rest `BackupKey` from the
/// keypair's seed at construction and keeps only that.
pub struct DraftsClient<R: RpcRequester> {
    nest: R,
    key: BackupKey,
}

impl<R: RpcRequester> DraftsClient<R> {
    /// Build over a transport handle and the user's identity keypair. The
    /// at-rest `BackupKey` is derived from the keypair's seed (BLAKE3
    /// `derive_key`); every device in the fleet derives the same key and so can
    /// unseal what another device sealed.
    pub fn new(nest: R, keypair: &ActorKeypair) -> Self {
        let key = backup_key_from_seed(keypair.secret_bytes());
        Self { nest, key }
    }

    /// Fetch + unseal the calling actor's draft blob for `path` (e.g.
    /// `"conversations"`). `Ok(None)` when the actor has never persisted drafts
    /// for this rail (first run) — the caller keeps its empty `DraftStore`. A
    /// present-but-undecryptable blob is a hard error ([`DraftsClientError::Seal`]),
    /// never silently masked as "no drafts" (which would let the next `save`
    /// clobber the user's real, just-unreadable drafts). The returned bytes are
    /// the canonical snapshot — hand them to `DraftStore::restore_from_bytes`.
    pub async fn load(&self, path: &str) -> Result<Option<Vec<u8>>, DraftsClientError<R::Error>> {
        let reply: GetDraftsReply = self
            .nest
            .request(
                KIND_GET,
                GetDraftsRequest {
                    path: path.to_string(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(DraftsClientError::Transport)?;
        match reply.blob {
            Some(blob) => unseal_drafts(blob.as_ref(), &self.key)
                .map(Some)
                .map_err(DraftsClientError::Seal),
            None => Ok(None),
        }
    }

    /// [`Self::load`], falling back to a predecessor's key when this actor's
    /// own does not open the blob — the **read** half of the post-succession
    /// `__drafts` leg, and the twin of the chunk corpus's own read fallback
    /// (`succession-aftermath.md` § Implementation status today, the
    /// successor-reads bullet).
    ///
    /// **Why a read fallback exists at all when
    /// [`Self::rekey_rail_from_predecessors`] re-seals the rail anyway.** The
    /// re-seal is a post-auth pass; this read runs at launch. Nothing orders
    /// them, so on a successor's *first* session the read usually wins — and
    /// without this fallback it hard-errors, the load gate never lifts, and the
    /// user's composers stay empty for the whole session even though the pass
    /// behind them is busy fixing exactly that. Recoverable at the next launch,
    /// but "your unsent drafts come back tomorrow" is not what § Re-key scope
    /// means by urgent.
    ///
    /// ⚠ **It reads only — it never writes.** The re-seal pass stays the single
    /// writer, so there is exactly one place that changes what rests. That
    /// separation is the same one the chunk corpus draws between its read
    /// fallback and its re-seal pass, and it is what keeps a *reader* from
    /// having to reason about the plane's missing CAS.
    ///
    /// Keys are tried successor-first, then `predecessors` in order.
    pub async fn load_with_predecessors(
        &self,
        path: &str,
        predecessors: &[BackupKey],
    ) -> Result<Option<Vec<u8>>, DraftsClientError<R::Error>> {
        let reply: GetDraftsReply = self
            .nest
            .request(
                KIND_GET,
                GetDraftsRequest {
                    path: path.to_string(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(DraftsClientError::Transport)?;
        let Some(blob) = reply.blob else {
            return Ok(None);
        };
        let blob = blob.as_ref();
        if let Ok(plaintext) = unseal_drafts(blob, &self.key) {
            return Ok(Some(plaintext));
        }
        for predecessor in predecessors {
            if let Ok(plaintext) = unseal_drafts(blob, predecessor) {
                return Ok(Some(plaintext));
            }
        }
        // No key opens it. Deliberately the SAME hard error `load` raises rather
        // than `Ok(None)`: masking it as "no drafts" would lift the save gate
        // and let the next autosave seal an empty store over the user's real,
        // merely-unreadable drafts.
        unseal_drafts(blob, &self.key)
            .map(Some)
            .map_err(DraftsClientError::Seal)
    }

    /// Seal + persist `snapshot_bytes` (from `DraftStore::snapshot_bytes`) for
    /// the calling actor's `path` rail, overwriting any prior blob. Idempotent
    /// overwrite (the kind is replay-safe), so an unchanged draft set re-uploads
    /// to the same content with no effect.
    pub async fn save(
        &self,
        path: &str,
        snapshot_bytes: &[u8],
    ) -> Result<(), DraftsClientError<R::Error>> {
        let blob = seal_drafts(snapshot_bytes, &self.key).map_err(DraftsClientError::Seal)?;
        let _: PutDraftsReply = self
            .nest
            .request(
                KIND_PUT,
                PutDraftsRequest {
                    path: path.to_string(),
                    blob: ByteBuf::from(blob),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(DraftsClientError::Transport)?;
        Ok(())
    }

    /// Re-seal this actor's `path` rail from a predecessor's key to its own —
    /// one rail of the post-succession `__drafts` corpus re-key
    /// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row:
    /// the seal re-keys **client-driven and urgent**).
    ///
    /// **Why it is urgent rather than tidy.** The succession transaction
    /// re-points the folder's owner, so the successor already *owns* the rail
    /// and fetches it by ordinary authenticated reads — but the seal did not
    /// move, so [`Self::load`] hard-errors and the load gate in
    /// [`crate::DraftsSync`] never opens. Until this runs the successor's
    /// drafts are **stuck, not corrupt**: unreadable here, and unreachable to
    /// the thief (no credential authenticates as the owner). Meanwhile the
    /// predecessor seed that opens them lives only in the account registry on
    /// the user's own devices, which is the device-loss race § Re-key scope
    /// names.
    ///
    /// **Safe to call unconditionally, on every device, at every sign-in.** The
    /// four [`DraftsRekeyOutcome`] arms are exhaustive and only one writes; a
    /// pass with nothing to do costs a single `get`. That is what makes it
    /// resumable with no progress state at rest — the corpus is its own
    /// progress record.
    ///
    /// `predecessors` is *every* retired identity's key in the account
    /// registry, not merely the one this session succeeded from: a phrase-only
    /// restore writes recovered predecessors back as ordinary registry rows, so
    /// a freshly-restored device holds them too, and a twice-succeeded chain
    /// leaves more than one. They are tried in order; the first that opens the
    /// rail wins.
    ///
    /// ⚠ **There is no compare-and-swap on this plane** (`fauna.drafts.put` is
    /// a plain overwrite — `fauna_protocol::drafts`), so this cannot write against the
    /// base it read at. The idempotent check immediately before the write is
    /// what stands in for it: a device that finds the rail already sealed under
    /// the successor's key writes nothing at all, which narrows the clobber
    /// window to one round trip. That is a genuine narrowing, not a fix — the
    /// drafts plane is last-writer-wins by construction, and this pass is one
    /// more writer within it, not a new hazard class.
    pub async fn rekey_rail_from_predecessors(
        &self,
        path: &str,
        predecessors: &[BackupKey],
    ) -> Result<DraftsRekeyOutcome, DraftsClientError<R::Error>> {
        let reply: GetDraftsReply = self
            .nest
            .request(
                KIND_GET,
                GetDraftsRequest {
                    path: path.to_string(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(DraftsClientError::Transport)?;

        let Some(blob) = reply.blob else {
            return Ok(DraftsRekeyOutcome::NothingStored);
        };
        let blob = blob.as_ref();

        // Already ours? The idempotent arm, and the one that keeps a resumed
        // pass from rolling the rail back over newer drafts.
        if unseal_drafts(blob, &self.key).is_ok() {
            return Ok(DraftsRekeyOutcome::AlreadyCurrent);
        }

        for predecessor in predecessors {
            // A key that does not open this rail is the expected case, not an
            // error to propagate — try the next. `rekey_drafts_blob` refuses
            // rather than writing when it cannot read, so nothing is destroyed
            // on the way through.
            let Ok(rekeyed) = rekey_drafts_blob(blob, predecessor, &self.key) else {
                continue;
            };
            let _: PutDraftsReply = self
                .nest
                .request(
                    KIND_PUT,
                    PutDraftsRequest {
                        path: path.to_string(),
                        blob: ByteBuf::from(rekeyed),
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(DraftsClientError::Transport)?;
            return Ok(DraftsRekeyOutcome::Rekeyed);
        }
        Ok(DraftsRekeyOutcome::NoKeyOpensIt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailingRequester, block_on};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Stateful in-memory fake nest: a `path → opaque blob` map. `put` stores the
    /// bytes; `get` returns them. Exercises the *real* seal → `fauna.drafts.put`
    /// → `fauna.drafts.get` → unseal round-trip with no HTTP and no tokio (the
    /// future is `Ready` on first poll — same shape as `FakeConfigNest`).
    #[derive(Default)]
    struct FakeDraftsNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl RpcRequester for FakeDraftsNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_PUT => {
                    let req: PutDraftsRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode put request");
                    self.stored
                        .lock()
                        .unwrap()
                        .insert(req.path, req.blob.into_vec());
                    fauna_protocol::encode_canonical(&PutDraftsReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_GET => {
                    let req: GetDraftsRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode get request");
                    let blob = self
                        .stored
                        .lock()
                        .unwrap()
                        .get(&req.path)
                        .cloned()
                        .map(ByteBuf::from);
                    fauna_protocol::encode_canonical(&GetDraftsReply {
                        blob,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn keypair() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }

    #[test]
    fn save_then_load_round_trips_byte_equal() {
        let client = DraftsClient::new(FakeDraftsNest::default(), &keypair());
        let snapshot = b"the whole conversations-rail draft set".repeat(4);

        block_on(client.save("conversations", &snapshot)).unwrap();
        let loaded = block_on(client.load("conversations")).unwrap();
        assert_eq!(loaded.as_deref(), Some(snapshot.as_slice()));
    }

    #[test]
    fn load_on_empty_rail_returns_none() {
        let client = DraftsClient::new(FakeDraftsNest::default(), &keypair());
        assert_eq!(block_on(client.load("conversations")).unwrap(), None);
    }

    #[test]
    fn paths_are_independent() {
        let client = DraftsClient::new(FakeDraftsNest::default(), &keypair());
        block_on(client.save("conversations", b"conv")).unwrap();
        block_on(client.save("posts", b"posts")).unwrap();
        assert_eq!(
            block_on(client.load("conversations")).unwrap().as_deref(),
            Some(b"conv".as_slice())
        );
        assert_eq!(
            block_on(client.load("posts")).unwrap().as_deref(),
            Some(b"posts".as_slice())
        );
        assert_eq!(block_on(client.load("events")).unwrap(), None);
    }

    /// A second `DraftsClient` for the *same identity* (a different device)
    /// unseals what the first sealed — the cross-device property, proven through
    /// the shared seal + the shared fake-nest blob store.
    #[test]
    fn second_device_same_identity_reads_first() {
        let shared = std::sync::Arc::new(FakeDraftsNest::default());
        // Two clients over the same backing store + same keypair = two devices.
        let device_a = DraftsClient::new(SharedNest(shared.clone()), &keypair());
        let device_b = DraftsClient::new(SharedNest(shared.clone()), &keypair());
        block_on(device_a.save("conversations", b"from A")).unwrap();
        assert_eq!(
            block_on(device_b.load("conversations")).unwrap().as_deref(),
            Some(b"from A".as_slice())
        );
    }

    /// The predecessor's own client — a device signed in as the retired
    /// identity, used to lay down predecessor-sealed rail blobs the successor
    /// then inherits.
    fn predecessor_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn predecessor_key() -> BackupKey {
        backup_key_from_seed(predecessor_keypair().secret_bytes())
    }

    /// The happy arm: the successor opens a rail its predecessor sealed and
    /// leaves it sealed under its own key, **with the draft prose intact**.
    /// Asserted by a plain `load` afterwards — the successor's ordinary read
    /// path is the observable that matters, not the outcome enum.
    #[test]
    fn rekey_reseals_a_predecessor_rail_and_the_drafts_survive() {
        let shared = std::sync::Arc::new(FakeDraftsNest::default());
        let predecessor = DraftsClient::new(SharedNest(shared.clone()), &predecessor_keypair());
        let successor = DraftsClient::new(SharedNest(shared.clone()), &keypair());
        let drafts = b"a half-written reply the succession must not eat".repeat(2);

        block_on(predecessor.save("conversations", &drafts)).unwrap();
        // Before the pass the successor cannot read its own inherited rail.
        assert!(block_on(successor.load("conversations")).is_err());

        let outcome =
            block_on(successor.rekey_rail_from_predecessors("conversations", &[predecessor_key()]))
                .unwrap();

        assert_eq!(outcome, DraftsRekeyOutcome::Rekeyed);
        assert_eq!(
            block_on(successor.load("conversations"))
                .unwrap()
                .as_deref(),
            Some(drafts.as_slice()),
        );
    }

    /// The idempotent arm — a resumed pass, or a second device arriving after
    /// the first. It must cost one `get` and **write nothing**: this plane has
    /// no CAS, so a needless re-write is a live clobber risk against whatever
    /// the user has typed since.
    #[test]
    fn rekey_is_idempotent_and_the_second_pass_does_not_write() {
        let shared = std::sync::Arc::new(FakeDraftsNest::default());
        let predecessor = DraftsClient::new(SharedNest(shared.clone()), &predecessor_keypair());
        let successor = DraftsClient::new(SharedNest(shared.clone()), &keypair());

        block_on(predecessor.save("posts", b"inherited")).unwrap();
        block_on(successor.rekey_rail_from_predecessors("posts", &[predecessor_key()])).unwrap();

        // The user types something new on this device after the pass.
        block_on(successor.save("posts", b"newer than the pass")).unwrap();

        let second =
            block_on(successor.rekey_rail_from_predecessors("posts", &[predecessor_key()]))
                .unwrap();

        assert_eq!(second, DraftsRekeyOutcome::AlreadyCurrent);
        assert_eq!(
            block_on(successor.load("posts")).unwrap().as_deref(),
            Some(b"newer than the pass".as_slice()),
            "a second pass must not roll the rail back to the inherited draft"
        );
    }

    /// A rail the predecessor never wrote. Distinct from every other arm
    /// because nothing is owed by *any* device — the progress surface must not
    /// report this as work still outstanding.
    #[test]
    fn rekey_on_an_untouched_rail_is_nothing_stored() {
        let successor = DraftsClient::new(FakeDraftsNest::default(), &keypair());
        assert_eq!(
            block_on(successor.rekey_rail_from_predecessors("events", &[predecessor_key()]))
                .unwrap(),
            DraftsRekeyOutcome::NothingStored
        );
    }

    /// ⚠ The arm that must never destroy: this device holds no key that opens
    /// the rail (the user succeeded elsewhere). The bytes must be left exactly
    /// as they are — a pass that sealed its own empty view here would eat the
    /// user's drafts and report success.
    #[test]
    fn rekey_without_the_opening_key_leaves_the_bytes_alone() {
        let shared = std::sync::Arc::new(FakeDraftsNest::default());
        let predecessor = DraftsClient::new(SharedNest(shared.clone()), &predecessor_keypair());
        let successor = DraftsClient::new(SharedNest(shared.clone()), &keypair());
        block_on(predecessor.save("conversations", b"only the other device can open this"))
            .unwrap();
        let before = shared.stored.lock().unwrap().get("conversations").cloned();

        let stranger = backup_key_from_seed(ActorKeypair::from_secret([77u8; 32]).secret_bytes());
        let outcome =
            block_on(successor.rekey_rail_from_predecessors("conversations", &[stranger])).unwrap();

        assert_eq!(outcome, DraftsRekeyOutcome::NoKeyOpensIt);
        assert_eq!(
            shared.stored.lock().unwrap().get("conversations").cloned(),
            before,
            "the rail must be byte-identical after a pass that could not open it"
        );
        // And the predecessor's own device can still open what it wrote.
        assert_eq!(
            block_on(predecessor.load("conversations"))
                .unwrap()
                .as_deref(),
            Some(b"only the other device can open this".as_slice())
        );
    }

    /// Predecessors are tried in order and the first that opens the rail wins —
    /// a twice-succeeded chain, or a phrase-only restore that wrote several
    /// recovered predecessors back as registry rows, hands this a list.
    #[test]
    fn rekey_tries_every_predecessor_offered() {
        let shared = std::sync::Arc::new(FakeDraftsNest::default());
        let predecessor = DraftsClient::new(SharedNest(shared.clone()), &predecessor_keypair());
        let successor = DraftsClient::new(SharedNest(shared.clone()), &keypair());
        block_on(predecessor.save("events", b"two successions ago")).unwrap();

        let decoy = backup_key_from_seed(ActorKeypair::from_secret([55u8; 32]).secret_bytes());
        let outcome =
            block_on(successor.rekey_rail_from_predecessors("events", &[decoy, predecessor_key()]))
                .unwrap();

        assert_eq!(outcome, DraftsRekeyOutcome::Rekeyed);
        assert_eq!(
            block_on(successor.load("events")).unwrap().as_deref(),
            Some(b"two successions ago".as_slice())
        );
    }

    /// Newtype so two `DraftsClient`s can share one `FakeDraftsNest` (the
    /// blocking-mutex slot) behind an `Arc` while each owns its `R`.
    struct SharedNest(std::sync::Arc<FakeDraftsNest>);
    impl RpcRequester for SharedNest {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.0.request(kind, payload).await
        }
    }

    /// Always answers `fauna.drafts.get` with a fixed, never-sealed-by-this-key
    /// blob — the "present but undecryptable" case `load`'s doc comment
    /// promises is a hard error, never silently masked as "no drafts" (which
    /// would let the next `save` clobber the user's real, just-unreadable
    /// drafts). No test exercised this branch before.
    struct GarbageNest;
    impl RpcRequester for GarbageNest {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, KIND_GET, "only load() should be exercised here");
            // Correct version byte + long enough to pass the length/version
            // checks, but not a real ChaCha20-Poly1305 ciphertext — fails at
            // AEAD decryption (wrong key or tampered data), never panics.
            let reply = fauna_protocol::encode_canonical(&GetDraftsReply {
                blob: Some(ByteBuf::from(vec![0x01u8; 40])),
                extra: Default::default(),
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[test]
    fn load_on_a_present_but_undecryptable_blob_is_a_hard_error() {
        let client = DraftsClient::new(GarbageNest, &keypair());
        assert!(matches!(
            block_on(client.load("conversations")),
            Err(DraftsClientError::Seal(DraftSealError::Decrypt(_)))
        ));
    }

    #[test]
    fn load_and_save_propagate_transport_error_unchanged() {
        // `load`/`save` must propagate `R::Error` unchanged as
        // `DraftsClientError::Transport` rather than swallowing or remapping
        // it — every fake nest above is `Infallible`, so nothing else in this
        // file exercises this arm.
        let client = DraftsClient::new(FailingRequester::new("transport unreachable"), &keypair());
        assert!(matches!(
            block_on(client.load("conversations")),
            Err(DraftsClientError::Transport(e)) if e.0 == "transport unreachable"
        ));
        assert!(matches!(
            block_on(client.save("conversations", b"x")),
            Err(DraftsClientError::Transport(e)) if e.0 == "transport unreachable"
        ));
    }

    #[test]
    fn display_formats_transport_and_seal_variants() {
        let transport: DraftsClientError<String> =
            DraftsClientError::Transport("transport unreachable".to_string());
        assert_eq!(
            transport.to_string(),
            "drafts transport: transport unreachable"
        );

        let seal: DraftsClientError<String> =
            DraftsClientError::Seal(DraftSealError::Decrypt("bad tag".to_string()));
        assert_eq!(
            seal.to_string(),
            "drafts seal/unseal: unsealing drafts blob failed (wrong key or tampered data): bad tag"
        );
    }
}
