//! The post-succession **aftermath**'s `__mls` leg — re-sealing the openMLS
//! state replica from a predecessor's `BackupKey` to the successor's
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope, the `BackupKey`
//! corpus row: the seal "re-keys **client-driven and urgent** — the re-seal
//! races device loss").
//!
//! # What is actually broken until this runs
//!
//! The succession transaction re-points *ownership* of the reserved `__mls`
//! folder (§ Re-key scope's ownership blockquote), so the successor fetches
//! these blobs by ordinary authenticated reads. What it cannot do is re-key the
//! **seal**: the bytes stay encrypted under the retired identity's `BackupKey`.
//! So the successor gets its replica back as ciphertext it holds no key for, and
//! [`MlsReplicaClient::load_provider_with_base`] fails at the unseal — which is
//! the whole conversations plane, since the provider snapshot *is* the openMLS
//! crypto state every group's decryption depends on.
//!
//! That is the same "stuck, not corrupt" state every sealed-plane re-seal leg repairs, and
//! it is urgent for the same reason: the predecessor seed that opens these bytes
//! lives only in the account registry on the user's own devices (the succession
//! transaction deleted the old escrow row), so losing every device before the
//! re-seal leaves the corpus sealed to a key that exists nowhere.
//!
//! # Why the enumeration needs no list RPC
//!
//! `__mls` has no list kind, which made "what do we re-seal?" look like it
//! needed a wire addition. It does not: the replica is **self-describing**. The
//! `provider` blob sits at a constant path ([`PATH_PROVIDER`]) and
//! [`ProviderReplica::channel_ids`] lists every channel whose history slice can
//! exist, exactly as [`crate::MlsStateSync::load`] already enumerates them at
//! restore.
//!
//! The consequence is that the ordering here is **intrinsic, not a convention**:
//! the provider must be readable before the history paths are even nameable, so
//! this pass re-seals `provider` first and derives the rest from what it just
//! opened. Nothing enforces that with a parameter because nothing can violate
//! it — the channel list does not exist until the provider is open.

use fauna_core::crypto::BackupKey;
use fauna_core::localized::LocalizedText;
use fauna_core::progress::ProgressOutcome;
use fauna_mls::state_replica::ProviderReplica;
use zeroize::Zeroizing;

use crate::seal::{seal_replica, unseal_replica};
use crate::store::{
    ClientResult, MlsReplicaClient, MlsReplicaClientError, PATH_PROVIDER, PutOutcome, base_of,
    history_path,
};

/// What [`MlsReplicaClient::reseal_from_predecessors`] found. Four values for
/// the same reason `DraftsRekeyOutcome` has four: three of them mean "no write
/// happened", and a progress surface that collapsed them would report success
/// while the user's conversations stay sealed to a retired key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaResealOutcome {
    /// No `provider` blob at rest — a predecessor that never synced a replica.
    /// Nothing is owed, by this device or any other. History slices cannot
    /// exist without a provider to name them, so this is genuinely empty.
    NothingStored,
    /// The provider **and** every history slice already open under the
    /// successor's own key. The idempotent arm: a completed pass, or a second
    /// device arriving after the first. Writes nothing.
    AlreadyCurrent,
    /// At least one path was opened under a predecessor's key and re-sealed
    /// under the successor's.
    Resealed {
        /// Whether the `provider` snapshot itself was re-sealed (`false` when a
        /// previous pass had already done it and only history slices remained —
        /// see [`MlsReplicaClient::reseal_from_predecessors`] on why the check
        /// is two-halved).
        provider: bool,
        /// History slices re-sealed in this pass.
        histories: usize,
        /// History slices still sealed to a key this device does not hold, and
        /// therefore **still owed** by another device. Non-zero means progress
        /// was made *and* the pass is unfinished — both facts are true, and
        /// collapsing either one lies to the user.
        owed: usize,
    },
    /// The `provider` blob is present and no offered key opens it. **Not a
    /// failure and not necessarily corruption** — the ordinary state on a
    /// device that never held the predecessor's seed, where the pass is still
    /// owed by a device that did. Nothing downstream is reachable either, since
    /// the channel list lives inside the blob we could not open.
    NoKeyOpensIt,
}

/// The `__mls` re-seal as a **progress surface**, mirroring
/// `ConfigResealProgress` and `BackupRegrantProgress` arm for arm via the
/// shared [`fauna_core::progress::Passage`] — the projection all 7 apps
/// render, so the copy lives where the outcome lives and no app writes a
/// `match` over the outcome (`succession-aftermath.md` § Re-key scope: the
/// aftermath is "surfaced with progress").
///
/// It lives in this crate rather than beside the other two legs' projections in
/// `fauna-client-config` for a layering reason worth stating: that crate is
/// deliberately wasm-clean and light, and depending on this one would drag
/// openMLS into the SPA build. The ratified rule points the same way — project
/// the copy where the *outcome* lives.
pub type ReplicaResealProgress = fauna_core::progress::Passage<ReplicaResealOutcome>;

/// i18n keys for [`ReplicaResealProgress::status_line`] — `settings.recovery_kit.*`,
/// beside the `NestBackupKey` re-grant lines this renders under.
const KEY_MLS_RESEAL_RUNNING: &str = "settings.recovery_kit.mls_reseal_running";
const KEY_MLS_RESEAL_DONE: &str = "settings.recovery_kit.mls_reseal_done";
const KEY_MLS_RESEAL_PARTLY_OWED: &str = "settings.recovery_kit.mls_reseal_partly_owed_elsewhere";
const KEY_MLS_RESEAL_OWED_ELSEWHERE: &str = "settings.recovery_kit.mls_reseal_owed_elsewhere";
const KEY_MLS_RESEAL_FAILED: &str = "settings.recovery_kit.mls_reseal_failed";

/// Two arms deliberately render nothing ([`ProgressOutcome::settled_line`]
/// returns `None`): `NothingStored` and `AlreadyCurrent` are both "nothing is
/// owed", and a line announcing a no-op at every later sign-in trains the
/// user to ignore the one that matters.
impl ProgressOutcome for ReplicaResealOutcome {
    const RUNNING_KEY: &'static str = KEY_MLS_RESEAL_RUNNING;
    const FAILED_KEY: &'static str = KEY_MLS_RESEAL_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            // A partially-applied pass gets its own line rather than the "done"
            // one: it really did unlock some conversations, and it really is
            // unfinished. Reporting either half alone would be a lie.
            Self::Resealed { owed, .. } if *owed > 0 => {
                Some(LocalizedText::key(KEY_MLS_RESEAL_PARTLY_OWED))
            }
            Self::Resealed { .. } => Some(LocalizedText::key(KEY_MLS_RESEAL_DONE)),
            Self::NoKeyOpensIt => Some(LocalizedText::key(KEY_MLS_RESEAL_OWED_ELSEWHERE)),
            Self::NothingStored | Self::AlreadyCurrent => None,
        }
    }

    /// Whether the pass is still owed by *someone* — this device on a retry, or
    /// another device that holds the retired seed. A partially-applied pass
    /// ([`Self::Resealed`] with `owed > 0`) counts as owed even though it made
    /// progress.
    fn still_owed(&self) -> bool {
        match self {
            Self::NothingStored | Self::AlreadyCurrent => false,
            Self::NoKeyOpensIt => true,
            Self::Resealed { owed, .. } => *owed > 0,
        }
    }
}

impl MlsReplicaClient {
    /// Re-seal this actor's `__mls` replica from any predecessor key that opens
    /// it — the aftermath's `__mls` leg.
    ///
    /// `predecessors` is *every* retired identity's `BackupKey` the account
    /// registry can resolve on this device, not merely the one this session
    /// succeeded from: a restore writes recovered predecessors back as ordinary
    /// registry rows, and a twice-succeeded chain can leave more than one. They
    /// are tried in order and the first that opens a blob wins.
    ///
    /// **Safe to call unconditionally, on every device, at every sign-in.** The
    /// idempotent arm costs one `get` and writes nothing, which is what makes
    /// the pass resumable with no progress state at rest — the corpus is its own
    /// progress record.
    ///
    /// # Why the idempotency check is two-halved
    ///
    /// A pass can crash (or lose its connection) between the provider write and
    /// the history writes, so "the provider opens under our key" does **not**
    /// imply the replica is done. Keying on the provider alone would report
    /// `AlreadyCurrent` forever while every channel's history stayed sealed to
    /// the retired key — a silent, permanent half-migration. So a provider that
    /// is already ours does not short-circuit: the pass still enumerates the
    /// channels and checks each slice.
    ///
    /// # Concurrency
    ///
    /// Each write is a raw CAS put against the base the read observed. A
    /// concurrent device that re-sealed the same path first wins the race and
    /// this pass leaves that path alone — the bytes are already under the
    /// successor's key, which is the outcome either device wanted. This
    /// deliberately does **not** merge: the plaintext is unchanged by a re-seal,
    /// so there is no divergence to reconcile, and running the three-way merge
    /// here would risk writing back a *stale* decoded replica in the name of a
    /// key change.
    pub async fn reseal_from_predecessors(
        &self,
        predecessors: &[BackupKey],
    ) -> ClientResult<ReplicaResealOutcome> {
        let Some(sealed) = self.get_blob(PATH_PROVIDER).await? else {
            return Ok(ReplicaResealOutcome::NothingStored);
        };

        // Ours already? Then we can read the channel list without writing —
        // but we must still check every history slice (see the doc comment).
        let (provider_plain, provider_resealed) = match unseal_replica(&sealed, self.key()) {
            Ok(plain) => (plain, false),
            Err(_) => {
                let Some((plain, resealed)) = self
                    .reseal_path(PATH_PROVIDER, &sealed, predecessors)
                    .await?
                else {
                    // Nothing opens the provider, so the channel list — and with
                    // it every history path — is unreachable from this device.
                    return Ok(ReplicaResealOutcome::NoKeyOpensIt);
                };
                (plain, resealed)
            }
        };

        let provider = ProviderReplica::from_bytes(&provider_plain)
            .map_err(|e| MlsReplicaClientError::Codec(e.to_string()))?;

        let mut histories = 0usize;
        let mut owed = 0usize;
        let channel_ids = provider.channel_ids();
        let channels = channel_ids.len();
        for channel in channel_ids {
            let path = history_path(&channel.to_string());
            let Some(sealed) = self.get_blob(&path).await? else {
                continue; // a channel with nothing folded yet
            };
            if unseal_replica(&sealed, self.key()).is_ok() {
                continue; // this slice is already ours
            }
            match self.reseal_path(&path, &sealed, predecessors).await? {
                Some(_) => histories += 1,
                None => owed += 1,
            }
        }

        // The three inputs the outcome is computed from, logged before it is
        // collapsed into one. `AlreadyCurrent` is the arm that most needs this:
        // it carries no fields, paints no line, and is reached by two very
        // different states that matter enormously to a reader —
        //
        //   * `channels > 0`: every slice was already ours, so the pass really
        //     had nothing left to do and the replica is whole;
        //   * `channels == 0`: the loop never executed at all, so *nothing was
        //     examined* — the provider at rest carries no channels, and any
        //     test asserting "the conversations came back" off the back of this
        //     arm is vacuously green.
        //
        // `provider_ours` is the other half a reader cannot otherwise get:
        // false means a predecessor key was needed, true means the blob at rest
        // already opened under this actor's own key — which for a successor's
        // FIRST load is a surprise worth seeing, since it means something wrote
        // it after the succession.
        tracing::info!(
            provider_ours = !provider_resealed,
            channels,
            histories,
            owed,
            "the __mls re-seal pass examined the replica"
        );
        if !provider_resealed && histories == 0 && owed == 0 {
            return Ok(ReplicaResealOutcome::AlreadyCurrent);
        }
        Ok(ReplicaResealOutcome::Resealed {
            provider: provider_resealed,
            histories,
            owed,
        })
    }

    /// Open `sealed` with the first predecessor key that works, re-seal it under
    /// this actor's key, and CAS it back to `path`.
    ///
    /// Returns the plaintext plus whether a write actually landed, or `None`
    /// when no offered key opens the bytes. A wrong key is the expected case
    /// here, not an error to propagate — AEAD cannot distinguish a wrong key
    /// from damaged ciphertext, and the only safe response to either is to leave
    /// the bytes exactly as they are.
    async fn reseal_path(
        &self,
        path: &str,
        sealed: &[u8],
        predecessors: &[BackupKey],
    ) -> ClientResult<Option<(Zeroizing<Vec<u8>>, bool)>> {
        let base = base_of(&Some(sealed.to_vec()));
        for key in predecessors {
            let Ok(plain) = unseal_replica(sealed, key) else {
                continue;
            };
            let resealed = seal_replica(&plain, self.key()).map_err(MlsReplicaClientError::Seal)?;
            // A conflict means another device wrote this path between our read
            // and our put. On a re-seal that is a *win*, not a retry: the only
            // write anyone is making here is the same key change.
            let landed = matches!(
                self.put_blob(path, resealed, base).await?,
                PutOutcome::Stored
            );
            return Ok(Some((plain, landed)));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MlsReplicaTransport, MlsTransportError};
    use crate::test_conv::block_on;
    use async_trait::async_trait;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::engine::MlsEngine;
    use fauna_protocol::mls_replica::ReplicaBase;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory `path → sealed blob` nest enforcing the same CAS contract as
    /// the real handler, so the re-seal's base handling is exercised rather than
    /// assumed. Counts puts so an idempotent pass can be asserted to write
    /// *nothing* (the check that a collapsed idempotency arm would fail).
    #[derive(Default)]
    struct FakeNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
        puts: Mutex<u32>,
    }

    struct Nest(Arc<FakeNest>);

    #[async_trait]
    impl MlsReplicaTransport for Nest {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            Ok(self.0.stored.lock().unwrap().get(&path).cloned())
        }
        async fn put(
            &self,
            path: String,
            blob: Vec<u8>,
            base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            let mut map = self.0.stored.lock().unwrap();
            let current = map.get(&path).map(|b| *blake3::hash(b).as_bytes());
            let ok = match base {
                ReplicaBase::Absent => current.is_none(),
                ReplicaBase::Hash(h) => current == Some(h),
            };
            if !ok {
                return Ok(PutOutcome::Conflict);
            }
            *self.0.puts.lock().unwrap() += 1;
            map.insert(path, blob);
            Ok(PutOutcome::Stored)
        }
    }

    fn keypair(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    fn key(seed: u8) -> BackupKey {
        crate::seal::backup_key_from_seed(keypair(seed).secret_bytes())
    }

    fn client(nest: &Arc<FakeNest>, seed: u8) -> MlsReplicaClient {
        MlsReplicaClient::new(Box::new(Nest(Arc::clone(nest))), &keypair(seed))
    }

    /// Seal `plain` under `seed`'s key and store it at `path`, as the
    /// predecessor's device would have left it.
    fn store_sealed(nest: &Arc<FakeNest>, path: &str, plain: &[u8], seed: u8) {
        let sealed = seal_replica(plain, &key(seed)).unwrap();
        nest.stored.lock().unwrap().insert(path.to_string(), sealed);
    }

    /// A real encoded `ProviderReplica` — with one bound channel when
    /// `with_channel`, so `channel_ids()` yields a history path to enumerate.
    fn provider_bytes(with_channel: bool) -> (Vec<u8>, Option<String>) {
        let alice = MlsEngine::new_in_memory(keypair(1)).unwrap();
        let mut channel_hex = None;
        if with_channel {
            let bob = MlsEngine::new_in_memory(keypair(2)).unwrap();
            let bob_kps = bob.generate_key_packages(1).unwrap();
            let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
            channel_hex = Some(channel.to_string());
        }
        (
            ProviderReplica::from_engine(&alice).to_bytes().unwrap(),
            channel_hex,
        )
    }

    #[test]
    fn a_provider_sealed_by_a_predecessor_is_resealed_and_then_opens() {
        let nest = Arc::new(FakeNest::default());
        let (bytes, _) = provider_bytes(false);
        store_sealed(&nest, PATH_PROVIDER, &bytes, 1);

        let successor = client(&nest, 7);
        let outcome = block_on(successor.reseal_from_predecessors(&[key(1)])).unwrap();

        assert_eq!(
            outcome,
            ReplicaResealOutcome::Resealed {
                provider: true,
                histories: 0,
                owed: 0
            }
        );
        assert!(!outcome.still_owed());
        // The successor's ordinary load path — the thing that was broken —
        // now works, which is the whole point of the leg.
        assert!(block_on(successor.load_provider_with_base()).is_ok());
    }

    #[test]
    fn a_replica_already_under_our_own_key_writes_nothing() {
        let nest = Arc::new(FakeNest::default());
        let (bytes, _) = provider_bytes(false);
        store_sealed(&nest, PATH_PROVIDER, &bytes, 7);

        let successor = client(&nest, 7);
        let outcome = block_on(successor.reseal_from_predecessors(&[key(1)])).unwrap();

        assert_eq!(outcome, ReplicaResealOutcome::AlreadyCurrent);
        assert!(!outcome.still_owed());
        assert_eq!(
            *nest.puts.lock().unwrap(),
            0,
            "the idempotent arm must not write"
        );
    }

    #[test]
    fn no_provider_at_rest_is_nothing_stored() {
        let nest = Arc::new(FakeNest::default());
        let outcome = block_on(client(&nest, 7).reseal_from_predecessors(&[key(1)])).unwrap();
        assert_eq!(outcome, ReplicaResealOutcome::NothingStored);
        assert!(!outcome.still_owed());
    }

    #[test]
    fn a_provider_no_offered_key_opens_is_owed_elsewhere_and_untouched() {
        let nest = Arc::new(FakeNest::default());
        let (bytes, _) = provider_bytes(false);
        store_sealed(&nest, PATH_PROVIDER, &bytes, 1);
        let before = nest.stored.lock().unwrap().get(PATH_PROVIDER).cloned();

        // Offered a stranger's key (and an empty slice is equally meaningful).
        let outcome = block_on(client(&nest, 7).reseal_from_predecessors(&[key(9)])).unwrap();

        assert_eq!(outcome, ReplicaResealOutcome::NoKeyOpensIt);
        assert!(outcome.still_owed(), "another device still owes this pass");
        assert_eq!(
            nest.stored.lock().unwrap().get(PATH_PROVIDER).cloned(),
            before,
            "bytes we cannot open must be left exactly as they are"
        );
    }

    /// The two-halved idempotency check. A pass that crashed between the
    /// provider write and the history writes leaves exactly this state; keying
    /// "done" on the provider alone reports `AlreadyCurrent` forever and strands
    /// every channel's history under the retired key.
    #[test]
    fn a_history_slice_is_resealed_even_when_the_provider_already_is() {
        let nest = Arc::new(FakeNest::default());
        let (bytes, channel_hex) = provider_bytes(true);
        let channel_hex = channel_hex.expect("a bound channel");
        // Provider: already ours (the crashed pass got this far).
        store_sealed(&nest, PATH_PROVIDER, &bytes, 7);
        // History: still the predecessor's. Opaque bytes — the re-seal never
        // decodes a history slice, only re-keys it.
        let history = history_path(&channel_hex);
        store_sealed(&nest, &history, b"a channel's folded history", 1);

        let successor = client(&nest, 7);
        let outcome = block_on(successor.reseal_from_predecessors(&[key(1)])).unwrap();

        assert_eq!(
            outcome,
            ReplicaResealOutcome::Resealed {
                provider: false,
                histories: 1,
                owed: 0
            }
        );
        let sealed = nest.stored.lock().unwrap().get(&history).cloned().unwrap();
        assert_eq!(
            unseal_replica(&sealed, &key(7)).unwrap().as_slice(),
            b"a channel's folded history",
            "the slice must now open under the successor's key"
        );
    }

    #[test]
    fn a_history_slice_no_key_opens_is_counted_as_still_owed() {
        let nest = Arc::new(FakeNest::default());
        let (bytes, channel_hex) = provider_bytes(true);
        let channel_hex = channel_hex.expect("a bound channel");
        store_sealed(&nest, PATH_PROVIDER, &bytes, 1);
        // Sealed by a *different* retired identity whose seed this device lacks.
        store_sealed(&nest, &history_path(&channel_hex), b"folded", 9);

        let outcome = block_on(client(&nest, 7).reseal_from_predecessors(&[key(1)])).unwrap();

        assert_eq!(
            outcome,
            ReplicaResealOutcome::Resealed {
                provider: true,
                histories: 0,
                owed: 1
            }
        );
        assert!(
            outcome.still_owed(),
            "progress was made AND the pass is unfinished — both are true"
        );
    }

    /// The two silent arms. A line at every later sign-in announcing that
    /// nothing happened trains the user to ignore the one that matters.
    #[test]
    fn the_nothing_owed_arms_render_nothing() {
        for outcome in [
            ReplicaResealOutcome::NothingStored,
            ReplicaResealOutcome::AlreadyCurrent,
        ] {
            assert_eq!(
                ReplicaResealProgress::Settled(outcome).status_line(),
                None,
                "{outcome:?} must render nothing"
            );
        }
    }

    /// A partially-applied pass must NOT render the "done" copy. Progress was
    /// real and the pass is unfinished; the done line would tell a user their
    /// conversations are all unlocked while some still are not.
    #[test]
    fn a_partly_owed_pass_renders_the_unfinished_line_not_done() {
        let partly = ReplicaResealProgress::Settled(ReplicaResealOutcome::Resealed {
            provider: true,
            histories: 2,
            owed: 1,
        });
        let done = ReplicaResealProgress::Settled(ReplicaResealOutcome::Resealed {
            provider: true,
            histories: 2,
            owed: 0,
        });
        assert_eq!(
            partly.status_line().unwrap().key,
            KEY_MLS_RESEAL_PARTLY_OWED
        );
        assert_eq!(done.status_line().unwrap().key, KEY_MLS_RESEAL_DONE);
        assert!(partly.still_owed());
        assert!(!done.still_owed());
    }

    /// `Failed` settles nothing, so the pass is still owed and retries next
    /// sign-in — the same rule the other two legs' projections follow.
    #[test]
    fn a_failed_pass_is_still_owed_and_names_the_reason() {
        let failed = ReplicaResealProgress::Failed("nest unreachable".into());
        assert!(failed.still_owed());
        let line = failed.status_line().unwrap();
        assert_eq!(line.key, KEY_MLS_RESEAL_FAILED);
        assert_eq!(
            line.args.get("reason").map(String::as_str),
            Some("nest unreachable")
        );
    }
}
