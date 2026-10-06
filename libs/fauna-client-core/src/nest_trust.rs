//! Nest-identity trust — the **shared** (native + wasm) core of
//! `docs/goal/architecture/security.md` § Transport trust, Axis 1 (channel
//! binding) + Axis 2 (identity root / TOFU pinning), client half.
//!
//! This module is deliberately **transport-free**: it neither opens a TLS
//! connection nor parses a cert. It takes an already-obtained SPKI and the
//! [`CertBinding`] the nest returned (in `fauna.auth.handshake` *or*
//! `fauna.auth.verify`) and answers two questions:
//!
//! 1. **Channel binding (Axis 1):** does the holder of `nest_actor_id`'s key
//!    attest to the *received* SPKI? ([`verify_cert_binding`].) Equal-iff-no-MITM:
//!    the nest signs the SPKI of the cert it itself serves, so the signature over
//!    the *received* SPKI verifies only if no middlebox substituted the cert.
//!    The browser variant ([`verify_cert_binding_possession`]) drops the
//!    received-cert compare — a browser cannot read the served cert's SPKI — and
//!    verifies key-possession only.
//! 2. **Identity root (Axis 2):** is `nest_actor_id` the nest we *meant* to
//!    reach? ([`check_identity_root`].) DNS `self=` for public domains (exact
//!    match required), TOFU for LAN/`.local` and the web origin (pin on first
//!    connect, error on a later change — the SSH `known_hosts` model).
//!
//! Lives in `fauna-client-core` (the wasm + UniFFI shared layer) so both the
//! native connector (`fauna-anon-client`, which adds the disk-backed pin store +
//! the rustls SPKI capture) and the web SPA (`fauna-wasm`, which adds a
//! localStorage pin store) call one implementation (priority #2). The persistent
//! pin stores and the connect-path wiring are per-platform; this module is what
//! they call.

use std::collections::HashMap;
use std::sync::Mutex;

// verify-ok(tofu-identity): the key here IS the identity being established —
// the nest's own cert-binding key — not an authorization over someone else's
// data. With an `expected_nest_actor_id` a weak key must equal it, and on TOFU
// first contact an active MITM already wins, so strict-vs-permissive changes
// nothing. Verifying a *claimed*
// identity owes `fauna_core::identity::verify_detached` instead.
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use fauna_protocol::auth::CertBinding;

/// Why a channel-binding verification failed. Every variant means the
/// connection must be torn down before any bearer or request is sent
/// (security.md § Connection-teardown rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// `nest_actor_id` is not 64 hex chars of a valid Ed25519 public key.
    MalformedActorId,
    /// `sig` is not 64 bytes of a valid Ed25519 signature.
    MalformedSignature,
    /// The SPKI the nest signed differs from the SPKI the client received over
    /// TLS — a middlebox substituted the cert (or the nest reported a stale
    /// cert). The exact condition the binding exists to catch.
    SpkiMismatch,
    /// The channel-binding signature did not verify over
    /// `(received_spki ‖ client_nonce)` against `nest_actor_id`'s key.
    SignatureFailed,
    /// The binding is valid, but `nest_actor_id` is not the identity the
    /// caller expected (the DNS `self=` root resolved a different nest).
    IdentityMismatch,
    /// `spki_sha256` is neither 32 bytes (a real fingerprint) nor 0 (a
    /// cert-less nest) — a length the verifying end must never accept, since
    /// nothing else pins where the `spki_sha256 ‖ nonce` boundary falls in the
    /// signed message; see `verify_cert_binding_possession`.
    MalformedSpki,
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BindingError::MalformedActorId => "channel binding: malformed nest_actor_id",
            BindingError::MalformedSignature => "channel binding: malformed signature",
            BindingError::SpkiMismatch => {
                "channel binding: served SPKI differs from received cert (possible MITM)"
            }
            BindingError::SignatureFailed => "channel binding: signature did not verify",
            BindingError::IdentityMismatch => {
                "channel binding: nest_actor_id is not the expected nest"
            }
            BindingError::MalformedSpki => {
                "channel binding: spki_sha256 is not a canonical length (32, or 0 on a cert-less nest)"
            }
        };
        f.write_str(s)
    }
}

impl std::error::Error for BindingError {}

/// Parse `nest_actor_id` (64-hex → 32-byte Ed25519 key). Shared by both verify
/// paths.
fn parse_nest_actor(binding: &CertBinding) -> Result<([u8; 32], VerifyingKey), BindingError> {
    let actor_bytes: [u8; 32] = fauna_core::hex32::decode(&binding.nest_actor_id)
        .map_err(|_| BindingError::MalformedActorId)?;
    let verifying_key =
        VerifyingKey::from_bytes(&actor_bytes).map_err(|_| BindingError::MalformedActorId)?;
    Ok((actor_bytes, verifying_key))
}

/// Verify the nest's Ed25519 signature over
/// [`fauna_protocol::auth::cert_binding_signed_message`]`(signed_spki, client_nonce)`.
/// The single cryptographic core shared by [`verify_cert_binding`] (which
/// passes the *received* SPKI) and [`verify_cert_binding_possession`] (which
/// passes the nest's *claimed* SPKI).
///
/// **Domain separation.** The deployment key signs several contexts, so
/// the cert-binding carries a distinct constant prefix
/// (`sig_domain::CERT_BINDING_V1`) in [`CertBinding::tagged_sig`] — the only
/// signature a binding carries: a signature in any other context can never be
/// reinterpreted here. A tagged signature that is malformed or does not
/// verify is corruption, a nest bug, or tampering, and is refused outright.
/// (The untagged `sig` a pre-tag nest once emitted, and the fallback that
/// accepted it, were removed 2026-09-24 by the compat-remnant sweep.)
fn verify_binding_signature(
    verifying_key: &VerifyingKey,
    binding: &CertBinding,
    signed_spki: &[u8],
    client_nonce: &[u8],
) -> Result<(), BindingError> {
    let signature = Signature::from_slice(binding.tagged_sig.as_slice())
        .map_err(|_| BindingError::MalformedSignature)?;
    let tagged_msg = fauna_protocol::auth::cert_binding_signed_message(signed_spki, client_nonce);
    verifying_key
        .verify(&tagged_msg, &signature)
        .map_err(|_| BindingError::SignatureFailed)
}

/// Verify the nest's channel-binding proof (Axis 1). Returns the verified
/// 32-byte `nest_actor_id` on success — the identity the channel is proven to
/// terminate at, to be checked against the identity root (Axis 2) by the caller
/// or via [`check_identity_root`].
///
/// `received_spki` is the SHA-256 SPKI fingerprint of the cert the client
/// *actually received* over TLS (captured by the connection layer).
/// `expected_nest_actor_id`, when `Some`, is the identity the caller already
/// resolved out-of-band (DNS `self=`, or the client's own injected deployment
/// seed on a client-provisioned box); it is enforced here so a pre-resolved-root
/// nest is rejected before the (cheaper-to-attack) TOFU path is ever reached.
/// Pass `None` for the TOFU path and resolve identity via [`check_identity_root`].
pub fn verify_cert_binding(
    received_spki: &[u8; 32],
    client_nonce: &[u8],
    binding: &CertBinding,
    expected_nest_actor_id: Option<&[u8; 32]>,
) -> Result<[u8; 32], BindingError> {
    // Parse the claimed identity.
    let (actor_bytes, verifying_key) = parse_nest_actor(binding)?;

    // If the caller already knows which identity to expect (DNS root), enforce
    // it up front — a mismatch means we resolved a different nest than the one
    // that answered, so do not even bother verifying its signature.
    if let Some(expected) = expected_nest_actor_id
        && expected != &actor_bytes
    {
        return Err(BindingError::IdentityMismatch);
    }

    // The nest signs the SPKI of the cert IT serves. If that differs from the
    // SPKI we received, the cert was substituted — surface it as the specific
    // MITM signal rather than a generic signature failure. (When they are
    // equal, the signature below is over an unambiguous message.)
    if binding.spki_sha256.as_slice() != received_spki.as_slice() {
        return Err(BindingError::SpkiMismatch);
    }

    // Verify over the SPKI WE received (not the one the nest reported) ‖ nonce.
    verify_binding_signature(&verifying_key, binding, received_spki, client_nonce)?;
    Ok(actor_bytes)
}

/// Possession-only verification of a [`CertBinding`] — the **web** variant of
/// [`verify_cert_binding`] for a client that cannot read the served TLS cert's
/// SPKI (a browser owns TLS verification; wasm has no captured cert to compare).
/// It verifies only that the holder of `nest_actor_id`'s key signed `(the nest's
/// *claimed* SPKI ‖ client_nonce)` — i.e. KEY POSSESSION + per-connection
/// freshness — and deliberately does **not** run the received-cert compare (the
/// [`BindingError::SpkiMismatch`] leg), because web has no received SPKI. Returns
/// the verified 32-byte `nest_actor_id`.
///
/// What this buys / doesn't (`security.md` § Transport trust, the web-exempt
/// note; the NT-1 hardening review, tracked internally): it catches benign
/// key rotation / redeploy (a different `nest_actor_id`
/// → [`check_identity_root`] returns [`IdentityError::PinChanged`]) and the *lazy*
/// attacker who breaks WebPKI but presents no binding (`Withdrawn`) or their own
/// key (`Changed`). It does **not** catch a WebPKI-breaking attacker willing to
/// obtain one genuine binding — possession-only verification has no received-cert
/// compare, so a binding is replayable by **live relay** (and, when signed over a
/// *server*-chosen nonce alone, by **offline harvest**: a fixed signature over
/// fixed public bytes, reusable). On the verify path that residual is narrowed by
/// folding a *client*-chosen nonce into the binding: the caller passes the
/// 64-byte `challenge_nonce ‖ client_nonce` as `client_nonce` here (native:
/// `fauna_anon_client::graduate_verify_path`).
pub fn verify_cert_binding_possession(
    client_nonce: &[u8],
    binding: &CertBinding,
) -> Result<[u8; 32], BindingError> {
    let (actor_bytes, verifying_key) = parse_nest_actor(binding)?;
    // The native verifier pins the spki_sha256 ‖ nonce split implicitly, by
    // requiring spki_sha256 to byte-equal the received (always 32-byte) SPKI
    // before it ever reaches the signature check. This verifier has no
    // received SPKI to compare against, so nothing else fixes where the
    // split falls in the signed message — an attacker-chosen length here
    // would let a genuine multi-part signature (e.g. the verify path's
    // `spki ‖ challenge_nonce ‖ client_nonce`) be re-presented under a moved
    // boundary and still verify; see
    // `the_split_boundary_is_not_movable`. 32 (a real fingerprint) and 0 (a
    // cert-less nest — `build_identity_binding`'s shape) are the only
    // lengths a genuine nest ever produces.
    let spki_len = binding.spki_sha256.as_slice().len();
    if spki_len != 32 && spki_len != 0 {
        return Err(BindingError::MalformedSpki);
    }
    // Verify over the nest's OWN claimed SPKI ‖ nonce — there is no received
    // cert to compare against, so this proves key possession only.
    verify_binding_signature(
        &verifying_key,
        binding,
        binding.spki_sha256.as_slice(),
        client_nonce,
    )?;
    Ok(actor_bytes)
}

/// The Axis-2 identity root for a given nest host — where the *expected*
/// `nest_actor_id` comes from.
pub enum IdentityRoot<'a> {
    /// The expected identity was pre-resolved before connecting — from DNS
    /// `_fauna.{domain}` TXT `self=` (public domain) or from the client's own
    /// injected deployment seed (a client-provisioned box's first contact,
    /// security.md § Transport trust Axis 2). Exact match required, no pin,
    /// no warning.
    PreResolved([u8; 32]),
    /// LAN IP / `.local` / a web origin: no DNS authority. Trust-on-first-use,
    /// keyed by host (native) or origin (web).
    Tofu { host: &'a str },
    /// The same no-DNS-authority case, but where minting is forbidden: a
    /// **pin-consumer** process — one that reads pins the interactive app
    /// minted and must never mint its own (a File Provider extension, a
    /// background sync agent) — or a graduation with no user behind it in
    /// any process (the bearer dial's graduate-and-retry fallback). First-trust
    /// is a user decision made where a user is present; a process that
    /// silently pinned would both forfeit the pin-change warning and let a
    /// MITM be pinned with no user in the loop. With no pin for `host` the
    /// check fails ([`IdentityError::PinRequired`]) instead of pinning — the
    /// SSH batch-mode analogue (`StrictHostKeyChecking=yes`): interactive
    /// sessions prompt, batch refuses unknown hosts.
    TofuStrict { host: &'a str },
}

/// Outcome of checking a verified identity against its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityOutcome {
    /// DNS `self=` matched, or a TOFU pin matched a prior connect.
    Verified,
    /// TOFU first connect — the identity was just pinned for `host`.
    Pinned,
}

/// Why an identity-root check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The pre-resolved root (DNS `self=`, or a client-injected deployment
    /// seed) names a different identity than the binding proved.
    RootMismatch,
    /// A TOFU pin exists for this host and the identity changed — the
    /// `known_hosts` "REMOTE HOST IDENTIFICATION HAS CHANGED" case. Warn loudly;
    /// never silently re-pin.
    PinChanged { pinned: [u8; 32], seen: [u8; 32] },
    /// A pin-consumer process ([`IdentityRoot::TofuStrict`]) reached a
    /// TOFU-rooted nest no pin exists for. Not an attack verdict — the
    /// interactive app simply hasn't minted the pin yet (mid-onboarding). The
    /// consumer retries; the
    /// connect succeeds once the app's pin is visible.
    PinRequired,
    /// Rotation-chain **fork evidence** (`box-recovery.md` § Client acceptance):
    /// this client previously accepted a rotation chain for `host`, and the
    /// chain the box now serves contradicts it — it does not extend the
    /// accepted head, or names a different head at an accepted seq. Hard
    /// warning naming both heads; **no re-trust on this surface** (the launch
    /// machine refuses `trust_nest_identity` for it) and never a silent path.
    PinForked { pinned: [u8; 32], seen: [u8; 32] },
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::RootMismatch => f.write_str(
                "nest identity: the pre-resolved identity root does not match the connected nest",
            ),
            IdentityError::PinChanged { .. } => f.write_str(
                "nest identity: the pinned identity for this host changed — possible impersonation",
            ),
            IdentityError::PinRequired => f.write_str(
                "nest identity: no pin for this host and this process cannot mint one — waiting for the app to trust the nest",
            ),
            IdentityError::PinForked { .. } => f.write_str(
                "nest identity: the nest's rotation history contradicts the one this client accepted — possible fork",
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

/// Persistent store of TOFU-pinned nest identities, keyed by host. The
/// connection layer supplies a backend impl (disk-backed on native via
/// `fauna_anon_client::cert_binding::DiskPinStore`, localStorage-backed on web);
/// [`MemoryPinStore`] is the in-memory default (and the test double).
pub trait NestIdentityPinStore: Send + Sync {
    /// The pinned `nest_actor_id` for `host`, if any.
    fn get(&self, host: &str) -> Option<[u8; 32]>;
    /// Pin `actor_id` for `host` (first connect). Overwrites only via an
    /// explicit user-approved re-pin path — the "trust this nest" affordance,
    /// or a claim ceremony seeding its possession-proven first-contact root
    /// (`security.md` § Transport trust) — never silently from a check.
    fn set(&self, host: &str, actor_id: [u8; 32]);
    /// Forget the pin for `host` — the explicit, user-approved recovery behind
    /// the "trust this nest" re-trust action (the `ssh-keygen -R host`
    /// analogue). The next connect re-TOFUs. Never called automatically; a pin
    /// only ever changes via this user action or a clean first-connect.
    fn remove(&self, host: &str);
    /// True for a **pin-consumer** backend that never mints or removes pins
    /// (`set`/`remove` are no-ops) — installed by processes with no user
    /// present (a File Provider extension, a background agent). The connection
    /// layer selects [`IdentityRoot::TofuStrict`] over [`IdentityRoot::Tofu`]
    /// when this is set, so an empty store fails the connect instead of
    /// silently trusting the first nest reached.
    fn read_only(&self) -> bool {
        false
    }
    /// The rotation-log seq at which the pin for `host` was accepted through a
    /// verified rotation chain, if it ever was (`box-recovery.md` § Client
    /// acceptance — the client stores `(head, seq)`). `None` for an ordinary
    /// TOFU pin that never moved through a chain — that distinction is what
    /// separates fork evidence (hard warning) from the ordinary
    /// identity-changed warning. Default `None` keeps pre-rotation backends
    /// compiling and behaving exactly as before.
    fn rotation_seq(&self, host: &str) -> Option<u64> {
        let _ = host;
        None
    }
    /// Record a chain-accepted re-pin: `head` becomes the pin for `host`, with
    /// `seq` remembered per [`Self::rotation_seq`]. The **only** sanctioned
    /// silent pin move ([`Self::set`]'s no-silent-re-pin rule stands for every
    /// other path); callers must have verified the chain AND the live channel
    /// binding first (`try_rotation_repin` is the one production caller).
    /// Default delegates to [`Self::set`] (dropping the seq), so a pin-consumer
    /// backend's no-op `set` keeps it structurally unable to move pins.
    fn set_rotation_accepted(&self, host: &str, head: [u8; 32], seq: u64) {
        let _ = seq;
        self.set(host, head);
    }
    /// Atomically pin `actor_id` for `host` **iff** no pin exists yet, returning
    /// the pin that was already there (`Some`) or `None` if this call minted the
    /// first one. The TOFU-mint half of [`check_identity_root`]'s `Tofu` arm —
    /// pulled out from under it (rather than a bare [`Self::get`] then a
    /// separate [`Self::set`]) because that two-step shape is a genuine
    /// check-then-act race: two independent connections can graduate the SAME
    /// host concurrently (native tui fires a synchronous bearer-mint graduation
    /// at login *and* a fire-and-forget background silent challenge
    /// (`spawn_domain_refresh`) in parallel — both TOFU-check the same host).
    /// Under load, one graduation's stale, still-in-flight mint can land
    /// **after** a second, newer decision already ran (a manual re-trust, a
    /// rotation-chain re-pin, or — as `test_nest_identity_pin_post_auth.py`
    /// caught intermittently — a fresh TOFU pin meant to replace an old one)
    /// and silently clobber it back, with no `IdentityChanged` warning ever
    /// firing for the connection that should have seen the mismatch.
    ///
    /// Default delegates to [`Self::get`] then [`Self::set`] — **not
    /// atomic** — correct only for a backend with no real concurrent writer
    /// (the wasm browser event loop can't preempt mid-call; the test-only
    /// `ReadOnly` mock never reaches this arm at all, since a read-only store
    /// selects [`IdentityRoot::TofuStrict`] instead). A backend a multi-threaded
    /// native runtime can call concurrently (`MemoryPinStore`, `DiskPinStore`)
    /// MUST override this under a single lock acquisition.
    fn pin_if_absent(&self, host: &str, actor_id: [u8; 32]) -> Option<[u8; 32]> {
        if let Some(existing) = self.get(host) {
            return Some(existing);
        }
        self.set(host, actor_id);
        None
    }
}

/// `host → (actor_id, chain-accepted seq)`; seq is `None` for an ordinary
/// TOFU pin (see [`NestIdentityPinStore::rotation_seq`]).
type PinEntries = HashMap<String, ([u8; 32], Option<u64>)>;

/// The atomic check-then-insert every locking [`NestIdentityPinStore`]
/// override needs under its single lock acquisition: return the existing
/// pin if one is already present, otherwise record `actor_id` as an
/// ordinary (non-rotation) pin and return `None`. Callers holding a
/// disk-backed store still own persisting the mutated map afterward — this
/// only does the map-level decision, identical across every backend.
pub fn pin_entry_if_absent(
    entries: &mut HashMap<String, ([u8; 32], Option<u64>)>,
    host: &str,
    actor_id: [u8; 32],
) -> Option<[u8; 32]> {
    if let Some((existing, _)) = entries.get(host) {
        return Some(*existing);
    }
    entries.insert(host.to_string(), (actor_id, None));
    None
}

/// In-memory [`NestIdentityPinStore`]. Pins live for the process lifetime — the
/// persistent stores that survive restarts are per-platform connection-layer
/// concerns.
#[derive(Default)]
pub struct MemoryPinStore {
    inner: Mutex<PinEntries>,
}

impl MemoryPinStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl NestIdentityPinStore for MemoryPinStore {
    fn get(&self, host: &str) -> Option<[u8; 32]> {
        self.inner.lock().unwrap().get(host).map(|(id, _)| *id)
    }
    fn set(&self, host: &str, actor_id: [u8; 32]) {
        self.inner
            .lock()
            .unwrap()
            .insert(host.to_string(), (actor_id, None));
    }
    fn remove(&self, host: &str) {
        self.inner.lock().unwrap().remove(host);
    }
    fn rotation_seq(&self, host: &str) -> Option<u64> {
        self.inner.lock().unwrap().get(host).and_then(|(_, s)| *s)
    }
    fn set_rotation_accepted(&self, host: &str, head: [u8; 32], seq: u64) {
        self.inner
            .lock()
            .unwrap()
            .insert(host.to_string(), (head, Some(seq)));
    }
    fn pin_if_absent(&self, host: &str, actor_id: [u8; 32]) -> Option<[u8; 32]> {
        pin_entry_if_absent(&mut self.inner.lock().unwrap(), host, actor_id)
    }
}

/// Check a channel-binding-verified `nest_actor_id` against its identity root
/// (Axis 2). For `PreResolved`, requires an exact match. For `Tofu`, pins on first
/// connect ([`IdentityOutcome::Pinned`]), confirms on a matching later connect
/// ([`IdentityOutcome::Verified`]), and errors with [`IdentityError::PinChanged`]
/// on a changed identity — never silently re-pinning. For `TofuStrict` (a
/// pin-consumer process), a missing pin is [`IdentityError::PinRequired`] —
/// never minted.
pub fn check_identity_root(
    verified: &[u8; 32],
    root: IdentityRoot<'_>,
    pins: &dyn NestIdentityPinStore,
) -> Result<IdentityOutcome, IdentityError> {
    match root {
        IdentityRoot::PreResolved(expected) => {
            if &expected == verified {
                Ok(IdentityOutcome::Verified)
            } else {
                Err(IdentityError::RootMismatch)
            }
        }
        IdentityRoot::Tofu { host } => match pins.pin_if_absent(host, *verified) {
            None => Ok(IdentityOutcome::Pinned),
            Some(pinned) if &pinned == verified => Ok(IdentityOutcome::Verified),
            Some(pinned) => Err(IdentityError::PinChanged {
                pinned,
                seen: *verified,
            }),
        },
        IdentityRoot::TofuStrict { host } => match pins.get(host) {
            None => Err(IdentityError::PinRequired),
            Some(pinned) if &pinned == verified => Ok(IdentityOutcome::Verified),
            Some(pinned) => Err(IdentityError::PinChanged {
                pinned,
                seen: *verified,
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Deployment-seed rotation — client acceptance (`box-recovery.md` § Client
// acceptance — re-pin on a verified chain). This is the shared pin core the
// doc names: the pure verdict (`evaluate_rotation_bridge`), and the
// fetch+decide+re-pin helper (`try_rotation_repin`) both the native
// graduation wrappers and the web paths call.
// ---------------------------------------------------------------------------

/// Verdict of weighing a fetched rotation chain against the identity this
/// client holds and the identity the live channel binding proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationBridge {
    /// A valid chain links `pinned` to `presented`. Combined with the live
    /// binding (the caller's precondition), this licenses the silent re-pin;
    /// `seq` is the accepted head's rotation-log position, stored beside the
    /// pin so a later chain must extend it.
    Accepted { seq: u64 },
    /// The box's served history contradicts the one this client accepted
    /// before — it does not extend the accepted head, or names a different
    /// head at an accepted seq. Fork evidence: hard warning, no re-trust.
    Fork,
    /// No valid bridge (empty chain — an un-rotated nest; a chain
    /// that never reaches the presented identity; a broken hop) and nothing
    /// previously chain-accepted to contradict. Fall through to today's
    /// identity-changed surface, unchanged.
    NoBridge,
}

/// Weigh a rotation chain: does it license moving this client's trust from
/// `pinned` to `presented`?
///
/// **Precondition — the live-binding rule (`box-recovery.md` § Client
/// acceptance, condition 2):** `presented` MUST be the identity the *live*
/// Axis-1 channel binding on this very connection proved possession of
/// (native: [`verify_cert_binding`]'s return; web: the possession-verified
/// id). Never pass the chain's own head or any unproven identity — a
/// harvested chain alone must never move a pin.
///
/// `accepted_seq` is [`NestIdentityPinStore::rotation_seq`] for the host:
/// `Some` means the `pinned` identity was itself chain-accepted at that seq,
/// which arms the fork detection — a box whose log is append-only can always
/// extend what this client accepted, so a served history that cannot is
/// evidence someone is lying (a superseded-seed holder, or a box restored
/// from a pre-rotation backup — `box-recovery.md`: a revoked identity never
/// returns). With `None` (an ordinary TOFU pin), a non-bridging chain is just
/// [`RotationBridge::NoBridge`].
pub fn evaluate_rotation_bridge(
    chain: &[fauna_protocol::nest_rotation::SignedNestRotation],
    pinned: &[u8; 32],
    presented: &[u8; 32],
    accepted_seq: Option<u64>,
) -> RotationBridge {
    // Fork clause B — "a second distinct head at an accepted seq": this client
    // accepted `pinned` as the head written at `accepted_seq`; a served hop at
    // that same seq naming a different successor contradicts it regardless of
    // whatever else the chain contains.
    if let Some(n) = accepted_seq
        && chain
            .iter()
            .any(|h| h.statement.seq == n && &h.statement.new_nest_actor_id != pinned)
    {
        return RotationBridge::Fork;
    }
    match fauna_protocol::nest_rotation::verify_chain(chain, pinned, presented) {
        Ok(seq) => RotationBridge::Accepted { seq },
        // Fork clause A — "a chain that does not extend the accepted head":
        // only a chain-accepted pin has an accepted head to fail to extend; an
        // ordinary TOFU pin falls through to the ordinary warning.
        Err(_) if accepted_seq.is_some() => RotationBridge::Fork,
        Err(_) => RotationBridge::NoBridge,
    }
}

/// Outcome of [`try_rotation_repin`] — [`RotationBridge`] with the accepted
/// arm's side effect (the pin move) already performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationRepin {
    /// Chain verified and the pin for the host now names the presented head.
    Repinned { seq: u64 },
    /// Fork evidence — surface the hard warning ([`IdentityError::PinForked`]).
    Fork,
    /// No valid bridge — surface today's identity-changed warning. `reason`
    /// says which leg declined (a read-only consumer store, a fetch failure —
    /// transport fault / throttle — or a chain that does not
    /// bridge), so the callers' warn-level logs make a declined acceptance
    /// diagnosable from the client log alone.
    NoBridge { reason: String },
}

/// The one production acceptance path: fetch the box's rotation chain **over
/// the same connection** whose live binding just proved `presented`
/// (pre-identity [`fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND`] — the
/// client has no usable session at this moment by construction), weigh it via
/// [`evaluate_rotation_bridge`], and on acceptance move the pin through
/// [`NestIdentityPinStore::set_rotation_accepted`].
///
/// Same precondition as [`evaluate_rotation_bridge`]: `presented` must be the
/// live-binding-verified identity. Any fetch failure — an unknown-kind
/// refusal, a transport fault, a
/// throttle refusal — degrades to [`RotationRepin::NoBridge`]: the existing
/// warning with its explicit re-trust, never a hard failure the ceremony
/// didn't earn.
pub async fn try_rotation_repin<R>(
    client: &R,
    host: &str,
    pins: &dyn NestIdentityPinStore,
    pinned: [u8; 32],
    presented: [u8; 32],
) -> RotationRepin
where
    R: fauna_protocol::RpcRequester,
{
    use fauna_protocol::nest_rotation::{
        ROTATION_CHAIN_KIND, RotationChainReply, RotationChainRequest,
    };
    // A pin-consumer process never moves pins — not even through a verified
    // chain (its `set_rotation_accepted` would silently no-op and the retried
    // graduation would fail anyway). It keeps failing `PinChanged` until the
    // interactive app accepts and the shared store shows the new head — the
    // same follow-the-app convergence as `TofuStrict`'s `PinRequired`.
    if pins.read_only() {
        return RotationRepin::NoBridge {
            reason: "pin-consumer (read-only) store never accepts a rotation".into(),
        };
    }
    let reply: RotationChainReply = match client
        .request(ROTATION_CHAIN_KIND, RotationChainRequest::default())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return RotationRepin::NoBridge {
                reason: format!("rotation-chain fetch failed: {e}"),
            };
        }
    };
    match evaluate_rotation_bridge(&reply.chain, &pinned, &presented, pins.rotation_seq(host)) {
        RotationBridge::Accepted { seq } => {
            pins.set_rotation_accepted(host, presented, seq);
            RotationRepin::Repinned { seq }
        }
        RotationBridge::Fork => RotationRepin::Fork,
        RotationBridge::NoBridge => RotationRepin::NoBridge {
            reason: format!(
                "chain does not bridge (chain len {}, accepted seq {:?})",
                reply.chain.len(),
                pins.rotation_seq(host)
            ),
        },
    }
}

/// Outcome of the **web** possession-pin check ([`check_web_nest_identity`]),
/// where the nest's identity proof may be **absent** — a plaintext / dev nest
/// sends no `cert_binding` — unlike the native channel-binding path, where a
/// self-signed cert always requires one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdentityOutcome {
    /// First connect to this origin with a possession-proven identity — just
    /// pinned it.
    Pinned,
    /// The possession-proven identity matches the existing pin.
    Verified,
    /// No pin yet **and** no provable identity (a plaintext / dev nest, or a
    /// first connect whose binding didn't verify) — proceed without pinning.
    /// TOFU trusts the first connect; there is nothing to compare against.
    Unprovable,
}

/// Why the web possession-pin check failed loudly — both map to the same
/// user-facing "nest identity" warning (the SSH `known_hosts` model: the host we
/// previously trusted can no longer prove it is the same host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdentityError {
    /// A pin exists for this origin and the nest proved a **different**
    /// identity — the `known_hosts` "IDENTIFICATION HAS CHANGED" case.
    Changed { pinned: [u8; 32], seen: [u8; 32] },
    /// A pin exists for this origin but the nest presented **no valid** identity
    /// proof this connect (the binding was absent or failed possession-verify).
    /// Downgrade protection: an attacker who simply omits the binding must not
    /// silently bypass a pin, so this warns just like [`Self::Changed`].
    Withdrawn { pinned: [u8; 32] },
}

impl std::fmt::Display for WebIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebIdentityError::Changed { .. } => f.write_str(
                "nest identity: the pinned identity for this nest changed — possible impersonation",
            ),
            WebIdentityError::Withdrawn { .. } => f.write_str(
                "nest identity: this nest can no longer prove the identity you previously trusted",
            ),
        }
    }
}

impl std::error::Error for WebIdentityError {}

/// The web TOFU pin/compare against an origin, where `seen` is the
/// possession-verified `nest_actor_id` ([`verify_cert_binding_possession`]) or
/// `None` when the nest presented no valid binding. Implements the SSH
/// `known_hosts` model for the browser (`docs/goal/architecture/security.md`
/// § Transport trust, the web-exempt note): pin on first provable connect,
/// confirm on a match, and **warn loudly** on either a changed identity
/// ([`WebIdentityError::Changed`]) or a withdrawn proof
/// ([`WebIdentityError::Withdrawn`]) — never silently re-pinning. Re-pinning a
/// changed identity is an explicit user-approved action (the warning UI's
/// "trust" affordance calls [`NestIdentityPinStore::set`] directly), never a
/// side effect of this check.
pub fn check_web_nest_identity(
    seen: Option<[u8; 32]>,
    origin: &str,
    pins: &dyn NestIdentityPinStore,
) -> Result<WebIdentityOutcome, WebIdentityError> {
    match (pins.get(origin), seen) {
        (None, Some(id)) => {
            pins.set(origin, id);
            Ok(WebIdentityOutcome::Pinned)
        }
        (None, None) => Ok(WebIdentityOutcome::Unprovable),
        (Some(pinned), Some(id)) if pinned == id => Ok(WebIdentityOutcome::Verified),
        (Some(pinned), Some(seen)) => Err(WebIdentityError::Changed { pinned, seen }),
        (Some(pinned), None) => Err(WebIdentityError::Withdrawn { pinned }),
    }
}

/// localStorage key holding the `host → nest_actor_id_hex` pin map — the
/// browser analogue of native `DiskPinStore`'s JSON file. One key for every
/// wasm consumer (the SPA's challenge_verify path and the launch machine's
/// silent-challenge connector), so the two paths can never disagree about
/// what is pinned.
#[cfg(target_arch = "wasm32")]
const LOCAL_STORAGE_PIN_KEY: &str = "fauna_nest_pins";

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn load_local_pins() -> HashMap<String, String> {
    local_storage()
        .and_then(|ls| ls.get_item(LOCAL_STORAGE_PIN_KEY).ok().flatten())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn save_local_pins(map: &HashMap<String, String>) {
    if let Some(ls) = local_storage()
        && let Ok(s) = serde_json::to_string(map)
    {
        // Best-effort, like native `DiskPinStore::persist`: a quota /
        // private-mode write error must never break a live sign-in.
        let _ = ls.set_item(LOCAL_STORAGE_PIN_KEY, &s);
    }
}

/// Split a stored pin value into `(actor_id_hex, chain-accepted seq)`. The
/// value is `"<hex>"` for an ordinary TOFU pin and `"<hex>@<seq>"` after a
/// rotation-chain acceptance (same encoding as native `DiskPinStore`). A
/// malformed value drops exactly that host's pin (a fresh-install re-TOFU)
/// and no other — the deliberately
/// small blast radius of the in-band encoding.
pub fn split_pin_value(value: &str) -> (&str, Option<u64>) {
    match value.split_once('@') {
        Some((hex_part, seq)) => (hex_part, seq.parse().ok()),
        None => (value, None),
    }
}

/// The name of the e2e trust seed — the stand-in for the TLS-channel-binding
/// pin a plaintext rig can never graduate: the environment variable natively
/// (`fauna_anon_client::trust`), the localStorage key on web (the
/// `fauna-client-region` seed precedent). Only a test-capable build ever reads
/// it (e2e convention 15); the grammar is [`seeded_nest_identity`]'s.
pub const E2E_TRUST_NEST_IDENTITY: &str = "FAUNA_E2E_TRUST_NEST_IDENTITY";

/// The e2e trust seed's identity for `nest_url`, parsed out of `seed` — ONE
/// grammar for every host. `seed` holds `<nest url>=<64 hex>` entries joined
/// by `,`; the entry whose key names the same authority as `nest_url` answers
/// (`fauna_core::web::authority_of`, the pin store's own key), and a later
/// entry for one authority supersedes an earlier one.
///
/// Keyed because the pin it stands in for is per nest: an unkeyed seed would
/// trust one nest's key for every nest, so a bare identity names no nest and
/// is honoured for none. Pure — the reader decides whether a build may consult
/// a seed at all.
pub fn seeded_nest_identity(seed: &str, nest_url: &str) -> Option<[u8; 32]> {
    let authority = fauna_core::web::authority_of(nest_url);
    if authority.is_empty() {
        return None;
    }
    seed.rsplit(',')
        .filter_map(|entry| entry.trim().rsplit_once('='))
        .find(|(key, _)| fauna_core::web::authority_of(key.trim()) == authority)
        .and_then(|(_, hex)| fauna_core::hex32::decode(hex.trim()).ok())
}

/// localStorage-backed [`NestIdentityPinStore`] — the browser twin of native
/// `fauna_anon_client::cert_binding::DiskPinStore`, keyed by nest origin.
/// Stateless over the one browser localStorage, so every construction sees the
/// same pins (`docs/goal/architecture/security.md`
/// § Transport trust — the web-exempt note).
#[cfg(target_arch = "wasm32")]
pub struct LocalStoragePinStore;

#[cfg(target_arch = "wasm32")]
impl NestIdentityPinStore for LocalStoragePinStore {
    fn get(&self, host: &str) -> Option<[u8; 32]> {
        load_local_pins()
            .get(host)
            .and_then(|v| fauna_core::hex32::decode(split_pin_value(v).0).ok())
    }
    fn set(&self, host: &str, actor_id: [u8; 32]) {
        let mut map = load_local_pins();
        map.insert(host.to_string(), fauna_core::hex32::encode(&actor_id));
        save_local_pins(&map);
    }
    fn rotation_seq(&self, host: &str) -> Option<u64> {
        load_local_pins()
            .get(host)
            .and_then(|v| split_pin_value(v).1)
    }
    fn set_rotation_accepted(&self, host: &str, head: [u8; 32], seq: u64) {
        let mut map = load_local_pins();
        map.insert(
            host.to_string(),
            format!("{}@{seq}", fauna_core::hex32::encode(&head)),
        );
        save_local_pins(&map);
    }
    /// Delete the pin for `host` — the explicit user-approved recovery the
    /// "trust this nest" warning action calls (the browser analogue of
    /// `ssh-keygen -R host`). The next connect re-TOFUs: a changed identity
    /// re-pins to the new one, a withdrawn proof proceeds unpinned.
    fn remove(&self, host: &str) {
        let mut map = load_local_pins();
        if map.remove(host).is_some() {
            save_local_pins(&map);
        }
    }
    /// Not a real atomicity concern here — a single browser tab's JS event loop
    /// can't preempt mid-call — but implemented explicitly (one
    /// load/decide/save round trip) rather than inheriting the trait's
    /// get-then-set default, so this backend never silently regresses to two
    /// separate `localStorage` round trips if the default ever changes.
    fn pin_if_absent(&self, host: &str, actor_id: [u8; 32]) -> Option<[u8; 32]> {
        let mut map = load_local_pins();
        if let Some(existing) = map
            .get(host)
            .and_then(|v| fauna_core::hex32::decode(split_pin_value(v).0).ok())
        {
            return Some(existing);
        }
        map.insert(host.to_string(), fauna_core::hex32::encode(&actor_id));
        save_local_pins(&map);
        None
    }
}

/// Why a first-contact possession proof failed. Every variant means the
/// connection must be torn down before any request rides it
/// (`security.md` § Connection-teardown rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstContactError {
    /// The nest produced no channel binding — it answered without one, or
    /// rejected the handshake kind outright. A verdict, never a degrade: a
    /// box that will not prove its identity is not spoken to (the pre-Track-2
    /// "legacy nest" skip that the native twin
    /// `fauna_anon_client::AnonymousNestClient::graduate_first_contact` once
    /// allowed on the TOFU ladder was removed 2026-09-24 by the compat-remnant
    /// sweep, so every caller hard-fails a rejection the same way).
    BindingRequired,
    /// A transport fault: nothing was proven either way, and the caller may
    /// retry. Distinct from [`Self::BindingRequired`], which is a verdict.
    Transport,
    /// A binding arrived and failed verification.
    Binding(BindingError),
}

impl std::fmt::Display for FirstContactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FirstContactError::BindingRequired => {
                write!(f, "first contact: nest produced no channel binding")
            }
            FirstContactError::Transport => {
                write!(f, "first contact: transport fault before any proof")
            }
            FirstContactError::Binding(e) => write!(f, "first contact: {e}"),
        }
    }
}

impl std::error::Error for FirstContactError {}

/// Make the box on this **pre-identity** connection prove it holds `expected`,
/// by possession of the key alone — the first-contact check for a client that
/// cannot capture the TLS certificate it received.
///
/// # Why this exists
///
/// Native's first contact runs the full Axis-1 channel binding: it compares the
/// nest's signature against the SPKI of the cert *it* received, which is what
/// defeats a relay (`security.md` § Two independent axes). A browser exposes no
/// raw-certificate primitive to WASM, so the wizard's wasm arm cannot do that —
/// and until this helper existed it did **nothing at all**: the identity pasted
/// as a `fauna://claim` URI was stored and never consulted, a silent no-op.
///
/// # What it proves, and what it does not
///
/// The nest signs `served_spki ‖ client_nonce` over a nonce **this client**
/// chose fresh, so the proof is not harvestable offline. Verifying it establishes
/// that the peer holds `expected`'s private key *right now*.
///
/// It does **not** bind that proof to the TLS channel. An attacker who already
/// holds a browser-trusted certificate for the host *and* can reach the real
/// nest may relay the handshake and pass. That residual is inherent to the
/// browser (no received-cert compare is available) and is the same one
/// `verify_cert_binding_possession` documents; native, which can compare, is
/// strictly stronger. What this closes is the far larger hole: any box at all
/// being accepted for a pasted identity it cannot sign for.
#[cfg(feature = "auth-ceremony")]
pub async fn prove_first_contact_identity_possession<R>(
    client: &R,
    expected: [u8; 32],
) -> Result<(), FirstContactError>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

    let mut client_nonce = [0u8; 32];
    getrandom::fill(&mut client_nonce).expect("OS RNG available for first-contact nonce");

    let reply: Result<NestHandshakeReply, R::Error> = client
        .request(
            NEST_HANDSHAKE_KIND,
            NestHandshakeRequest {
                client_nonce: fauna_protocol::ByteBuf::from(client_nonce.to_vec()),
                extra: Default::default(),
            },
        )
        .await;

    let binding = match reply {
        Ok(r) => r.cert_binding.ok_or(FirstContactError::BindingRequired)?,
        // A wire-level rejection is a verdict, not a fault: see
        // `FirstContactError::BindingRequired` for why there is no fallback.
        Err(e) if fauna_protocol::RpcErrorClass::is_rejection(&e) => {
            return Err(FirstContactError::BindingRequired);
        }
        Err(_) => return Err(FirstContactError::Transport),
    };

    let seen = verify_cert_binding_possession(&client_nonce, &binding)
        .map_err(FirstContactError::Binding)?;
    if seen != expected {
        return Err(FirstContactError::Binding(BindingError::IdentityMismatch));
    }
    Ok(())
}

/// Why [`read_login_binding`] yielded no identity to bind. The transport's
/// own error is kept **verbatim** on the two arms that carry one, so the
/// caller classifies it exactly as it classifies the ceremony's own refusals
/// — a degraded nest answers `fauna.nest.outdated` to this opening read as
/// much as to `fauna.auth.challenge`, and it must still reach the update
/// prompt rather than a retry spinner.
#[cfg(feature = "auth-ceremony")]
#[derive(Debug)]
pub enum LoginBindingError<E> {
    /// The nest refused `fauna.auth.nest_handshake` (a wire rejection).
    Refused(E),
    /// A transport fault; nothing was proven either way.
    Transport(E),
    /// The nest answered without a binding — a keyless box proves no
    /// identity, so there is nothing a login signature can bind to.
    NoBinding,
    /// A binding arrived and failed verification: a relay on native (the
    /// served SPKI is not the one this connection received), or a claimed
    /// identity the box cannot sign for.
    Binding(BindingError),
}

#[cfg(feature = "auth-ceremony")]
impl<E: core::fmt::Display> core::fmt::Display for LoginBindingError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "nest identity read refused: {e}"),
            Self::Transport(e) => write!(f, "nest identity read: {e}"),
            Self::NoBinding => f.write_str("nest proved no identity to bind the login to"),
            Self::Binding(e) => write!(f, "nest identity binding: {e}"),
        }
    }
}

/// **The one reader every login signer runs first** (`login.md` § Binding
/// the nest): learn the identity of the box at the far end of this
/// pre-identity connection, so the login signature about to be made —
/// `fauna.auth.{handshake,verify,device_handshake,custody_handshake}` — names
/// exactly the nest it is addressed to and verifies at no other — and the
/// same reader the nest-bound `fauna.setup.nat_mode` commit binds with
/// (`fauna-onboarding-machine`'s `submit_nat_mode`). A `fauna.auth.nest_handshake`
/// over a fresh client nonce, SPKI-compared when the caller captured the
/// received cert — native TLS — and possession-only otherwise. There is
/// **no** "sign the unbound form" degrade: a box that proves no identity
/// gets no signature, and its refusal rides out verbatim so a degraded
/// nest's `fauna.nest.outdated` classifies as it would from the ceremony
/// itself.
///
/// What the identity source defends, per seat: native TLS compares the
/// binding's SPKI against the cert this connection received, so a relaying
/// nest cannot present the real nest's identity over its own channel. Web
/// and plaintext are possession-only; there the caller checks the identity
/// against its pin for this origin **before signing**
/// ([`run_pinned_silent_challenge`], `fauna-wasm`'s `challenge_verify`), so
/// a relay is caught from the second contact on — the same residual
/// [`verify_cert_binding_possession`] documents, and strictly narrower than
/// the unbound form's, which needed no relay at all.
#[cfg(feature = "auth-ceremony")]
pub async fn read_login_binding<R>(
    client: &R,
    captured_spki: Option<&[u8; 32]>,
) -> Result<[u8; 32], LoginBindingError<R::Error>>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

    let mut client_nonce = [0u8; 32];
    getrandom::fill(&mut client_nonce).expect("OS RNG available for login-binding nonce");

    let reply: NestHandshakeReply = client
        .request(
            NEST_HANDSHAKE_KIND,
            NestHandshakeRequest {
                client_nonce: fauna_protocol::ByteBuf::from(client_nonce.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| {
            if fauna_protocol::RpcErrorClass::is_rejection(&e) {
                LoginBindingError::Refused(e)
            } else {
                LoginBindingError::Transport(e)
            }
        })?;
    let binding = reply.cert_binding.ok_or(LoginBindingError::NoBinding)?;
    match captured_spki {
        Some(spki) => verify_cert_binding(spki, &client_nonce, &binding, None),
        None => verify_cert_binding_possession(&client_nonce, &binding),
    }
    .map_err(LoginBindingError::Binding)
}

/// The identity an **already-open** connection is bound to — the one body
/// behind every "which nest is this" decision a client makes after login
/// (`security.md` § Transport trust → the connection-bound identity rule):
/// `pinned`, the origin's TOFU pin the caller read from its own platform's
/// pin store (graduated by the login's SPKI compare on native TLS,
/// possession-verified at every login on web), else — no pin, i.e. a
/// plaintext or first-contact nest — a possession proof over `conn` itself
/// ([`read_login_binding`] without a captured SPKI). **Never
/// `fauna.nest.info`**: that is the nest's own claim about itself, which a
/// box answering the URL with valid TLS can set to anything.
#[cfg(feature = "auth-ceremony")]
pub async fn read_bound_identity<R>(
    conn: &R,
    pinned: Option<[u8; 32]>,
) -> Result<[u8; 32], LoginBindingError<R::Error>>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    match pinned {
        Some(id) => Ok(id),
        None => read_login_binding(conn, None).await,
    }
}

/// The **pinned** flavour of the shared silent-challenge ceremony — the web
/// TOFU-pin model (`docs/goal/architecture/security.md` § Transport trust) folded into `fauna_protocol::auth::run_silent_challenge`:
/// the nest's identity is read off the connection through
/// [`read_login_binding`] (possession-verified over a fresh client nonce) and
/// checked against the `host` pin via [`check_web_nest_identity`] **before**
/// anything is signed — it is the identity the login signature then binds
/// (`login.md` § Binding the nest). A changed or withdrawn identity becomes
/// [`SilentChallengeOutcome::IdentityChanged`] — the caller (the launch
/// machine's wasm connector; `fauna-wasm`'s `challenge_verify_inner` runs the
/// same sequence) surfaces it as the blocking re-trust warning. The verify
/// reply's own `cert_binding` is not consulted here any more: the opening
/// read already proved the same identity on the same connection (a plaintext
/// dev nest, which serves no verify-path binding at all, proves it there
/// too), and the login signature is what holds the nest to it.
///
/// This is the *wasm* detection point. Native apps never call this: their
/// pin rides the connect-time channel binding (full SPKI compare, strictly
/// stronger), and `fauna-launch-machine`'s native connect wrapper maps that
/// connect error to the same outcome variant.
#[cfg(feature = "auth-ceremony")]
pub async fn run_pinned_silent_challenge<R>(
    client: &R,
    secret: &[u8],
    host: &str,
    pins: &dyn NestIdentityPinStore,
) -> fauna_protocol::auth::SilentChallengeOutcome
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    use fauna_protocol::auth::{
        SilentChallengeOutcome, run_silent_challenge, silent_challenge_error,
    };

    // The identity this login binds (`login.md` § Binding the nest), and the
    // pin check on it BEFORE anything is signed: a relaying box that presents
    // the real nest's identity over its own origin is caught here, against
    // the origin's pin, with no signature yet in existence. (Checking the
    // verify reply's binding instead, as this path did before the binding,
    // would let a relay serve its OWN identity on the reply while the
    // signature it forwarded named the real nest.)
    let nest_id = match read_login_binding(client, None).await {
        Ok(id) => id,
        Err(LoginBindingError::Refused(e)) | Err(LoginBindingError::Transport(e)) => {
            return silent_challenge_error(&e);
        }
        Err(e) => {
            return SilentChallengeOutcome::Transient {
                error: e.to_string(),
            };
        }
    };
    match check_web_nest_identity(Some(nest_id), host, pins) {
        Ok(_) => {}
        // A *changed* identity may be a committed deployment-seed rotation:
        // fetch the box's rotation chain over this same connection and re-pin
        // silently when it bridges pinned → the possession-proven identity
        // (`box-recovery.md` § Client acceptance).
        Err(WebIdentityError::Changed { pinned, seen }) => {
            match try_rotation_repin(client, host, pins, pinned, seen).await {
                RotationRepin::Repinned { .. } => {}
                fetched => {
                    return SilentChallengeOutcome::IdentityChanged {
                        host: host.to_string(),
                        pinned_hex: fauna_core::hex32::encode(&pinned),
                        seen_hex: Some(fauna_core::hex32::encode(&seen)),
                        fork: matches!(fetched, RotationRepin::Fork),
                    };
                }
            }
        }
        Err(WebIdentityError::Withdrawn { pinned }) => {
            return SilentChallengeOutcome::IdentityChanged {
                host: host.to_string(),
                pinned_hex: fauna_core::hex32::encode(&pinned),
                seen_hex: None,
                fork: false,
            };
        }
    }

    run_silent_challenge(client, secret, &nest_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use fauna_protocol::ByteBuf;

    /// Build a CertBinding exactly as a nest would — the domain-tagged
    /// `tagged_sig` over `CERT_BINDING_V1 ‖ served_spki ‖ nonce`, through the one shared producer.
    ///
    /// `served_spki` is `&[u8]`, not `&[u8; 32]`: a genuine nest only ever signs
    /// the canonical 32 (or, cert-less, 0) bytes, but `the_split_boundary_is_not_movable`
    /// below needs to build the exact non-canonical wire shape a movable split
    /// makes available — a fixed-width parameter would make that test unwritable.
    fn make_binding(nest: &SigningKey, served_spki: &[u8], nonce: &[u8]) -> CertBinding {
        CertBinding::sign(nest, served_spki, nonce)
    }

    /// A binding whose tagged signature was made by a key other than the
    /// one `nest_actor_id` names — the forgery every verifier must refuse.
    fn make_forged_binding(
        claimed: &SigningKey,
        attacker: &SigningKey,
        served_spki: &[u8],
        nonce: &[u8],
    ) -> CertBinding {
        let mut b = CertBinding::sign(attacker, served_spki, nonce);
        b.nest_actor_id = hex::encode(claimed.verifying_key().to_bytes());
        b
    }

    #[test]
    fn genuine_binding_verifies() {
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);

        let got = verify_cert_binding(&spki, &nonce, &binding, None).expect("verifies");
        assert_eq!(got, nest.verifying_key().to_bytes());
    }

    #[test]
    fn dns_expected_identity_enforced() {
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);

        // Right identity passes, wrong identity is rejected before sig check.
        let right = nest.verifying_key().to_bytes();
        assert!(verify_cert_binding(&spki, &nonce, &binding, Some(&right)).is_ok());
        let wrong = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_cert_binding(&spki, &nonce, &binding, Some(&wrong)),
            Err(BindingError::IdentityMismatch)
        );
    }

    #[test]
    fn substituted_cert_is_rejected() {
        // The MITM terminates TLS with its OWN cert, so the client receives a
        // different SPKI than the nest signed. The binding must reject it.
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let nest_spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let binding = make_binding(&nest, &nest_spki, &nonce);

        let mitm_spki = [0xeeu8; 32];
        assert_eq!(
            verify_cert_binding(&mitm_spki, &nonce, &binding, None),
            Err(BindingError::SpkiMismatch)
        );
    }

    #[test]
    fn forged_signature_is_rejected() {
        // An attacker who knows the SPKI but not the nest key cannot forge the
        // signature: sign with a different key but claim the nest's actor_id.
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let attacker = SigningKey::from_bytes(&[4u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];

        let binding = make_forged_binding(&nest, &attacker, &spki, &nonce);
        assert_eq!(
            verify_cert_binding(&spki, &nonce, &binding, None),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn nonce_replay_across_connections_is_rejected() {
        // A binding captured on one connection (nonce A) must not validate on a
        // new connection that issued a fresh nonce B — the per-connection nonce
        // is what makes the binding connection-specific.
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let binding = make_binding(&nest, &spki, &[0xa1u8; 32]);
        assert_eq!(
            verify_cert_binding(&spki, &[0xb2u8; 32], &binding, None),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn malformed_fields_are_rejected() {
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);

        let mut bad_id = binding.clone();
        bad_id.nest_actor_id = "zz".repeat(32);
        assert_eq!(
            verify_cert_binding(&spki, &nonce, &bad_id, None),
            Err(BindingError::MalformedActorId)
        );

        // A malformed `tagged_sig` is a MalformedSignature.
        let mut malformed = make_binding(&nest, &spki, &nonce);
        malformed.tagged_sig = ByteBuf::from(vec![0u8; 10]); // not 64 bytes
        assert_eq!(
            verify_cert_binding(&spki, &nonce, &malformed, None),
            Err(BindingError::MalformedSignature)
        );
    }

    #[test]
    fn possession_verify_ignores_received_cert() {
        // The web variant has no received SPKI to compare: a genuine binding
        // possession-verifies regardless of what cert the browser actually got.
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let nest_spki = [0x33u8; 32];
        let nonce = [0x44u8; 32];
        let binding = make_binding(&nest, &nest_spki, &nonce);

        let got = verify_cert_binding_possession(&nonce, &binding).expect("possession verifies");
        assert_eq!(got, nest.verifying_key().to_bytes());
    }

    #[test]
    fn possession_verify_rejects_forged_and_replayed() {
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let spki = [0x33u8; 32];
        let nonce = [0x44u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);

        // A fresh nonce (different connection) must not validate the captured sig.
        assert_eq!(
            verify_cert_binding_possession(&[0x55u8; 32], &binding),
            Err(BindingError::SignatureFailed)
        );

        // An attacker lacking the deployment key cannot forge the possession sig.
        let attacker = SigningKey::from_bytes(&[8u8; 32]);
        let forged_binding = make_forged_binding(&nest, &attacker, &spki, &nonce);
        assert_eq!(
            verify_cert_binding_possession(&nonce, &forged_binding),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn possession_verify_accepts_canonical_spki_lengths() {
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let nonce = [0x44u8; 32];
        // 32 — a real fingerprint.
        let spki32 = [0x11u8; 32];
        let binding32 = make_binding(&nest, &spki32, &nonce);
        assert!(verify_cert_binding_possession(&nonce, &binding32).is_ok());
        // 0 — a cert-less nest, `build_identity_binding`'s shape.
        let binding0 = make_binding(&nest, &[], &nonce);
        assert!(verify_cert_binding_possession(&nonce, &binding0).is_ok());
    }

    #[test]
    fn possession_verify_rejects_a_non_canonical_spki_length() {
        // The verifying end must not let the wire `spki_sha256` field pick its
        // own length — see `the_split_boundary_is_not_movable` below for why.
        // 32 (a real fingerprint) and 0 (cert-less) are the only legitimate
        // widths; every other length is refused BEFORE the signature check.
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let nonce = [0x44u8; 32];
        for bad_len in [1usize, 4, 31, 33, 36, 64, 96] {
            let bad_spki = vec![0x99u8; bad_len];
            let binding = make_binding(&nest, &bad_spki, &nonce);
            assert_eq!(
                verify_cert_binding_possession(&nonce, &binding),
                Err(BindingError::MalformedSpki),
                "a {bad_len}-byte spki_sha256 must be refused before verifying"
            );
        }
    }

    /// **The split-boundary demo, permanently pinned.** Before
    /// `possession_verify_rejects_a_non_canonical_spki_length`'s length check, the
    /// verifying end chose where the `spki_sha256 ‖ nonce` boundary fell purely
    /// from the wire field's length — so a genuine 3-part verify-path message
    /// (`spki(32) ‖ challenge(32) ‖ client(32)`) could be re-presented as a 2-part
    /// one (`spki(64) ‖ nonce(32)`, with the 64-byte "spki" being `spki(32) ‖
    /// challenge(32)`) and still verify: same signature, genuine, only the
    /// claimed split moved. This test is the reason the length check is real
    /// coverage, not a defensive no-op — it went RED against pre-fix code
    /// (`verify_cert_binding_possession` returned `Ok` here), confirmed before
    /// the fix landed.
    #[test]
    fn the_split_boundary_is_not_movable() {
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let spki = [0x11u8; 32];
        let p = [0x22u8; 32];
        let n = [0x33u8; 32];

        // Sign the genuine 3-part verify-path message: spki ‖ challenge(p) ‖ client(n).
        let mut combined = p.to_vec();
        combined.extend_from_slice(&n);
        let genuine = make_binding(&nest, &spki, &combined);

        // Re-present the SAME signature with the split moved: spki_sha256 :=
        // spki ‖ p (64 bytes), nonce := n alone. Byte-identical message, genuine
        // signature — only the claimed boundary changed.
        let mut moved_spki = spki.to_vec();
        moved_spki.extend_from_slice(&p);
        let mut moved = genuine.clone();
        moved.spki_sha256 = ByteBuf::from(moved_spki);

        assert_eq!(
            verify_cert_binding_possession(&n, &moved),
            Err(BindingError::MalformedSpki),
            "the split must not be movable — a re-split binding must not verify"
        );
    }

    /// A refused proof and an absent one land in the same `Unprovable` bucket on
    /// an unpinned (first-contact) origin — DECLARED, ACCEPTED behavior on the
    /// unrooted/TOFU ladder (`docs/goal/architecture/security.md` § Transport
    /// trust; same reasoning as the ratified DNS/TOFU kind-rejection residual,
    /// `:469-475`), not a live defect: web has no root to authenticate ANY
    /// first-contact signal against, so a corrupted proof is exactly as cheap
    /// for an active attacker to send as no proof at all. This test pins the
    /// helper's collapse so a change to it is a deliberate one.
    ///
    /// This pins the *rule* in isolation — it calls `verify_cert_binding_possession`/
    /// `check_web_nest_identity` directly and never drives a production call
    /// site. Since the login binding (2026-09-23) no web call site consults the
    /// verify reply's proof at all — the verdict rides the opening
    /// `read_login_binding`, where a corrupted proof is a hard refusal (no
    /// identity → no login signature) — so this helper's collapse is the
    /// helper's own contract, not a production posture.
    #[test]
    fn a_refused_verify_path_binding_collapses_to_unprovable_when_unpinned() {
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let challenge_nonce = [0x22u8; 32];
        let client_nonce = [0x33u8; 32];
        let mut combined = challenge_nonce.to_vec();
        combined.extend_from_slice(&client_nonce);
        let mut binding = make_binding(&nest, &spki, &combined);
        // A corrupted tagged_sig is refused outright.
        let mut corrupted = binding.tagged_sig.to_vec();
        corrupted[0] ^= 0xFF;
        binding.tagged_sig = ByteBuf::from(corrupted);

        // The production call sites all collapse this exact failure the
        // same way.
        let seen = verify_cert_binding_possession(&combined, &binding).ok();
        assert_eq!(seen, None, "a corrupted binding must not possession-verify");

        let pins = MemoryPinStore::new();
        let origin = "https://nest.example";
        assert_eq!(
            check_web_nest_identity(seen, origin, &pins),
            Ok(WebIdentityOutcome::Unprovable),
            "on an unpinned origin a refused proof reads the same as no proof at all"
        );
        assert_eq!(pins.get(origin), None, "an unprovable connect pins nothing");
    }

    // ── cert-binding domain separation (TRACK E, context A) ──

    /// An UNTAGGED signature over the same bytes — the pre-tag `sig` half a
    /// pre-2026-06-30 nest emitted — placed in the `tagged_sig` slot does not
    /// verify: the verifier requires the tag, and since the compat-remnant
    /// sweep (2026-09-24) has no untagged leg to fall back to. Flipped from
    /// `legacy_binding_without_tagged_sig_still_verifies`.
    #[test]
    fn an_untagged_signature_is_refused() {
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let mut msg = spki.to_vec();
        msg.extend_from_slice(&nonce);
        let mut untagged = make_binding(&nest, &spki, &nonce);
        untagged.tagged_sig = ByteBuf::from(nest.sign(&msg).to_bytes().to_vec());

        assert_eq!(
            verify_cert_binding(&spki, &nonce, &untagged, None),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn a_corrupted_tagged_sig_is_refused() {
        // : a tagged_sig that fails to verify is corruption, a nest
        // bug, or tampering, and is refused outright.
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let mut binding = make_binding(&nest, &spki, &nonce);
        let mut corrupted = binding.tagged_sig.to_vec();
        corrupted[0] ^= 0xFF;
        binding.tagged_sig = ByteBuf::from(corrupted);

        assert_eq!(
            verify_cert_binding(&spki, &nonce, &binding, None),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn cross_context_signature_cannot_verify_as_cert_binding() {
        // The structural guarantee of key-material-hierarchy.md #8: a signature the
        // genuine nest key made in ANOTHER deployment-key context (here the live
        // federation-handshake context) can never be reinterpreted as a
        // cert-binding. We place a FEDERATION-tagged signature in
        // `tagged_sig`; it does not match the cert-binding
        // message the verifier reconstructs, so it fails. (This used the outbox tag
        // until that context was retired — post forwarding is channel-authed and
        // nest-signs nothing; a live context makes the assertion stronger.)
        let nest = SigningKey::from_bytes(&[3u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];

        let mut msg = Vec::new();
        msg.extend_from_slice(&spki);
        msg.extend_from_slice(&nonce);
        let cross_context_msg = fauna_protocol::sig_domain::domain_separated(
            fauna_protocol::sig_domain::FEDERATION_HELLO_V1,
            &msg,
        );
        let cross = nest.sign(&cross_context_msg);
        let binding = CertBinding {
            extra: Default::default(),
            nest_actor_id: hex::encode(nest.verifying_key().to_bytes()),
            spki_sha256: ByteBuf::from(spki.to_vec()),
            tagged_sig: ByteBuf::from(cross.to_bytes().to_vec()),
        };
        assert_eq!(
            verify_cert_binding(&spki, &nonce, &binding, None),
            Err(BindingError::SignatureFailed)
        );
    }

    #[test]
    fn pre_resolved_root_requires_exact_match() {
        // Same check whether the root came from DNS `self=` or a client-injected
        // deployment seed — the source is the caller's concern.
        let pins = MemoryPinStore::new();
        let id = [0x44u8; 32];
        assert_eq!(
            check_identity_root(&id, IdentityRoot::PreResolved(id), &pins),
            Ok(IdentityOutcome::Verified)
        );
        assert_eq!(
            check_identity_root(&id, IdentityRoot::PreResolved([0x55u8; 32]), &pins),
            Err(IdentityError::RootMismatch)
        );
    }

    #[test]
    fn tofu_pins_then_confirms_then_warns_on_change() {
        let pins = MemoryPinStore::new();
        let host = "pi.local";
        let first = [0x44u8; 32];

        // First connect pins.
        assert_eq!(
            check_identity_root(&first, IdentityRoot::Tofu { host }, &pins),
            Ok(IdentityOutcome::Pinned)
        );
        assert_eq!(pins.get(host), Some(first));

        // Same identity later → verified, no re-pin churn.
        assert_eq!(
            check_identity_root(&first, IdentityRoot::Tofu { host }, &pins),
            Ok(IdentityOutcome::Verified)
        );

        // Changed identity → loud error, pin unchanged.
        let imposter = [0x99u8; 32];
        assert_eq!(
            check_identity_root(&imposter, IdentityRoot::Tofu { host }, &pins),
            Err(IdentityError::PinChanged {
                pinned: first,
                seen: imposter
            })
        );
        assert_eq!(pins.get(host), Some(first), "pin must not silently change");
    }

    /// `pin_if_absent` must be a single atomic check-and-set, not the trait
    /// default's separate `get()` then `set()` — the TOCTOU gap that let a
    /// stale, still-in-flight TOFU mint from one connection silently clobber a
    /// newer decision from another (`test_nest_identity_pin_post_auth.py`
    /// caught it intermittently under load).
    /// Races `N` threads to TOFU-pin the SAME host with `N` DISTINCT
    /// identities: with a real lock covering the whole decision exactly one
    /// call may report `None` (the winner), and every other call — regardless
    /// of scheduling order — must report `Some(<that same winner>)`. A
    /// get-then-set default under this same harness intermittently lets a
    /// second thread also observe `None` (both raced past the `get`) or
    /// disagree on which identity won, which is exactly the silent-clobber
    /// shape the production bug hit — but only on SOME schedules, so a single
    /// race is a coin toss, not a detector. Looping `ROUNDS` fresh
    /// races (a new store per round, so no pin survives across rounds) turns
    /// "reds 1 run in 3" into "reds effectively every run": a get-then-set
    /// regression must win every single round to pass, at roughly
    /// `(2/3)^ROUNDS` odds.
    #[test]
    fn pin_if_absent_is_atomic_under_concurrent_first_contact() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        const N: usize = 32;
        const ROUNDS: usize = 200;

        for round in 0..ROUNDS {
            // Fresh store each round: a pin left over from a prior round would
            // make every later round's race a no-op (every thread loses to
            // the earlier pin), passing even a broken get-then-set default.
            let pins = Arc::new(MemoryPinStore::new());
            let host = "race.local";
            // Every thread starts its `pin_if_absent` call in the same instant —
            // a barrier makes the race actually contend instead of trusting OS
            // scheduling jitter to interleave them.
            let barrier = Arc::new(Barrier::new(N));

            let handles: Vec<_> = (0..N)
                .map(|i| {
                    let pins = Arc::clone(&pins);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        let mut actor_id = [0u8; 32];
                        actor_id[0] = i as u8;
                        barrier.wait();
                        (actor_id, pins.pin_if_absent(host, actor_id))
                    })
                })
                .collect();

            let results: Vec<([u8; 32], Option<[u8; 32]>)> =
                handles.into_iter().map(|h| h.join().unwrap()).collect();

            let winners: Vec<_> = results
                .iter()
                .filter(|(_, prior)| prior.is_none())
                .collect();
            assert_eq!(
                winners.len(),
                1,
                "round {round}: exactly one call may mint the first pin; got {} winners: {results:?}",
                winners.len()
            );
            let winning_id = winners[0].0;
            assert_eq!(
                pins.get(host),
                Some(winning_id),
                "round {round}: the store must hold the winner"
            );
            for (candidate_id, prior) in &results {
                if *candidate_id == winning_id {
                    continue;
                }
                assert_eq!(
                    *prior,
                    Some(winning_id),
                    "round {round}: every losing call must see the SAME winner the store \
                     holds, never its own id nor a different loser's"
                );
            }
        }
    }

    #[test]
    fn tofu_strict_never_mints_and_verifies_existing_pins() {
        // A pin-consumer process (File Provider extension, background agent):
        // no pin → PinRequired, and crucially nothing is minted; an existing
        // pin verifies / warns exactly like the interactive arm.
        let pins = MemoryPinStore::new();
        let host = "pi.local";
        let id = [0x44u8; 32];

        assert_eq!(
            check_identity_root(&id, IdentityRoot::TofuStrict { host }, &pins),
            Err(IdentityError::PinRequired)
        );
        assert_eq!(pins.get(host), None, "strict arm must never mint a pin");

        // Once the interactive app has pinned, the consumer verifies…
        pins.set(host, id);
        assert_eq!(
            check_identity_root(&id, IdentityRoot::TofuStrict { host }, &pins),
            Ok(IdentityOutcome::Verified)
        );
        // …and a changed identity still warns loudly.
        let imposter = [0x99u8; 32];
        assert_eq!(
            check_identity_root(&imposter, IdentityRoot::TofuStrict { host }, &pins),
            Err(IdentityError::PinChanged {
                pinned: id,
                seen: imposter
            })
        );
    }

    #[test]
    fn web_identity_pins_then_verifies_then_warns_on_change() {
        let pins = MemoryPinStore::new();
        let origin = "https://nest.example";
        let first = [0x44u8; 32];

        // First provable connect pins silently.
        assert_eq!(
            check_web_nest_identity(Some(first), origin, &pins),
            Ok(WebIdentityOutcome::Pinned)
        );
        assert_eq!(pins.get(origin), Some(first));

        // Same identity later → verified, no warning.
        assert_eq!(
            check_web_nest_identity(Some(first), origin, &pins),
            Ok(WebIdentityOutcome::Verified)
        );

        // Changed identity → loud warning, pin unchanged.
        let imposter = [0x99u8; 32];
        assert_eq!(
            check_web_nest_identity(Some(imposter), origin, &pins),
            Err(WebIdentityError::Changed {
                pinned: first,
                seen: imposter
            })
        );
        assert_eq!(
            pins.get(origin),
            Some(first),
            "pin must not silently change"
        );
    }

    #[test]
    fn web_identity_unprovable_first_connect_proceeds_without_pinning() {
        // A plaintext / dev nest sends no binding → no pin, no warning, no pin
        // written (so the e2e plaintext nest is unaffected).
        let pins = MemoryPinStore::new();
        let origin = "http://127.0.0.1:8080";
        assert_eq!(
            check_web_nest_identity(None, origin, &pins),
            Ok(WebIdentityOutcome::Unprovable)
        );
        assert_eq!(pins.get(origin), None, "an unprovable connect pins nothing");
    }

    #[test]
    fn web_identity_withdrawn_proof_after_pin_warns() {
        // Downgrade protection: once pinned, a connect with no valid binding
        // (attacker omits it, or the proof failed possession-verify → seen=None)
        // must warn, not silently bypass the pin.
        let pins = MemoryPinStore::new();
        let origin = "https://nest.example";
        let pinned = [0x44u8; 32];
        pins.set(origin, pinned);
        assert_eq!(
            check_web_nest_identity(None, origin, &pins),
            Err(WebIdentityError::Withdrawn { pinned })
        );
        assert_eq!(pins.get(origin), Some(pinned));
    }

    #[test]
    fn pin_value_splits_plain_and_seq_forms() {
        assert_eq!(split_pin_value("abcd"), ("abcd", None));
        assert_eq!(split_pin_value("abcd@7"), ("abcd", Some(7)));
        // A garbled seq keeps the id readable (the pin survives; the fork
        // detection just disarms — strictly safer than dropping the pin).
        assert_eq!(split_pin_value("abcd@x"), ("abcd", None));
    }
}

// ── Deployment-seed rotation acceptance (`box-recovery.md` § Client acceptance) ──

#[cfg(test)]
mod rotation_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use ed25519_dalek::SigningKey;
    use fauna_protocol::nest_rotation::{NestRotation, SignedNestRotation};

    fn key(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }
    fn id(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }
    fn hop(old: &SigningKey, new: &SigningKey, seq: u64) -> SignedNestRotation {
        NestRotation {
            old_nest_actor_id: id(old),
            new_nest_actor_id: id(new),
            seq,
            rotated_at: 1_800_000_000 + seq as i64,
        }
        .sign(old, new)
        .unwrap()
    }

    // ---- evaluate_rotation_bridge: the pure verdict ----

    #[test]
    fn a_valid_chain_accepts_from_the_pinned_identity_to_the_live_head() {
        let (a, b, c) = (key(1), key(2), key(3));
        let chain = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        // Single hop for a client pinned one rotation back…
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&b), &id(&c), None),
            RotationBridge::Accepted { seq: 2 }
        );
        // …and the full walk for one pinned at the very first identity.
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&a), &id(&c), None),
            RotationBridge::Accepted { seq: 2 }
        );
    }

    #[test]
    fn a_chain_that_reaches_a_different_head_than_the_live_binding_never_moves_a_pin() {
        // The harvested-chain rule: the walk must land on exactly the identity
        // the LIVE binding proved. A genuine chain ending at B is worthless to
        // an attacker presenting C.
        let (a, b, c) = (key(1), key(2), key(4));
        let chain = vec![hop(&a, &b, 1)];
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&a), &id(&c), None),
            RotationBridge::NoBridge
        );
    }

    #[test]
    fn an_empty_chain_is_the_ordinary_warning_never_a_hard_verdict() {
        // An un-rotated box serves nothing — fall through to today's surface.
        let (a, b) = (key(1), key(2));
        assert_eq!(
            evaluate_rotation_bridge(&[], &id(&a), &id(&b), None),
            RotationBridge::NoBridge
        );
    }

    #[test]
    fn a_presented_superseded_ancestor_is_refused() {
        // The eviction: a client converged on B meets something presenting the
        // superseded A. The genuine chain runs A→B, so no walk from B reaches
        // A — the ancestor never silently authenticates again.
        let (a, b) = (key(1), key(2));
        let chain = vec![hop(&a, &b, 1)];
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&b), &id(&a), None),
            RotationBridge::NoBridge
        );
        // …and for a client whose pin was itself chain-accepted, the same
        // encounter is FORK-grade: it cryptographically accepted history the
        // presenter now contradicts.
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&b), &id(&a), Some(1)),
            RotationBridge::Fork
        );
    }

    #[test]
    fn a_second_distinct_head_at_an_accepted_seq_is_fork_evidence() {
        // The discriminating fork shape: this client accepted B as the head
        // written at seq 1, and the served chain BOTH contradicts that (a
        // seq-1 hop naming X) AND still walks B → C — the exact chain a thief
        // of the pre-rotation seed can mint, since B's key signs the walk.
        // Without the accepted-seq contradiction check this would ACCEPT;
        // fork clause B is what makes the contradiction disqualifying.
        let (a, b, c, x) = (key(1), key(2), key(3), key(5));
        let contradicting = vec![hop(&a, &x, 1), hop(&b, &c, 2)];
        assert_eq!(
            evaluate_rotation_bridge(&contradicting, &id(&b), &id(&c), Some(1)),
            RotationBridge::Fork
        );
        // Control: the same chain against a pin that was NEVER chain-accepted
        // has no accepted seq to contradict — the walk stands on its own.
        assert_eq!(
            evaluate_rotation_bridge(&contradicting, &id(&b), &id(&c), None),
            RotationBridge::Accepted { seq: 2 }
        );
    }

    #[test]
    fn a_chain_that_cannot_extend_a_chain_accepted_head_is_fork_evidence() {
        // Fork clause A: the pin was chain-accepted (seq 2), and the served
        // chain has no hop moving it forward — a legit append-only log can
        // always extend what this client accepted.
        let (a, x, c) = (key(1), key(7), key(3));
        let unrelated = vec![hop(&a, &x, 1)];
        assert_eq!(
            evaluate_rotation_bridge(&unrelated, &id(&c), &id(&x), Some(2)),
            RotationBridge::Fork
        );
    }

    #[test]
    fn a_chain_accepted_pin_still_extends_forward() {
        // The ordinary next rotation for an already-converged client: the
        // accepted-seq hop matches the pin, and the walk continues past it.
        let (a, b, c) = (key(1), key(2), key(3));
        let chain = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        assert_eq!(
            evaluate_rotation_bridge(&chain, &id(&b), &id(&c), Some(1)),
            RotationBridge::Accepted { seq: 2 }
        );
    }

    #[test]
    fn a_forged_hop_never_bridges() {
        // An attacker without the pinned key cannot mint the continuity
        // license: a hop whose old_sig is by the wrong key fails the walk.
        let (a, b, e) = (key(1), key(2), key(8));
        let mut forged = hop(&a, &b, 1);
        forged.statement.old_nest_actor_id = id(&e); // claims to move E's pin
        assert_eq!(
            evaluate_rotation_bridge(&[forged], &id(&e), &id(&b), None),
            RotationBridge::NoBridge
        );
    }

    // ---- try_rotation_repin: fetch + decide + move the pin ----

    /// Serves a canned rotation chain (counting fetches), or errors.
    struct ChainRequester {
        chain: Option<Vec<SignedNestRotation>>,
        fetches: AtomicUsize,
    }

    impl ChainRequester {
        fn serving(chain: Vec<SignedNestRotation>) -> Self {
            Self {
                chain: Some(chain),
                fetches: AtomicUsize::new(0),
            }
        }
        fn failing() -> Self {
            Self {
                chain: None,
                fetches: AtomicUsize::new(0),
            }
        }
    }

    #[derive(Debug)]
    struct MockErr;
    impl std::fmt::Display for MockErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("mock transport fault")
        }
    }

    impl fauna_protocol::RpcRequester for ChainRequester {
        type Error = MockErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, MockErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(
                kind,
                fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND,
                "the acceptance path fetches exactly the pre-identity chain kind"
            );
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let chain = self.chain.as_ref().ok_or(MockErr)?.clone();
            let reply = fauna_protocol::nest_rotation::RotationChainReply {
                chain,
                extra: Default::default(),
            };
            Ok(serde_json::from_value(serde_json::to_value(reply).unwrap()).unwrap())
        }
    }

    #[tokio::test]
    async fn acceptance_moves_the_pin_and_stores_the_seq() {
        let (a, b) = (key(1), key(2));
        let pins = MemoryPinStore::new();
        let host = "rotated.local";
        pins.set(host, id(&a));
        let client = ChainRequester::serving(vec![hop(&a, &b, 1)]);

        let got = try_rotation_repin(&client, host, &pins, id(&a), id(&b)).await;
        assert_eq!(got, RotationRepin::Repinned { seq: 1 });
        assert_eq!(pins.get(host), Some(id(&b)), "pin names the head");
        assert_eq!(pins.rotation_seq(host), Some(1), "seq stored beside it");
    }

    #[tokio::test]
    async fn fork_and_no_bridge_leave_the_pin_untouched() {
        let (a, b, x, y) = (key(1), key(2), key(5), key(6));
        let host = "forked.local";

        // Fork: chain-accepted pin contradicted at its accepted seq.
        let pins = MemoryPinStore::new();
        pins.set_rotation_accepted(host, id(&b), 1);
        let client = ChainRequester::serving(vec![hop(&a, &x, 1), hop(&x, &y, 2)]);
        let got = try_rotation_repin(&client, host, &pins, id(&b), id(&y)).await;
        assert_eq!(got, RotationRepin::Fork);
        assert_eq!(pins.get(host), Some(id(&b)), "fork must not move the pin");

        // NoBridge: empty chain against an ordinary pin.
        let pins = MemoryPinStore::new();
        pins.set(host, id(&a));
        let client = ChainRequester::serving(vec![]);
        let got = try_rotation_repin(&client, host, &pins, id(&a), id(&b)).await;
        assert!(matches!(got, RotationRepin::NoBridge { .. }), "{got:?}");
        assert_eq!(pins.get(host), Some(id(&a)));
    }

    #[tokio::test]
    async fn a_fetch_failure_degrades_to_the_ordinary_warning() {
        // An unknown-kind refusal is an ordinary one;
        // any transport fault behaves the same — never a hard verdict.
        let (a, b) = (key(1), key(2));
        let pins = MemoryPinStore::new();
        let host = "unreachable-nest.local";
        pins.set(host, id(&a));
        let client = ChainRequester::failing();
        let got = try_rotation_repin(&client, host, &pins, id(&a), id(&b)).await;
        assert!(matches!(got, RotationRepin::NoBridge { .. }), "{got:?}");
        assert_eq!(pins.get(host), Some(id(&a)));
    }

    /// Read-only wrapper — the pin-consumer double.
    struct ReadOnly(MemoryPinStore);
    impl NestIdentityPinStore for ReadOnly {
        fn get(&self, host: &str) -> Option<[u8; 32]> {
            self.0.get(host)
        }
        fn set(&self, _host: &str, _actor_id: [u8; 32]) {}
        fn remove(&self, _host: &str) {}
        fn read_only(&self) -> bool {
            true
        }
    }

    // ---- run_pinned_silent_challenge: the WEB acceptance arm ----
    //
    // This is the web Changed-arm's ONLY coverage by design: the e2e web
    // origin is never TLS, so a live journey can only reach the Withdrawn arm
    // (`tests/e2e-unified/tests/test_nest_identity_pin.py`'s module docs).

    /// A rotated nest behind the full challenge/verify ceremony: answers
    /// challenge/verify as the NEW key's holder (binding folded over the
    /// caller's fresh client nonce, exactly like a current nest) and serves
    /// the rotation chain.
    #[cfg(feature = "auth-ceremony")]
    struct RotatedCeremonyNest {
        new: SigningKey,
        chain: Vec<SignedNestRotation>,
        spki: [u8; 32],
        challenge_nonce: [u8; 32],
    }

    #[cfg(feature = "auth-ceremony")]
    impl fauna_protocol::RpcRequester for RotatedCeremonyNest {
        type Error = core::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::auth::{CertBinding, ChallengeReply, VerifyReply, VerifyRequest};
            let payload = serde_json::to_value(payload).unwrap();
            let reply = match kind {
                // The opening identity read every login binds (`login.md`
                // § Binding the nest): the NEW key proves itself.
                k if k == fauna_protocol::auth::NEST_HANDSHAKE_KIND => {
                    let req: fauna_protocol::auth::NestHandshakeRequest =
                        serde_json::from_value(payload).unwrap();
                    serde_json::to_value(fauna_protocol::auth::NestHandshakeReply {
                        cert_binding: Some(CertBinding::sign(
                            &self.new,
                            &self.spki,
                            req.client_nonce.as_slice(),
                        )),
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                "fauna.auth.challenge" => serde_json::to_value(ChallengeReply {
                    nonce: hex::encode(self.challenge_nonce),
                    expires_in: 300,
                    expires_at: 2_000_000_000,
                    extra: Default::default(),
                })
                .unwrap(),
                "fauna.auth.verify" => {
                    let req: VerifyRequest = serde_json::from_value(payload).unwrap();
                    assert_eq!(
                        req.nest_id,
                        hex::encode(self.new.verifying_key().to_bytes()),
                        "the verify must name the identity the opening read proved"
                    );
                    let mut nonce = self.challenge_nonce.to_vec();
                    if let Some(client_nonce) = req.client_nonce {
                        nonce.extend_from_slice(client_nonce.as_slice());
                    }
                    serde_json::to_value(VerifyReply {
                        token: "tok".into(),
                        token_id: "0011223344556677".into(),
                        handle: "alice".into(),
                        domain: "nest.example".into(),
                        tier: "free".into(),
                        expires_at: 2_000_000_000,
                        expires_in: 300,
                        cert_binding: Some(Box::new(CertBinding::sign(
                            &self.new, &self.spki, &nonce,
                        ))),
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                k if k == fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND => {
                    serde_json::to_value(fauna_protocol::nest_rotation::RotationChainReply {
                        chain: self.chain.clone(),
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                other => panic!("unexpected kind {other}"),
            };
            Ok(serde_json::from_value(reply).unwrap())
        }
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn the_web_launch_path_silently_repins_through_a_verified_chain() {
        use fauna_protocol::auth::SilentChallengeOutcome;
        let (old, new) = (key(11), key(12));
        let host = "https://rotated-web.example";
        let pins = MemoryPinStore::new();
        pins.set(host, id(&old)); // pinned before the box rotated
        let nest = RotatedCeremonyNest {
            chain: vec![hop(&old, &new, 1)],
            new,
            spki: [0x42u8; 32],
            challenge_nonce: [0x24u8; 32],
        };

        let outcome = run_pinned_silent_challenge(&nest, &[9u8; 32], host, &pins).await;
        assert!(
            matches!(outcome, SilentChallengeOutcome::Success(_)),
            "a committed rotation must not surface the identity-changed warning, got {outcome:?}"
        );
        assert_eq!(pins.get(host), Some(id(&key(12)))); // the head
        assert_eq!(pins.rotation_seq(host), Some(1));
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn the_web_launch_path_marks_fork_evidence_and_keeps_the_warning_otherwise() {
        use fauna_protocol::auth::SilentChallengeOutcome;
        let (old, new, x) = (key(11), key(12), key(13));
        let host = "https://forked-web.example";

        // Fork: this client chain-accepted `old` at seq 1; the box's served
        // history names X there instead — while still walking old → new, the
        // shape only a contradiction check refuses.
        let pins = MemoryPinStore::new();
        pins.set_rotation_accepted(host, id(&old), 1);
        let nest = RotatedCeremonyNest {
            chain: vec![hop(&key(10), &x, 1), hop(&old, &new, 2)],
            new: new.clone(),
            spki: [0x42u8; 32],
            challenge_nonce: [0x24u8; 32],
        };
        let outcome = run_pinned_silent_challenge(&nest, &[9u8; 32], host, &pins).await;
        assert!(
            matches!(
                outcome,
                SilentChallengeOutcome::IdentityChanged { fork: true, .. }
            ),
            "fork evidence must ride the warning, got {outcome:?}"
        );
        assert_eq!(pins.get(host), Some(id(&old)), "fork never moves the pin");

        // No chain at all: today's warning, fork: false.
        let host2 = "https://plain-changed.example";
        pins.set(host2, id(&old));
        let nest = RotatedCeremonyNest {
            chain: vec![],
            new,
            spki: [0x42u8; 32],
            challenge_nonce: [0x24u8; 32],
        };
        let outcome = run_pinned_silent_challenge(&nest, &[9u8; 32], host2, &pins).await;
        assert!(
            matches!(
                outcome,
                SilentChallengeOutcome::IdentityChanged { fork: false, .. }
            ),
            "no bridge keeps the ordinary warning, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_pin_consumer_process_never_accepts_a_rotation() {
        // A background agent follows the app's trust decisions — it neither
        // fetches nor moves anything; it keeps failing until the shared store
        // shows the new head (the TofuStrict convergence model).
        let (a, b) = (key(1), key(2));
        let host = "consumer.local";
        let inner = MemoryPinStore::new();
        inner.set(host, id(&a));
        let pins = ReadOnly(inner);
        let client = ChainRequester::serving(vec![hop(&a, &b, 1)]);

        let got = try_rotation_repin(&client, host, &pins, id(&a), id(&b)).await;
        assert!(matches!(got, RotationRepin::NoBridge { .. }), "{got:?}");
        assert_eq!(pins.get(host), Some(id(&a)), "consumer pin untouched");
        assert_eq!(
            client.fetches.load(Ordering::SeqCst),
            0,
            "a consumer does not even fetch the chain"
        );
    }

    /// A nest whose OPENING identity read is genuine but whose verify reply
    /// carries a **present but forged** `tagged_sig`. Drives the real
    /// production entry point (`run_pinned_silent_challenge`): since the login
    /// binding, the web verdict rides the opening read alone (`login.md`
    /// § Binding the nest) and the verify reply's proof is not consulted — the
    /// nest is held to the identity it proved by the signature that names it.
    #[cfg(feature = "auth-ceremony")]
    struct CorruptedProofNest {
        nest: SigningKey,
        spki: [u8; 32],
        challenge_nonce: [u8; 32],
    }

    #[cfg(feature = "auth-ceremony")]
    impl fauna_protocol::RpcRequester for CorruptedProofNest {
        type Error = core::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::ByteBuf;
            use fauna_protocol::auth::{CertBinding, ChallengeReply, VerifyReply, VerifyRequest};
            let payload = serde_json::to_value(payload).unwrap();
            let reply = match kind {
                // The opening identity read: a GENUINE proof (the forgery under
                // test is on the verify reply's binding, not here).
                k if k == fauna_protocol::auth::NEST_HANDSHAKE_KIND => {
                    let req: fauna_protocol::auth::NestHandshakeRequest =
                        serde_json::from_value(payload).unwrap();
                    serde_json::to_value(fauna_protocol::auth::NestHandshakeReply {
                        cert_binding: Some(CertBinding::sign(
                            &self.nest,
                            &self.spki,
                            req.client_nonce.as_slice(),
                        )),
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                "fauna.auth.challenge" => serde_json::to_value(ChallengeReply {
                    nonce: hex::encode(self.challenge_nonce),
                    expires_in: 300,
                    expires_at: 2_000_000_000,
                    extra: Default::default(),
                })
                .unwrap(),
                "fauna.auth.verify" => {
                    let req: VerifyRequest = serde_json::from_value(payload).unwrap();
                    let mut nonce = self.challenge_nonce.to_vec();
                    if let Some(client_nonce) = req.client_nonce {
                        nonce.extend_from_slice(client_nonce.as_slice());
                    }
                    // The genuine domain-tagged signature, then flip a bit —
                    // present, but forged: its failure is fatal.
                    let mut binding = CertBinding::sign(&self.nest, &self.spki, &nonce);
                    let mut tagged_sig = binding.tagged_sig.to_vec();
                    tagged_sig[0] ^= 0xFF;
                    binding.tagged_sig = ByteBuf::from(tagged_sig);
                    serde_json::to_value(VerifyReply {
                        token: "tok".into(),
                        token_id: "0011223344556677".into(),
                        handle: "alice".into(),
                        domain: "nest.example".into(),
                        tier: "free".into(),
                        expires_at: 2_000_000_000,
                        expires_in: 300,
                        cert_binding: Some(Box::new(binding)),
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                other => panic!("unexpected kind {other}"),
            };
            Ok(serde_json::from_value(reply).unwrap())
        }
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn the_web_launch_path_pins_the_identity_the_opening_read_proved() {
        use fauna_protocol::auth::SilentChallengeOutcome;
        let host = "https://corrupted-web.example";
        let pins = MemoryPinStore::new(); // unpinned: first contact
        let nest = CorruptedProofNest {
            nest: key(21),
            spki: [0x42u8; 32],
            challenge_nonce: [0x24u8; 32],
        };

        let outcome = run_pinned_silent_challenge(&nest, &[9u8; 32], host, &pins).await;
        assert!(
            matches!(outcome, SilentChallengeOutcome::Success(_)),
            "the identity was proven and pinned by the opening read; the verify \
             reply's forged proof is not consulted (`login.md` § Binding the \
             nest), got {outcome:?}"
        );
        assert_eq!(
            pins.get(host),
            Some(id(&key(21))),
            "the pin names the identity the OPENING read proved — the one the \
             login signature bound — never anything from the verify reply"
        );
    }

    // ---- prove_first_contact_identity_possession: the WASM claim-pin arm ----
    //
    // A browser cannot hand WASM the SPKI of the cert it
    // received, so the wizard's wasm arm cannot run native's SPKI compare. It
    // can still make the box PROVE possession of the pasted `fauna://claim`
    // identity over a fresh client nonce — strictly more than the nothing it
    // did before, when the pin was stored and never consulted.

    /// A nest answering `fauna.auth.nest_handshake`, binding over
    /// `spki ‖ client_nonce` exactly as a current nest does.
    #[cfg(feature = "auth-ceremony")]
    struct HandshakeNest {
        /// The key that actually signs.
        signer: SigningKey,
        /// The identity the reply *claims* — normally `signer`'s, but a forging
        /// box claims one it cannot sign for.
        claims: Option<[u8; 32]>,
        spki: [u8; 32],
        /// Answer with no `cert_binding` (the keyless / plain-HTTP nest).
        omit_binding: bool,
        /// Reject the kind outright (a MITM refusing it, or a box that does
        /// not route it).
        reject: bool,
    }

    #[cfg(feature = "auth-ceremony")]
    impl HandshakeNest {
        fn serving(signer: SigningKey) -> Self {
            Self {
                signer,
                claims: None,
                spki: [7u8; 32],
                omit_binding: false,
                reject: false,
            }
        }
    }

    #[cfg(feature = "auth-ceremony")]
    #[derive(Debug)]
    struct Rejected;

    #[cfg(feature = "auth-ceremony")]
    impl std::fmt::Display for Rejected {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "rejected")
        }
    }

    #[cfg(feature = "auth-ceremony")]
    impl std::error::Error for Rejected {}

    #[cfg(feature = "auth-ceremony")]
    impl fauna_protocol::RpcErrorClass for Rejected {
        fn is_rejection(&self) -> bool {
            true
        }
    }

    #[cfg(feature = "auth-ceremony")]
    impl fauna_protocol::RpcRequester for HandshakeNest {
        type Error = Rejected;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::auth::{CertBinding, NestHandshakeReply, NestHandshakeRequest};
            assert_eq!(kind, fauna_protocol::auth::NEST_HANDSHAKE_KIND);
            if self.reject {
                return Err(Rejected);
            }
            let req: NestHandshakeRequest =
                serde_json::from_value(serde_json::to_value(payload).unwrap()).unwrap();
            let binding = (!self.omit_binding).then(|| {
                let mut b =
                    CertBinding::sign(&self.signer, &self.spki, req.client_nonce.as_slice());
                if let Some(claimed) = self.claims {
                    b.nest_actor_id = hex::encode(claimed);
                }
                b
            });
            let reply = NestHandshakeReply {
                cert_binding: binding,
                extra: Default::default(),
            };
            Ok(serde_json::from_value(serde_json::to_value(reply).unwrap()).unwrap())
        }
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn possession_proof_accepts_the_box_holding_the_pasted_identity() {
        let nest = key(1);
        let client = HandshakeNest::serving(nest.clone());
        assert_eq!(
            prove_first_contact_identity_possession(&client, id(&nest)).await,
            Ok(())
        );
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn possession_proof_refuses_a_box_that_is_not_the_pasted_identity() {
        // The whole point of the row: before it, ANY box was accepted, because
        // the pin was never consulted. A box that cannot sign as the pasted
        // identity must now fail closed.
        let served = key(1);
        let pasted = key(2);
        let client = HandshakeNest::serving(served);
        assert_eq!(
            prove_first_contact_identity_possession(&client, id(&pasted)).await,
            Err(FirstContactError::Binding(BindingError::IdentityMismatch))
        );
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn possession_proof_refuses_a_forged_signature() {
        // A box that echoes the pasted actor id but cannot sign for it — the
        // claim is checked against the signature, not taken at its word.
        let pasted = key(1);
        let impostor = key(2);
        let mut client = HandshakeNest::serving(impostor);
        client.claims = Some(id(&pasted));
        assert_eq!(
            prove_first_contact_identity_possession(&client, id(&pasted)).await,
            Err(FirstContactError::Binding(BindingError::SignatureFailed))
        );
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn possession_proof_refuses_a_reply_carrying_no_binding() {
        let nest = key(1);
        let mut client = HandshakeNest::serving(nest.clone());
        client.omit_binding = true;
        assert_eq!(
            prove_first_contact_identity_possession(&client, id(&nest)).await,
            Err(FirstContactError::BindingRequired)
        );
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn possession_proof_refuses_a_nest_that_rejects_the_kind() {
        // Mirrors the native rule (`graduate_first_contact`'s doc): with a held
        // root there is NO pre-Track-2 fallback — the caller pasted this exact
        // box's identity, so a rejection is hostile-or-broken, never a benign
        // old nest. A MITM must not be able to downgrade by refusing the kind.
        let nest = key(1);
        let mut client = HandshakeNest::serving(nest.clone());
        client.reject = true;
        assert_eq!(
            prove_first_contact_identity_possession(&client, id(&nest)).await,
            Err(FirstContactError::BindingRequired)
        );
    }

    // ---- read_login_binding: the one reader every signer binds with ----

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn login_binding_read_learns_the_proven_identity() {
        let nest = key(1);
        let expected = id(&nest);
        let client = HandshakeNest::serving(nest);
        assert_eq!(read_login_binding(&client, None).await.unwrap(), expected);
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn login_binding_read_spki_compare_defeats_a_relay_when_the_cert_was_captured() {
        // Native TLS: the caller captured the received cert's SPKI. A relayed
        // binding signs the REAL nest's served SPKI, which differs from the
        // relay's cert the caller received — refused, never degraded.
        let nest = key(1);
        let client = HandshakeNest::serving(nest); // signs over spki [7u8; 32]
        let received = [8u8; 32];
        assert!(matches!(
            read_login_binding(&client, Some(&received)).await,
            Err(LoginBindingError::Binding(BindingError::SpkiMismatch))
        ));
        // And the match case still learns the identity.
        let nest = key(1);
        let expected = id(&nest);
        let client = HandshakeNest::serving(nest);
        assert_eq!(
            read_login_binding(&client, Some(&[7u8; 32])).await.unwrap(),
            expected
        );
    }

    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn login_binding_read_refuses_a_claimed_identity_the_box_cannot_sign_for() {
        // The reader has no expected value, so THIS is the whole security
        // argument: a box cannot claim an identity whose key it does not hold.
        let claimed = key(1);
        let impostor = key(2);
        let mut client = HandshakeNest::serving(impostor);
        client.claims = Some(id(&claimed));
        assert!(matches!(
            read_login_binding(&client, None).await,
            Err(LoginBindingError::Binding(BindingError::SignatureFailed))
        ));
    }

    /// A rejected kind and a withheld binding are both verdicts — nothing to
    /// bind, no signature made. There is no "sign the unbound form" degrade:
    /// the `read_nest_binding` reader that answered these two shapes with
    /// `Ok(None)` (the NAT-mode commit's sign-V1 signal) was removed 2026-09-24
    /// by the compat-remnant sweep; every signer now binds through this one
    /// reader.
    #[cfg(feature = "auth-ceremony")]
    #[tokio::test]
    async fn login_binding_read_never_degrades_on_a_rejection_or_a_withheld_binding() {
        let mut client = HandshakeNest::serving(key(1));
        client.reject = true;
        assert!(matches!(
            read_login_binding(&client, None).await,
            Err(LoginBindingError::Refused(Rejected))
        ));

        let mut client = HandshakeNest::serving(key(1));
        client.omit_binding = true;
        assert!(matches!(
            read_login_binding(&client, None).await,
            Err(LoginBindingError::NoBinding)
        ));
    }
}
