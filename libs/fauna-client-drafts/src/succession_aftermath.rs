//! The `__drafts` leg of the post-succession corpus re-key
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope — the
//! `BackupKey` corpus row names `__drafts` explicitly, and rules the seal
//! re-keys **client-driven and urgent**).
//!
//! [`DraftsClient::rekey_rail_from_predecessors`](crate::DraftsClient::rekey_rail_from_predecessors)
//! moves **one** rail; this module drives **all** of them and projects the
//! result as the progress surface all 7 apps render.
//!
//! # Why the driver enumerates rather than healing on demand
//!
//! The tempting shape is to re-key a rail when someone opens it — the compose
//! surfaces already call `DraftsSync::load` at launch, so the hook is free. That
//! shape is wrong here for the reason the second finding names: a demand-driven
//! heal *looks* built while § Re-key scope's requirement (the aftermath runs
//! "without the user issuing a single command") stays unmet, and it fakes its own
//! test green, because opening the rail under test is what repairs it. A user who
//! never opens the Events composer would carry a predecessor-sealed rail
//! indefinitely, and the seed that opens it is racing device loss.
//!
//! So the pass walks [`DRAFT_RAILS`] itself. That is affordable *only* because
//! the rail vocabulary is closed and nest-validated — three constants on the
//! wire, not a client-chosen key — which is also what makes it correct: there is
//! no rail this pass can fail to know about. Prior scoping treated the missing
//! `fauna.drafts.list` RPC as a blocker needing a wire addition; it is not one,
//! because the enumeration was ratified into the protocol from the start.
//!
//! # What it deliberately does not do
//!
//! It takes no other leg's outcome as a parameter, per the warning against
//! cargo-culting another leg's sequencing: this leg
//! reads no account state at all — it needs the predecessor keys and nothing
//! else. Coupling it to another leg would let an outage there suppress a
//! drafts pass that would have succeeded.

use fauna_core::crypto::BackupKey;
use fauna_core::localized::LocalizedText;
use fauna_core::progress::ProgressOutcome;
use fauna_protocol::RpcRequester;
use fauna_protocol::drafts::DRAFT_RAILS;

use crate::store::{DraftsClient, DraftsClientError, DraftsRekeyOutcome};

/// What [`rekey_drafts_after_succession`] found across every rail.
///
/// Mirrors `fauna_client_mls_sync::ReplicaResealOutcome` arm for arm — the
/// other multi-unit leg — rather than the two-valued shape the single-blob legs
/// use. The `Resealed { owed }` split is the fifth rendered state,
/// *partly-owed*: progress was real **and** the pass is unfinished, which on a
/// multi-rail plane is an ordinary outcome rather than an edge case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftsResealOutcome {
    /// No rail has a blob at rest — a predecessor who never composed anything
    /// anywhere. Nothing is owed, by this device or any other.
    NothingStored,
    /// Every rail that has a blob already opens under the successor's own key.
    /// The idempotent arm: a completed pass, a second device arriving after the
    /// first, or a resumed one. Writes nothing.
    AlreadyCurrent,
    /// At least one rail was opened under a predecessor's key and re-sealed.
    Resealed {
        /// Rails re-sealed in this pass.
        rails: usize,
        /// Rails still sealed to a key this device does not hold, and therefore
        /// **still owed** by another device. Non-zero means progress was made
        /// *and* the pass is unfinished — both facts are true, and collapsing
        /// either one lies to the user.
        owed: usize,
    },
    /// Every rail with a blob is closed to every offered key, and none was
    /// re-sealed. **Not a failure and not necessarily corruption** — the
    /// ordinary state on a device that never held the predecessor's seed (the
    /// user succeeded elsewhere), where the pass is still owed by a device that
    /// did. AEAD cannot distinguish a wrong key from damaged ciphertext, so
    /// genuine corruption lands here too; the safe response to both is to leave
    /// the bytes alone.
    NoKeyOpensIt,
}

/// The `__drafts` re-seal as a **progress surface**, mirroring
/// `ConfigResealProgress`, `BackupRegrantProgress` and `ReplicaResealProgress`
/// arm for arm via the shared [`fauna_core::progress::Passage`] — the
/// projection all 7 apps render, so the copy lives where the outcome lives
/// and no app writes a `match` over the outcome (`succession-aftermath.md`
/// § Re-key scope: the aftermath is "surfaced with progress").
pub type DraftsResealProgress = fauna_core::progress::Passage<DraftsResealOutcome>;

/// i18n keys for [`DraftsResealProgress::status_line`] — `settings.recovery_kit.*`,
/// beside the `NestBackupKey` and `__mls` lines this renders under.
const KEY_DRAFTS_RESEAL_RUNNING: &str = "settings.recovery_kit.drafts_reseal_running";
const KEY_DRAFTS_RESEAL_DONE: &str = "settings.recovery_kit.drafts_reseal_done";
const KEY_DRAFTS_RESEAL_PARTLY_OWED: &str =
    "settings.recovery_kit.drafts_reseal_partly_owed_elsewhere";
const KEY_DRAFTS_RESEAL_OWED_ELSEWHERE: &str = "settings.recovery_kit.drafts_reseal_owed_elsewhere";
const KEY_DRAFTS_RESEAL_FAILED: &str = "settings.recovery_kit.drafts_reseal_failed";

/// Two arms deliberately render nothing ([`ProgressOutcome::settled_line`]
/// returns `None`): `NothingStored` and `AlreadyCurrent` are both "nothing is
/// owed", and a line announcing a no-op at every later sign-in trains the
/// user to ignore the one that matters.
impl ProgressOutcome for DraftsResealOutcome {
    const RUNNING_KEY: &'static str = KEY_DRAFTS_RESEAL_RUNNING;
    const FAILED_KEY: &'static str = KEY_DRAFTS_RESEAL_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            // A partially-applied pass gets its own line rather than the "done"
            // one: it really did recover some drafts, and it really is
            // unfinished. Reporting either half alone would be a lie.
            Self::Resealed { owed, .. } if *owed > 0 => {
                Some(LocalizedText::key(KEY_DRAFTS_RESEAL_PARTLY_OWED))
            }
            Self::Resealed { .. } => Some(LocalizedText::key(KEY_DRAFTS_RESEAL_DONE)),
            Self::NoKeyOpensIt => Some(LocalizedText::key(KEY_DRAFTS_RESEAL_OWED_ELSEWHERE)),
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

/// Re-seal every `__drafts` rail this successor inherited — the aftermath's
/// `__drafts` leg, driven at first successor sign-in and resumed at every
/// sign-in while [`DraftsResealOutcome::still_owed`] holds.
///
/// `predecessors` is *every* retired identity's `BackupKey` in the account
/// registry, not merely the one this session succeeded from: a phrase-only
/// restore writes recovered predecessors back as ordinary registry rows, so a
/// freshly-restored device holds them too, and a twice-succeeded chain leaves
/// more than one.
///
/// **Rails are independent and a failure on one does not abandon the rest.**
/// A transport error is returned (the caller renders
/// [`DraftsResealProgress::Failed`] and retries next sign-in), but a rail this
/// device simply cannot open is counted as `owed` and the walk continues — the
/// user's Conversations drafts should not stay locked because their Events rail
/// was sealed by an identity two successions back.
///
/// Calling it with an empty `predecessors` list is meaningless but harmless: it
/// costs one `get` per rail and reports whatever is already true.
pub async fn rekey_drafts_after_succession<R: RpcRequester>(
    client: &DraftsClient<R>,
    predecessors: &[BackupKey],
) -> Result<DraftsResealOutcome, DraftsClientError<R::Error>> {
    let mut stored = 0usize;
    let mut rails = 0usize;
    let mut owed = 0usize;

    for rail in DRAFT_RAILS {
        match client
            .rekey_rail_from_predecessors(rail, predecessors)
            .await?
        {
            DraftsRekeyOutcome::NothingStored => {}
            DraftsRekeyOutcome::AlreadyCurrent => stored += 1,
            DraftsRekeyOutcome::Rekeyed => {
                stored += 1;
                rails += 1;
            }
            DraftsRekeyOutcome::NoKeyOpensIt => {
                stored += 1;
                owed += 1;
            }
        }
    }

    Ok(match (stored, rails, owed) {
        (0, _, _) => DraftsResealOutcome::NothingStored,
        (_, 0, 0) => DraftsResealOutcome::AlreadyCurrent,
        (_, 0, _) => DraftsResealOutcome::NoKeyOpensIt,
        (_, rails, owed) => DraftsResealOutcome::Resealed { rails, owed },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{backup_key_from_seed, seal_drafts};
    use fauna_client_testkit::block_on;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::drafts::{
        GetDraftsReply, GetDraftsRequest, KIND_GET, KIND_PUT, PutDraftsReply, PutDraftsRequest,
    };
    use serde_bytes::ByteBuf;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Stateful `path → opaque blob` fake nest, shared between the predecessor's
    /// and successor's clients so the pass runs against real sealed bytes.
    #[derive(Default)]
    struct FakeNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl RpcRequester for &FakeNest {
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
                        fauna_protocol::decode_strict(&bytes).expect("decode put");
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
                        fauna_protocol::decode_strict(&bytes).expect("decode get");
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

    fn successor() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }

    fn key_of(seed: u8) -> BackupKey {
        backup_key_from_seed(ActorKeypair::from_secret([seed; 32]).secret_bytes())
    }

    /// Lay a rail down sealed under `key`, bypassing any client so the fixture
    /// cannot accidentally seal under the identity under test.
    fn seed_rail(nest: &FakeNest, rail: &str, key: &BackupKey, plaintext: &[u8]) {
        nest.stored
            .lock()
            .unwrap()
            .insert(rail.into(), seal_drafts(plaintext, key).unwrap());
    }

    /// The whole point of the driver: a rail the user never opens is re-sealed
    /// anyway. `events` is seeded and never read through a compose surface —
    /// if this leg were a heal-on-open hook, this assertion could not exist.
    #[test]
    fn the_pass_reaches_every_rail_including_ones_never_opened() {
        let nest = FakeNest::default();
        let old = key_of(21);
        seed_rail(&nest, "conversations", &old, b"half-written reply");
        seed_rail(&nest, "posts", &old, b"half-written post");
        seed_rail(&nest, "events", &old, b"half-written invitation");

        let client = DraftsClient::new(&nest, &successor());
        let outcome = block_on(rekey_drafts_after_succession(&client, &[old])).unwrap();

        assert_eq!(outcome, DraftsResealOutcome::Resealed { rails: 3, owed: 0 });
        assert!(!outcome.still_owed());
        for (rail, expected) in [
            ("conversations", b"half-written reply".as_slice()),
            ("posts", b"half-written post".as_slice()),
            ("events", b"half-written invitation".as_slice()),
        ] {
            assert_eq!(
                block_on(client.load(rail)).unwrap().as_deref(),
                Some(expected),
                "{rail} did not survive the pass"
            );
        }
    }

    /// A rail count is not a rail *set*: the pass must not stop at the first
    /// rail it cannot open. Conversations is sealed by an identity this device
    /// does not hold; posts must still be recovered.
    #[test]
    fn a_rail_this_device_cannot_open_does_not_abandon_the_others() {
        let nest = FakeNest::default();
        let held = key_of(21);
        let unheld = key_of(66);
        seed_rail(
            &nest,
            "conversations",
            &unheld,
            b"sealed two successions ago",
        );
        seed_rail(&nest, "posts", &held, b"recoverable right here");
        // Snapshot the actual bytes: the seal draws a fresh AEAD nonce every
        // time, so re-sealing the same plaintext is NOT a valid comparison.
        let untouchable = nest.stored.lock().unwrap().get("conversations").cloned();

        let client = DraftsClient::new(&nest, &successor());
        let outcome = block_on(rekey_drafts_after_succession(&client, &[held])).unwrap();

        assert_eq!(
            outcome,
            DraftsResealOutcome::Resealed { rails: 1, owed: 1 },
            "progress was real AND the pass is unfinished — both must survive"
        );
        assert!(outcome.still_owed());
        assert_eq!(
            block_on(client.load("posts")).unwrap().as_deref(),
            Some(b"recoverable right here".as_slice())
        );
        // And the rail it could not open is untouched, not emptied.
        assert_eq!(
            nest.stored.lock().unwrap().get("conversations").cloned(),
            untouchable,
            "an unopenable rail must be left byte-identical"
        );
        // Byte-identity alone would also hold if the pass had never run, so
        // pin the meaning too: the device that *does* hold that key still
        // opens the rail afterwards.
        let other_device = DraftsClient::new(&nest, &ActorKeypair::from_secret([66u8; 32]));
        assert_eq!(
            block_on(other_device.load("conversations"))
                .unwrap()
                .as_deref(),
            Some(b"sealed two successions ago".as_slice())
        );
    }

    /// The partly-owed outcome renders the unfinished line, never the done one.
    /// This is the assertion the leg-3 finding bought: a multi-unit pass that
    /// reported "done" while work remained would strand the user with no reason
    /// to sign in on the device that can finish it.
    #[test]
    fn a_partly_owed_pass_renders_the_unfinished_line_not_done() {
        let progress =
            DraftsResealProgress::Settled(DraftsResealOutcome::Resealed { rails: 1, owed: 2 });
        assert_eq!(
            progress
                .status_line()
                .expect("a partly-owed pass must render")
                .key,
            KEY_DRAFTS_RESEAL_PARTLY_OWED
        );
        assert!(progress.still_owed());
    }

    /// Every rail closed to us is the *owed-elsewhere* arm, not partly-owed:
    /// nothing moved, so the copy must name the one fix (sign in where you took
    /// the account back) rather than claiming partial progress.
    #[test]
    fn no_rail_opening_is_owed_elsewhere() {
        let nest = FakeNest::default();
        let unheld = key_of(66);
        seed_rail(&nest, "conversations", &unheld, b"nope");
        seed_rail(&nest, "posts", &unheld, b"also nope");

        let client = DraftsClient::new(&nest, &successor());
        let outcome = block_on(rekey_drafts_after_succession(&client, &[key_of(21)])).unwrap();

        assert_eq!(outcome, DraftsResealOutcome::NoKeyOpensIt);
        assert_eq!(
            DraftsResealProgress::Settled(outcome)
                .status_line()
                .expect("owed-elsewhere must render")
                .key,
            KEY_DRAFTS_RESEAL_OWED_ELSEWHERE
        );
    }

    /// A predecessor who never composed. Distinct from every other arm because
    /// nothing is owed by *any* device — and it must render nothing at all.
    #[test]
    fn a_predecessor_who_never_composed_renders_nothing() {
        let nest = FakeNest::default();
        let client = DraftsClient::new(&nest, &successor());
        let outcome = block_on(rekey_drafts_after_succession(&client, &[key_of(21)])).unwrap();

        assert_eq!(outcome, DraftsResealOutcome::NothingStored);
        assert!(!outcome.still_owed());
        assert!(
            DraftsResealProgress::Settled(outcome)
                .status_line()
                .is_none()
        );
    }

    /// The idempotent arm across the whole plane: a second pass writes nothing
    /// and renders nothing. Silence is the requirement — a "your drafts are
    /// unlocked" banner at every later sign-in trains the user to ignore the
    /// one that matters.
    #[test]
    fn a_second_pass_is_silent() {
        let nest = FakeNest::default();
        let old = key_of(21);
        seed_rail(&nest, "posts", &old, b"inherited");

        let client = DraftsClient::new(&nest, &successor());
        let preds = std::slice::from_ref(&old);
        block_on(rekey_drafts_after_succession(&client, preds)).unwrap();
        let second = block_on(rekey_drafts_after_succession(&client, preds)).unwrap();

        assert_eq!(second, DraftsResealOutcome::AlreadyCurrent);
        assert!(!second.still_owed());
        assert!(
            DraftsResealProgress::Settled(second)
                .status_line()
                .is_none()
        );
    }

    /// A rail with nothing stored must not be counted as "already current" —
    /// otherwise a predecessor who composed nowhere would report the same as
    /// one whose rails were all recovered, and the two mean different things to
    /// the resume condition.
    #[test]
    fn empty_rails_do_not_mask_a_real_recovery() {
        let nest = FakeNest::default();
        let old = key_of(21);
        seed_rail(&nest, "events", &old, b"only the events rail was ever used");

        let client = DraftsClient::new(&nest, &successor());
        let outcome = block_on(rekey_drafts_after_succession(&client, &[old])).unwrap();

        assert_eq!(
            outcome,
            DraftsResealOutcome::Resealed { rails: 1, owed: 0 },
            "two empty rails must not dilute the one that moved"
        );
        assert_eq!(
            DraftsResealProgress::Settled(outcome)
                .status_line()
                .expect("a real recovery must render")
                .key,
            KEY_DRAFTS_RESEAL_DONE
        );
    }

    /// `Running` and `Failed` both count as owed — a pass in flight or one that
    /// never reached the nest has settled nothing, so the caller must retry.
    #[test]
    fn running_and_failed_are_both_still_owed() {
        assert!(DraftsResealProgress::Running.still_owed());
        let failed = DraftsResealProgress::Failed("nest unreachable".into());
        assert!(failed.still_owed());
        let line = failed.status_line().expect("a failure must render");
        assert_eq!(line.key, KEY_DRAFTS_RESEAL_FAILED);
        assert_eq!(
            line.args.get("reason").map(String::as_str),
            Some("nest unreachable"),
            "the failure line must name why, or it is unactionable"
        );
    }
}
