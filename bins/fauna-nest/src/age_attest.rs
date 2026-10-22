//! Attested age-claim verification — the nest-local seam for the platform
//! attestations `family-safety.md` § The account age band (D5) adopts.
//!
//! The claim contract is [`fauna_protocol::age::age_claim_signed_message`]
//! `(nonce, band, application_id, actor_id)`: the mobile app binds that
//! message's SHA-256 into what its platform attestation covers (iOS as the
//! client-data hash; android as the Play Integrity nonce), and this
//! module recomputes it, so **the signature that matters is the platform's**
//! (Apple's / Google's), never a key held by the Fauna app or the actor.
//!
//! Arms:
//! - **iOS — Apple App Attest, built.** The attestation object's certificate
//!   chain verifies **nest-locally** against Apple's published App Attestation
//!   Root CA ([`APPLE_APP_ATTEST_ROOT_PEM`]) — no Apple service call, which is
//!   what keeps a self-hosted nest sovereign here.
//! - **Android — Google Play Integrity, nest-local (plumbing ruled
//!   2026-08-24).** The app makes a Play Integrity **classic** request whose
//!   nonce is the SHA-256 of our claim message, and the nest opens the verdict token
//!   **locally** with the app's Play-Console response-encryption keys pinned
//!   as constants ([`FAUNA_ANDROID_PLAY_APPS`]) — the same shape as
//!   the iOS arm: no Google service call, no Fauna-org relay, sovereign and
//!   works out of the box. Google signs the verdict (`ES256`); the pinned keys
//!   only open its envelope (JWE `A256KW` + `A256GCM`), so publishing them in
//!   the tree costs verdict *secrecy*, never authenticity. `None` leaves the
//!   platform unarmed until the org downloads the keys (`family-safety.md`
//!   § The account age band — *Play Integrity plumbing*).
//!
//! **Three outcomes, never two** (`family-safety.md` § The account age band →
//! *An attestation the nest cannot check*): an attestation for a platform this
//! build holds a verifier for either verifies ([`AttestationOutcome::Verified`])
//! or fails the check — a refusal of the whole admission; one for a platform
//! this build holds **no** verifier for (unarmed, or a token it does not know)
//! is [`AttestationOutcome::CannotCheck`]: ignored unread, its nonce not
//! consumed, the claim handled as declared-only. Which platforms are armed is
//! one function, [`attestation_platforms`], read by both the verifier and the
//! `fauna.account.age_nonce` advertisement.
//!
//! Verification is deliberately **narrow and fixed-profile** on both arms.
//! iOS: not a general X.509 path builder but exactly `credCert ← intermediate
//! ← pinned root`, ECDSA P-256/P-384 with SHA-256/SHA-384 (the only algorithms
//! Apple's App Attest chain has ever used), validity-window + CA +
//! issuer-linkage checks, and the App-Attest-specific bindings (nonce
//! extension, key id, RP-ID hash, zero counter). Android: not a general JOSE
//! library but exactly the one JWE/JWS profile Play Integrity emits, every
//! algorithm string pinned. Structural parsing is `x509-parser` /
//! `serde_json`; the signature math is `p256`/`p384`; the envelope is
//! `aes-gcm` plus a 30-line RFC 3394 key unwrap — all crates already in the
//! graph, chosen over enabling a dep's verify glue so every step of the
//! boundary stays readable in our own code.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::aes::cipher::BlockDecrypt;
use aes_gcm::aes::{Aes256, Block};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use sha2::{Digest, Sha256, Sha384};

use fauna_protocol::age::{
    AGE_ATTESTATION_PLATFORM_ANDROID, AGE_ATTESTATION_PLATFORM_IOS, AgeAttestation, AgeBand,
    AgeBandProvenance, age_claim_signed_message,
};

use crate::challenge_auth::{ChallengeStore, ChallengeStoreFull, IssueSource};
use crate::routes::AppState;

// ---------------------------------------------------------------------------
// Pinned trust + application identity
// ---------------------------------------------------------------------------

/// Apple's published **App Attestation Root CA** (ECC P-384, CN "Apple App
/// Attestation Root CA", serial 0BF3BE0EF1CDD2E0FB8C6E721F621798, valid
/// 2020-03-18 → 2045-03-15), fetched verbatim from
/// <https://www.apple.com/certificateauthority/Apple_App_Attestation_Root_CA.pem>
/// (2026-08-24). The chain in every App Attest attestation object terminates
/// here; pinning the root — rather than any intermediate — is Apple's own
/// guidance and survives intermediate rotation.
pub const APPLE_APP_ATTEST_ROOT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIICITCCAaegAwIBAgIQC/O+DvHN0uD7jG5yH2IXmDAKBggqhkjOPQQDAzBSMSYw
JAYDVQQDDB1BcHBsZSBBcHAgQXR0ZXN0YXRpb24gUm9vdCBDQTETMBEGA1UECgwK
QXBwbGUgSW5jLjETMBEGA1UECAwKQ2FsaWZvcm5pYTAeFw0yMDAzMTgxODMyNTNa
Fw00NTAzMTUwMDAwMDBaMFIxJjAkBgNVBAMMHUFwcGxlIEFwcCBBdHRlc3RhdGlv
biBSb290IENBMRMwEQYDVQQKDApBcHBsZSBJbmMuMRMwEQYDVQQIDApDYWxpZm9y
bmlhMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAERTHhmLW07ATaFQIEVwTtT4dyctdh
NbJhFs/Ii2FdCgAHGbpphY3+d8qjuDngIN3WVhQUBHAoMeQ/cLiP1sOUtgjqK9au
Yen1mMEvRq9Sk3Jm5X8U62H+xTD3FE9TgS41o0IwQDAPBgNVHRMBAf8EBTADAQH/
MB0GA1UdDgQWBBSskRBTM72+aEH/pwyp5frq5eWKoTAOBgNVHQ8BAf8EBAMCAQYw
CgYIKoZIzj0EAwMDaAAwZQIwQgFGnByvsiVbpTKwSga0kP0e8EeDS4+sQmTvb7vn
53O5+FRXgeLhpJ06ysC5PrOyAjEAp5U4xDgEgllF7En3VcE3iexZZtKeYnpqtijV
oyFraWVIyd/dganmrduC1bmTBGwD
-----END CERTIFICATE-----
";

/// The genuine Fauna iOS apps' App IDs — the main app and Fauna Kids — owned
/// by [`fauna_protocol::age::FAUNA_IOS_APP_IDS`], the one arming point of the
/// nest verifier. **Empty leaves iOS unarmed**: [`verify_age_attestation`]
/// answers every iOS attestation [`AttestationOutcome::CannotCheck`] and the
/// age-nonce reply does not list `ios`. Armed with the org's two App IDs
/// (`fauna_core::platform_ids::APPLE_IOS_APP_ID`, `APPLE_IOS_KIDS_APP_ID`);
/// [`verify_ios_age_attestation`] picks the one the attestation's RP-ID hash
/// names.
pub use fauna_protocol::age::FAUNA_IOS_APP_IDS;

/// The genuine Fauna android app's Play application id — the permanent store
/// identity, owned by [`fauna_core::platform_ids::ANDROID_APPLICATION_ID`].
pub const FAUNA_ANDROID_APPLICATION_ID: &str = fauna_core::platform_ids::ANDROID_APPLICATION_ID;

/// The Fauna Kids android app's Play application id — its own Play record,
/// owned by [`fauna_core::platform_ids::ANDROID_KIDS_APPLICATION_ID`].
pub const FAUNA_ANDROID_KIDS_APPLICATION_ID: &str =
    fauna_core::platform_ids::ANDROID_KIDS_APPLICATION_ID;

/// One genuine Fauna android app the Play arm verifies: its Play application
/// id and the Play Integrity **response-encryption keys** of ITS Play record,
/// exactly as Play Console hands them to the developer (App integrity → Play
/// Integrity API → Response encryption → *Manage and download my response
/// encryption keys*). Play Console issues the pair **per app**, so the main
/// app and Fauna Kids each carry their own.
///
/// **Why the keys may live in a public tree (ruled 2026-08-24,
/// `family-safety.md` § The account age band):** Google signs the verdict with
/// a key only Google holds; these two only *open the envelope*. Publishing
/// them lets anyone read a Fauna verdict token they already possess — verdict
/// secrecy — and forges nothing. That is the price of a self-hosted nest
/// verifying nest-locally, with no Fauna-org relay in the path. Rotation (Play
/// Console can mint a new pair) is a deliberate nest release that swaps the
/// app's pair.
///
/// **`keys: None` leaves THAT app unarmed.** With every entry unarmed the
/// android platform is unarmed exactly like an empty [`FAUNA_IOS_APP_IDS`]:
/// every android attestation is [`AttestationOutcome::CannotCheck`] and the
/// age-nonce reply does not list `android`. With some armed and some not, the
/// platform is listed and a token no armed app's key opens is cannot-check
/// ([`pinned_play_app_for`]; `family-safety.md` § *An attestation the nest
/// cannot check*, the half-armed ruling). A pinned pair that fails to decode
/// counts as unarmed ([`android_arming`]).
#[derive(Debug, Clone, Copy)]
pub struct PlayIntegrityApp {
    pub package: &'static str,
    pub keys: Option<PlayIntegrityKeys>,
}

/// The pinned android apps — the main app and Fauna Kids — and the one arming
/// point of the Play arm: land an app's Play Console pair in its `keys`.
pub const FAUNA_ANDROID_PLAY_APPS: &[PlayIntegrityApp] = &[
    PlayIntegrityApp {
        package: FAUNA_ANDROID_APPLICATION_ID,
        keys: None,
    },
    PlayIntegrityApp {
        package: FAUNA_ANDROID_KIDS_APPLICATION_ID,
        keys: None,
    },
];

/// One Play Integrity response-encryption key pair as Play Console exports
/// it (see [`PlayIntegrityApp`]).
#[derive(Debug, Clone, Copy)]
pub struct PlayIntegrityKeys {
    /// Base64 (standard alphabet) of the 32-byte AES decryption key.
    pub decryption_key_b64: &'static str,
    /// Base64 (standard alphabet) of the DER `SubjectPublicKeyInfo` of the EC
    /// P-256 verification key.
    pub verification_key_der_b64: &'static str,
}

impl PlayIntegrityKeys {
    /// Decode the console's base64 forms into the material the verifier
    /// consumes; a malformed constant is a refusal, never a panic.
    pub fn material(&self) -> Result<PlayIntegrityKeyMaterial, &'static str> {
        use base64::Engine as _;
        let decryption_key: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(self.decryption_key_b64)
            .map_err(|_| "malformed Play Integrity decryption key")?
            .try_into()
            .map_err(|_| "Play Integrity decryption key is not 32 bytes")?;
        let verification_key_der = base64::engine::general_purpose::STANDARD
            .decode(self.verification_key_der_b64)
            .map_err(|_| "malformed Play Integrity verification key")?;
        Ok(PlayIntegrityKeyMaterial {
            decryption_key,
            verification_key_der,
        })
    }
}

/// Decoded [`PlayIntegrityKeys`] — what [`verify_play_integrity`] takes, so
/// tests inject a synthetic pair the same way the iOS tests inject a root.
#[derive(Debug, Clone)]
pub struct PlayIntegrityKeyMaterial {
    pub decryption_key: [u8; 32],
    pub verification_key_der: Vec<u8>,
}

/// How long a minted age nonce stays redeemable (the app has to run the
/// platform attestation round inside it) — the ceremony store's own TTL, the
/// age store being one.
pub const AGE_NONCE_TTL_SECS: u64 = crate::challenge_auth::CHALLENGE_TTL_SECS;

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

// ---------------------------------------------------------------------------
// Nonce store
// ---------------------------------------------------------------------------

/// The actor every age nonce is issued to: none. The minting caller is
/// pre-identity, so the inner store's actor re-check is vacuous here.
const UNBOUND_ACTOR: [u8; 32] = [0u8; 32];

/// Single-use nonces for the attested age claim (`fauna.account.age_nonce`) —
/// a [`ChallengeStore`] whose every nonce is issued to [`UNBOUND_ACTOR`]: the
/// minting caller is **pre-identity** (no account yet), and the actor is bound
/// inside the attested payload itself (`age_claim_signed_message` carries
/// `actor_id`), not at mint time. Being that store, it carries the same bounds
/// as every ceremony nonce store — the per-source cap, the global cap with
/// heaviest-network eviction, expired entries swept before counting
/// (`deployment-threat-model.md` § Concern 1a, finding H2). In-memory on
/// purpose: a nest restart mid-onboarding just re-mints, exactly like a
/// sign-in challenge.
#[derive(Default)]
pub struct AgeNonceStore {
    inner: ChallengeStore,
}

impl AgeNonceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store with explicit caps (see [`ChallengeStore::with_caps`]).
    pub fn with_caps(per_source_cap: usize, global_cap: usize) -> Self {
        Self {
            inner: ChallengeStore::with_caps(per_source_cap, global_cap),
        }
    }

    /// Mint a fresh nonce charged to `source`. Returns
    /// `(nonce, expires_in_secs)`, or [`ChallengeStoreFull`] when `source`
    /// already holds its cap of outstanding nonces.
    pub async fn issue(&self, source: IssueSource) -> Result<([u8; 32], u64), ChallengeStoreFull> {
        let (nonce, _) = self.inner.issue(UNBOUND_ACTOR, source).await?;
        Ok((nonce, AGE_NONCE_TTL_SECS))
    }

    /// Consume a nonce, removing it. `true` only for a known, unexpired entry
    /// — single-use by removal. Called **before** the attestation is verified
    /// (the `recovery_handlers` anti-grind ordering): a failed verification
    /// burns its nonce, so the expensive chain check can never be ground
    /// against one mint.
    pub async fn consume(&self, nonce: &[u8; 32]) -> bool {
        self.inner.consume(&UNBOUND_ACTOR, nonce).await
    }

    /// Drop entries whose TTL expired more than 60 seconds ago.
    pub async fn gc(&self) -> usize {
        self.inner.gc().await
    }

    /// Outstanding nonces — what the caps bound.
    pub async fn len(&self) -> usize {
        self.inner.len().await
    }

    /// `true` when no nonce is outstanding.
    pub async fn is_empty(&self) -> bool {
        self.inner.is_empty().await
    }
}

// ---------------------------------------------------------------------------
// The verification seam
// ---------------------------------------------------------------------------

/// What [`verify_age_attestation`] made of an attestation it did not refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationOutcome {
    /// The platform's signature checked out over the claim message and a live
    /// nest-minted nonce: the claim earns this `attested-*` provenance.
    Verified(AgeBandProvenance),
    /// This build holds no verifier for the attestation's platform (unarmed, or
    /// a token it does not know). Nothing of the attestation was parsed and its
    /// nonce was not consumed; the caller handles the claim exactly as the
    /// declared-only claim it would be without it.
    CannotCheck,
}

/// The cfg-free body of [`attestation_platforms`] — the arming decisions taken
/// as plain booleans so every answer is assertable from one build (the
/// `discovery_core::capabilities_for` shape: while
/// every [`FAUNA_ANDROID_PLAY_APPS`] entry is unarmed, the android-armed answer
/// is compiled by no other path).
pub fn attestation_platforms_for(ios_armed: bool, android_armed: bool) -> Vec<String> {
    let mut platforms = Vec::new();
    if ios_armed {
        platforms.push(AGE_ATTESTATION_PLATFORM_IOS.to_string());
    }
    if android_armed {
        platforms.push(AGE_ATTESTATION_PLATFORM_ANDROID.to_string());
    }
    platforms
}

/// The [`AgeAttestation::platform`] tokens this nest build holds a verifier
/// for — the `attestation_platforms` the `fauna.account.age_nonce` reply
/// advertises, computed from the same arming [`verify_age_attestation`] reads.
pub fn attestation_platforms() -> Vec<String> {
    attestation_platforms_for(
        !FAUNA_IOS_APP_IDS.is_empty(),
        !android_arming().armed.is_empty(),
    )
}

/// One [`FAUNA_ANDROID_PLAY_APPS`] entry whose pinned pair decoded.
#[derive(Debug, Clone)]
pub struct ArmedPlayApp {
    pub package: &'static str,
    pub material: PlayIntegrityKeyMaterial,
}

/// The Play arm's arming, per pinned app.
#[derive(Debug, Clone)]
pub struct PlayArming {
    /// The apps whose pair is pinned and decodes. Empty = android unarmed.
    pub armed: Vec<ArmedPlayApp>,
    /// Every pinned app is armed — no genuine verdict can come from an app
    /// this nest cannot open (the half-armed ruling, [`pinned_play_app_for`]).
    pub fully_armed: bool,
}

/// The android verifier's arming: each pinned app's Play Integrity key pair,
/// decoded. A pair that fails to decode is a release bug (a unit test pins
/// that every `Some` pair decodes), so it counts as unarmed for that app on
/// both sides of [`attestation_platforms`] and is logged, never a refusal of
/// an admission that did nothing wrong.
fn android_arming() -> PlayArming {
    let mut armed = Vec::new();
    for app in FAUNA_ANDROID_PLAY_APPS {
        let Some(keys) = app.keys else { continue };
        match keys.material() {
            Ok(material) => armed.push(ArmedPlayApp {
                package: app.package,
                material,
            }),
            Err(e) => tracing::error!(
                error = e,
                package = app.package,
                "pinned Play Integrity keys fail to decode; that app's android attestation is unarmed"
            ),
        }
    }
    let fully_armed = armed.len() == FAUNA_ANDROID_PLAY_APPS.len();
    PlayArming { armed, fully_armed }
}

/// Verify one attested age claim (`family-safety.md` § The account age band →
/// *An attestation the nest cannot check*). Three outcomes:
/// - `Ok(Verified(_))` — the provenance the minted band row records;
/// - `Ok(CannotCheck)` — no verifier for this platform in this build;
///   decided **before** any field is parsed or the nonce touched;
/// - `Err(_)` — a check this build can run failed (malformed fields, an
///   unknown/spent/expired nonce, a chain, binding or verdict that does not
///   verify): a `&'static str` the caller wraps in its typed refusal
///   (`fauna.account.age_attestation_invalid`), never a downgrade.
pub async fn verify_age_attestation(
    state: &AppState,
    attestation: &AgeAttestation,
    band: AgeBand,
    actor: &[u8; 32],
) -> Result<AttestationOutcome, &'static str> {
    // The attacker-supplied `platform` string is only ever compared here —
    // never logged — so an unknown token costs nothing.
    match attestation.platform.as_str() {
        AGE_ATTESTATION_PLATFORM_IOS => {
            if FAUNA_IOS_APP_IDS.is_empty() {
                return Ok(AttestationOutcome::CannotCheck);
            }
            let nonce = parse_hex_32(&attestation.nonce).ok_or("malformed nonce")?;
            let key_id = parse_hex_32(&attestation.key_id).ok_or("malformed key id")?;
            // Consume FIRST (single-use, anti-grind): a claim that fails below
            // has still spent its nonce.
            if !state.auth.age_nonce_store.consume(&nonce).await {
                return Err("unknown, expired, or already-used nonce");
            }
            verify_ios_age_attestation(
                &attestation.attestation_object,
                &key_id,
                &nonce,
                band,
                actor,
                FAUNA_IOS_APP_IDS,
                APPLE_APP_ATTEST_ROOT_PEM,
                now_secs() as i64,
            )?;
            Ok(AttestationOutcome::Verified(AgeBandProvenance::AttestedIos))
        }
        AGE_ATTESTATION_PLATFORM_ANDROID => {
            let arming = android_arming();
            if arming.armed.is_empty() {
                return Ok(AttestationOutcome::CannotCheck);
            }
            let token = &attestation.attestation_object;
            // Picked BEFORE the nonce is touched: a token no armed app's key
            // opens while a pinned app is unarmed is cannot-check, and a
            // cannot-check attestation leaves its nonce unconsumed.
            let pick = pinned_play_app_for(token, &arming.armed, arming.fully_armed);
            if matches!(pick, Ok(None)) {
                return Ok(AttestationOutcome::CannotCheck);
            }
            let nonce = parse_hex_32(&attestation.nonce).ok_or("malformed nonce")?;
            // Consume FIRST — same anti-grind reasoning as the iOS arm.
            if !state.auth.age_nonce_store.consume(&nonce).await {
                return Err("unknown, expired, or already-used nonce");
            }
            let app = pick?.ok_or("no pinned Play app's key opens the verdict")?;
            verify_android_age_attestation(token, &nonce, band, actor, app, now_secs())?;
            Ok(AttestationOutcome::Verified(
                AgeBandProvenance::AttestedAndroid,
            ))
        }
        // A token this build does not know — a platform a newer app speaks.
        _ => Ok(AttestationOutcome::CannotCheck),
    }
}

fn parse_hex_32(hex_str: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_str).ok()?;
    bytes.try_into().ok()
}

// ---------------------------------------------------------------------------
// App Attest — the fixed-profile verifier (pure; root + clock are parameters
// so unit tests substitute a synthetic chain without any test-only backdoor)
// ---------------------------------------------------------------------------

/// Apple's nonce-carrying certificate extension on the credential certificate.
const APPLE_NONCE_EXTENSION_OID: &str = "1.2.840.113635.100.8.2";

/// The pinned App ID whose SHA-256 is the attestation's RP-ID hash — the
/// App ID of the bundle that ran App Attest. Only the attestation's
/// `authData` names it, and only a pinned entry can answer: no App ID is ever
/// read from the client. No match refuses (outcome 2 — an attestation from a
/// bundle this nest does not pin).
pub fn pinned_app_id_for<'a>(
    attestation_object: &[u8],
    pinned: &[&'a str],
) -> Result<&'a str, &'static str> {
    let parsed = parse_attestation_object(attestation_object)?;
    let rp_id_hash = parsed
        .auth_data
        .get(..32)
        .ok_or("authenticator data too short")?;
    pinned
        .iter()
        .copied()
        .find(|app_id| Sha256::digest(app_id.as_bytes()).as_slice() == rp_id_hash)
        .ok_or("RP ID hash matches no pinned app id")
}

/// The iOS arm of an attested age claim, pure: pick the pinned App ID the
/// attestation's RP-ID hash names ([`pinned_app_id_for`]), rebuild
/// [`age_claim_signed_message`] with THAT App ID, and verify the object
/// against it ([`verify_app_attest`]). The App ID the RP-ID hash names and
/// the one the claim was signed over must therefore be the same: an
/// attestation from one bundle over a message naming the other fails the
/// nonce binding. The caller has already consumed `nonce`.
#[allow(clippy::too_many_arguments)]
pub fn verify_ios_age_attestation(
    attestation_object: &[u8],
    key_id: &[u8; 32],
    nonce: &[u8; 32],
    band: AgeBand,
    actor: &[u8; 32],
    pinned: &[&str],
    trust_root_pem: &str,
    now_secs: i64,
) -> Result<(), &'static str> {
    let app_id = pinned_app_id_for(attestation_object, pinned)?;
    let message = age_claim_signed_message(nonce, band.as_str(), app_id, actor);
    let client_data_hash: [u8; 32] = Sha256::digest(&message).into();
    verify_app_attest(
        attestation_object,
        key_id,
        &client_data_hash,
        app_id,
        trust_root_pem,
        now_secs,
    )
}

/// Verify an App Attest **attestation object** (the `attestKey` output; the
/// per-operation `generateAssertion` flow is not this ceremony — an age claim
/// is attested exactly once, at admission). Steps per Apple's published
/// procedure:
///
/// 1. decode the CBOR envelope (`fmt = "apple-appattest"`, `attStmt.x5c`,
///    `authData`) — a strict definite-length reader ([`cbor`] below);
/// 2. verify the certificate chain `credCert ← intermediate ← trust root`:
///    issuer/subject linkage, validity windows at `now`, the intermediate's CA
///    bit, and every signature (ECDSA P-256/P-384 × SHA-256/SHA-384, verified
///    over the raw `tbsCertificate` bytes with the issuer's key);
/// 3. `nonce = SHA256(authData ‖ clientDataHash)` must equal the octet string
///    inside credCert's [`APPLE_NONCE_EXTENSION_OID`] extension — this is the
///    binding that makes the platform's signature cover OUR
///    `age_claim_signed_message`;
/// 4. `key_id` must equal SHA256(credCert's public key) AND the attested
///    credential id inside `authData`;
/// 5. `authData`: RP-ID hash = SHA256(app id), counter = 0, aaguid ∈
///    {`appattest`, `appattestdevelop`} (a dev-environment attestation from a
///    genuine build is accepted — the claim is corroboration, and refusing
///    TestFlight installs would only punish real families on beta builds).
pub fn verify_app_attest(
    attestation_object: &[u8],
    key_id: &[u8; 32],
    client_data_hash: &[u8; 32],
    app_id: &str,
    trust_root_pem: &str,
    now_secs: i64,
) -> Result<(), &'static str> {
    // ── 1. CBOR envelope ──
    let parsed = parse_attestation_object(attestation_object)?;
    if parsed.x5c.len() < 2 {
        return Err("attestation chain too short");
    }
    let cred_der = &parsed.x5c[0];
    let intermediate_der = &parsed.x5c[1];

    // ── 2. Chain to the pinned root ──
    let root_der = pem_to_der(trust_root_pem).ok_or("malformed trust root PEM")?;
    let (_, root) =
        x509_parser::parse_x509_certificate(&root_der).map_err(|_| "unparseable trust root")?;
    let (_, intermediate) = x509_parser::parse_x509_certificate(intermediate_der)
        .map_err(|_| "unparseable intermediate certificate")?;
    let (_, cred) = x509_parser::parse_x509_certificate(cred_der)
        .map_err(|_| "unparseable credential certificate")?;

    let now = x509_parser::time::ASN1Time::from_timestamp(now_secs)
        .map_err(|_| "invalid verification time")?;
    if !cred.validity().is_valid_at(now) {
        return Err("credential certificate outside its validity window");
    }
    if !intermediate.validity().is_valid_at(now) {
        return Err("intermediate certificate outside its validity window");
    }
    if !root.validity().is_valid_at(now) {
        return Err("trust root outside its validity window");
    }
    match intermediate.basic_constraints() {
        Ok(Some(bc)) if bc.value.ca => {}
        _ => return Err("intermediate certificate is not a CA"),
    }
    if cred.issuer() != intermediate.subject() {
        return Err("credential certificate not issued by the intermediate");
    }
    if intermediate.issuer() != root.subject() {
        return Err("intermediate not issued by the trust root");
    }
    verify_cert_signature(&cred, &intermediate).map_err(|_| "credential signature invalid")?;
    verify_cert_signature(&intermediate, &root).map_err(|_| "intermediate signature invalid")?;

    // ── 3. The nonce binding ──
    let mut nonce_input = Vec::with_capacity(parsed.auth_data.len() + 32);
    nonce_input.extend_from_slice(&parsed.auth_data);
    nonce_input.extend_from_slice(client_data_hash);
    let expected_nonce: [u8; 32] = Sha256::digest(&nonce_input).into();
    let cert_nonce = extract_apple_nonce(&cred)?;
    if cert_nonce != expected_nonce {
        return Err("attestation nonce mismatch");
    }

    // ── 4. Key identity ──
    let cred_public_key = cred
        .tbs_certificate
        .subject_pki
        .subject_public_key
        .data
        .as_ref();
    let computed_key_id: [u8; 32] = Sha256::digest(cred_public_key).into();
    if &computed_key_id != key_id {
        return Err("key id does not match the attested public key");
    }

    // ── 5. authData ──
    let auth = &parsed.auth_data;
    if auth.len() < 87 {
        return Err("authenticator data too short");
    }
    let rp_id_hash: [u8; 32] = Sha256::digest(app_id.as_bytes()).into();
    if auth[..32] != rp_id_hash {
        return Err("RP ID hash does not match the pinned app id");
    }
    if auth[33..37] != [0, 0, 0, 0] {
        return Err("attestation counter is not zero");
    }
    let aaguid = &auth[37..53];
    if aaguid != b"appattest\0\0\0\0\0\0\0" && aaguid != b"appattestdevelop" {
        return Err("unexpected attestation environment");
    }
    let cred_id_len = u16::from_be_bytes([auth[53], auth[54]]) as usize;
    if cred_id_len != 32 || auth.len() < 55 + 32 {
        return Err("unexpected credential id length");
    }
    if &auth[55..87] != key_id {
        return Err("credential id does not match the key id");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Play Integrity — the fixed-profile verifier (pure; keys + clock are
// parameters so unit tests substitute a synthetic pair without any test-only
// backdoor)
// ---------------------------------------------------------------------------

/// The pinned android app whose response-encryption key opens the verdict
/// token's envelope — the app whose Play record the verdict was minted for,
/// since Play Console issues the pair per app. Only the token names it, and
/// only a pinned entry's key can answer: no package is ever read from the
/// client. `A256KW` is an integrity-checked unwrap (RFC 3394's IV check), so
/// another app's key does not open it.
///
/// - `Ok(Some(app))` — `app`'s key opened it; the caller verifies against
///   `app` alone ([`verify_android_age_attestation`]).
/// - `Ok(None)` — **cannot check (the half-armed ruling, `family-safety.md`
///   § *An attestation the nest cannot check*)**: no armed app's key opens it
///   while some pinned app is unarmed, so it may be that app's genuine
///   verdict — exactly what an unarmed platform's would be.
/// - `Err(_)` — refused (outcome 2): a malformed or off-profile envelope, or,
///   with every pinned app armed, a token none of their keys opens.
pub fn pinned_play_app_for<'a>(
    token: &[u8],
    armed: &'a [ArmedPlayApp],
    fully_armed: bool,
) -> Result<Option<&'a ArmedPlayApp>, &'static str> {
    let token = std::str::from_utf8(token).map_err(|_| "token is not ASCII")?;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 5 {
        return Err("token is not a five-part JWE");
    }
    let header: serde_json::Value =
        serde_json::from_slice(&b64url(parts[0]).ok_or("malformed JWE header")?)
            .map_err(|_| "unparseable JWE header")?;
    if header.get("alg").and_then(|v| v.as_str()) != Some("A256KW") {
        return Err("JWE key algorithm is not A256KW");
    }
    let wrapped_cek = b64url(parts[1]).ok_or("malformed JWE encrypted key")?;
    match armed
        .iter()
        .find(|app| aes_key_unwrap(&app.material.decryption_key, &wrapped_cek).is_ok())
    {
        Some(app) => Ok(Some(app)),
        None if fully_armed => Err("no pinned Play app's key opens the verdict"),
        None => Ok(None),
    }
}

/// The android arm of an attested age claim, pure: rebuild
/// [`age_claim_signed_message`] with the package of the app whose key opened
/// the token ([`pinned_play_app_for`]) and verify the verdict against that app
/// alone ([`verify_play_integrity`]) — both package fields must name it and
/// the nonce must bind a message signed over it, so a verdict for one app
/// opened under the other's key, or one app's verdict over a message naming
/// the other, refuses. The caller has already consumed `nonce`.
pub fn verify_android_age_attestation(
    token: &[u8],
    nonce: &[u8; 32],
    band: AgeBand,
    actor: &[u8; 32],
    app: &ArmedPlayApp,
    now_secs: u64,
) -> Result<(), &'static str> {
    let message = age_claim_signed_message(nonce, band.as_str(), app.package, actor);
    verify_play_integrity(token, &message, app.package, &app.material, now_secs)
}

/// Verify a Play Integrity **classic-request verdict token** for one attested
/// age claim. `claim_message` is the exact [`age_claim_signed_message`]; the
/// app passed its **SHA-256, base64url unpadded**, as the classic request's
/// nonce and Google echoes that verbatim in `requestDetails.nonce` — the
/// binding that makes Google's signature cover OUR claim, and the exact twin
/// of the iOS `clientDataHash`. Hashing is deliberate: Google's logs see a
/// digest, never the band or the actor id inside the message. Steps, per
/// Google's published "decrypt and verify locally" procedure:
///
/// 1. the token is a compact JWE `header.encrypted_key.iv.ciphertext.tag`
///    with `alg = A256KW`, `enc = A256GCM` — both pinned, any other string
///    refuses: unwrap the content key with the decryption key
///    ([`aes_key_unwrap`], RFC 3394), then AES-256-GCM-open the payload with
///    the header segment as AAD;
/// 2. the payload is a compact JWS `header.payload.signature` with
///    `alg = ES256` (pinned): verify Google's P-256 signature over
///    `header.payload` with the verification key;
/// 3. the verdict must say: `requestDetails.requestPackageName` and
///    `appIntegrity.packageName` are the pinned application id;
///    `requestDetails.nonce` is ours; `requestDetails.timestampMillis` lies
///    within [`AGE_NONCE_TTL_SECS`] of `now_secs` (the nonce's own lifetime —
///    a verdict cannot be older than the nonce it echoes);
///    `appIntegrity.appRecognitionVerdict = PLAY_RECOGNIZED` — Google's own
///    statement that the binary AND its signing certificate are the
///    Play-distributed ones, i.e. the genuine-binary proof (no certificate
///    digest is pinned separately: it would restate that verdict from a value
///    Google also holds, the Play App Signing key being Google-generated);
///    `deviceIntegrity.deviceRecognitionVerdict` contains
///    `MEETS_DEVICE_INTEGRITY`; `accountDetails.appLicensingVerdict =
///    LICENSED` — the third leg of "genuine binary + certified device +
///    Play-licensed install" the memo adopted.
pub fn verify_play_integrity(
    token: &[u8],
    claim_message: &[u8],
    application_id: &str,
    keys: &PlayIntegrityKeyMaterial,
    now_secs: u64,
) -> Result<(), &'static str> {
    let expected_nonce: [u8; 32] = Sha256::digest(claim_message).into();
    let token = std::str::from_utf8(token).map_err(|_| "token is not ASCII")?;

    // ── 1. JWE envelope ──
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 5 {
        return Err("token is not a five-part JWE");
    }
    let header: serde_json::Value =
        serde_json::from_slice(&b64url(parts[0]).ok_or("malformed JWE header")?)
            .map_err(|_| "unparseable JWE header")?;
    if header.get("alg").and_then(|v| v.as_str()) != Some("A256KW") {
        return Err("JWE key algorithm is not A256KW");
    }
    if header.get("enc").and_then(|v| v.as_str()) != Some("A256GCM") {
        return Err("JWE content algorithm is not A256GCM");
    }
    let wrapped_cek = b64url(parts[1]).ok_or("malformed JWE encrypted key")?;
    let iv = b64url(parts[2]).ok_or("malformed JWE iv")?;
    let mut sealed = b64url(parts[3]).ok_or("malformed JWE ciphertext")?;
    let tag = b64url(parts[4]).ok_or("malformed JWE tag")?;
    if iv.len() != 12 {
        return Err("JWE iv is not 96 bits");
    }
    if tag.len() != 16 {
        return Err("JWE tag is not 128 bits");
    }
    let cek = aes_key_unwrap(&keys.decryption_key, &wrapped_cek)?;
    if cek.len() != 32 {
        return Err("unwrapped content key is not 32 bytes");
    }
    sealed.extend_from_slice(&tag);
    let cipher = Aes256Gcm::new_from_slice(&cek).map_err(|_| "content key rejected")?;
    let jws_bytes = cipher
        .decrypt(
            GcmNonce::from_slice(&iv),
            Payload {
                msg: &sealed,
                aad: parts[0].as_bytes(),
            },
        )
        .map_err(|_| "JWE authentication failed")?;

    // ── 2. JWS signature ──
    let jws = std::str::from_utf8(&jws_bytes).map_err(|_| "JWS is not ASCII")?;
    let jparts: Vec<&str> = jws.split('.').collect();
    if jparts.len() != 3 {
        return Err("payload is not a three-part JWS");
    }
    let jheader: serde_json::Value =
        serde_json::from_slice(&b64url(jparts[0]).ok_or("malformed JWS header")?)
            .map_err(|_| "unparseable JWS header")?;
    if jheader.get("alg").and_then(|v| v.as_str()) != Some("ES256") {
        return Err("JWS algorithm is not ES256");
    }
    let sig_bytes = b64url(jparts[2]).ok_or("malformed JWS signature")?;
    let sig = p256::ecdsa::Signature::from_slice(&sig_bytes)
        .map_err(|_| "JWS signature is not a P-256 r‖s pair")?;
    let key = p256::ecdsa::VerifyingKey::from_public_key_der(&keys.verification_key_der)
        .map_err(|_| "verification key is not a P-256 SubjectPublicKeyInfo")?;
    let signing_input = format!("{}.{}", jparts[0], jparts[1]);
    key.verify(signing_input.as_bytes(), &sig)
        .map_err(|_| "verdict signature invalid")?;

    // ── 3. The verdict ──
    let verdict: serde_json::Value =
        serde_json::from_slice(&b64url(jparts[1]).ok_or("malformed JWS payload")?)
            .map_err(|_| "unparseable verdict")?;
    let request = &verdict["requestDetails"];
    if request["requestPackageName"].as_str() != Some(application_id) {
        return Err("verdict was requested for another package");
    }
    let nonce = request["nonce"]
        .as_str()
        .ok_or("verdict carries no nonce")?;
    let nonce_bytes = b64url(nonce).ok_or("verdict nonce is not base64url")?;
    if nonce_bytes != expected_nonce {
        return Err("verdict nonce does not bind this claim");
    }
    let ts_millis = json_u64(&request["timestampMillis"]).ok_or("verdict carries no timestamp")?;
    if (ts_millis / 1000).abs_diff(now_secs) > AGE_NONCE_TTL_SECS {
        return Err("verdict timestamp outside the nonce lifetime");
    }
    let app = &verdict["appIntegrity"];
    if app["appRecognitionVerdict"].as_str() != Some("PLAY_RECOGNIZED") {
        return Err("app is not Play-recognized");
    }
    if app["packageName"].as_str() != Some(application_id) {
        return Err("verdict names another package");
    }
    let device_ok = verdict["deviceIntegrity"]["deviceRecognitionVerdict"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .any(|l| l.as_str() == Some("MEETS_DEVICE_INTEGRITY"))
        })
        .unwrap_or(false);
    if !device_ok {
        return Err("device does not meet Play device integrity");
    }
    if verdict["accountDetails"]["appLicensingVerdict"].as_str() != Some("LICENSED") {
        return Err("install is not Play-licensed");
    }
    Ok(())
}

/// Play emits `timestampMillis` as a JSON string; accept a number too.
fn json_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// base64url (RFC 4648 §5) — the JOSE alphabet; padding tolerated.
fn b64url(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
}

/// RFC 3394 AES key unwrap with a 256-bit KEK — the `A256KW` half of the JWE.
/// Written out over the AES block cipher already in the graph for GCM rather
/// than pulling a key-wrap crate: the algorithm is thirty lines, and the
/// default-IV check at the end is its whole integrity story.
pub fn aes_key_unwrap(kek: &[u8; 32], wrapped: &[u8]) -> Result<Vec<u8>, &'static str> {
    if wrapped.len() < 24 || !wrapped.len().is_multiple_of(8) {
        return Err("wrapped key has an invalid length");
    }
    let n = wrapped.len() / 8 - 1;
    let cipher = Aes256::new_from_slice(kek).map_err(|_| "key-encryption key rejected")?;
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut r: Vec<[u8; 8]> = wrapped[8..]
        .chunks(8)
        .map(|c| {
            let mut b = [0u8; 8];
            b.copy_from_slice(c);
            b
        })
        .collect();
    for j in (0..6).rev() {
        for i in (1..=n).rev() {
            let t = (n * j + i) as u64;
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&(u64::from_be_bytes(a) ^ t).to_be_bytes());
            block[8..].copy_from_slice(&r[i - 1]);
            let mut block = Block::clone_from_slice(&block);
            cipher.decrypt_block(&mut block);
            a.copy_from_slice(&block[..8]);
            r[i - 1].copy_from_slice(&block[8..]);
        }
    }
    if a != [0xA6; 8] {
        return Err("key unwrap integrity check failed");
    }
    Ok(r.concat())
}

/// First PEM certificate block → DER.
fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    match CertificateDer::pem_slice_iter(pem.as_bytes()).next()? {
        Ok(cert) => Some(cert.as_ref().to_vec()),
        Err(_) => None,
    }
}

/// Verify `child`'s signature with `issuer`'s public key — the fixed App
/// Attest profile: ECDSA over P-256 (65-byte SEC1 point) or P-384 (97-byte),
/// digest SHA-256 or SHA-384 per the certificate's signature-algorithm OID,
/// computed over the raw `tbsCertificate` DER. Anything else is refused —
/// Apple's chain has never used another combination, and a narrow verifier
/// cannot be steered onto a weak algorithm.
fn verify_cert_signature(
    child: &x509_parser::certificate::X509Certificate<'_>,
    issuer: &x509_parser::certificate::X509Certificate<'_>,
) -> Result<(), &'static str> {
    use p256::ecdsa::signature::hazmat::PrehashVerifier;

    let tbs = child.tbs_certificate.as_ref();
    let sig_der = child.signature_value.data.as_ref();
    let digest: Vec<u8> = match child.signature_algorithm.algorithm.to_id_string().as_str() {
        // ecdsa-with-SHA256 / ecdsa-with-SHA384
        "1.2.840.10045.4.3.2" => Sha256::digest(tbs).to_vec(),
        "1.2.840.10045.4.3.3" => Sha384::digest(tbs).to_vec(),
        _ => return Err("unsupported signature algorithm"),
    };
    let issuer_point = issuer
        .tbs_certificate
        .subject_pki
        .subject_public_key
        .data
        .as_ref();
    match issuer_point.len() {
        // Uncompressed SEC1 point lengths disambiguate the curve.
        65 => {
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(issuer_point)
                .map_err(|_| "bad issuer key")?;
            let sig =
                p256::ecdsa::Signature::from_der(sig_der).map_err(|_| "bad signature encoding")?;
            key.verify_prehash(&digest, &sig)
                .map_err(|_| "signature verification failed")
        }
        97 => {
            let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(issuer_point)
                .map_err(|_| "bad issuer key")?;
            let sig =
                p384::ecdsa::Signature::from_der(sig_der).map_err(|_| "bad signature encoding")?;
            key.verify_prehash(&digest, &sig)
                .map_err(|_| "signature verification failed")
        }
        _ => Err("unsupported issuer key"),
    }
}

/// Pull the 32-byte nonce out of credCert's Apple extension. The value is the
/// fixed DER shape `SEQUENCE { [1] { OCTET STRING (32) } }`.
fn extract_apple_nonce(
    cred: &x509_parser::certificate::X509Certificate<'_>,
) -> Result<[u8; 32], &'static str> {
    let ext = cred
        .extensions()
        .iter()
        .find(|e| e.oid.to_id_string() == APPLE_NONCE_EXTENSION_OID)
        .ok_or("credential certificate carries no nonce extension")?;
    let value = ext.value;
    // SEQUENCE
    let (tag, _, header) = der_header(value)?;
    if tag != 0x30 {
        return Err("malformed nonce extension");
    }
    let rest = &value[header..];
    // context-specific [1], constructed
    let (tag, _, header) = der_header(rest)?;
    if tag != 0xA1 {
        return Err("malformed nonce extension");
    }
    let rest = &rest[header..];
    // OCTET STRING of exactly 32 bytes
    let (tag, len, header) = der_header(rest)?;
    if tag != 0x04 || len != 32 || rest.len() < header + 32 {
        return Err("malformed nonce extension");
    }
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&rest[header..header + 32]);
    Ok(nonce)
}

/// Minimal DER header read: `(tag, content_len, header_len)`. Short and
/// long-form lengths (≤ 4 length bytes — far beyond this profile's needs).
fn der_header(bytes: &[u8]) -> Result<(u8, usize, usize), &'static str> {
    if bytes.len() < 2 {
        return Err("truncated DER");
    }
    let tag = bytes[0];
    let first = bytes[1];
    if first < 0x80 {
        return Ok((tag, first as usize, 2));
    }
    let n = (first & 0x7F) as usize;
    if n == 0 || n > 4 || bytes.len() < 2 + n {
        return Err("unsupported DER length");
    }
    let mut len = 0usize;
    for b in &bytes[2..2 + n] {
        len = (len << 8) | *b as usize;
    }
    Ok((tag, len, 2 + n))
}

// ---------------------------------------------------------------------------
// CBOR — a strict definite-length reader for the attestation envelope. The
// object is attacker-supplied, so this deliberately does NOT ride
// `fauna_cbor::decode_strict`: App Attest CBOR is CTAP2-canonical, not
// DAG-CBOR, and only *coincidentally* passes the DAG canonical pre-walk — a
// contract too fragile to lean on at a security boundary. Modeled on
// `fauna_cbor::canonical`'s major-type walker.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParsedAttestation {
    x5c: Vec<Vec<u8>>,
    auth_data: Vec<u8>,
}

fn parse_attestation_object(bytes: &[u8]) -> Result<ParsedAttestation, &'static str> {
    let mut reader = Cbor { bytes, pos: 0 };
    let entries = reader.map_header()?;
    let mut fmt_ok = false;
    let mut x5c: Option<Vec<Vec<u8>>> = None;
    let mut auth_data: Option<Vec<u8>> = None;
    for _ in 0..entries {
        let key = reader.text()?;
        match key {
            "fmt" => {
                if reader.text()? != "apple-appattest" {
                    return Err("not an apple-appattest attestation");
                }
                fmt_ok = true;
            }
            "attStmt" => {
                let stmt_entries = reader.map_header()?;
                for _ in 0..stmt_entries {
                    let stmt_key = reader.text()?;
                    match stmt_key {
                        "x5c" => {
                            let n = reader.array_header()?;
                            if n > 4 {
                                return Err("attestation chain too long");
                            }
                            let mut certs = Vec::with_capacity(n);
                            for _ in 0..n {
                                certs.push(reader.byte_string()?.to_vec());
                            }
                            x5c = Some(certs);
                        }
                        _ => reader.skip_item()?,
                    }
                }
            }
            "authData" => auth_data = Some(reader.byte_string()?.to_vec()),
            _ => reader.skip_item()?,
        }
    }
    if !fmt_ok {
        return Err("attestation carries no format");
    }
    Ok(ParsedAttestation {
        x5c: x5c.ok_or("attestation carries no certificate chain")?,
        auth_data: auth_data.ok_or("attestation carries no authenticator data")?,
    })
}

struct Cbor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cbor<'a> {
    /// Initial byte + argument, definite-length only.
    fn head(&mut self) -> Result<(u8, u64), &'static str> {
        let b = *self.bytes.get(self.pos).ok_or("truncated CBOR")?;
        self.pos += 1;
        let major = b >> 5;
        let info = b & 0x1F;
        let arg = match info {
            0..=23 => info as u64,
            24..=27 => {
                let n = 1usize << (info - 24);
                let raw = self
                    .bytes
                    .get(self.pos..self.pos + n)
                    .ok_or("truncated CBOR")?;
                self.pos += n;
                let mut v = 0u64;
                for byte in raw {
                    v = (v << 8) | *byte as u64;
                }
                v
            }
            _ => return Err("indefinite-length CBOR refused"),
        };
        Ok((major, arg))
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], &'static str> {
        let n = usize::try_from(n).map_err(|_| "oversized CBOR item")?;
        let slice = self
            .bytes
            .get(self.pos..self.pos.checked_add(n).ok_or("oversized CBOR item")?)
            .ok_or("truncated CBOR")?;
        self.pos += n;
        Ok(slice)
    }

    fn map_header(&mut self) -> Result<usize, &'static str> {
        match self.head()? {
            (5, n) if n <= 32 => Ok(n as usize),
            _ => Err("expected a small CBOR map"),
        }
    }

    fn array_header(&mut self) -> Result<usize, &'static str> {
        match self.head()? {
            (4, n) if n <= 32 => Ok(n as usize),
            _ => Err("expected a small CBOR array"),
        }
    }

    fn text(&mut self) -> Result<&'a str, &'static str> {
        match self.head()? {
            (3, n) => std::str::from_utf8(self.take(n)?).map_err(|_| "invalid CBOR text"),
            _ => Err("expected CBOR text"),
        }
    }

    fn byte_string(&mut self) -> Result<&'a [u8], &'static str> {
        match self.head()? {
            (2, n) => self.take(n),
            _ => Err("expected CBOR bytes"),
        }
    }

    /// Skip one item of any type (bounded recursion for nested containers).
    fn skip_item(&mut self) -> Result<(), &'static str> {
        self.skip_item_depth(0)
    }

    fn skip_item_depth(&mut self, depth: u8) -> Result<(), &'static str> {
        if depth > 8 {
            return Err("CBOR nesting too deep");
        }
        let (major, arg) = self.head()?;
        match major {
            0 | 1 | 7 => Ok(()),
            2 | 3 => self.take(arg).map(|_| ()),
            4 => {
                for _ in 0..arg {
                    self.skip_item_depth(depth + 1)?;
                }
                Ok(())
            }
            5 => {
                for _ in 0..arg {
                    self.skip_item_depth(depth + 1)?;
                    self.skip_item_depth(depth + 1)?;
                }
                Ok(())
            }
            6 => self.skip_item_depth(depth + 1),
            _ => Err("unknown CBOR major type"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::age::{FAUNA_IOS_APP_ID, FAUNA_IOS_KIDS_APP_ID};
    use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair};

    const TEST_APP_ID: &str = "TESTTEAM.social.fauna.fauna";

    /// A synthetic App-Attest-shaped chain + envelope: root CA → intermediate
    /// CA → credential cert carrying the Apple nonce extension, all P-256 /
    /// SHA-256 (a combination the fixed profile accepts alongside Apple's
    /// production P-384). The trust root is INJECTED as a parameter — the
    /// production Apple root is never involved, and no test-only seam exists
    /// in the verifier.
    struct Fixture {
        attestation_object: Vec<u8>,
        key_id: [u8; 32],
        client_data_hash: [u8; 32],
        root_pem: String,
    }

    fn cbor_text(out: &mut Vec<u8>, s: &str) {
        cbor_head(out, 3, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }

    fn cbor_bytes(out: &mut Vec<u8>, b: &[u8]) {
        cbor_head(out, 2, b.len() as u64);
        out.extend_from_slice(b);
    }

    fn cbor_head(out: &mut Vec<u8>, major: u8, arg: u64) {
        if arg < 24 {
            out.push((major << 5) | arg as u8);
        } else if arg <= u8::MAX as u64 {
            out.push((major << 5) | 24);
            out.push(arg as u8);
        } else {
            out.push((major << 5) | 25);
            out.extend_from_slice(&(arg as u16).to_be_bytes());
        }
    }

    fn build_fixture(mutate_auth_data: impl FnOnce(&mut Vec<u8>)) -> Fixture {
        build_fixture_for(
            TEST_APP_ID,
            Sha256::digest(b"the client data").into(),
            mutate_auth_data,
        )
    }

    /// [`build_fixture`] with the RP-ID (the App ID the attesting bundle runs
    /// under) and the client-data hash chosen by the caller.
    fn build_fixture_for(
        rp_id: &str,
        client_data_hash: [u8; 32],
        mutate_auth_data: impl FnOnce(&mut Vec<u8>),
    ) -> Fixture {
        let root_key = KeyPair::generate().expect("root keygen");
        let mut root_params = CertificateParams::new(Vec::<String>::new()).expect("root params");
        root_params
            .distinguished_name
            .push(DnType::CommonName, "Fauna Test Attestation Root");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let root_cert = root_params.self_signed(&root_key).expect("root self-sign");

        let inter_key = KeyPair::generate().expect("intermediate keygen");
        let mut inter_params = CertificateParams::new(Vec::<String>::new()).expect("inter params");
        inter_params
            .distinguished_name
            .push(DnType::CommonName, "Fauna Test Attestation CA 1");
        inter_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let inter_cert = inter_params
            .signed_by(&inter_key, &root_cert, &root_key)
            .expect("sign intermediate");

        let cred_key = KeyPair::generate().expect("cred keygen");
        let key_id: [u8; 32] = Sha256::digest(cred_key.public_key_raw()).into();

        // authData: rpIdHash ‖ flags ‖ counter=0 ‖ aaguid ‖ credIdLen ‖ credId
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
        auth_data.push(0x40); // AT flag
        auth_data.extend_from_slice(&[0, 0, 0, 0]);
        auth_data.extend_from_slice(b"appattest\0\0\0\0\0\0\0");
        auth_data.extend_from_slice(&32u16.to_be_bytes());
        auth_data.extend_from_slice(&key_id);
        mutate_auth_data(&mut auth_data);

        let mut nonce_input = auth_data.clone();
        nonce_input.extend_from_slice(&client_data_hash);
        let nonce: [u8; 32] = Sha256::digest(&nonce_input).into();

        // The Apple extension value: SEQUENCE { [1] { OCTET STRING nonce } }.
        let mut ext = vec![0x30, 38, 0xA1, 36, 0x04, 32];
        ext.extend_from_slice(&nonce);

        let mut cred_params = CertificateParams::new(Vec::<String>::new()).expect("cred params");
        cred_params
            .distinguished_name
            .push(DnType::CommonName, "Fauna Test Credential");
        cred_params
            .custom_extensions
            .push(CustomExtension::from_oid_content(
                &[1, 2, 840, 113635, 100, 8, 2],
                ext,
            ));
        let cred_cert = cred_params
            .signed_by(&cred_key, &inter_cert, &inter_key)
            .expect("sign credential");

        // The CBOR envelope, CTAP2-shaped.
        let mut obj = Vec::new();
        cbor_head(&mut obj, 5, 3);
        cbor_text(&mut obj, "fmt");
        cbor_text(&mut obj, "apple-appattest");
        cbor_text(&mut obj, "attStmt");
        cbor_head(&mut obj, 5, 2);
        cbor_text(&mut obj, "x5c");
        cbor_head(&mut obj, 4, 2);
        cbor_bytes(&mut obj, cred_cert.der());
        cbor_bytes(&mut obj, inter_cert.der());
        cbor_text(&mut obj, "receipt");
        cbor_bytes(&mut obj, b"");
        cbor_text(&mut obj, "authData");
        cbor_bytes(&mut obj, &auth_data);

        Fixture {
            attestation_object: obj,
            key_id,
            client_data_hash,
            root_pem: root_cert.pem(),
        }
    }

    fn now() -> i64 {
        fauna_core::data::Timestamp::now_secs()
    }

    #[test]
    fn a_well_formed_attestation_verifies() {
        let f = build_fixture(|_| {});
        verify_app_attest(
            &f.attestation_object,
            &f.key_id,
            &f.client_data_hash,
            TEST_APP_ID,
            &f.root_pem,
            now(),
        )
        .expect("synthetic attestation verifies");
    }

    #[test]
    fn a_foreign_root_is_refused() {
        // A structurally-perfect attestation chained to the WRONG root — the
        // pinned-root check is the whole trust story, so this is the test
        // that red-verifies the verifier actually verifies.
        let f = build_fixture(|_| {});
        let other = build_fixture(|_| {});
        let err = verify_app_attest(
            &f.attestation_object,
            &f.key_id,
            &f.client_data_hash,
            TEST_APP_ID,
            &other.root_pem,
            now(),
        )
        .expect_err("foreign root must refuse");
        assert!(err.contains("intermediate"), "refused as: {err}");
    }

    #[test]
    fn a_tampered_client_data_hash_is_refused() {
        let f = build_fixture(|_| {});
        let wrong: [u8; 32] = Sha256::digest(b"other client data").into();
        let err = verify_app_attest(
            &f.attestation_object,
            &f.key_id,
            &wrong,
            TEST_APP_ID,
            &f.root_pem,
            now(),
        )
        .expect_err("hash mismatch must refuse");
        assert_eq!(err, "attestation nonce mismatch");
    }

    #[test]
    fn a_wrong_app_id_is_refused() {
        let f = build_fixture(|_| {});
        let err = verify_app_attest(
            &f.attestation_object,
            &f.key_id,
            &f.client_data_hash,
            "OTHERTEAM.some.other.app",
            &f.root_pem,
            now(),
        )
        .expect_err("wrong app id must refuse");
        assert_eq!(err, "RP ID hash does not match the pinned app id");
    }

    #[test]
    fn a_nonzero_counter_is_refused() {
        // Counter bytes live at 33..37; a replayed later-operation object
        // (counter > 0) is not an attestation.
        let f = build_fixture(|auth| auth[36] = 1);
        let err = verify_app_attest(
            &f.attestation_object,
            &f.key_id,
            &f.client_data_hash,
            TEST_APP_ID,
            &f.root_pem,
            now(),
        )
        .expect_err("nonzero counter must refuse");
        // The counter tamper also moves the nonce (authData is covered by it),
        // so either refusal is a correct fail-closed outcome; pin that it DOES
        // refuse and for a counter-or-nonce reason.
        assert!(
            err == "attestation counter is not zero" || err == "attestation nonce mismatch",
            "refused as: {err}"
        );
    }

    #[test]
    fn a_wrong_key_id_is_refused() {
        let f = build_fixture(|_| {});
        let err = verify_app_attest(
            &f.attestation_object,
            &[0xAB; 32],
            &f.client_data_hash,
            TEST_APP_ID,
            &f.root_pem,
            now(),
        )
        .expect_err("wrong key id must refuse");
        assert_eq!(err, "key id does not match the attested public key");
    }

    // ── The iOS arm over BOTH pinned App IDs (the main app + Fauna Kids) ──

    const ACTOR: [u8; 32] = [7; 32];
    const NONCE: [u8; 32] = [9; 32];

    /// An attestation from the bundle `rp_id` over a claim message naming
    /// `signed_app_id`, checked against the shipped pinned set.
    fn verify_ios_pair(rp_id: &str, signed_app_id: &str) -> Result<(), &'static str> {
        let message =
            age_claim_signed_message(&NONCE, AgeBand::Teen13To15.as_str(), signed_app_id, &ACTOR);
        let f = build_fixture_for(rp_id, Sha256::digest(&message).into(), |_| {});
        verify_ios_age_attestation(
            &f.attestation_object,
            &f.key_id,
            &NONCE,
            AgeBand::Teen13To15,
            &ACTOR,
            FAUNA_IOS_APP_IDS,
            &f.root_pem,
            now(),
        )
    }

    #[test]
    fn the_main_app_attestation_verifies() {
        verify_ios_pair(FAUNA_IOS_APP_ID, FAUNA_IOS_APP_ID).expect("main app verifies");
    }

    #[test]
    fn the_kids_app_attestation_verifies() {
        verify_ios_pair(FAUNA_IOS_KIDS_APP_ID, FAUNA_IOS_KIDS_APP_ID).expect("kids app verifies");
    }

    /// The RP-ID hash names one pinned App ID and the signed message the
    /// other: the nest signs over the one the hash names, so the client's
    /// message no longer binds — refused, both ways round.
    #[test]
    fn a_mismatched_app_id_pair_is_refused() {
        for (rp_id, signed) in [
            (FAUNA_IOS_KIDS_APP_ID, FAUNA_IOS_APP_ID),
            (FAUNA_IOS_APP_ID, FAUNA_IOS_KIDS_APP_ID),
        ] {
            assert_eq!(
                verify_ios_pair(rp_id, signed),
                Err("attestation nonce mismatch"),
                "rp {rp_id} / signed {signed}"
            );
        }
    }

    #[test]
    fn an_unpinned_bundle_is_refused() {
        let other = "7457N3M72H.social.fauna.other";
        assert_eq!(
            verify_ios_pair(other, other),
            Err("RP ID hash matches no pinned app id")
        );
    }

    #[test]
    fn the_pinned_app_id_is_chosen_by_the_rp_id_hash() {
        for app_id in FAUNA_IOS_APP_IDS {
            let f = build_fixture_for(app_id, [0; 32], |_| {});
            assert_eq!(
                pinned_app_id_for(&f.attestation_object, FAUNA_IOS_APP_IDS),
                Ok(*app_id)
            );
        }
    }

    #[test]
    fn indefinite_length_cbor_is_refused() {
        // 0xBF = indefinite-length map — CTAP2 canonical form never emits it,
        // and the strict reader must not walk it.
        let err = parse_attestation_object(&[0xBF, 0xFF]).expect_err("indefinite must refuse");
        assert_eq!(err, "indefinite-length CBOR refused");
    }

    /// The arming function's cfg-free body, every pair — the android-armed
    /// answers are reachable from no other path while the keys are unset.
    #[test]
    fn attestation_platforms_follow_both_armings() {
        assert_eq!(
            attestation_platforms_for(false, false),
            Vec::<String>::new()
        );
        assert_eq!(
            attestation_platforms_for(true, false),
            vec!["ios".to_string()]
        );
        assert_eq!(
            attestation_platforms_for(false, true),
            vec!["android".to_string()]
        );
        assert_eq!(
            attestation_platforms_for(true, true),
            vec!["ios".to_string(), "android".to_string()]
        );
        // The shipped build answers from the same constants the verifier reads.
        assert_eq!(
            attestation_platforms(),
            attestation_platforms_for(
                !FAUNA_IOS_APP_IDS.is_empty(),
                FAUNA_ANDROID_PLAY_APPS.iter().any(|app| app.keys.is_some())
            )
        );
    }

    /// A pinned Play Integrity pair must decode — a malformed constant fails
    /// this test, never a family's admission (it would read as unarmed).
    #[test]
    fn a_pinned_play_integrity_key_pair_decodes() {
        for app in FAUNA_ANDROID_PLAY_APPS {
            if let Some(keys) = app.keys {
                keys.material().expect("pinned Play Integrity keys decode");
            }
        }
    }

    /// The Play arm pins both android apps, each its own Play record.
    #[test]
    fn the_play_arm_pins_the_main_and_kids_packages() {
        let packages: Vec<&str> = FAUNA_ANDROID_PLAY_APPS.iter().map(|a| a.package).collect();
        assert_eq!(
            packages,
            vec![
                fauna_core::platform_ids::ANDROID_APPLICATION_ID,
                fauna_core::platform_ids::ANDROID_KIDS_APPLICATION_ID,
            ]
        );
    }

    const SRC: IssueSource = IssueSource::Peer([0x5e; 32]);

    #[tokio::test]
    async fn nonces_are_single_use() {
        let store = AgeNonceStore::new();
        let (nonce, expires_in) = store.issue(SRC).await.unwrap();
        assert_eq!(expires_in, AGE_NONCE_TTL_SECS);
        assert!(store.consume(&nonce).await, "first consume succeeds");
        assert!(!store.consume(&nonce).await, "second consume refused");
        assert!(!store.consume(&[0u8; 32]).await, "unknown nonce refused");
    }

    #[tokio::test]
    async fn one_source_is_refused_past_its_cap() {
        let store = AgeNonceStore::with_caps(2, 100);
        assert!(store.issue(SRC).await.is_ok());
        assert!(store.issue(SRC).await.is_ok());
        assert_eq!(store.issue(SRC).await, Err(ChallengeStoreFull));
        assert_eq!(store.len().await, 2, "a refusal writes nothing");
        assert!(store.issue(IssueSource::Peer([0x42; 32])).await.is_ok());
    }

    #[tokio::test]
    async fn a_many_slash64_flood_stays_bounded_and_a_light_source_still_mints() {
        // The age nonce is pre-identity: a flood spread over many /64s of one
        // /48 passes every per-source bound, so the store's global cap — with
        // the heaviest network evicted first — is what holds it.
        let store = AgeNonceStore::with_caps(3, 12);
        let (honest, _) = store.issue(SRC).await.unwrap();
        for sub in 0..40u8 {
            let mut key = [0u8; 32];
            key[..6].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0x00, 0x03]);
            key[7] = sub;
            for _ in 0..3 {
                assert!(store.issue(IssueSource::Peer(key)).await.is_ok());
            }
        }
        assert_eq!(store.len().await, 12, "the store stays at its cap");
        let light = IssueSource::Peer([0x77; 32]);
        let (fresh, _) = store
            .issue(light)
            .await
            .expect("a light source still mints");
        assert_eq!(store.len().await, 12);
        assert!(
            store.consume(&honest).await,
            "the flood evicted the light source's nonce"
        );
        assert!(store.consume(&fresh).await);
    }

    // ── Play Integrity ──────────────────────────────────────────────────────

    /// RFC 3394 AES key wrap — the test-side inverse of [`aes_key_unwrap`],
    /// used to seal synthetic tokens. Kept out of the production surface: the
    /// nest never wraps.
    fn aes_key_wrap(kek: &[u8; 32], key: &[u8]) -> Vec<u8> {
        use aes_gcm::aes::cipher::BlockEncrypt;
        let n = key.len() / 8;
        let cipher = Aes256::new_from_slice(kek).unwrap();
        let mut a = [0xA6u8; 8];
        let mut r: Vec<[u8; 8]> = key
            .chunks(8)
            .map(|c| {
                let mut b = [0u8; 8];
                b.copy_from_slice(c);
                b
            })
            .collect();
        for j in 0..6 {
            for i in 1..=n {
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&a);
                block[8..].copy_from_slice(&r[i - 1]);
                let mut block = Block::clone_from_slice(&block);
                cipher.encrypt_block(&mut block);
                let t = (n * j + i) as u64;
                let msb = u64::from_be_bytes(block[..8].try_into().unwrap()) ^ t;
                a = msb.to_be_bytes();
                r[i - 1].copy_from_slice(&block[8..]);
            }
        }
        let mut out = a.to_vec();
        out.extend(r.concat());
        out
    }

    #[test]
    fn aes_key_unwrap_matches_the_rfc_3394_vector() {
        // RFC 3394 § 4.6 — wrap 256 bits of key data with a 256-bit KEK.
        let kek: [u8; 32] =
            hex::decode("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F")
                .unwrap()
                .try_into()
                .unwrap();
        let key = hex::decode("00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F")
            .unwrap();
        let wrapped = hex::decode(
            "28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21",
        )
        .unwrap();
        assert_eq!(
            aes_key_wrap(&kek, &key),
            wrapped,
            "wrap reproduces the RFC vector"
        );
        assert_eq!(
            aes_key_unwrap(&kek, &wrapped).unwrap(),
            key,
            "unwrap inverts it"
        );
        let mut corrupt = wrapped.clone();
        corrupt[0] ^= 1;
        assert_eq!(
            aes_key_unwrap(&kek, &corrupt).unwrap_err(),
            "key unwrap integrity check failed"
        );
    }

    struct PlayFixture {
        token: Vec<u8>,
        message: Vec<u8>,
        keys: PlayIntegrityKeyMaterial,
    }

    const PLAY_NOW: u64 = 1_800_000_000;
    const PLAY_CLAIM_NONCE: [u8; 32] = [0x11; 32];
    const PLAY_CLAIM_ACTOR: [u8; 32] = [0x22; 32];

    /// A synthetic Play Integrity classic-request verdict token: an `ES256`
    /// JWS over the verdict JSON, sealed in an `A256KW`+`A256GCM` JWE — the
    /// exact profile Google emits. The signing key and the envelope keys are
    /// generated per fixture and INJECTED as [`PlayIntegrityKeyMaterial`]:
    /// the production constants are never involved and no test-only seam
    /// exists in the verifier (the App Attest fixture's discipline).
    fn play_fixture(mutate_verdict: impl FnOnce(&mut serde_json::Value)) -> PlayFixture {
        play_fixture_signed_by(
            &p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng),
            mutate_verdict,
        )
    }

    fn play_fixture_signed_by(
        signing_key: &p256::ecdsa::SigningKey,
        mutate_verdict: impl FnOnce(&mut serde_json::Value),
    ) -> PlayFixture {
        use rand::RngCore;
        let mut kek = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut kek);
        play_fixture_for(
            &kek,
            signing_key,
            FAUNA_ANDROID_APPLICATION_ID,
            FAUNA_ANDROID_APPLICATION_ID,
            mutate_verdict,
        )
    }

    /// The general fixture: a verdict Google minted for `package`, over a
    /// claim the app signed naming `signed_package`, sealed under `kek` and
    /// signed by `signing_key` — one app's Play Console pair.
    fn play_fixture_for(
        kek: &[u8; 32],
        signing_key: &p256::ecdsa::SigningKey,
        package: &str,
        signed_package: &str,
        mutate_verdict: impl FnOnce(&mut serde_json::Value),
    ) -> PlayFixture {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer;
        use p256::pkcs8::EncodePublicKey;
        use rand::RngCore;
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);

        let message = age_claim_signed_message(
            &PLAY_CLAIM_NONCE,
            "13-15",
            signed_package,
            &PLAY_CLAIM_ACTOR,
        );
        let mut verdict = serde_json::json!({
            "requestDetails": {
                "requestPackageName": package,
                "nonce": b64(&Sha256::digest(&message)),
                "timestampMillis": (PLAY_NOW * 1000).to_string(),
            },
            "appIntegrity": {
                "appRecognitionVerdict": "PLAY_RECOGNIZED",
                "packageName": package,
                "certificateSha256Digest": ["6a6a1474b5cbbb2b1aa57e0bc3"],
                "versionCode": "42",
            },
            "deviceIntegrity": {
                "deviceRecognitionVerdict": ["MEETS_DEVICE_INTEGRITY", "MEETS_BASIC_INTEGRITY"],
            },
            "accountDetails": { "appLicensingVerdict": "LICENSED" },
        });
        mutate_verdict(&mut verdict);

        let verification_key_der = signing_key
            .verifying_key()
            .to_public_key_der()
            .expect("spki")
            .as_bytes()
            .to_vec();
        let jws_header = b64(br#"{"alg":"ES256"}"#);
        let jws_payload = b64(verdict.to_string().as_bytes());
        let signing_input = format!("{jws_header}.{jws_payload}");
        let sig: p256::ecdsa::Signature = signing_key.sign(signing_input.as_bytes());
        let jws = format!("{signing_input}.{}", b64(&sig.to_bytes()));

        let kek = *kek;
        let mut cek = [0u8; 32];
        let mut iv = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut cek);
        rand::rngs::OsRng.fill_bytes(&mut iv);
        let jwe_header = b64(br#"{"alg":"A256KW","enc":"A256GCM"}"#);
        let sealed = Aes256Gcm::new_from_slice(&cek)
            .unwrap()
            .encrypt(
                GcmNonce::from_slice(&iv),
                Payload {
                    msg: jws.as_bytes(),
                    aad: jwe_header.as_bytes(),
                },
            )
            .expect("seal");
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let token = format!(
            "{jwe_header}.{}.{}.{}.{}",
            b64(&aes_key_wrap(&kek, &cek)),
            b64(&iv),
            b64(ciphertext),
            b64(tag)
        );
        PlayFixture {
            token: token.into_bytes(),
            message,
            keys: PlayIntegrityKeyMaterial {
                decryption_key: kek,
                verification_key_der,
            },
        }
    }

    fn verify_play(f: &PlayFixture) -> Result<(), &'static str> {
        verify_play_integrity(
            &f.token,
            &f.message,
            FAUNA_ANDROID_APPLICATION_ID,
            &f.keys,
            PLAY_NOW,
        )
    }

    /// One app's Play Console pair, as a test generates it.
    struct PlayPair {
        kek: [u8; 32],
        signer: p256::ecdsa::SigningKey,
    }

    fn play_pair() -> PlayPair {
        use rand::RngCore;
        let mut kek = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut kek);
        PlayPair {
            kek,
            signer: p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng),
        }
    }

    fn armed(package: &'static str, pair: &PlayPair) -> ArmedPlayApp {
        use p256::pkcs8::EncodePublicKey;
        ArmedPlayApp {
            package,
            material: PlayIntegrityKeyMaterial {
                decryption_key: pair.kek,
                verification_key_der: pair
                    .signer
                    .verifying_key()
                    .to_public_key_der()
                    .expect("spki")
                    .as_bytes()
                    .to_vec(),
            },
        }
    }

    /// A token `pair` sealed for `package` over a claim signed naming
    /// `signed_package`.
    fn play_token(pair: &PlayPair, package: &str, signed_package: &str) -> Vec<u8> {
        play_fixture_for(&pair.kek, &pair.signer, package, signed_package, |_| {}).token
    }

    /// Pick the app from `entries`, then verify against it — the android arm
    /// of `verify_age_attestation` minus the nonce store.
    fn verify_android(
        token: &[u8],
        entries: &[ArmedPlayApp],
        fully_armed: bool,
    ) -> Result<Option<&'static str>, &'static str> {
        let Some(app) = pinned_play_app_for(token, entries, fully_armed)? else {
            return Ok(None);
        };
        verify_android_age_attestation(
            token,
            &PLAY_CLAIM_NONCE,
            AgeBand::Teen13To15,
            &PLAY_CLAIM_ACTOR,
            app,
            PLAY_NOW,
        )?;
        Ok(Some(app.package))
    }

    #[test]
    fn each_pinned_play_app_verifies_under_its_own_pair() {
        let (main, kids) = (play_pair(), play_pair());
        let entries = [
            armed(FAUNA_ANDROID_APPLICATION_ID, &main),
            armed(FAUNA_ANDROID_KIDS_APPLICATION_ID, &kids),
        ];
        let main_token = play_token(
            &main,
            FAUNA_ANDROID_APPLICATION_ID,
            FAUNA_ANDROID_APPLICATION_ID,
        );
        let kids_token = play_token(
            &kids,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
        );
        assert_eq!(
            verify_android(&main_token, &entries, true),
            Ok(Some(FAUNA_ANDROID_APPLICATION_ID))
        );
        assert_eq!(
            verify_android(&kids_token, &entries, true),
            Ok(Some(FAUNA_ANDROID_KIDS_APPLICATION_ID))
        );
    }

    #[test]
    fn a_verdict_naming_another_app_than_the_key_that_opened_it_is_refused() {
        let (main, kids) = (play_pair(), play_pair());
        let entries = [
            armed(FAUNA_ANDROID_APPLICATION_ID, &main),
            armed(FAUNA_ANDROID_KIDS_APPLICATION_ID, &kids),
        ];
        // A kids verdict sealed under the MAIN app's pair: the main entry is
        // the one its key opens, and the verdict does not name it.
        let crossed = play_token(
            &main,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
        );
        assert_eq!(
            verify_android(&crossed, &entries, true),
            Err("verdict was requested for another package")
        );
        // A genuine kids verdict over a claim the app signed naming the main
        // package: the nonce binds a message the nest never rebuilds.
        let mis_signed = play_token(
            &kids,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
            FAUNA_ANDROID_APPLICATION_ID,
        );
        assert_eq!(
            verify_android(&mis_signed, &entries, true),
            Err("verdict nonce does not bind this claim")
        );
    }

    #[test]
    fn a_kids_token_checked_against_the_main_entry_alone_is_refused() {
        let (main, kids) = (play_pair(), play_pair());
        let kids_token = play_token(
            &kids,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
        );
        let main_entry = armed(FAUNA_ANDROID_APPLICATION_ID, &main);
        // Verified straight against the main entry, it fails at the envelope.
        assert_eq!(
            verify_android_age_attestation(
                &kids_token,
                &PLAY_CLAIM_NONCE,
                AgeBand::Teen13To15,
                &PLAY_CLAIM_ACTOR,
                &main_entry,
                PLAY_NOW,
            ),
            Err("key unwrap integrity check failed")
        );
        // With only the main entry pinned, no key opens it: refused.
        assert_eq!(
            verify_android(&kids_token, std::slice::from_ref(&main_entry), true),
            Err("no pinned Play app's key opens the verdict")
        );
    }

    /// The half-armed ruling: the main app armed, Fauna Kids pinned but
    /// unarmed. The kids app's genuine verdict opens under no armed key, so it
    /// is cannot-check (`Ok(None)`), while the main app still verifies.
    #[test]
    fn a_half_armed_play_arm_cannot_check_a_token_no_armed_key_opens() {
        let (main, kids) = (play_pair(), play_pair());
        let entries = [armed(FAUNA_ANDROID_APPLICATION_ID, &main)];
        let kids_token = play_token(
            &kids,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
            FAUNA_ANDROID_KIDS_APPLICATION_ID,
        );
        assert_eq!(verify_android(&kids_token, &entries, false), Ok(None));
        let main_token = play_token(
            &main,
            FAUNA_ANDROID_APPLICATION_ID,
            FAUNA_ANDROID_APPLICATION_ID,
        );
        assert_eq!(
            verify_android(&main_token, &entries, false),
            Ok(Some(FAUNA_ANDROID_APPLICATION_ID))
        );
        // A token off the pinned profile is still refused, half-armed or not.
        assert_eq!(
            verify_android(b"not.a.jwe", &entries, false),
            Err("token is not a five-part JWE")
        );
    }

    #[test]
    fn a_genuine_play_verdict_verifies() {
        let f = play_fixture(|_| {});
        assert_eq!(verify_play(&f), Ok(()));
    }

    #[test]
    fn a_play_verdict_for_another_nonce_is_refused() {
        let f = play_fixture(|v| {
            v["requestDetails"]["nonce"] = serde_json::Value::String("AAAA".into());
        });
        assert_eq!(
            verify_play(&f),
            Err("verdict nonce does not bind this claim")
        );
    }

    #[test]
    fn a_play_verdict_signed_by_a_foreign_key_is_refused() {
        let mut f = play_fixture(|_| {});
        let foreign = play_fixture(|_| {});
        // Google's signature is the one that matters: the same verdict under
        // another signer, opened with the pinned verification key, refuses.
        f.keys.verification_key_der = foreign.keys.verification_key_der;
        assert_eq!(verify_play(&f), Err("verdict signature invalid"));
    }

    #[test]
    fn a_tampered_play_envelope_is_refused() {
        let mut f = play_fixture(|_| {});
        // Flip one ciphertext byte — the GCM tag catches it before any JSON
        // is ever parsed.
        let token = String::from_utf8(f.token.clone()).unwrap();
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut ct = b64url(&parts[3]).unwrap();
        ct[0] ^= 0x01;
        use base64::Engine as _;
        parts[3] = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&ct);
        f.token = parts.join(".").into_bytes();
        assert_eq!(verify_play(&f), Err("JWE authentication failed"));
    }

    #[test]
    fn a_play_envelope_under_the_wrong_decryption_key_is_refused() {
        let mut f = play_fixture(|_| {});
        f.keys.decryption_key[0] ^= 0x01;
        assert_eq!(verify_play(&f), Err("key unwrap integrity check failed"));
    }

    #[test]
    fn an_unpinned_jwe_algorithm_is_refused() {
        let mut f = play_fixture(|_| {});
        use base64::Engine as _;
        let token = String::from_utf8(f.token.clone()).unwrap();
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        parts[0] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"A128KW","enc":"A256GCM"}"#);
        f.token = parts.join(".").into_bytes();
        assert_eq!(verify_play(&f), Err("JWE key algorithm is not A256KW"));
    }

    #[test]
    fn an_unrecognized_play_app_is_refused() {
        let f = play_fixture(|v| {
            v["appIntegrity"]["appRecognitionVerdict"] = "UNRECOGNIZED_VERSION".into();
        });
        assert_eq!(verify_play(&f), Err("app is not Play-recognized"));
    }

    #[test]
    fn a_play_verdict_for_another_package_is_refused() {
        let f = play_fixture(|v| {
            v["requestDetails"]["requestPackageName"] = "com.example.impostor".into();
        });
        assert_eq!(
            verify_play(&f),
            Err("verdict was requested for another package")
        );
        let f = play_fixture(|v| {
            v["appIntegrity"]["packageName"] = "com.example.impostor".into();
        });
        assert_eq!(verify_play(&f), Err("verdict names another package"));
    }

    #[test]
    fn a_device_without_play_integrity_is_refused() {
        let f = play_fixture(|v| {
            v["deviceIntegrity"]["deviceRecognitionVerdict"] =
                serde_json::json!(["MEETS_BASIC_INTEGRITY"]);
        });
        assert_eq!(
            verify_play(&f),
            Err("device does not meet Play device integrity")
        );
    }

    #[test]
    fn an_unlicensed_play_install_is_refused() {
        let f = play_fixture(|v| {
            v["accountDetails"]["appLicensingVerdict"] = "UNLICENSED".into();
        });
        assert_eq!(verify_play(&f), Err("install is not Play-licensed"));
    }

    #[test]
    fn a_stale_play_verdict_is_refused() {
        let f = play_fixture(|v| {
            v["requestDetails"]["timestampMillis"] = ((PLAY_NOW - AGE_NONCE_TTL_SECS - 1) * 1000)
                .to_string()
                .into();
        });
        assert_eq!(
            verify_play(&f),
            Err("verdict timestamp outside the nonce lifetime")
        );
        // Exactly the TTL is still inside the nonce's lifetime (inclusive
        // boundary — the push relay's freshness rule, `api-layers.md`).
        let f = play_fixture(|v| {
            v["requestDetails"]["timestampMillis"] =
                ((PLAY_NOW - AGE_NONCE_TTL_SECS) * 1000).to_string().into();
        });
        assert_eq!(verify_play(&f), Ok(()));
    }
}
