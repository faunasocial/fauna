//! Kit creation and replacement — the ceremony behind the recovery-kit screen
//! and the Settings/Security re-issue path
//! (`docs/goal/behavior/identity-succession.md` § The RecoveryKey).
//!
//! ## The secret never rests
//!
//! `identity-succession.md:38` is iron-clad: the RecoveryKey secret is
//! displayed once as 64-hex + QR and **never stored on any device, on
//! the account plane, or anywhere the identity seed unlocks**. This module therefore
//! has no persistence seam at all — not an optional one, not a
//! feature-gated one. [`RecoveryKit`] hands the secret to the caller as a
//! zeroizing string it must display and drop. A client that wants to "helpfully
//! remember" it has defeated the plane: a recovery secret the thief of a device
//! can hold is no recovery secret.

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::{
    ChainHead, EscrowChallenge, RecoveryKey, RecoveryKeyRegistration, RecoveryKitQr,
};
use fauna_mls::wrapped_blob::{
    PredecessorSeed, PredecessorsOutcome, seal_seed_escrow_with_predecessors,
    unseal_seed_escrow_with_predecessors,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use zeroize::Zeroizing;

use crate::error::{RecoveryError, Result};
use crate::nest::RecoveryClient;

/// What became of the escrow blob in a kit ceremony.
///
/// Separate from the ceremony's `Result` on purpose. The registration and the
/// escrow put are two calls, and the second can fail after the first has
/// landed — at which point the nest has already **deleted** the old escrow row
/// (it deletes on any registration that changes the registered pubkey,
/// `identity-succession.md:51`). Collapsing that into `Err` would throw away
/// the freshly minted secret, which exists in exactly one place: the value
/// being returned. So the kit always comes back, and the escrow outcome rides
/// alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscrowOutcome {
    /// The blob landed. Unix seconds the nest recorded it at.
    Stored { updated_at: i64 },
    /// The seal or the put failed. **The kit is still fully valid** — it is
    /// registered and authorizes succession; what is unavailable until a re-put
    /// is phrase-only restore after total device loss. The surface must say so
    /// and offer the retry, never silently imply the account is protected.
    Failed { reason: String },
}

impl EscrowOutcome {
    pub fn is_stored(&self) -> bool {
        matches!(self, Self::Stored { .. })
    }
}

/// Turn an account registry's predecessor rows into the slice every
/// escrow-writing ceremony must carry — `(actor_id_hex, seed)` pairs in, decoded
/// [`PredecessorSeed`]s out.
///
/// **This is the one definition of that decode, and it is shared for a reason
/// the type signature cannot show.** [`create_kit`]'s `predecessors` is a
/// *parameter* rather than an internal default precisely so the site that must
/// be non-empty cannot silently pass nothing — but that only helps if every
/// caller builds the slice the same way, and the interesting behaviour here is
/// the **skip**: a row whose actor id will not hex-decode is dropped rather than
/// failing the ceremony, because a kit with an incomplete predecessor section
/// beats no kit at all (§ Seed escrow). Two apps had already written that
/// judgment out by hand — `apps/fauna-tui/src/session.rs` and
/// `apps/fauna-linux/src/client.rs`, identically — and the FFI apps' leg would
/// have been the third. A second copy of a rule whose failure mode is *silent*
/// (a blob written without seeds it should have carried re-opens the device-loss
/// race the escrow backstop closes) is exactly the copy that drifts unnoticed.
///
/// Takes the rows rather than the registry: this crate keeps its
/// no-persistence-seam rule, so the caller does the read
/// (`AccountRegistry::predecessor_seeds`) and this does the decode.
///
/// An empty input yields an empty slice — honest for an identity that never
/// succeeded, and the *only* case where passing `&[]` onward is correct.
pub fn predecessor_seeds_from_rows(
    rows: impl IntoIterator<Item = (String, [u8; 32])>,
) -> Vec<PredecessorSeed> {
    rows.into_iter()
        .filter_map(|(hex, seed)| {
            let actor_id = fauna_core::hex32::decode(&hex).ok()?;
            Some(PredecessorSeed { actor_id, seed })
        })
        .collect()
}

/// A freshly minted recovery kit — the one-time payload the kit screen renders.
///
/// Holds the only copy of the secret in the process. Both accessors return
/// zeroizing values, and the struct zeroizes on drop.
pub struct RecoveryKit {
    secret_hex: Zeroizing<String>,
    /// The account this kit protects.
    pub actor_id: ActorId,
    /// The Ed25519 public half now registered. Together with [`Self::seq`] it
    /// is the chain head the owner's client mirrors into its signed
    /// `Profile.recovery_head` — via [`Self::chain_head`], never this half
    /// alone (a pubkey a consumer holds no `seq` for cannot anchor the chain
    /// rewrite/truncation guard) — so peers can verify a succession
    /// from the profile they already cache (`identity-succession.md:40`).
    /// Doing that mirror is the caller's step; this crate does not reach into
    /// the profile plane.
    pub recovery_pubkey: [u8; 32],
    /// The `seq` this registration landed at (1 for a first registration).
    pub seq: u64,
    /// Whether the seed-escrow blob landed alongside it.
    pub escrow: EscrowOutcome,
}

impl RecoveryKit {
    /// The 64-hex recovery secret — display once, never persist.
    pub fn secret_hex(&self) -> &str {
        &self.secret_hex
    }

    /// The chain head this ceremony registered — the value the caller mirrors
    /// into the signed profile (`fauna_client_profile::build_profile_with_recovery_head`)
    /// and the anchor a consumer later passes to
    /// [`crate::succession::resolve_successor`].
    pub fn chain_head(&self) -> ChainHead {
        ChainHead::new(self.recovery_pubkey, self.seq)
    }

    /// The `fauna://recovery` URI to render as a QR code.
    ///
    /// Deliberately its own host, never `fauna://identity`: the two secrets
    /// have different custody rules, and a scanner that took one for the other
    /// would let a user register their identity seed as their own recovery
    /// root, defeating the plane entirely.
    ///
    /// `handle` is the account's handle when the minting surface knows one —
    /// Settings does, and passing it is what lets a later restore find the home
    /// nest without asking (`fauna_core::recovery::RecoveryKitQr::to_uri`). The
    /// ceremony itself never needs it, which is why it is a display argument
    /// rather than a field: at the onboarding kit screen's ratified position no
    /// handle is chosen yet, so that surface passes `None` truthfully.
    pub fn uri(&self, handle: Option<&str>) -> Zeroizing<String> {
        Zeroizing::new(RecoveryKitQr::to_uri(
            &self.secret_hex,
            Some(&hex32(&self.actor_id.0)),
            handle,
        ))
    }
}

/// The `fauna://recovery` URI a Settings kit screen puts behind its QR **and**
/// its copy button — the one builder every app calls (priority #2), for any of
/// the three minting ceremonies (create, replace, lost), from the raw secret the
/// ceremony returned rather than a [`RecoveryKit`] only one arm has.
///
/// `handle` may be the bare local part the session holds or an already
/// qualified `user@host`; it is qualified with `node_url`'s host here, because
/// a bare local part read months later on a device that has never seen this
/// account names nothing (`fauna_core::resolve::qualify_handle`). An empty
/// handle emits no `handle=` param. The on-screen display stays the bare hex —
/// `identity-succession.md` § The RecoveryKey, *Which encoding each affordance
/// carries*.
pub fn kit_display_uri(
    secret_hex: &str,
    actor_id_hex: &str,
    handle: &str,
    node_url: &str,
) -> String {
    let qualified = fauna_core::resolve::qualify_handle(handle, node_url);
    RecoveryKitQr::to_uri(secret_hex, Some(actor_id_hex), qualified.as_deref())
}

impl core::fmt::Debug for RecoveryKit {
    /// Redacted — a kit in a log line or a panic message is exactly the leak
    /// the offline-only rule exists to prevent.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecoveryKit")
            .field("secret_hex", &"<redacted>")
            .field("actor_id", &self.actor_id)
            .field("seq", &self.seq)
            .field("escrow", &self.escrow)
            .finish()
    }
}

/// Create or replace this identity's recovery kit, under RecoveryKey authority.
///
/// Reads the chain head first and picks the arm from what is actually
/// registered, so a caller never has to know:
///
/// - **nothing registered** → a first registration at `seq = 1`; `prior` must
///   be `None`.
/// - **something registered** → an immediate replacement at `seq + 1`,
///   co-signed by the prior kit (`identity-succession.md:42`). `prior` is
///   mandatory here; without it the caller wants
///   [`crate::replacement::request_seed_alone_replacement`] and its 30-day
///   window instead.
///
/// Then — always, both arms — it seals the identity seed to the **new** kit and
/// puts the blob. That second half is not optional politeness: replacement
/// changes the sealing key, the nest deletes the row the moment the
/// registration lands, and `identity-succession.md:51` makes re-putting "in the
/// same ceremony" a requirement precisely so an account is never left with no
/// escrow at all.
///
/// `predecessors` rides into the escrow blob's additive section
/// (`identity-succession.md` § Seed escrow) — the material that opens a corpus
/// still sealed under a retired identity after total device loss. It is a
/// parameter rather than an internal default precisely so the sites that must
/// be non-empty cannot be forgotten silently: the kit a succession owes its
/// successor, and every ceremony run by a device whose registry holds a
/// predecessor row.
///
/// **It is a floor, not the whole answer, and only on the replacement arm.**
/// The re-put *replaces* the resting blob, so on `prior: Some(..)` this
/// ceremony first reads the blob it is about to overwrite (under `prior`, the
/// last moment that key is still the head) and unions its section in — see
/// [`carry_forward_predecessors`], which also owns the refusal when that blob
/// cannot be read. A caller therefore cannot lose an *existing* section by
/// passing `&[]`; it can only fail to *add* material the blob never held,
/// which is exactly what the first-registration arm needs the parameter for.
///
/// Runs over the **authenticated** connection (both kinds are USER class).
pub async fn create_kit<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    prior: Option<&RecoveryKey>,
    predecessors: &[PredecessorSeed],
) -> Result<RecoveryKit>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    create_kit_with_root(
        client,
        identity,
        prior,
        RecoveryKey::generate(),
        predecessors,
    )
    .await
}

/// [`create_kit`], but registering a **pre-minted** root instead of minting
/// one — the onboarding kit screen's deferred ceremony. That screen mints and
/// displays the root before any nest exists (its ratified position is ahead of
/// `handle_entry` — `identity-succession.md` § The RecoveryKey → *Creation
/// UX*, 2026-08-01), so the wizard's signed-in handoff calls this with the
/// held root and registers **exactly the phrase the user already saved**;
/// minting here would silently invalidate it. Everything else — arm selection
/// from the chain head, co-signing, the same-ceremony escrow put, the
/// kit-always-comes-back rule — is identical.
pub async fn create_kit_with_root<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    prior: Option<&RecoveryKey>,
    recovery: RecoveryKey,
    predecessors: &[PredecessorSeed],
) -> Result<RecoveryKit>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let actor_id = identity.actor_id();
    let head = client.chain_head(&actor_id).await?;
    let seq = next_seq(head.as_ref(), prior)?;

    // ── The replacement arm's carry-forward, ordered BEFORE the registration.
    //
    // Two reasons it cannot go anywhere else: `escrow.fetch` verifies against
    // the chain **head**, which the registration below is about to move, and
    // the nest deletes the resting row the instant a pubkey-changing
    // registration lands. So this is the last moment the current blob is both
    // readable and alive — and `prior` is exactly the key that opens it.
    let carried_forward = match prior {
        Some(prior) => {
            Some(carry_forward_predecessors(client, &actor_id, prior, predecessors).await?)
        }
        // The first-registration arm: nothing rests to be overwritten (the row
        // dies with the key it was sealed to, and no key has ever been
        // registered here). Whatever the caller resolved is the whole truth.
        None => None,
    };
    let predecessors = carried_forward.as_deref().unwrap_or(predecessors);

    let registration = RecoveryKeyRegistration {
        actor_id,
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp::now(),
    };
    let signed = registration
        .sign(identity.signing_key(), &recovery, prior)
        .map_err(|e| RecoveryError::Crypto(format!("signing the registration: {e}")))?;

    let landed_seq = client.submit_registration(&signed).await?;
    tracing::debug!(seq = landed_seq, "recovery kit registered");

    // From here the registration has landed and the old escrow row (if any) is
    // gone. Every path below must still return the kit.
    let escrow = put_escrow(client, identity, &recovery, predecessors).await;

    Ok(RecoveryKit {
        secret_hex: Zeroizing::new(recovery.to_hex()),
        actor_id,
        recovery_pubkey: recovery.public(),
        seq: landed_seq,
        escrow,
    })
}

/// What the resting blob turned out to be holding, ahead of the re-put that
/// replaces it.
///
/// Three-valued for the same reason [`PredecessorsOutcome`] is: "the blob
/// carries nothing" and "we could not find out what the blob carries" are
/// opposite answers to the only question that matters here — *may this
/// ceremony overwrite it?* — and flattening them is precisely the bug filed.
enum RestingSection {
    /// The blob opened. Its section, possibly empty.
    Read(Vec<PredecessorSeed>),
    /// No blob rests for this account (`no_escrow` — the honest signal, not a
    /// fault). There is nothing to overwrite, so nothing to protect.
    NothingResting,
    /// The blob could not be fetched, or did not open. What it held is
    /// **unknown**, so a re-put can only be assumed to destroy it.
    Unknown(String),
}

/// Resolve the predecessor section a kit **replacement** must carry into the
/// blob it is about to overwrite.
///
/// The rule this implements is the one the resting blob defines, not the one
/// the local account registry happens to know: `put_escrow` seals exactly the
/// slice it is handed and the nest's `escrow.put` **replaces** the row, so a
/// section this device cannot see is a section this ceremony destroys — and
/// the predecessor seeds are the only material that opens a corpus still
/// sealed under a retired identity after total device loss
/// (`identity-succession.md` § Seed escrow).
///
/// The registry is a strictly weaker source and was the whole defect: it is
/// empty on any device that never held the predecessor's row (an ordinary
/// second device, or the one device after the user removed the retired
/// account), and a replacement from such a device used to write `&[]` over a
/// section it was holding the key to read.
///
/// So: read the resting blob under `prior` — the current head, hence the key
/// the nest will verify the fetch against — and **union** it with what the
/// caller resolved. The registry wins a duplicate `actor_id` because its
/// material is live (parsed out of a stored secret this device holds) while
/// the blob's is whatever some past ceremony sealed.
///
/// Fail toward carrying: an unfetchable or unreadable blob refuses the
/// ceremony ([`RecoveryError::PriorEscrowUnreadable`]) rather than silently
/// narrowing the section. `no_escrow` is not that case — it is a definite
/// "nothing rests", so the replacement proceeds with whatever the caller
/// resolved.
async fn carry_forward_predecessors<R>(
    client: &RecoveryClient<R>,
    actor_id: &ActorId,
    prior: &RecoveryKey,
    resolved: &[PredecessorSeed],
) -> Result<Vec<PredecessorSeed>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let resting = read_resting_section(client, actor_id, prior).await;

    let mut out = resolved.to_vec();
    match resting {
        RestingSection::Read(section) => {
            for entry in section {
                // Registry wins the duplicate: same identity, and the local
                // material is the one that was just proven to parse.
                if !out.iter().any(|s| s.actor_id == entry.actor_id) {
                    out.push(entry);
                }
            }
        }
        RestingSection::NothingResting => {}
        RestingSection::Unknown(reason) => {
            tracing::warn!(
                %reason,
                carried = out.len(),
                "the resting escrow blob could not be read before a kit replacement"
            );
            // Nothing else carries what that blob may have held, so the re-put
            // would be the last event in its history. Refuse — before the
            // registration, so the account is untouched and a retry is free.
            return Err(RecoveryError::PriorEscrowUnreadable { reason });
        }
    }
    Ok(out)
}

/// Fetch + unseal the blob currently at rest under `prior`, classifying every
/// failure as [`RestingSection::Unknown`] rather than as an absence.
async fn read_resting_section<R>(
    client: &RecoveryClient<R>,
    actor_id: &ActorId,
    prior: &RecoveryKey,
) -> RestingSection
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let challenge = match client.escrow_challenge(actor_id).await {
        Ok(c) => c,
        Err(RecoveryError::NoEscrow) => return RestingSection::NothingResting,
        Err(e) => return RestingSection::Unknown(e.to_string()),
    };
    let signature = match EscrowChallenge::new(*actor_id, challenge.nonce).sign(prior) {
        Ok(sig) => sig,
        Err(e) => return RestingSection::Unknown(format!("signing the escrow challenge: {e}")),
    };
    let blob = match client
        .escrow_fetch(actor_id, &challenge.nonce, &signature)
        .await
    {
        Ok(blob) => blob,
        // The one refusal that is an answer: no row rests, so the re-put
        // creates rather than replaces.
        Err(RecoveryError::NoEscrow) => return RestingSection::NothingResting,
        Err(e) => return RestingSection::Unknown(e.to_string()),
    };
    let opened = match unseal_seed_escrow_with_predecessors(&blob, &prior.escrow_secret()) {
        Ok(opened) => opened,
        Err(e) => return RestingSection::Unknown(format!("unsealing the resting blob: {e}")),
    };
    match opened.predecessors {
        PredecessorsOutcome::Absent => RestingSection::Read(Vec::new()),
        PredecessorsOutcome::Opened(list) => RestingSection::Read(
            list.into_iter()
                .map(|p| PredecessorSeed {
                    actor_id: p.actor_id,
                    seed: *p.seed,
                })
                .collect(),
        ),
        // A section is there and did not open. On the *restore* path that
        // degrades (never let the auxiliary cost the primary); here the
        // asymmetry reverses — we are about to WRITE, and writing over bytes
        // we could not read is the loss itself.
        PredecessorsOutcome::Unreadable(e) => {
            RestingSection::Unknown(format!("the resting predecessor section did not open: {e}"))
        }
    }
}

/// Seal the identity seed to `recovery` and put the blob, propagating failures.
///
/// `predecessors` is the additive section described on [`create_kit`]; an empty
/// slice seals byte-for-byte the classic blob, with no `pred` key in the
/// encoding at all.
///
/// An `Err` here means nothing about a registration — the two callers differ in
/// exactly what a failure costs, which is why both shapes exist: a *minting*
/// ceremony wraps this through [`put_escrow`] (its secret must survive the
/// failure), while the held-kit re-seal
/// ([`crate::replacement::reseal_escrow_with_held_kit`]) propagates it — the
/// kit is on paper in the user's hand, so a failed put loses nothing and the
/// honest answer is the error itself.
pub(crate) async fn try_put_escrow<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    recovery: &RecoveryKey,
    predecessors: &[PredecessorSeed],
) -> Result<i64>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let sealed = seal_seed_escrow_with_predecessors(
        identity.secret_bytes(),
        &identity.actor_id().0,
        &recovery.escrow_public(),
        predecessors,
    )
    .map_err(|e| crate::error::RecoveryError::Crypto(format!("sealing the seed escrow: {e}")))?;
    client.escrow_put(&sealed).await
}

/// [`try_put_escrow`], with every failure mapped into
/// [`EscrowOutcome::Failed`] rather than an `Err` — the shape a **minting**
/// ceremony needs: by the time the put runs the registration has landed, so an
/// `Err` return would discard the freshly minted secret riding beside it.
pub(crate) async fn put_escrow<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    recovery: &RecoveryKey,
    predecessors: &[PredecessorSeed],
) -> EscrowOutcome
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    match try_put_escrow(client, identity, recovery, predecessors).await {
        Ok(updated_at) => EscrowOutcome::Stored { updated_at },
        Err(e) => {
            tracing::warn!(error = %e, "the seed-escrow re-put failed");
            EscrowOutcome::Failed {
                reason: e.to_string(),
            }
        }
    }
}

/// Decide the `seq` to register at, refusing the two caller states that would
/// produce a record the nest must reject.
///
/// Checking the prior key against the head **locally** is not redundant with
/// the nest's own verification: it turns "your record was refused" into "that
/// is not the kit registered for this account", which is the difference between
/// a dead end and an actionable screen.
fn next_seq(head: Option<&ChainHead>, prior: Option<&RecoveryKey>) -> Result<u64> {
    match (head, prior) {
        (None, None) => Ok(1),
        (Some(head), Some(prior)) if prior.public() == head.recovery_pubkey => Ok(head.seq + 1),
        // A prior kit that is not the registered head — a superseded kit, or
        // one belonging to another account.
        (Some(_), Some(_)) | (None, Some(_)) => Err(RecoveryError::PriorKitMismatch),
        // Something is registered and the caller holds no prior kit: the
        // honest-loss path, which is the 30-day window, not this ceremony.
        (Some(_), None) => Err(RecoveryError::PriorKitRequired),
    }
}

pub(crate) fn hex32(bytes: &[u8; 32]) -> String {
    fauna_core::format::hex_full(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Settings copy button and QR carry one URI on every app
    /// (`identity-succession.md` § The RecoveryKey — *Which encoding each
    /// affordance carries*): the account's actor id plus its handle QUALIFIED
    /// with the nest's host, because a bare local part read months later on a
    /// device that never saw this account names nothing.
    #[test]
    fn the_display_uri_names_the_account_with_a_host_qualified_handle() {
        let secret = "ab".repeat(32);
        let actor = "cd".repeat(32);
        assert_eq!(
            kit_display_uri(&secret, &actor, "ada", "https://nest.example:8443"),
            format!("fauna://recovery?secret={secret}&actor={actor}&handle=ada@nest.example:8443")
        );
        // An already-qualified handle (web's session form) passes through.
        assert_eq!(
            kit_display_uri(&secret, &actor, "ada@other.example", "https://nest.example"),
            format!("fauna://recovery?secret={secret}&actor={actor}&handle=ada@other.example")
        );
        // No handle known → no `handle=` param, never an empty one.
        assert_eq!(
            kit_display_uri(&secret, &actor, "", "https://nest.example"),
            format!("fauna://recovery?secret={secret}&actor={actor}")
        );
    }

    /// Whatever the display URI carries, the one restore grammar reads it back
    /// to the same account — the copy must restore knowing where it lives.
    #[test]
    fn the_display_uri_parses_back_to_the_same_account() {
        let secret = "ab".repeat(32);
        let actor = "cd".repeat(32);
        let uri = kit_display_uri(&secret, &actor, "ada", "https://nest.example");
        let parsed = crate::restore::parse_kit(&uri).expect("the display URI parses");
        assert_eq!(parsed.actor_id.map(|a| a.to_hex()), Some(actor));
        assert_eq!(parsed.handle.as_deref(), Some("ada@nest.example"));
    }
}
