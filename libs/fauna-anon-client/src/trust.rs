//! Process-wide TLS-trust state + the channel-binding graduation entrypoint —
//! the glue both native bearer paths share (`docs/goal/architecture/security.md`
//! § Transport trust, § Cross-connection binding).
//!
//! A Fauna app opens two TLS connections to a nest: the pre-identity one that
//! carries `fauna.auth.handshake` (which authenticates a `cert → identity`
//! binding), and a separate bearer-carrying one (the authenticated WS, and the
//! residual HTTP content API). The binding is verified on the first; this module
//! carries its result to the second by pinning the bound SPKI. Both
//! `fauna_client::ws_challenge_bearer::WsChallengeBearer` (FFI clients) and `fauna-launch-machine` (the linux desktop) graduate their
//! handshake through [`graduate_handshake`] and read the pin through
//! [`pinned_spki`], so the trust state is one process-global rather than threaded
//! through every client object — the nest's served cert is a property of the
//! *host*, not of any particular client (the SSH `known_hosts` model).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use fauna_protocol::auth::CertBinding;

use crate::cert_binding::{
    BindingError, IdentityError, IdentityRoot, MemoryPinStore, NestIdentityPinStore,
    check_identity_root, verify_cert_binding,
};
use crate::tls_verify::CapturedCert;

/// Two layers of trust state: durable TOFU **identity** pins (`nest_actor_id`
/// per host; survive restarts when a disk-backed store is installed) and an
/// ephemeral per-host **bound-SPKI** cache the binding refreshes.
struct TrustState {
    pins: RwLock<Arc<dyn NestIdentityPinStore>>,
    spki: Mutex<HashMap<String, [u8; 32]>>,
}

fn state() -> &'static TrustState {
    static STATE: OnceLock<TrustState> = OnceLock::new();
    STATE.get_or_init(|| TrustState {
        pins: RwLock::new(Arc::new(MemoryPinStore::new())),
        spki: Mutex::new(HashMap::new()),
    })
}

/// The identity every login signature on `client` binds — the nest at the far
/// end of THIS connection, read through `fauna.auth.nest_handshake` before
/// anything is signed (`login.md` § Binding the nest). The native arm of the
/// one shared reader, `fauna_client_core::nest_trust::read_login_binding`: on
/// TLS the binding is SPKI-compared against the cert this connection received,
/// so a relaying box cannot present the real nest's identity over its own
/// channel; on plaintext (the loopback dev/e2e nest) it is possession-only.
///
/// A refusal or transport fault rides out as the connection's own error
/// (verbatim — a degraded nest's `fauna.nest.outdated` classifies as it would
/// from the ceremony itself); a box proving no identity, or a binding that
/// fails verification, is a [`TrustError`] — the connection is untrusted and
/// no login is signed over it (security.md § Connection-teardown rule).
pub async fn read_login_binding(
    client: &crate::AnonymousNestClient,
) -> Result<[u8; 32], crate::AnonClientError> {
    use fauna_client_core::nest_trust::{LoginBindingError, read_login_binding};
    let captured = client.captured_cert();
    match read_login_binding(client, captured.spki.as_ref()).await {
        Ok(id) => Ok(id),
        Err(LoginBindingError::Refused(e)) | Err(LoginBindingError::Transport(e)) => Err(e),
        Err(LoginBindingError::NoBinding) => {
            Err(crate::AnonClientError::Trust(TrustError::BindingRequired))
        }
        Err(LoginBindingError::Binding(e)) => {
            Err(crate::AnonClientError::Trust(TrustError::Binding(e)))
        }
    }
}

/// Install a (typically disk-backed) identity pin store at app startup, replacing
/// the in-memory default so TOFU pins survive restarts. Last writer wins; clients
/// call this once with a store rooted at their data dir.
pub fn install_pin_store(store: Arc<dyn NestIdentityPinStore>) {
    *state().pins.write().unwrap() = store;
}

/// The bound-SPKI a prior handshake graduated for `host`, if any. The bearer
/// connection requires it (`tls_verify::spki_pinned_client_config`).
pub fn pinned_spki(host: &str) -> Option<[u8; 32]> {
    state().spki.lock().unwrap().get(host).copied()
}

/// The TOFU-pinned `nest_actor_id` for `host`, if any — read through whatever
/// pin store is installed. Lets the launch path distinguish "no valid binding,
/// nothing pinned" (a plaintext / first-contact nest — proceed) from "no valid
/// binding but a pin EXISTS" (the withdrawn/downgrade case — warn like a
/// changed identity, `security.md` § Transport trust).
pub fn pinned_identity(host: &str) -> Option<[u8; 32]> {
    state().pins.read().unwrap().get(host)
}

/// The identity the open connection `conn` to `nest_url` is **bound** to,
/// natively: the host's pin (graduated by the login's SPKI compare, and the
/// bearer connection's TLS is pinned to that SPKI, so on `https` it names the
/// box at the far end of this very connection), else a possession proof over
/// `conn` — [`fauna_client_core::nest_trust::read_bound_identity`] over this
/// process's pin store. Never the nest's own `fauna.nest.info` claim.
pub async fn connection_bound_identity<R>(
    conn: &R,
    nest_url: &str,
) -> Result<[u8; 32], fauna_client_core::nest_trust::LoginBindingError<R::Error>>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    fauna_client_core::nest_trust::read_bound_identity(
        conn,
        pinned_identity(&authority_of(nest_url)),
    )
    .await
}

/// The escrow-holder trust set for `nest_url` — the pinned identity
/// (`pinned_identity` over `authority_of`), which every consumer of the R14 (account-data-plane.md § The ratified decisions)
/// escrow trust reads through ONE door so the rule ("the identity we pinned,
/// never the nest's own claim about itself") lives in one place. The pin is
/// TOFU's first-contact mint, the claim ceremony's seed, or — since the login's
/// pin ruling (2026-10-05, [`record_login_identity`]) — the identity any TLS
/// login verified, a public-CA nest's included; so the set is empty only on a
/// plaintext nest or before this machine's first login.
///
/// **Test-capable builds only:** `FAUNA_E2E_TRUST_NEST_IDENTITY` joins the
/// set — the e2e's stand-in for the TLS-channel-binding pin a plaintext rig
/// can never graduate, keyed by nest exactly as that pin is (`<nest url>=<64
/// hex>` entries; `e2e_seeded_identity` below owns the grammar). An inner
/// runtime switch within a test-capable build, never the security boundary
/// (e2e convention 15); the variable is read per call, but consumers that
/// capture the set at assembly (the account runtime's `GenerationTrust`) still
/// need it present at launch.
pub fn trusted_escrow_holders(nest_url: &str) -> Vec<[u8; 32]> {
    // Only the cfg-gated e2e arm below ever pushes to `holders` — a release
    // build without test-helpers never mutates it and `-D unused-mut` fires.
    // Scoped to exactly that configuration so a genuinely redundant `mut`
    // still goes red whenever the e2e arm IS compiled in.
    #[cfg_attr(
        not(any(test, debug_assertions, feature = "test-helpers")),
        allow(unused_mut)
    )]
    let mut holders: Vec<[u8; 32]> = pinned_identity(&authority_of(nest_url))
        .into_iter()
        .collect();
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    if let Some(id) = e2e_seeded_identity(nest_url)
        && !holders.contains(&id)
    {
        holders.push(id);
    }
    holders
}

/// The identity a NEST-form custody accept binds for `nest_url` — the
/// TOFU-pinned identity (`pinned_identity` over [`authority_of`]), read
/// through ONE door for the same reason [`trusted_escrow_holders`] is: the
/// accept names *the identity this app pinned*, never the nest's own claim
/// about itself (the device-or-nest bullet's item 1; the R14 escrow-holder
/// rule generalized). `None` → the consent card's nest choice is absent.
///
/// **Test-capable builds only:** `FAUNA_E2E_TRUST_NEST_IDENTITY` stands in
/// when no pin exists — exactly as it does for the escrow set, keyed by nest
/// the same way, and for the same reason: a plaintext e2e rig can never
/// graduate the TLS channel-binding pin. An inner runtime switch within a
/// test-capable build, never the security boundary (e2e convention 15).
pub fn pinned_nest_custodian_identity(nest_url: &str) -> Option<[u8; 32]> {
    let pinned = pinned_identity(&authority_of(nest_url));
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    if pinned.is_none() {
        return e2e_seeded_identity(nest_url);
    }
    pinned
}

/// **Test-capable builds only.** The e2e trust seed's identity for `nest_url`,
/// read from `FAUNA_E2E_TRUST_NEST_IDENTITY` in the one grammar every host
/// parses (`fauna_client_core::nest_trust::seeded_nest_identity` — web reads
/// the same seed from localStorage).
///
/// Keyed because the pin it stands in for is per nest. An unkeyed seed
/// trusted one nest's key for every nest, so an e2e app pointed at a second
/// nest trusted the first nest's escrow receipts there, and a custody accept
/// would have bound the first nest's identity. A bare identity names no nest,
/// so it is honoured for none. Pinned by `tests/e2e_trust_seed.rs`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn e2e_seeded_identity(nest_url: &str) -> Option<[u8; 32]> {
    let seed = std::env::var(fauna_client_core::nest_trust::E2E_TRUST_NEST_IDENTITY).ok()?;
    fauna_client_core::nest_trust::seeded_nest_identity(&seed, nest_url)
}

/// Forget the TOFU identity pin for `host` — the explicit, user-approved
/// recovery behind the "trust this nest" re-trust action (the `ssh-keygen -R
/// host` analogue; never called automatically). Also drops the graduated
/// bound-SPKI cache for `host`, so the next connect re-runs the full
/// channel-binding graduation and re-TOFUs from scratch.
pub fn forget_identity_pin(host: &str) {
    state().pins.read().unwrap().remove(host);
    state().spki.lock().unwrap().remove(host);
}

/// Seed the identity pin for `nest_url`'s authority from a **claim ceremony's
/// possession-proven first-contact root** — the native half of the shared
/// claim-time seeding (`security.md` § Transport trust: the wizard proves the
/// box's identity against the injected-seed / pasted-URI root on every
/// pre-claim dial, and this write is what lets that proof outlive the wizard,
/// so the first post-claim dial — usually over the reach hint, before the
/// domain resolves — verifies instead of TOFU-ing whatever answers).
///
/// Deliberately an authoritative overwrite, not `pin_if_absent`: the claim is
/// a user-initiated ceremony on a root the connection just possession-proved —
/// evidence strictly stronger than the TOFU pin it may replace — and a "start
/// over onto a new box, same domain" must install the new box's root rather
/// than wedge the first launch on an `IdentityChanged` the old pin would
/// force. This and the user's explicit re-trust are the only sanctioned
/// callers of a bare [`NestIdentityPinStore::set`]; a *check* still never
/// re-pins.
///
/// Keyed by [`authority_of`] — exactly the key the launch path reads
/// (`connect_silent_challenge` graduates against `authority_of(nest_url)`) —
/// and written through the **installed** store, so whatever backend the app
/// installed at startup (`DiskPinStore` on every native app) is what the next
/// launch reads back. The wasm twin writes `LocalStoragePinStore` keyed by the
/// nest URL verbatim, from the same machine helper.
pub fn seed_claimed_identity_pin(nest_url: &str, actor_id: [u8; 32]) {
    let pins = state().pins.read().unwrap();
    let authority = authority_of(nest_url);
    // Re-seeding the root the pin ALREADY NAMES is a no-op: `set` replaces
    // the whole entry, so it would erase a chain-accepted `rotation_seq` —
    // the state both of `evaluate_rotation_bridge`'s fork clauses are gated
    // on — silently downgrading a hard `PinForked` to a re-trustable
    // `IdentityChanged` on the repeat-claim path. The seeding exists to replace TOFU-or-nothing with the
    // proven root; when the pin already names that root there is nothing to
    // seed. A DIFFERING root still overwrites authoritatively — the old
    // chain's seq belongs to the old box.
    if pins.get(&authority) == Some(actor_id) {
        return;
    }
    pins.set(&authority, actor_id);
}

/// **Test-only.** Pin `actor_id` for `nest_url`'s authority in the *installed*
/// store — the native half of the E2E bridge's `set_nest_identity_pin_for_test`
/// (`docs/goal/behavior/onboarding.md` § E2E bridge contract). A test seeds a pin
/// the nest cannot prove, relaunches, and the launch path must then warn
/// (`LaunchPhase::IdentityChanged`) instead of auto-entering.
///
/// Writing through the **installed** store — not a fresh one — is what makes the
/// seam backend-agnostic: whatever the client installed at startup (a
/// `DiskPinStore` rooted at its data dir, on every native app) is what the
/// test seeds and the next launch reads back, so the harness never has to know
/// the on-disk shape. The wasm twin writes `LocalStoragePinStore` instead; both
/// hang off the one dispatcher arm.
///
/// Gated because a *silent* global pin write is precisely what the TOFU model
/// forbids in production ([`NestIdentityPinStore::set`]: a pin changes only on a
/// clean first connect or an explicit user-approved re-pin, never from a check).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn pin_identity_for_test(nest_url: &str, actor_id: [u8; 32]) {
    state()
        .pins
        .read()
        .unwrap()
        .set(&authority_of(nest_url), actor_id);
}

/// **Test-only.** The pin the installed store holds for `nest_url`'s authority —
/// the read half of [`pin_identity_for_test`], so a cross-app e2e can assert
/// "the trust button forgot the pin" through the bridge rather than reaching into
/// a per-app backend (the web journey used to read localStorage via `eval_js`,
/// which no native app can do).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn pinned_identity_for_test(nest_url: &str) -> Option<[u8; 32]> {
    pinned_identity(&authority_of(nest_url))
}

/// **Test-only.** Record a chain-accepted re-pin (`NestIdentityPinStore::
/// set_rotation_accepted`) for `nest_url`'s authority in the *installed*
/// store — the seed half of the guard's own precondition: a test asserting that a same-root
/// re-seed leaves an existing chain-accepted `rotation_seq` intact needs a
/// way to put that seq on the pin FIRST, and no production path does this
/// outside a real verified rotation chain.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn set_rotation_accepted_for_test(nest_url: &str, actor_id: [u8; 32], seq: u64) {
    state()
        .pins
        .read()
        .unwrap()
        .set_rotation_accepted(&authority_of(nest_url), actor_id, seq);
}

/// **Test-only.** The chain-accepted `rotation_seq` the installed store holds
/// for `nest_url`'s authority, if any — the read half of
/// [`set_rotation_accepted_for_test`], and the property the guard
/// protects: a
/// same-root re-seed through [`seed_claimed_identity_pin`] must never erase
/// this.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn rotation_seq_for_test(nest_url: &str) -> Option<u64> {
    state()
        .pins
        .read()
        .unwrap()
        .rotation_seq(&authority_of(nest_url))
}

/// Build a rustls `ClientConfig` for the **bearer reqwest leg** (the residual HTTP
/// content API + `POST /register`) that pins the SPKI the WS handshake graduated
/// for `nest_url`'s host, reading the *live* pin per-handshake (security.md
/// § Cross-connection binding).
///
/// The reqwest client is built before any handshake graduates a pin and rustls'
/// `ServerName` lacks the port the pin map is keyed on, so a fixed
/// [`crate::tls_verify::spki_pinned_client_config`] can't be baked in — this reads
/// `pinned_spki(authority)` each handshake, picking up the pin the WS leg
/// graduates and re-reading it across a cert rotation.
///
/// No graduated pin + a non-WebPKI cert ⇒ **refused** ([`NoPinPolicy::RequireWebPki`]),
/// never accept-any — which is exactly what retires `FAUNA_INSECURE_TLS`; a
/// public-CA nest still takes the boring WebPKI path. Pass the result to
/// [`reqwest::ClientBuilder::use_preconfigured_tls`].
pub fn store_pinned_reqwest_tls(nest_url: &str) -> rustls::ClientConfig {
    use crate::tls_verify::{NoPinPolicy, dynamic_pinned_client_config};
    let authority = authority_of(nest_url);
    dynamic_pinned_client_config(
        Arc::new(move || pinned_spki(&authority)),
        NoPinPolicy::RequireWebPki,
    )
}

/// Why a handshake failed to graduate to a trusted connection. Every variant
/// means the connection must be torn down before any bearer or request is sent
/// (security.md § Connection-teardown rule).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    /// The nest served a cert that does not chain to the public WebPKI roots
    /// (self-signed / LAN), yet sent no channel binding — so it cannot be
    /// authenticated at all.
    #[error("nest served a non-WebPKI cert but sent no channel binding")]
    BindingRequired,
    /// No leaf cert was captured during the TLS handshake (a malformed cert, or
    /// graduate_handshake called on a plaintext connection — a caller bug).
    #[error("no server certificate was captured during the TLS handshake")]
    NoCapturedSpki,
    #[error(transparent)]
    Binding(#[from] BindingError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

/// A graduation failure that *is* the identity-changed verdict, with the
/// fingerprints its warning names.
///
/// Produced only by [`classify_identity_changed`] — the one table for "is this
/// failure the `known_hosts` warning, or ordinary trust trouble?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityChangedVerdict {
    /// Hex `nest_actor_id` this client had pinned for the host.
    pub pinned_hex: String,
    /// Hex `nest_actor_id` the nest actually proved — `None` in the
    /// withdrawn/downgrade case, where it proved no identity at all.
    pub seen_hex: Option<String>,
    /// Rotation-chain **fork evidence** rather than a plain change: the same
    /// blocking surface, but no re-trust affordance (`box-recovery.md`
    /// § Client acceptance).
    pub fork: bool,
}

/// Decide whether a graduation failure is the identity-changed surface:
/// a [`IdentityError::PinChanged`] (the nest proved a *different* identity than
/// the pin), a [`IdentityError::PinForked`] (rotation-chain fork evidence — same
/// surface, `fork: true`, no re-trust), or a binding absence/failure while a pin
/// EXISTS for `host` (the withdrawn/downgrade case — an attacker who omits the
/// binding must not demote the loud warning to a retry spinner). `None` means the
/// failure is not pin-related (first-contact binding trouble, root mismatch) and
/// keeps its caller's existing mapping.
///
/// **Shared because this function *is* the rule** — the same reasoning
/// `fauna_launch_machine::classify_silent_challenge` records for the silent-
/// challenge channel. It began private to the launch machine's token-refresh
/// path; the bearer mint became the second consumer when
/// `security.md` § Post-auth surfacing's taxonomy leg landed, and a rule with two
/// call sites and one copy is the only kind that cannot drift between them.
///
/// The pin lookup is the only I/O-ish part and it reads the installed pin store,
/// so callers pass the same `host` authority they graduated against
/// ([`authority_of`]).
pub fn classify_identity_changed(e: &TrustError, host: &str) -> Option<IdentityChangedVerdict> {
    match e {
        TrustError::Identity(IdentityError::PinChanged { pinned, seen }) => {
            Some(IdentityChangedVerdict {
                pinned_hex: hex::encode(pinned),
                seen_hex: Some(hex::encode(seen)),
                fork: false,
            })
        }
        TrustError::Identity(IdentityError::PinForked { pinned, seen }) => {
            Some(IdentityChangedVerdict {
                pinned_hex: hex::encode(pinned),
                seen_hex: Some(hex::encode(seen)),
                fork: true,
            })
        }
        // No valid binding this connect. With no pin that's first-contact
        // trouble (the caller's existing transient/unauthorized mapping); with a
        // pin it is the withdrawn case — the nest we previously trusted can no
        // longer prove it is the same nest.
        TrustError::BindingRequired | TrustError::Binding(_) => {
            pinned_identity(host).map(|pinned| IdentityChangedVerdict {
                pinned_hex: hex::encode(pinned),
                seen_hex: None,
                fork: false,
            })
        }
        _ => None,
    }
}

/// Whether a WebPKI-valid cert waives the channel binding for `host` — the
/// boring public-CA path, taken **only when no Axis-2 root is held**
/// (security.md § Transport trust: non-WebPKI certs require the binding, "and
/// so does *any* cert once a root is held"). A root is held in either of two
/// ways:
///
/// - the caller pre-resolved one (`expected_root`: DNS `self=`, the injected
///   deployment seed, a grant-carried federation identity), or
/// - the install-scoped pin store already names an identity for `host` — the
///   claim-seeded pin on a client-provisioned box
///   ([`seed_claimed_identity_pin`]), or an ordinary TOFU pin.
///
/// The second arm is what makes the ruling outlive first contact. The
/// bearer-mint and launch wrappers ([`graduate_handshake`],
/// [`graduate_verify_path`]) never see the seed, so on a § B-IP bridged box —
/// publicly-trusted `ip`-identifier cert **and** a held seed — the first bearer
/// mint after first contact used to take the waiver unconditionally, and
/// [`clear_graduated_spki`] dropped the pin first contact had just written: the
/// seed-derived protection lasted exactly one connection. The claim ceremony
/// seeds the pin store with that same identity, so consulting the pin is how
/// "a root is held" reaches every later graduation. With a root held the full
/// core runs and **re-pins the served SPKI**, which is also what carries a
/// native app across the bridge cert's ~4-day rotation
/// (tls-certificates.md § B-IP) — merely refusing to clear would strand a
/// stale pin no bearer dial can refresh (`tls_dial`'s graduate-and-retry arm
/// runs only when no pin exists). This is the ONE decision point all four
/// graduation entrypoints share; none re-derives the rule inline.
///
/// **A binding already in hand is never waived (ruled 2026-10-05, security.md
/// § Transport trust → *The login's pin*).** The waiver answers "may a
/// public-CA nest go *unbound*?" — the question a pre-identity first contact
/// asks before spending a round trip on `fauna.auth.nest_handshake`. The login
/// paths never ask it: `read_login_binding` demanded a binding before anything
/// was signed, and the bearer-mint reply carries one on every TLS nest. A
/// WebPKI-valid cert says the client reached the name it dialed; the binding
/// says *which nest* that is, and throwing it away left a public-CA nest that
/// no claim ceremony had pinned with no identity pin at all — so the account
/// runtime's R14 escrow trust (`trusted_escrow_holders`, defined AS the pin)
/// was empty and every GenerationTip write was refused for good. With the
/// binding kept the full core runs: SPKI compare, the DNS `self=` root where
/// the zone publishes one, else TOFU — exactly what web always did
/// (`check_web_nest_identity` pins the possession-proved identity on first
/// contact) and what a claimed B-IP box already rides.
pub(crate) fn webpki_waives_binding(
    host: &str,
    captured: &CapturedCert,
    expected_root: Option<[u8; 32]>,
    binding: Option<&CertBinding>,
) -> bool {
    binding.is_none()
        && captured.webpki_valid
        && expected_root.is_none()
        && pinned_identity(host).is_none()
}

/// A WebPKI-valid graduation that **waives the binding** — no root held,
/// [`webpki_waives_binding`] — clears any SPKI an earlier graduation pinned for
/// the same host. Without this, a nest that rotates from the self-signed floor
/// to an ACME cert mid-process bricks every pinned bearer dial until the
/// process restarts: the stale pin never matches the new leaf, and the
/// WebPKI-valid graduations that follow used to early-return without touching
/// it. On the waived path that is consistent with the ratified model — a
/// WebPKI-valid cert with no root is authenticated the boring way and needs no
/// pin (`graduate_handshake_with_root`'s own contract) — and this runs only
/// inside a graduation, i.e. at an authenticated moment on the mint path,
/// never from an unauthenticated observation. Once a root is held the clearing
/// never runs: the core re-pins the fresh leaf instead, which serves the same
/// rotation without downgrading a pinned host.
fn clear_graduated_spki(host: &str) {
    state().spki.lock().unwrap().remove(host);
}

/// 32 fresh random bytes for a handshake's per-connection `client_nonce`
/// (security.md § Transport trust, Axis 1). Panics only if the OS RNG fails,
/// which is unrecoverable.
pub fn fresh_nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    getrandom::fill(&mut n).expect("OS RNG available for channel-binding nonce");
    n
}

/// Extract the `host[:port]` authority from a nest URL, for keying trust
/// state — this crate's **pin-store key**, and what [`is_loopback_authority`]
/// classifies, where a `[::1]@` prefix would otherwise buy a remote nest the
/// same-box self-signed-cert carve-out. The parsing contract (WHATWG-parity
/// scheme strip, userinfo drop, the `/`-vs-`\` boundary) lives in
/// [`fauna_core::web::authority_of`], the shared home this and `fauna-wasm`'s
/// browser twin both delegate to.
pub fn authority_of(nest_url: &str) -> String {
    fauna_core::web::authority_of(nest_url)
}

/// True if the `host[:port]` authority (see [`authority_of`]) names a loopback
/// host — IPv4 `127.0.0.0/8`, IPv6 `::1`, or `localhost`. The residual-HTTP
/// cert-validation callback uses this to accept a same-box self-signed nest (the
/// sanctioned `danger_accept_invalid_certs` / `InsecureSkipVerify` prior art,
/// installers/windows.md) — including the unauthenticated health check that can run
/// before any handshake graduates a pin. Private LAN, public domains, and
/// unparseable authorities are non-loopback.
pub fn is_loopback_authority(authority: &str) -> bool {
    let host = host_of_authority(authority);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `host[:port]` → bare `host`, ready to `parse::<IpAddr>()`. Handles bracketed
/// IPv6 (`[::1]:443` → `::1`), bare IPv6 (`::1` — multiple colons, no port),
/// `host:port`, and a bare host. The canonical userinfo-strip, split, and
/// bracket-strip composed; see [`fauna_core::web::split_host_port`] for why
/// those are separate primitives rather than one flag.
///
/// The userinfo strip is applied here as well as in [`authority_of`] on
/// purpose: [`is_loopback_authority`] is a UniFFI export every app's cert
/// callback reaches, so it must classify by the real host whoever built the
/// authority string it is handed.
fn host_of_authority(authority: &str) -> &str {
    let hostport = fauna_core::web::strip_userinfo(authority);
    fauna_core::resolve::strip_ipv6_brackets(fauna_core::web::split_host_port(hostport).0)
}

/// Resolve the Axis-2 DNS `self=` identity root for a nest authority, if any
/// (security.md § Transport trust, Axis 2). Returns the expected `nest_actor_id`
/// for a **public** domain that publishes `_fauna.{host}` TXT `self=`, and `None`
/// for everything else — loopback / IP-literal / `.local` (no DNS authority), and
/// public domains that publish no `self=` (both fall back to TOFU-on-host, which
/// is non-breaking: the channel binding still authenticates the connection, just
/// without the stronger pre-resolved identity). This is the **only** async,
/// I/O-doing part of graduation, deliberately kept out of the sync verification
/// core ([`graduate_handshake_with_root`]).
pub async fn resolve_dns_self_root(authority: &str) -> Option<[u8; 32]> {
    // Reuse the resolver's `.local`/IP classification so "what counts as local"
    // (and thus has no DNS authority) has one source of truth.
    let host = fauna_provisioning::probe::public_dns_host(authority)?;
    let txt = fauna_core::resolve::lookup_fauna_txt(host).await;
    let raw = fauna_core::resolve::extract_txt_value(&txt, "self")?;
    // A malformed `self=` falls back to TOFU rather than failing the connection —
    // TOFU still requires a valid channel binding, so this is safe, not a bypass.
    fauna_core::hex32::decode(raw).ok()
}

/// Verify a `fauna.auth.handshake` reply's channel binding against the cert the
/// client actually received, and update the process trust state. Call **only**
/// for a TLS (`https`/`wss`) connection — a plaintext loopback dev nest has no
/// cert to bind and keeps the network-trust posture (the caller guards on scheme).
///
/// This is the production entrypoint: it resolves the Axis-2 identity root
/// (a DNS `self=` lookup for public domains, [`resolve_dns_self_root`] — the only
/// async step) and delegates to the sync [`graduate_handshake_with_root`]. A
/// WebPKI cert short-circuits before any DNS lookup only when the reply
/// carried no binding and nothing is held ([`webpki_waives_binding`]); the
/// bearer-mint reply of every TLS nest carries one, so on the login path the
/// full core runs and the identity it verifies is recorded as the host's pin
/// ([`record_login_identity`]) — the pin the account runtime's escrow trust
/// reads.
///
/// `client` is the requester over the **same connection** the binding rode —
/// on an identity mismatch the rotation bridge fetches the box's rotation
/// chain there before any warning surfaces (`box-recovery.md` § Client
/// acceptance; [`try_rotation_bridge`]).
pub async fn graduate_handshake<R: fauna_protocol::RpcRequester>(
    client: &R,
    host: &str,
    captured: &CapturedCert,
    client_nonce: &[u8],
    binding: Option<&CertBinding>,
) -> Result<(), TrustError> {
    // WebPKI-valid, no binding offered, no root held → boring path; skip the
    // DNS lookup. A binding in hand, or a held identity pin (claim-seeded or
    // TOFU), runs the full core below, which re-pins the served SPKI.
    if webpki_waives_binding(host, captured, None, binding) {
        clear_graduated_spki(host);
        return Ok(());
    }
    let dns_self = resolve_dns_self_root(host).await;
    let graduated =
        match graduate_handshake_with_root(host, captured, client_nonce, binding, dns_self) {
            Err(e) => {
                try_rotation_bridge(client, host, captured, client_nonce, binding, dns_self, e)
                    .await?
            }
            Ok(g) => g,
        };
    record_login_identity(host, graduated);
    Ok(())
}

/// What a graduation proved — the core's answer, which the two login wrappers
/// record ([`record_login_identity`]) and every other caller may ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graduated {
    /// The WebPKI waiver ([`webpki_waives_binding`]): a publicly-trusted cert,
    /// no binding offered, no root held. Authenticated by the CA alone; nothing
    /// was pinned.
    Waived,
    /// The binding verified against the received SPKI and the identity it names
    /// passed its Axis-2 root check; the bound SPKI is pinned for the host.
    Verified([u8; 32]),
}

/// **The login's pin** (security.md § Transport trust → *The login's pin*,
/// ruled 2026-10-05): a login graduation leaves the pin store naming the
/// identity it verified, whatever root proved it. The TOFU arm pinned it in
/// the core already (a no-op here); the pre-resolved arms — DNS `self=`, the
/// pasted or injected root — never TOFU-pin in the core, so this is where the
/// root a login verified becomes the host's pin, **authoritatively**: a root
/// outranks any TOFU pin (the same reasoning as [`seed_claimed_identity_pin`],
/// and the public-domain row's "the client accepts whatever head the zone
/// names"). Recording the identity the pin already names is a no-op, so an
/// accepted rotation's seq survives every login. Why the LOGIN and not the
/// core: the escrow-holder set the account runtime trusts is defined as this
/// pin (R14), and a login is the one graduation that both proves the bound
/// nest's identity and runs in the process that owns the writable store —
/// `graduate_first_contact`'s callers (the wizard, whose claim seeds its own
/// pin; the byte-plane's grant-carried root, which security.md fences from
/// the login rows; the consumer's never-minting heal) record nothing new. A
/// read-only (consumer) store is never written.
fn record_login_identity(host: &str, graduated: Graduated) {
    let Graduated::Verified(id) = graduated else {
        return;
    };
    let pins = state().pins.read().unwrap();
    if pins.read_only() || pins.get(host) == Some(id) {
        return;
    }
    pins.set(host, id);
}

/// The rotation-acceptance arm shared by [`graduate_handshake`] and
/// [`graduate_verify_path`] (`box-recovery.md` § Client acceptance — re-pin on
/// a verified chain). Two identity-mismatch shapes can be bridged by a
/// committed deployment-seed rotation, both fetched over the SAME connection
/// whose live binding already proved the presented identity:
///
/// - **TOFU row** — [`IdentityError::PinChanged`]: fetch the chain, and when it
///   links the pinned identity to the live-verified one, move the pin
///   (storing the accepted seq) and re-run the graduation. Fork evidence
///   surfaces as [`IdentityError::PinForked`] (hard warning, no re-trust);
///   no valid chain keeps the original `PinChanged` (today's warning).
/// - **Pre-resolved row** — [`BindingError::IdentityMismatch`] with a DNS
///   `self=` root (the propagation window, § DNS row): re-verify the binding
///   without the expected root to learn the live identity, walk the chain
///   expected → live, and on success graduate against the live identity. No
///   pin is involved (pre-resolved roots are never TOFU-pinned), so there is
///   no fork arm — failure keeps the original error (today's retry surface,
///   which heals when DNS converges).
///
/// `nonce` is the exact nonce message the failing graduation verified over —
/// the handshake path's `client_nonce`, or the verify path's NT-1 combined
/// `challenge ‖ client` form. The re-verification and the post-acceptance
/// re-graduation use the same message, so the bridge can never accept under
/// a nonce the original verification would have refused. (The verify path
/// once carried a second, challenge-only arm for a pre-fold nest; the
/// compat-remnant sweep removed it 2026-09-24.)
async fn try_rotation_bridge<R: fauna_protocol::RpcRequester>(
    client: &R,
    host: &str,
    captured: &CapturedCert,
    nonce: &[u8],
    binding: Option<&CertBinding>,
    dns_self: Option<[u8; 32]>,
    err: TrustError,
) -> Result<Graduated, TrustError> {
    use fauna_client_core::nest_trust::{RotationRepin, try_rotation_repin};

    // The original graduation, re-run against a (possibly updated) identity
    // root.
    let regraduate =
        |root: Option<[u8; 32]>| graduate_handshake_with_root(host, captured, nonce, binding, root);

    match err {
        TrustError::Identity(IdentityError::PinChanged { pinned, seen }) => {
            let pins = state().pins.read().unwrap().clone();
            match try_rotation_repin(client, host, pins.as_ref(), pinned, seen).await {
                RotationRepin::Repinned { seq } => {
                    // The pin now names the head; the same verification passes
                    // and pins the bound SPKI for the bearer connection.
                    tracing::warn!(
                        "rotation chain accepted for {host}: re-pinned {} -> {} (seq {seq})",
                        fauna_core::hex32::encode(&pinned),
                        fauna_core::hex32::encode(&seen)
                    );
                    regraduate(dns_self)
                }
                RotationRepin::Fork => {
                    tracing::warn!(
                        "rotation FORK evidence for {host}: pinned {}, presented {}",
                        fauna_core::hex32::encode(&pinned),
                        fauna_core::hex32::encode(&seen)
                    );
                    Err(TrustError::Identity(IdentityError::PinForked {
                        pinned,
                        seen,
                    }))
                }
                RotationRepin::NoBridge { reason } => {
                    tracing::warn!(
                        "rotation chain did not bridge for {host} ({reason}) — keeping the                          identity-changed warning"
                    );
                    Err(TrustError::Identity(IdentityError::PinChanged {
                        pinned,
                        seen,
                    }))
                }
            }
        }
        TrustError::Binding(BindingError::IdentityMismatch) => {
            // Only a pre-resolved root produces this; re-verify rootless to
            // learn which identity the live binding actually proves.
            let (Some(expected), Some(b), Some(received_spki)) = (dns_self, binding, captured.spki)
            else {
                return Err(err);
            };
            let Ok(live) = verify_cert_binding(&received_spki, nonce, b, None) else {
                return Err(err);
            };
            let reply: fauna_protocol::nest_rotation::RotationChainReply = match client
                .request(
                    fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND,
                    fauna_protocol::nest_rotation::RotationChainRequest::default(),
                )
                .await
            {
                Ok(r) => r,
                Err(_) => return Err(err),
            };
            match fauna_client_core::nest_trust::evaluate_rotation_bridge(
                &reply.chain,
                &expected,
                &live,
                None,
            ) {
                fauna_client_core::nest_trust::RotationBridge::Accepted { .. } => {
                    regraduate(Some(live))
                }
                _ => Err(err),
            }
        }
        other => Err(other),
    }
}

/// Verify a `fauna.auth.verify` reply's channel binding against the received
/// cert — the **silent-challenge launch path's** graduation, twin to
/// [`graduate_handshake`]. The nest signs `served_SPKI ‖ challenge_nonce ‖
/// client_nonce` on this path (the NT-1 client-nonce fold), and that exact
/// message is the only one verified — the challenge-only retry that once
/// accepted a pre-fold nest's 2-part binding (and its downgrade-harvest
/// residual) was removed 2026-09-24 by the compat-remnant sweep; the refusal
/// is pinned on this function by
/// `a_two_part_verify_path_binding_is_refused_by_the_production_door`. Call
/// only for TLS (`https`/`wss`) connections, like the handshake twin.
///
/// Before this existed the native launch path minted and *used* a bearer with
/// no identity check at all — only the token-refresh handshake graduated, so
/// an impersonated nest was spoken to until the first refresh or content
/// connect. `security.md` § Transport trust.
pub async fn graduate_verify_path<R: fauna_protocol::RpcRequester>(
    client: &R,
    host: &str,
    captured: &CapturedCert,
    challenge_nonce: &[u8; 32],
    client_nonce: &[u8; 32],
    binding: Option<&CertBinding>,
) -> Result<(), TrustError> {
    // Same waiver rule as `graduate_handshake`: a binding in hand or a pinned
    // host runs the core, and the identity it verifies becomes the pin.
    if webpki_waives_binding(host, captured, None, binding) {
        clear_graduated_spki(host);
        return Ok(());
    }
    let dns_self = resolve_dns_self_root(host).await;
    let mut combined = Vec::with_capacity(64);
    combined.extend_from_slice(challenge_nonce);
    combined.extend_from_slice(client_nonce);
    let graduated = match graduate_handshake_with_root(host, captured, &combined, binding, dns_self)
    {
        Err(e) => {
            try_rotation_bridge(client, host, captured, &combined, binding, dns_self, e).await?
        }
        Ok(g) => g,
    };
    record_login_identity(host, graduated);
    Ok(())
}

/// The sync, transport-free verification core (no TLS, no DNS — the I/O lives in
/// the async [`graduate_handshake`] wrapper). Given a *pre-resolved* identity root
/// (`expected_root` — from DNS `self=` **or** a client-injected deployment seed on
/// a client-provisioned box, security.md § Transport trust Axis 2), verify the
/// binding and update trust state.
///
/// - **WebPKI-valid cert, no binding offered AND no root held** — no
///   `binding`, no `expected_root`, and no identity pin for `host`
///   ([`webpki_waives_binding`]) → authenticated the boring way (public CA +
///   hostname); no SPKI is pinned (WebPKI secures the bearer connection too).
///   Returns [`Graduated::Waived`]. A binding in hand or a held root
///   suppresses this arm — see the notes on both below.
/// - **Otherwise** (a self-signed / LAN cert, or any cert with a binding in
///   hand) → the binding is the authentication: require it, verify the nest's
///   signature over the **received** SPKI, check the identity against its
///   root — `expected_root = Some(expected)` → exact match (the core pins no
///   identity; the login wrappers record it, [`record_login_identity`]);
///   `None` → TOFU-on-host (pin on first connect) — and pin the bound SPKI
///   for the bearer connection. Returns [`Graduated::Verified`] with the
///   identity the binding proved. Any failure is a hard error.
pub fn graduate_handshake_with_root(
    host: &str,
    captured: &CapturedCert,
    client_nonce: &[u8],
    binding: Option<&CertBinding>,
    expected_root: Option<[u8; 32]>,
) -> Result<Graduated, TrustError> {
    graduate_handshake_with_root_minting(
        host,
        captured,
        client_nonce,
        binding,
        expected_root,
        PinMinting::StoreDecides,
    )
}

/// Whether a graduation that finds **no identity pin and no pre-resolved
/// root** for its host may TOFU-mint one (security.md § Pin custody across
/// processes, rule 2: first trust is a user decision, made where a user is
/// present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMinting {
    /// The installed store decides: a writable store mints on first contact
    /// (the interactive app's signed bearer mint and the claim ceremony — the
    /// paths a user is behind); a read-only store
    /// ([`NestIdentityPinStore::read_only`], a pin-consumer process) never
    /// does. What every graduation did before the variant below existed.
    StoreDecides,
    /// Never mint, whatever the store: verify against a held identity pin or
    /// a pre-resolved root, else refuse ([`IdentityError::PinRequired`]). The
    /// bearer dial's graduate-and-retry fallback graduates this way
    /// ([`crate::AnonymousNestClient::graduate_transport_trust`]): it runs on
    /// a *failed* dial, with a bearer in hand and no user in the loop, in
    /// every process shape — an interactive app's writable store included —
    /// and a public-CA nest is unpinned until this machine's first login
    /// completes (the login's pin, [`record_login_identity`]; before that
    /// ruling it stayed unpinned for good), so letting the store decide there
    /// let an on-path box that serves its own
    /// self-signed cert and answers the handshake with its own key be pinned,
    /// then re-dialed with the real bearer.
    Never,
}

/// [`graduate_handshake_with_root`] with the TOFU-mint decision made
/// explicit — the core the wrapper and the never-minting fallback share.
pub fn graduate_handshake_with_root_minting(
    host: &str,
    captured: &CapturedCert,
    client_nonce: &[u8],
    binding: Option<&CertBinding>,
    expected_root: Option<[u8; 32]>,
    minting: PinMinting,
) -> Result<Graduated, TrustError> {
    // ⚠ Like `graduate_first_contact`'s twin arm, this short-circuit is
    // suppressed whenever a root is held (ruled 2026-09-02). It used to return before
    // `expected_root` was read, safe on a premise about what a box *serves* — a
    // domainless box served its self-signed floor, so `webpki_valid` was false
    // through the whole pre-claim window — which `tls-certificates.md` § B-IP
    // (the IP bridge cert) deletes. The full reasoning is at `client.rs`'s
    // `SkippedWebPki` arm; in short, a held root is the stronger authenticator
    // ("is this the nest I provisioned", not "is this the address I dialed") and
    // is therefore always used when the caller has one. "Held" also covers an
    // identity pin the store already names for `host` — the claim-seeded pin
    // on a provisioned box — which is how the ruling outlives first contact on
    // the bearer and launch paths ([`webpki_waives_binding`]), and a binding
    // in hand is never waived either (the login's pin, ruled 2026-10-05).
    if webpki_waives_binding(host, captured, expected_root, binding) {
        clear_graduated_spki(host);
        return Ok(Graduated::Waived);
    }
    let received_spki = captured.spki.ok_or(TrustError::NoCapturedSpki)?;
    let binding = binding.ok_or(TrustError::BindingRequired)?;
    // When a pre-resolved root exists (DNS `self=`, or the injected deployment
    // seed), enforce it up front (and again below); otherwise the identity root
    // is TOFU-on-host (LAN/`.local`/self-hosted, or a public domain that
    // published no `self=`).
    let verified = verify_cert_binding(
        &received_spki,
        client_nonce,
        binding,
        expected_root.as_ref(),
    )?;
    let pins = state().pins.read().unwrap().clone();
    let root = match expected_root {
        Some(expected) => IdentityRoot::PreResolved(expected),
        // A pin-consumer process (read-only store — a File Provider extension,
        // a background agent) never TOFU-mints, and neither does a graduation
        // whose caller said so ([`PinMinting::Never`] — the bearer dial's
        // fallback, whatever store the process installed): no pin for `host`
        // fails the connect (`IdentityError::PinRequired`) until the
        // interactive path pins the nest. security.md § Pin custody across
        // processes.
        None if minting == PinMinting::Never || pins.read_only() => {
            IdentityRoot::TofuStrict { host }
        }
        None => IdentityRoot::Tofu { host },
    };
    check_identity_root(&verified, root, pins.as_ref())?;
    state()
        .spki
        .lock()
        .unwrap()
        .insert(host.to_string(), received_spki);
    Ok(Graduated::Verified(verified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn make_binding(nest: &SigningKey, spki: &[u8; 32], nonce: &[u8]) -> CertBinding {
        CertBinding::sign(nest, spki, nonce)
    }

    #[test]
    fn authority_strips_scheme_and_path() {
        assert_eq!(authority_of("https://pi.local:8443/api"), "pi.local:8443");
        assert_eq!(authority_of("http://192.168.1.5"), "192.168.1.5");
        assert_eq!(authority_of("wss://nest.example.com/"), "nest.example.com");
    }

    #[test]
    fn authority_strips_userinfo_so_the_pin_key_is_the_real_host() {
        // The authority is the pin-store key *and* the loopback classifier's
        // input, so it must name the host a URL parser would connect to. A
        // crafted `userinfo@` prefix used to survive into both: it keyed the
        // TOFU identity pin under a host-that-isn't (defeating the
        // `known_hosts` change warning for the real host by adding `user@`),
        // and it let `[::1]@` masquerade as loopback. WHATWG takes the host
        // after the *last* `@`, which is what `strip_userinfo` does.
        assert_eq!(
            authority_of("https://[::1]@nest.example.com/api/v1/health"),
            "nest.example.com"
        );
        assert_eq!(
            authority_of("https://user:pass@nest.example.com:8443/api"),
            "nest.example.com:8443"
        );
        assert_eq!(authority_of("wss://alice@pi.local:8443"), "pi.local:8443");
        // No userinfo: unchanged, including a bare IPv6 literal whose colons
        // must not be mistaken for a userinfo separator.
        assert_eq!(authority_of("https://[::1]:8443/x"), "[::1]:8443");
    }

    #[test]
    fn is_loopback_authority_classifies_host() {
        // The residual-HTTP cert-validation callback accepts a same-box self-signed
        // nest by classifying its `host[:port]` authority as loopback. IPv4 loopback
        // is the whole `127.0.0.0/8`; IPv6 loopback is `::1` (bracketed or bare);
        // `localhost` is loopback by name. Private LAN (`192.168.*`) and public
        // domains are not. (Mirrors the windows C# `NestCertTrustTests` spec.)
        for (authority, expected) in [
            ("127.0.0.1:443", true),
            ("127.0.0.1", true),
            ("127.5.6.7", true), // anywhere in 127.0.0.0/8
            ("localhost:443", true),
            ("localhost", true),
            ("LocalHost", true), // case-insensitive
            ("[::1]:443", true),
            ("::1", true),
            ("192.168.1.5:443", false),
            ("nest.example.com:443", false),
            ("nest.example.com", false),
            ("", false),
            // Unparseable authorities are non-loopback, as this function's own
            // doc says. A non-numeric port suffix used to be discarded, so
            // these reached the loopback branch as bare `localhost`/`127.0.0.1`
            // and were granted same-box self-signed-cert trust.
            ("localhost:junk", false),
            ("127.0.0.1:junk", false),
            ("localhost:99999", false),
            // The bracketed twins of the three above. They used to classify
            // LOOPBACK, because the bracketed branch of `split_host_port`
            // silently discarded an unparseable port suffix while the
            // unbracketed branch left the whole string as host — the opposite
            // fail direction, in the same function, for the same input class.
            ("[::1]:junk", false),
            ("[::1]:99999", false),
            ("[127.0.0.1]:junk", false),
            // Userinfo names a *credential*, not a host: WHATWG resolves the
            // host of `https://[::1]@nest.example.com/` to `nest.example.com`,
            // so a `[::1]@` prefix must not buy same-box self-signed trust for
            // a connection that really goes to a remote nest.
            ("[::1]@nest.example.com", false),
            ("127.0.0.1@nest.example.com", false),
            ("localhost@nest.example.com:443", false),
            // ...and the converse still classifies by the *real* host, so the
            // strip is not a blanket "anything with an `@` is remote".
            ("alice@127.0.0.1:443", true),
            // Positive controls: the fix must not make brackets categorically
            // non-loopback, nor break a well-formed bracketed authority.
            ("[::1]:8443", true),
            ("[::1]", true),
            ("[127.0.0.1]:443", true),
        ] {
            assert_eq!(
                is_loopback_authority(authority),
                expected,
                "is_loopback_authority({authority:?})"
            );
        }

        // Composition row: `authority_of` used to end the authority at `/`
        // or `\` only, so this query-carried `[::1]` was read as the
        // authority and classified loopback, granting a *remote* nest the
        // same-box self-signed-cert carve-out .
        assert!(!is_loopback_authority(&authority_of(
            "https://nest.example.com?@[::1]"
        )));
    }

    #[test]
    fn webpki_valid_needs_no_binding_and_pins_nothing() {
        let cap = CapturedCert {
            spki: Some([0x11; 32]),
            webpki_valid: true,
        };
        assert!(
            graduate_handshake_with_root("ca.example.com", &cap, &[0u8; 32], None, None).is_ok()
        );
        assert_eq!(
            pinned_spki("ca.example.com"),
            None,
            "WebPKI host is not pinned"
        );
    }

    #[test]
    fn webpki_valid_still_requires_the_binding_when_a_root_is_held() {
        // Ruled 2026-09-02: a
        // caller holding an Axis-2 root (the injected deployment seed on a
        // client-provisioned box) holds the *stronger* authenticator, so WebPKI
        // no longer waives the binding for it.
        //
        // Before `tls-certificates.md` § B-IP this arm could not be reached with
        // a held root — a domainless box served an untrusted floor, so
        // `webpki_valid` was false through the entire pre-claim window. The IP
        // bridge cert makes the box publicly trusted from first boot, which is
        // exactly when the seed is the only thing that says *which* nest this is.
        let cap = CapturedCert {
            spki: Some([0x11; 32]),
            webpki_valid: true,
        };
        assert!(
            matches!(
                graduate_handshake_with_root(
                    "ca.example.com",
                    &cap,
                    &[0u8; 32],
                    None,
                    Some([0x99; 32]),
                ),
                Err(TrustError::BindingRequired)
            ),
            "a held root must not be waived by a publicly-trusted cert"
        );
        assert!(
            graduate_handshake_with_root("ca.example.com", &cap, &[0u8; 32], None, None).is_ok(),
            "…while the no-root case keeps the boring WebPKI path, unchanged"
        );
    }

    #[test]
    fn self_signed_requires_binding() {
        let cap = CapturedCert {
            spki: Some([0x22; 32]),
            webpki_valid: false,
        };
        assert_eq!(
            graduate_handshake_with_root("host-no-binding.local", &cap, &[0u8; 32], None, None),
            Err(TrustError::BindingRequired)
        );
    }

    #[test]
    fn self_signed_binding_verifies_then_pins() {
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let spki = [0x33u8; 32];
        let nonce = [0x44u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let host = "pinme.local";
        assert!(graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), None).is_ok());
        assert_eq!(
            pinned_spki(host),
            Some(spki),
            "bound SPKI is pinned for the bearer conn"
        );
    }

    #[test]
    fn webpki_graduation_with_no_root_clears_a_stale_pin() {
        // A DNS-`self=`-rooted public nest on the self-signed floor rotates to
        // an ACME cert mid-process. Its floor graduation pinned the bound SPKI
        // but — pre-resolved roots never TOFU-pin — no identity, so the next
        // graduation holds no root: WebPKI-valid, boring path, and the stale
        // SPKI must not outlive the rotation (every pinned bearer dial would
        // hard-fail until the process restarts).
        let nest = SigningKey::from_bytes(&[9u8; 32]);
        let spki = [0x99u8; 32];
        let nonce = [0xabu8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let host = "floor-then-acme.example";
        let root = Some(nest.verifying_key().to_bytes());
        assert!(graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), root).is_ok());
        assert_eq!(pinned_spki(host), Some(spki), "floor graduation pins");
        assert_eq!(
            pinned_identity(host),
            None,
            "a pre-resolved root never TOFU-pins an identity"
        );
        // The next graduation sees the ACME cert: WebPKI-valid, nothing held,
        // boring path — and the stale pin is gone.
        let rotated = CapturedCert {
            spki: Some([0x9au8; 32]),
            webpki_valid: true,
        };
        assert!(graduate_handshake_with_root(host, &rotated, &nonce, None, None).is_ok());
        assert_eq!(
            pinned_spki(host),
            None,
            "a WebPKI-valid graduation with no root held clears the stale pin"
        );
    }

    #[test]
    fn a_pinned_host_re_pins_across_a_floor_to_acme_rotation() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        // A TOFU-pinned nest (LAN / no `self=`) rotates from the floor to an
        // ACME cert. The identity pin is a held root, so the WebPKI-valid
        // graduation does not waive the binding: it verifies the same identity
        // and RE-PINS the new leaf's SPKI — the rotation is served by refreshing
        // the pin, never by downgrading the host to WebPKI alone.
        let nest = SigningKey::from_bytes(&[10u8; 32]);
        let id = nest.verifying_key().to_bytes();
        let host = "pinned-floor-then-acme.local";
        let nonce = [0xacu8; 32];
        let floor_spki = [0xa1u8; 32];
        let floor = CapturedCert {
            spki: Some(floor_spki),
            webpki_valid: false,
        };
        let binding = make_binding(&nest, &floor_spki, &nonce);
        assert!(graduate_handshake_with_root(host, &floor, &nonce, Some(&binding), None).is_ok());
        assert_eq!(pinned_identity(host), Some(id), "TOFU pins the identity");
        assert_eq!(pinned_spki(host), Some(floor_spki));

        let acme_spki = [0xa2u8; 32];
        let acme = CapturedCert {
            spki: Some(acme_spki),
            webpki_valid: true,
        };
        // A pinned nest may not go silent: no binding is a hard failure…
        assert!(
            matches!(
                graduate_handshake_with_root(host, &acme, &nonce, None, None),
                Err(TrustError::BindingRequired)
            ),
            "a pinned host requires the binding even on a WebPKI-valid cert"
        );
        assert_eq!(
            pinned_spki(host),
            Some(floor_spki),
            "a refused graduation touches no pin"
        );
        // …and with the binding present the fresh leaf is re-pinned.
        let binding = make_binding(&nest, &acme_spki, &nonce);
        assert!(graduate_handshake_with_root(host, &acme, &nonce, Some(&binding), None).is_ok());
        assert_eq!(
            pinned_spki(host),
            Some(acme_spki),
            "the ACME leaf replaces the floor pin — nothing stranded"
        );
        assert_eq!(
            pinned_identity(host),
            Some(id),
            "same identity, pin untouched"
        );
    }

    #[test]
    fn webpki_waives_the_binding_only_with_no_root_and_no_pin() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        let webpki = CapturedCert {
            spki: Some([0xc1u8; 32]),
            webpki_valid: true,
        };
        let floor = CapturedCert {
            spki: Some([0xc1u8; 32]),
            webpki_valid: false,
        };
        let host = "waiver-table.local";
        assert!(
            webpki_waives_binding(host, &webpki, None, None),
            "public CA, nothing held, no binding offered: the boring path"
        );
        let nest = SigningKey::from_bytes(&[13u8; 32]);
        let binding = make_binding(&nest, &[0xc1u8; 32], &[0u8; 32]);
        assert!(
            !webpki_waives_binding(host, &webpki, None, Some(&binding)),
            "a binding in hand is never waived (the login's pin, 2026-10-05)"
        );
        assert!(
            !webpki_waives_binding(host, &webpki, Some([0xc2u8; 32]), None),
            "a pre-resolved root is held"
        );
        assert!(
            !webpki_waives_binding(host, &floor, None, None),
            "a non-WebPKI cert never waives"
        );
        state().pins.read().unwrap().set(host, [0xc3u8; 32]);
        assert!(
            !webpki_waives_binding(host, &webpki, None, None),
            "an identity pin for the host is a held root"
        );
        assert!(
            webpki_waives_binding("other-host.local", &webpki, None, None),
            "the pin is per host"
        );
    }

    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_claim_seeded_pin_keeps_a_bridged_box_bound_past_first_contact() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        // A § B-IP bridged box: dialed by public IP (no DNS authority, so the
        // wrappers do no lookup), serving a publicly-trusted `ip`-identifier
        // cert, provisioned by this client (the injected seed's identity is
        // the held root). Ruled 2026-09-02: the pin
        // first contact writes must survive every later bearer-path
        // graduation — which never sees the seed itself, only the identity
        // pin the claim ceremony seeded from it.
        let nest = SigningKey::from_bytes(&[11u8; 32]);
        let impostor = SigningKey::from_bytes(&[12u8; 32]);
        let id = nest.verifying_key().to_bytes();
        let nest_url = "https://203.0.113.7:8443";
        let authority = authority_of(nest_url);
        let host = authority.as_str();
        let bridge_spki = [0xb1u8; 32];
        let bridge = CapturedCert {
            spki: Some(bridge_spki),
            webpki_valid: true,
        };
        let no_chain = ChainRequester(vec![]);

        // 1. First contact with the injected root (`graduate_first_contact`'s
        //    core call) pins the bridge cert's SPKI; the claim ceremony then
        //    seeds the identity pin (`seed_identity_pin_at_claim`).
        let nonce = [0xb2u8; 32];
        let binding = make_binding(&nest, &bridge_spki, &nonce);
        graduate_handshake_with_root(host, &bridge, &nonce, Some(&binding), Some(id))
            .expect("first contact graduates against the injected root");
        assert_eq!(pinned_spki(host), Some(bridge_spki));
        seed_claimed_identity_pin(nest_url, id);
        assert_eq!(pinned_identity(host), Some(id));

        // 2. The next bearer mint graduates through the wrapper. Before the
        //    fix its WebPKI arm cleared the pin here — one connection's worth
        //    of protection.
        let nonce = [0xb3u8; 32];
        let binding = make_binding(&nest, &bridge_spki, &nonce);
        graduate_handshake(&no_chain, host, &bridge, &nonce, Some(&binding))
            .await
            .expect("the bearer mint graduates against the seeded pin");
        assert_eq!(
            pinned_spki(host),
            Some(bridge_spki),
            "the seed-derived pin survives the bearer mint"
        );

        // 3. The silent-challenge launch twin: same host, same outcome.
        let challenge = [0xb4u8; 32];
        let client_nonce = [0xb5u8; 32];
        let mut combined = Vec::with_capacity(64);
        combined.extend_from_slice(&challenge);
        combined.extend_from_slice(&client_nonce);
        let binding = make_binding(&nest, &bridge_spki, &combined);
        graduate_verify_path(
            &no_chain,
            host,
            &bridge,
            &challenge,
            &client_nonce,
            Some(&binding),
        )
        .await
        .expect("the launch path graduates against the seeded pin");
        assert_eq!(pinned_spki(host), Some(bridge_spki), "…and the launch path");

        // 4. The bridge cert rotates (~4 days): the wrapper re-pins the fresh
        //    leaf rather than stranding the old one.
        let rotated_spki = [0xb6u8; 32];
        let rotated = CapturedCert {
            spki: Some(rotated_spki),
            webpki_valid: true,
        };
        let nonce = [0xb7u8; 32];
        let binding = make_binding(&nest, &rotated_spki, &nonce);
        graduate_handshake(&no_chain, host, &rotated, &nonce, Some(&binding))
            .await
            .expect("the rotated bridge cert graduates");
        assert_eq!(
            pinned_spki(host),
            Some(rotated_spki),
            "the rotation re-pins"
        );

        // 5. A recycled address: another box at the same IP holding its own
        //    publicly-trusted cert — WebPKI alone would accept it. The seeded
        //    pin refuses both shapes: a binding by the wrong key, and no
        //    binding at all.
        let stranger_spki = [0xb8u8; 32];
        let stranger = CapturedCert {
            spki: Some(stranger_spki),
            webpki_valid: true,
        };
        let nonce = [0xb9u8; 32];
        let binding = make_binding(&impostor, &stranger_spki, &nonce);
        let err = graduate_handshake(&no_chain, host, &stranger, &nonce, Some(&binding))
            .await
            .expect_err("a different identity behind a valid IP cert is refused");
        assert!(
            matches!(err, TrustError::Identity(IdentityError::PinChanged { .. })),
            "got {err:?}"
        );
        let err = graduate_handshake(&no_chain, host, &stranger, &nonce, None)
            .await
            .expect_err("withholding the binding is not a way around the pin");
        assert!(matches!(err, TrustError::BindingRequired), "got {err:?}");
        assert_eq!(
            pinned_spki(host),
            Some(rotated_spki),
            "a refused graduation touches no pin"
        );
        assert_eq!(pinned_identity(host), Some(id));
    }

    /// The verify-path downgrade-harvest refusal, pinned on the PRODUCTION
    /// door: a client that folded
    /// a nonce verifies `challenge ‖ client` and nothing else, so a 2-part
    /// binding (the shape a nest signs for a client that sent no nonce, and
    /// the one an attacker harvests by omitting its own) is refused, never
    /// retried over the challenge alone. Self-signed capture with no pin
    /// held, so the WebPKI waiver cannot short-circuit it. Mutation check:
    /// re-adding a challenge-only fallback to `graduate_verify_path` reds the
    /// first assertion and nothing else.
    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_two_part_verify_path_binding_is_refused_by_the_production_door() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        let nest = SigningKey::from_bytes(&[21u8; 32]);
        let spki = [0xd1u8; 32];
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let host = "two-part-refused.local";
        let no_chain = ChainRequester(vec![]);
        let challenge = [0xd2u8; 32];
        let client_a = [0xd3u8; 32];
        let client_b = [0xd4u8; 32];

        // 1. A genuine 2-part binding over the challenge alone is refused.
        let two_part = make_binding(&nest, &spki, &challenge);
        let err = graduate_verify_path(
            &no_chain,
            host,
            &cap,
            &challenge,
            &client_a,
            Some(&two_part),
        )
        .await
        .expect_err("a challenge-only binding must not graduate a nonce-folding client");
        assert!(matches!(err, TrustError::Binding(_)), "got {err:?}");
        assert_eq!(pinned_spki(host), None, "a refused graduation pins nothing");

        // 2. A 3-part binding harvested under client_a is refused for a
        //    victim that folded a fresh client_b.
        let mut combined_a = challenge.to_vec();
        combined_a.extend_from_slice(&client_a);
        let harvested = make_binding(&nest, &spki, &combined_a);
        let err = graduate_verify_path(
            &no_chain,
            host,
            &cap,
            &challenge,
            &client_b,
            Some(&harvested),
        )
        .await
        .expect_err("a binding harvested under another client nonce must not graduate");
        assert!(matches!(err, TrustError::Binding(_)), "got {err:?}");
        assert_eq!(pinned_spki(host), None);

        // 3. Control: the same harvested binding graduates the client that
        //    chose client_a — the refusals above are about the nonce, not
        //    about this fixture being unusable.
        graduate_verify_path(
            &no_chain,
            host,
            &cap,
            &challenge,
            &client_a,
            Some(&harvested),
        )
        .await
        .expect("the 3-part binding graduates its own client");
        assert_eq!(pinned_spki(host), Some(spki));
    }

    #[test]
    fn substituted_cert_rejected_and_not_pinned() {
        // The nest signs the SPKI it serves; the client received a different one
        // (a MITM). The binding must reject it and pin nothing.
        let nest = SigningKey::from_bytes(&[7u8; 32]);
        let nest_spki = [0x33u8; 32];
        let nonce = [0x55u8; 32];
        let binding = make_binding(&nest, &nest_spki, &nonce);
        let cap = CapturedCert {
            spki: Some([0xeeu8; 32]), // what the MITM presented
            webpki_valid: false,
        };
        let host = "mitm-target.local";
        assert_eq!(
            graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), None),
            Err(TrustError::Binding(BindingError::SpkiMismatch))
        );
        assert_eq!(pinned_spki(host), None);
    }

    /// The claim seed must not disarm fork detection: re-seeding the root the pin already
    /// names is a NO-OP, because `set` replaces the whole entry and would
    /// erase the chain-accepted `rotation_seq` — the state both of
    /// `evaluate_rotation_bridge`'s fork clauses are gated on. A DIFFERING
    /// root still overwrites authoritatively (the second half below): a
    /// start-over onto a new box on the same domain installs the new root,
    /// and the old chain's seq rightly dies with the old box's pin.
    #[test]
    fn a_reseed_of_the_same_root_keeps_the_chain_accepted_seq() {
        let host = "seed-keeps-seq.example";
        let head = [0xABu8; 32];
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = Arc::new(MemoryPinStore::new());
        install_pin_store(store.clone());
        store.set_rotation_accepted(host, head, 7);

        seed_claimed_identity_pin("https://seed-keeps-seq.example", head);
        assert_eq!(pinned_identity(host), Some(head));
        assert_eq!(
            store.rotation_seq(host),
            Some(7),
            "ROTATION UNTOUCHED: the chain-accepted seq must survive a claim \
             seed of the SAME root (it is what arms fork detection in \
             evaluate_rotation_bridge)"
        );

        // The differing-root half doubles as the same-store witness: if the
        // global had been swapped mid-test, this write would miss `store` and
        // the read below would still answer `head`.
        let other = [0xCDu8; 32];
        seed_claimed_identity_pin("https://seed-keeps-seq.example", other);
        assert_eq!(store.get(host), Some(other));
        assert_eq!(
            store.rotation_seq(host),
            None,
            "a differing root overwrites authoritatively — the old chain's \
             seq belongs to the old box"
        );
    }

    #[test]
    fn dns_self_root_verifies_exact_match_and_pins_no_identity() {
        // A public domain serving a self-signed cert with a resolved DNS `self=`
        // root: the binding verifies against the expected identity, the bound
        // SPKI is pinned for the bearer connection, but no TOFU *identity* pin is
        // written (the DNS root needs none — security.md § Axis 2).
        let nest = SigningKey::from_bytes(&[8u8; 32]);
        let expected = nest.verifying_key().to_bytes();
        let spki = [0x66u8; 32];
        let nonce = [0x77u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let host = "selfsigned-public.example";
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        assert!(
            graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), Some(expected))
                .is_ok()
        );
        assert_eq!(
            pinned_spki(host),
            Some(spki),
            "bound SPKI pinned for bearer"
        );
        assert_eq!(
            pinned_identity(host),
            None,
            "DNS-root nests are not TOFU identity-pinned"
        );
    }

    #[test]
    fn dns_self_root_mismatch_is_rejected() {
        // The DNS `self=` resolved a *different* identity than the binding proves
        // → rejected before TOFU is ever reached (verify_cert_binding enforces it).
        let nest = SigningKey::from_bytes(&[8u8; 32]);
        let spki = [0x66u8; 32];
        let nonce = [0x88u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let other = SigningKey::from_bytes(&[9u8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            graduate_handshake_with_root(
                "wrong-identity.example",
                &cap,
                &nonce,
                Some(&binding),
                Some(other)
            ),
            Err(TrustError::Binding(BindingError::IdentityMismatch))
        );
    }

    #[tokio::test]
    async fn resolve_dns_self_root_skips_local_hosts() {
        // Loopback / IP / `.local` have no DNS authority → None (TOFU), with no
        // network lookup attempted (classification short-circuits).
        assert_eq!(resolve_dns_self_root("pi.local:8443").await, None);
        assert_eq!(resolve_dns_self_root("192.168.1.57").await, None);
        assert_eq!(resolve_dns_self_root("localhost:3000").await, None);
    }

    // ── Deployment-seed rotation: the native graduation bridge ──
    // (`box-recovery.md` § Client acceptance; the shared verdict logic is
    // tested in `fauna_client_core::nest_trust::rotation_tests` — these pin
    // the *wiring*: the graduation retries and the trust state moves.)

    use fauna_protocol::nest_rotation::{NestRotation, SignedNestRotation};

    fn rot_key(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }
    fn rot_id(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }
    fn rot_hop(old: &SigningKey, new: &SigningKey, seq: u64) -> SignedNestRotation {
        NestRotation {
            old_nest_actor_id: rot_id(old),
            new_nest_actor_id: rot_id(new),
            seq,
            rotated_at: 1_800_000_000 + seq as i64,
        }
        .sign(old, new)
        .unwrap()
    }

    /// Serializes every test that INSTALLS a process-global pin store
    /// (`install_pin_store` replaces the store under any concurrently running
    /// test, so their reads/writes land in each other's stores otherwise).
    ///
    /// The async tests below hold this guard across `.await`, which
    /// `clippy::await_holding_lock` flags in general and which is correct here:
    /// serializing the whole test body IS the guard's purpose, each
    /// `#[tokio::test]` gets its own current-thread runtime with a single task
    /// (so there is nothing on that runtime to deadlock against), and the
    /// cross-test blocking the lint warns about is precisely the exclusion
    /// being asked for. A `tokio::sync::Mutex` would not work: the sync tests
    /// below take the same guard, and `blocking_lock()` panics inside a
    /// runtime. Each such test therefore carries a scoped `#[allow]`.
    fn global_store_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// Serves a canned rotation chain for `fauna.auth.rotation_chain`.
    struct ChainRequester(Vec<SignedNestRotation>);

    #[derive(Debug)]
    struct NoServe;
    impl std::fmt::Display for NoServe {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("mock")
        }
    }

    impl fauna_protocol::RpcRequester for ChainRequester {
        type Error = NoServe;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, NoServe>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, fauna_protocol::nest_rotation::ROTATION_CHAIN_KIND);
            let reply = fauna_protocol::nest_rotation::RotationChainReply {
                chain: self.0.clone(),
                extra: Default::default(),
            };
            Ok(serde_json::from_value(serde_json::to_value(reply).unwrap()).unwrap())
        }
    }

    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_rotated_nest_graduates_silently_and_moves_the_whole_trust_state() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Pin the OLD identity, present a binding by the NEW key, serve the
        // bridging chain: graduation succeeds, the identity pin names the head
        // (with its seq), and the bound SPKI is pinned for the bearer leg.
        let (old, new) = (rot_key(31), rot_key(32));
        let host = "rotated-box.local";
        install_pin_store(Arc::new(MemoryPinStore::new()));
        state().pins.read().unwrap().set(host, rot_id(&old));

        let spki = [0x51u8; 32];
        let nonce = [0x52u8; 32];
        let binding = make_binding(&new, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let client = ChainRequester(vec![rot_hop(&old, &new, 1)]);

        graduate_handshake(&client, host, &cap, &nonce, Some(&binding))
            .await
            .expect("a committed rotation graduates with no warning");
        assert_eq!(pinned_identity(host), Some(rot_id(&new)), "pin = head");
        assert_eq!(
            state().pins.read().unwrap().rotation_seq(host),
            Some(1),
            "accepted seq stored beside the pin"
        );
        assert_eq!(pinned_spki(host), Some(spki), "bearer SPKI pinned");
    }

    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn fork_evidence_surfaces_as_pin_forked_and_no_bridge_keeps_pin_changed() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (old, new, x, y) = (rot_key(31), rot_key(32), rot_key(33), rot_key(34));
        install_pin_store(Arc::new(MemoryPinStore::new()));

        let spki = [0x61u8; 32];
        let nonce = [0x62u8; 32];
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };

        // Fork: the pin was chain-accepted at seq 1; the served history names
        // a different head there. No re-trustable warning — PinForked.
        let host = "forked-box.local";
        state()
            .pins
            .read()
            .unwrap()
            .set_rotation_accepted(host, rot_id(&old), 1);
        let binding = make_binding(&y, &spki, &nonce);
        let client = ChainRequester(vec![rot_hop(&x, &y, 1)]);
        let err = graduate_handshake(&client, host, &cap, &nonce, Some(&binding))
            .await
            .expect_err("fork evidence must not graduate");
        assert!(
            matches!(
                err,
                TrustError::Identity(IdentityError::PinForked { pinned, seen })
                    if pinned == rot_id(&old) && seen == rot_id(&y)
            ),
            "got {err:?}"
        );
        assert_eq!(pinned_identity(host), Some(rot_id(&old)), "pin untouched");

        // No bridge: empty chain keeps today's PinChanged (explicit re-trust).
        let host2 = "plain-changed-box.local";
        state().pins.read().unwrap().set(host2, rot_id(&old));
        let binding = make_binding(&new, &spki, &nonce);
        let client = ChainRequester(vec![]);
        let err = graduate_handshake(&client, host2, &cap, &nonce, Some(&binding))
            .await
            .expect_err("no chain keeps the warning");
        assert!(
            matches!(err, TrustError::Identity(IdentityError::PinChanged { .. })),
            "got {err:?}"
        );
        assert_eq!(pinned_identity(host2), Some(rot_id(&old)));
    }

    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn the_dns_row_accepts_the_chain_for_the_propagation_window() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `box-recovery.md` § DNS row: a resolver still holding the stale
        // `self=` root accepts the chain stale-root → live head; no TOFU pin
        // is written (pre-resolved roots never pin identities).
        let (old, new) = (rot_key(31), rot_key(32));
        let host = "stale-self.example";
        install_pin_store(Arc::new(MemoryPinStore::new()));

        let spki = [0x71u8; 32];
        let nonce = [0x72u8; 32];
        let binding = make_binding(&new, &spki, &nonce);
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };
        let client = ChainRequester(vec![rot_hop(&old, &new, 1)]);

        // Drive the private bridge directly with the stale pre-resolved root —
        // `graduate_handshake` would try a real DNS lookup for a public name.
        let first =
            graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), Some(rot_id(&old)));
        let err = first.expect_err("stale root mismatches the live head");
        try_rotation_bridge(
            &client,
            host,
            &cap,
            &nonce,
            Some(&binding),
            Some(rot_id(&old)),
            err,
        )
        .await
        .expect("the chain bridges the propagation window");
        assert_eq!(pinned_spki(host), Some(spki), "bearer SPKI pinned");
        assert_eq!(
            pinned_identity(host),
            None,
            "DNS-root nests stay un-TOFU-pinned even through a rotation"
        );

        // Without a bridging chain the stale root keeps today's hard mismatch.
        let host2 = "stale-self-no-chain.example";
        let first =
            graduate_handshake_with_root(host2, &cap, &nonce, Some(&binding), Some(rot_id(&old)));
        let err = first.expect_err("mismatch");
        let err = try_rotation_bridge(
            &ChainRequester(vec![]),
            host2,
            &cap,
            &nonce,
            Some(&binding),
            Some(rot_id(&old)),
            err,
        )
        .await
        .expect_err("no chain, no acceptance");
        assert!(matches!(
            err,
            TrustError::Binding(BindingError::IdentityMismatch)
        ));
    }

    // ── classify_identity_changed — the one table for "is this the known_hosts
    //    verdict?", shared by the bearer mint and the launch machine's two
    //    detection channels ──────────────────────────────────────────────────

    /// A changed pin is the verdict, and it names both fingerprints.
    #[test]
    fn a_changed_pin_is_the_verdict() {
        let v = classify_identity_changed(
            &TrustError::Identity(IdentityError::PinChanged {
                pinned: [1u8; 32],
                seen: [2u8; 32],
            }),
            "any.host",
        )
        .expect("a changed pin is the identity-changed surface");
        assert_eq!(v.pinned_hex, "01".repeat(32));
        assert_eq!(v.seen_hex.as_deref(), Some("02".repeat(32).as_str()));
        assert!(!v.fork, "a plain change is not fork evidence");
    }

    /// Fork evidence takes the same surface but flags itself, because the launch
    /// machine REFUSES re-trust on it (`box-recovery.md` § Client acceptance).
    /// Losing the flag would silently restore a re-trust button that must not
    /// exist.
    #[test]
    fn fork_evidence_is_the_verdict_and_keeps_its_flag() {
        let v = classify_identity_changed(
            &TrustError::Identity(IdentityError::PinForked {
                pinned: [3u8; 32],
                seen: [4u8; 32],
            }),
            "any.host",
        )
        .expect("fork evidence is the identity-changed surface");
        assert!(v.fork, "fork evidence must stay distinguishable");
    }

    /// The negative half, and it carries as much weight as the positive one.
    /// `PinRequired` is a consumer process reaching a nest before the
    /// interactive app minted the pin; `RootMismatch` is a pre-resolved root
    /// disagreeing. Both are real trust failures, neither is the `known_hosts`
    /// verdict, and blocking a session on a re-trust surface for either would be
    /// a lie the user cannot act on.
    #[test]
    fn ordinary_trust_failures_are_not_the_verdict() {
        assert!(
            classify_identity_changed(
                &TrustError::Identity(IdentityError::PinRequired),
                "any.host"
            )
            .is_none()
        );
        assert!(
            classify_identity_changed(
                &TrustError::Identity(IdentityError::RootMismatch),
                "any.host"
            )
            .is_none()
        );
        assert!(classify_identity_changed(&TrustError::NoCapturedSpki, "any.host").is_none());
    }

    /// The one arm that reads *state* rather than the error value, and it turns
    /// entirely on whether a pin exists. No pin: first-contact trouble…
    #[test]
    fn a_missing_binding_with_no_pin_is_not_the_verdict() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        assert!(
            classify_identity_changed(&TrustError::BindingRequired, "never-pinned.local").is_none()
        );
    }

    /// …and with a pin it is the withdrawn/downgrade case: a nest that
    /// previously proved an identity now proves none. The verdict, with
    /// `seen_hex: None` — an attacker who simply *omits* the binding must not be
    /// able to demote the loud warning to a retry spinner.
    #[test]
    fn a_missing_binding_on_a_pinned_host_is_the_withdrawn_verdict() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let host = "withdrawn-box.local";
        install_pin_store(Arc::new(MemoryPinStore::new()));
        state().pins.read().unwrap().set(host, [9u8; 32]);

        let v = classify_identity_changed(&TrustError::BindingRequired, host)
            .expect("a pinned host that stops proving its identity is the verdict");
        assert_eq!(v.pinned_hex, "09".repeat(32));
        assert_eq!(
            v.seen_hex, None,
            "nothing was proven, so there is no `seen` fingerprint to show"
        );
        assert!(!v.fork);

        // The `Binding(_)` arm is the same case reached a different way: a
        // binding that WAS served but did not verify.
        assert!(
            classify_identity_changed(&TrustError::Binding(BindingError::IdentityMismatch), host)
                .is_some()
        );
    }

    /// The login's pin (security.md § Transport trust → *The login's pin*,
    /// ruled 2026-10-05): a public-CA nest that a login never CLAIMED pinned
    /// nothing — the WebPKI waiver returned before the binding was read — so
    /// `trusted_escrow_holders` was empty and every GenerationTip write was
    /// refused for good on exactly the deployments the project ships. A
    /// binding in hand is never waived: the core verifies it, TOFU-pins the
    /// identity it proves, and an impostor behind its own valid cert cannot
    /// nominate itself as the escrow holder.
    #[test]
    fn a_webpki_login_with_a_binding_in_hand_pins_the_identity_it_proved() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        let nest = SigningKey::from_bytes(&[21u8; 32]);
        let impostor = SigningKey::from_bytes(&[22u8; 32]);
        let id = nest.verifying_key().to_bytes();
        let nest_url = "https://public-ca-never-claimed.example";
        let authority = authority_of(nest_url);
        let host = authority.as_str();
        let spki = [0xd1u8; 32];
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: true,
        };
        assert!(
            trusted_escrow_holders(nest_url).is_empty(),
            "nothing pinned before the first login"
        );

        // No binding offered, no root, no pin: the waiver stands and pins nothing.
        assert_eq!(
            graduate_handshake_with_root(host, &cap, &[0u8; 32], None, None),
            Ok(Graduated::Waived)
        );
        assert_eq!(pinned_identity(host), None);

        // The login path always has a binding in hand (`read_login_binding`
        // demanded one before signing): it is verified and the identity pinned.
        let nonce = [0xd2u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        assert_eq!(
            graduate_handshake_with_root(host, &cap, &nonce, Some(&binding), None),
            Ok(Graduated::Verified(id)),
            "a WebPKI-valid login with the binding in hand verifies it"
        );
        assert_eq!(pinned_identity(host), Some(id), "…and pins the identity");
        assert_eq!(pinned_spki(host), Some(spki), "…and the bound SPKI");
        assert_eq!(
            trusted_escrow_holders(nest_url),
            vec![id],
            "the pinned identity is the escrow holder this machine trusts"
        );

        // An impostor behind its own publicly-trusted cert for the same name
        // cannot replace the holder: the pin refuses it.
        let stranger_spki = [0xd3u8; 32];
        let stranger = CapturedCert {
            spki: Some(stranger_spki),
            webpki_valid: true,
        };
        let binding = make_binding(&impostor, &stranger_spki, &nonce);
        assert!(
            matches!(
                graduate_handshake_with_root(host, &stranger, &nonce, Some(&binding), None),
                Err(TrustError::Identity(IdentityError::PinChanged { .. }))
            ),
            "an impostor cannot nominate itself as the escrow holder"
        );
        assert_eq!(trusted_escrow_holders(nest_url), vec![id]);
    }

    /// The pre-resolved rows (DNS `self=`, the injected or pasted root) never
    /// TOFU-pin in the core; the LOGIN records the root it verified as the
    /// pin, authoritatively — the root outranks any TOFU pin, as the claim
    /// seed does — and a record of the identity the pin already names is a
    /// no-op, so an accepted rotation's seq survives. A waived graduation
    /// records nothing.
    #[test]
    fn a_login_records_the_pre_resolved_root_it_verified_as_the_pin() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        let nest = SigningKey::from_bytes(&[23u8; 32]);
        let root = nest.verifying_key().to_bytes();
        let host = "dns-rooted.example";
        let spki = [0xd4u8; 32];
        let nonce = [0xd5u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        let floor = CapturedCert {
            spki: Some(spki),
            webpki_valid: false,
        };

        let graduated =
            graduate_handshake_with_root(host, &floor, &nonce, Some(&binding), Some(root))
                .expect("the floor graduates against the DNS root");
        assert_eq!(graduated, Graduated::Verified(root));
        assert_eq!(
            pinned_identity(host),
            None,
            "the core itself never TOFU-pins a pre-resolved root"
        );

        record_login_identity(host, Graduated::Waived);
        assert_eq!(
            pinned_identity(host),
            None,
            "a waived login records nothing"
        );

        record_login_identity(host, graduated);
        assert_eq!(
            pinned_identity(host),
            Some(root),
            "the login records the verified root as the pin"
        );
        assert_eq!(
            trusted_escrow_holders(&format!("https://{host}")),
            vec![root]
        );

        // Re-recording the identity the pin already names keeps the entry
        // (and with it an accepted rotation seq) untouched.
        let pins = state().pins.read().unwrap().clone();
        pins.set_rotation_accepted(host, root, 3);
        record_login_identity(host, Graduated::Verified(root));
        assert_eq!(
            pins.rotation_seq(host),
            Some(3),
            "a same-identity record is a no-op"
        );

        // A different root the zone now names (the public-domain row: the
        // client accepts whatever head the zone names) replaces the pin.
        let successor = [0xd6u8; 32];
        record_login_identity(host, Graduated::Verified(successor));
        assert_eq!(pinned_identity(host), Some(successor));
    }

    /// The two login wrappers end-to-end: a public-CA nest at an IP authority
    /// (no DNS to ask, so the wrappers resolve nothing) that no claim ever
    /// pinned — the bearer mint and the silent-challenge launch both leave the
    /// pin store naming the nest, which is what the account runtime's escrow
    /// trust reads.
    // Held across .await deliberately — see `global_store_lock`'s doc comment.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn both_login_wrappers_pin_a_never_claimed_public_ca_nest() {
        let _guard = global_store_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        install_pin_store(Arc::new(MemoryPinStore::new()));
        let nest = SigningKey::from_bytes(&[24u8; 32]);
        let id = nest.verifying_key().to_bytes();
        let nest_url = "https://203.0.113.9";
        let authority = authority_of(nest_url);
        let host = authority.as_str();
        let spki = [0xd7u8; 32];
        let cap = CapturedCert {
            spki: Some(spki),
            webpki_valid: true,
        };
        let no_chain = ChainRequester(vec![]);

        let nonce = [0xd8u8; 32];
        let binding = make_binding(&nest, &spki, &nonce);
        graduate_handshake(&no_chain, host, &cap, &nonce, Some(&binding))
            .await
            .expect("the bearer mint graduates a never-claimed public-CA nest");
        assert_eq!(pinned_identity(host), Some(id), "the handshake path pins");
        assert_eq!(trusted_escrow_holders(nest_url), vec![id]);

        forget_identity_pin(host);
        assert_eq!(pinned_identity(host), None);
        let challenge = [0xd9u8; 32];
        let client_nonce = [0xdau8; 32];
        let mut combined = Vec::with_capacity(64);
        combined.extend_from_slice(&challenge);
        combined.extend_from_slice(&client_nonce);
        let binding = make_binding(&nest, &spki, &combined);
        graduate_verify_path(
            &no_chain,
            host,
            &cap,
            &challenge,
            &client_nonce,
            Some(&binding),
        )
        .await
        .expect("the launch path graduates a never-claimed public-CA nest");
        assert_eq!(pinned_identity(host), Some(id), "the verify path pins");
        assert_eq!(trusted_escrow_holders(nest_url), vec![id]);
    }
}
