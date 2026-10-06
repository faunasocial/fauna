//! [`SecretBytes`] — the standard newtype for any secret-bearing byte field.
//!
//! Wraps `Zeroizing<Vec<u8>>` so the plaintext is wiped on drop, and serializes
//! **byte-for-byte identically to a plain `Vec<u8>`** (a `#[serde(transparent)]`
//! equivalent, hand-written because `Zeroizing` does not implement `serde`) —
//! an array of integers, which the canonical encoder's debug guard refuses: a
//! raw-byte field on the wire or at rest is a byte string
//! (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
//! "Variable-length byte fields"). So a secret field in a struct that reaches
//! the canonical encoder is a [`SecretByteBuf`] (as
//! [`crate::data::MailCredential::secret`] is); `SecretBytes` serves the
//! decode-only and UniFFI paths (a dispatched action's secret, read from JS or
//! handed across FFI), keeping the Rust-side zeroization discipline uniform
//! there too. The matching UniFFI custom-type registration (so the
//! foreign clients can pass these into a `uniffi::Enum`/`Record`) lives **here**,
//! at the bottom of this module behind `feature = "uniffi"`, in the **blanket**
//! (non-`remote`) form — so a single registration serves every consuming crate's
//! `UniFfiTag` (a `remote` reg in one consumer is tag-local and can't be shared
//! across crates that don't depend on each other). Consuming crates forward
//! `fauna-core/uniffi` from their own `uniffi` feature.
//!
//! What this does NOT close: the UniFFI marshalling copy + the foreign-UI buffer
//! are plaintext and un-zeroizable from Rust (value-passing FFI + GC'd managed
//! UIs). Closing that needs a handle-passing redesign — out of scope here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Constant-time byte-slice equality — every candidate byte is compared
/// regardless of where the first mismatch falls, closing the timing side
/// channel a short-circuiting `==` would open when comparing a caller-known
/// value against a secret (a MAC, an unsubscribe token, a shared secret).
///
/// Differing lengths return `false` immediately without a byte-by-byte scan,
/// so **callers MUST ensure the length of both sides is not itself secret.**
/// This is a precondition on you, not a property of this function: it holds
/// today because every call site compares fixed-width values on the legitimate
/// path (an HMAC digest, a base64url token, a hex-encoded shared secret), and
/// a future caller comparing a variable-length secret would leak its length
/// here. Stated as a contract rather than as a census of current callers —
/// a census reads as reassurance about the *function* to the next caller.
///
/// Backed by `subtle::ConstantTimeEq`, whose entire job is carrying an
/// optimization barrier across toolchains, profiles and targets. The previous
/// hand-rolled XOR-accumulate loop was *verified* data-independent by reading
/// its codegen at one pinned nightly, on aarch64 `-O`, aarch64 `opt-level=s`
/// (`[profile.dist]`, which ships the Windows binaries) and wasm32 `-O` — but
/// nothing in this tree pins that. The toolchain-pin ritual
/// (`build-system.md` § Rust toolchain pin) would not notice a future nightly
/// lowering the loop to a short-circuiting `bcmp`, and no behavioural test can
/// see the difference: equal, unequal and differing-length cases all stay
/// green through the entire defect class. `subtle` moves the property from
/// "held when someone last looked" to "carried by the dependency".
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Kept explicit even though `subtle`'s slice impl short-circuits on length
    // too, so the precondition documented above stays visible in this function
    // rather than resting on a dependency's internals.
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Mint 32 fresh secret bytes from the OS CSPRNG — the one minter every
/// client-side `fresh_*` key is cut from.
///
/// **Hands the bytes out non-`Copy`**, which is the whole point of the
/// signature (`key-material-hierarchy.md` § Plaintext key lifetime on bridges →
/// *Carrier shape*, first bullet). A bare `[u8; 32]` is `Copy`, so minting into
/// a `Zeroizing` local and dereferencing it away on return zeroizes the local
/// and hands the caller an unzeroized duplicate — the wrapper reads as custody
/// while delivering none. That is not hypothetical: all three of this
/// function's callers did exactly that until the 2026-08-12 carrier survey
/// found it (`owner-key-material.md` § Key-carrier custody), and it took two
/// review findings to catch because it was three copies, not one.
///
/// The domain minters — `fauna_client_mail_settings::fresh_msek`,
/// `fauna_client_folders::fresh_content_key`,
/// `fauna_client_subscriptions::fresh_period_key` — keep their own names, their
/// domain docs, and their own compile-time non-`Copy` pins, and delegate here.
/// So the *shape* rule has one owner: a fourth minter inherits it instead of
/// re-deriving it, and a regression is one edit to make rather than three to
/// keep in step.
#[must_use]
pub fn fresh_secret_32() -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), out.as_mut());
    out
}

/// Compile-time pin that [`fresh_secret_32`] keeps handing its output out
/// non-`Copy`. Reverting the return type to `[u8; 32]` fails the *build* here
/// rather than only wherever a call site happens to be strictly typed
/// (`key-material-hierarchy.md` § Carrier shape → *Pinned at compile time*).
const _FRESH_SECRET_32_IS_NOT_COPY: fn() -> Zeroizing<[u8; 32]> = fresh_secret_32;

/// Secret bytes, zeroized on drop. Serializes exactly like `Vec<u8>`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    /// Wrap owned bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Borrow the bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Copy the bytes into a fresh (non-zeroizing) `Vec`. Used when crossing a
    /// boundary that takes ownership (e.g. the UniFFI `lower` to `Vec<u8>`).
    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

impl From<Zeroizing<Vec<u8>>> for SecretBytes {
    fn from(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self(bytes)
    }
}

impl From<SecretBytes> for Vec<u8> {
    /// Copies the plaintext out into a non-zeroizing `Vec` (the value-passing FFI
    /// boundary is un-zeroizable by design — see the module docs).
    fn from(s: SecretBytes) -> Self {
        s.to_vec()
    }
}

impl std::ops::Deref for SecretBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for SecretBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Redacted — never print secret bytes (length only, for diagnostics).
impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes(<redacted {} bytes>)", self.0.len())
    }
}

impl Serialize for SecretBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Delegate to the inner `Vec<u8>` so the wire is byte-identical to a
        // plain `Vec<u8>` field (`#[serde(transparent)]` equivalent).
        self.0.as_slice().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(Zeroizing::new(Vec::<u8>::deserialize(deserializer)?)))
    }
}

/// Secret bytes, zeroized on drop. Serializes as a **byte string** (`serde_bytes`
/// / CBOR major type 2), *not* like a plain `Vec<u8>`.
///
/// The byte-**string** sibling of [`SecretBytes`] (as [`SecretArray32`] is the
/// fixed-array sibling). Use it for a secret-bearing byte field that must cross
/// the wire as a CBOR byte string — one a Go peer reads as `[]byte`, or one that
/// mirrors a neighbouring `serde_bytes` / `ByteBuf` field. Whereas [`SecretBytes`]
/// delegates to `<[u8]>::serialize` and so encodes as a CBOR **array of
/// integers** (major type 4 — identical to a plain serde `Vec<u8>`), this
/// delegates to `serialize_bytes` and encodes as a byte string, staying
/// wire-compatible with a `#[serde(with = "serde_bytes")]` / `ByteBuf` field it
/// replaces (and ~half the size for large keys). Its first consumer is the
/// one-shot AEAD key in `fauna_protocol::bridge_routing::StagedBodyRef`, which
/// sits beside a `Vec<ByteBuf>` chunk-hash list and is read as `[]byte` by the
/// Go mail bridge. Redacted `Debug`, zeroize on drop, same value-passing-FFI
/// caveat as the module docs. **No UniFFI registration** (its consumers are
/// plain-serde wire structs, not `uniffi::Record`s); add the blanket reg here if
/// a foreign-FFI consumer ever needs it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretByteBuf(Zeroizing<Vec<u8>>);

impl SecretByteBuf {
    /// Wrap owned bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Borrow the bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Copy the bytes into a fresh (non-zeroizing) `Vec` (a value-passing / FFI
    /// boundary — un-zeroizable there by design, see the module docs).
    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<u8>> for SecretByteBuf {
    fn from(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

impl From<SecretBytes> for SecretByteBuf {
    /// Re-home the same zeroizing buffer — no plaintext copy — when a value
    /// held as the integer-array sibling moves into a byte-string field (the
    /// mail-settings machine's credential input into a `MailCredential`).
    fn from(s: SecretBytes) -> Self {
        Self(s.0)
    }
}

impl From<SecretByteBuf> for Vec<u8> {
    /// Copies the plaintext out (the value-passing boundary is un-zeroizable).
    fn from(s: SecretByteBuf) -> Self {
        s.to_vec()
    }
}

impl std::ops::Deref for SecretByteBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for SecretByteBuf {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Redacted — never print secret bytes (length only, for diagnostics).
impl std::fmt::Debug for SecretByteBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretByteBuf(<redacted {} bytes>)", self.0.len())
    }
}

impl Serialize for SecretByteBuf {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A CBOR byte string (major type 2), NOT the seq-of-u8 that
        // `<[u8]>::serialize` would emit — so the wire matches a `ByteBuf`.
        serializer.serialize_bytes(self.0.as_slice())
    }
}

impl<'de> Deserialize<'de> for SecretByteBuf {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `ByteBuf`'s visitor accepts a byte string *or* a seq, so this decodes
        // tolerantly while our `Serialize` always emits the byte string.
        let bb = serde_bytes::ByteBuf::deserialize(deserializer)?;
        Ok(Self(Zeroizing::new(bb.into_vec())))
    }
}

/// Secret text, zeroized on drop. Serializes exactly like `String`.
///
/// The text sibling of [`SecretBytes`] — for secret-bearing fields whose value
/// is genuinely a string rather than raw bytes (e.g. a DNS-provider API token,
/// [`crate::data::DnsProviderCredential`]'s field-bag values). Serializes
/// **byte-for-byte identically to a plain `String`** (a CBOR text string, major
/// type 3 — distinct from `SecretBytes`' byte string), so swapping a `String`
/// field for `SecretString` leaves the at-rest dag-cbor wire unchanged and is
/// not a wire-breaking change. Use this for secret text, `SecretBytes` for
/// secret bytes; the same value-passing-FFI caveat in this module's docs applies.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    /// Wrap an owned string.
    pub fn new(s: String) -> Self {
        Self(Zeroizing::new(s))
    }

    /// Borrow the text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self(Zeroizing::new(s))
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        Self(Zeroizing::new(s.to_owned()))
    }
}

impl From<Zeroizing<String>> for SecretString {
    fn from(s: Zeroizing<String>) -> Self {
        Self(s)
    }
}

impl From<SecretString> for String {
    /// Copies the plaintext out into a non-zeroizing `String` (the value-passing
    /// FFI / external-API boundary is un-zeroizable by design — see module docs).
    fn from(s: SecretString) -> Self {
        s.0.as_str().to_owned()
    }
}

impl std::ops::Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SecretString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Redacted — never print secret text (length only, for diagnostics).
impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretString(<redacted {} bytes>)", self.0.len())
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Delegate to the inner `str` so the wire is byte-identical to a plain
        // `String` field (`#[serde(transparent)]` equivalent).
        self.0.as_str().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(Zeroizing::new(String::deserialize(deserializer)?)))
    }
}

/// Secret 32-byte array, zeroized on drop. Serializes as a 32-byte byte string.
///
/// The fixed-array sibling of [`SecretBytes`] — for secret-bearing fields whose
/// value is a 32-byte key/seed held as a `[u8; 32]` rather than a length-prefixed
/// `Vec<u8>` (e.g.
/// [`crate::data::DeploymentSeedEntry::seed`], the off-box-custodied nest
/// deployment seed — the irreplaceable identity). It serializes exactly like an
/// attributed `[u8; 32]` field (`#[serde(with = "serde_bytes")]`): a CBOR **byte
/// string** of exactly 32 bytes, as every serialized fixed-width byte field is
/// (`docs/goal/architecture/serialization.md` § "Fixed-size byte arrays"); the
/// distinct type exists for the zeroize-on-drop and the fixed width, which
/// `SecretBytes` does not enforce. Deliberately **not `Copy`** (so each holding drops —
/// and zeroizes — rather than leaving silent bitwise copies); call [`Self::to_array`]
/// / deref to copy the plaintext out at a crypto/FFI boundary (un-zeroizable there
/// by design — see module docs). No UniFFI registration: the consumers
/// ([`crate::data::DeploymentSeedEntry`]) are not UniFFI records (a bare `[u8; 32]` is not
/// UniFFI-representable), so the seed reaches the apps as a hex `String` getter,
/// never as a `SecretArray32` across FFI.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretArray32(Zeroizing<[u8; 32]>);

impl SecretArray32 {
    /// Wrap an owned 32-byte array.
    pub fn new(arr: [u8; 32]) -> Self {
        Self(Zeroizing::new(arr))
    }

    /// Copy the bytes into a fresh (non-zeroizing) `[u8; 32]`. Used when crossing a
    /// boundary that takes the raw seed (a crypto op, the hex-encode at the read
    /// edge of the restore path) — un-zeroizable there by design (module docs).
    pub fn to_array(&self) -> [u8; 32] {
        *self.0
    }
}

impl From<[u8; 32]> for SecretArray32 {
    fn from(arr: [u8; 32]) -> Self {
        Self(Zeroizing::new(arr))
    }
}

/// Move an already-`Zeroizing` derivation straight into custody, with **no bare
/// `[u8; 32]` hop** in between.
///
/// The `[u8; 32]` impl above is the right door for a caller that genuinely holds
/// a bare array; a caller holding `Zeroizing<[u8; 32]>` — what every derivation
/// governed by `key-material-hierarchy.md` § Plaintext key lifetime on bridges →
/// *Carrier shape* now returns — would otherwise have to deref through a `Copy`
/// temporary to reach it, reintroducing for one expression exactly the
/// duplication the return type exists to prevent. This impl takes the wrapper by
/// value, so the bytes move.
impl From<Zeroizing<[u8; 32]>> for SecretArray32 {
    fn from(arr: Zeroizing<[u8; 32]>) -> Self {
        Self(arr)
    }
}

impl From<SecretArray32> for [u8; 32] {
    /// Copies the plaintext out into a non-zeroizing array (see module docs).
    fn from(s: SecretArray32) -> Self {
        *s.0
    }
}

impl std::ops::Deref for SecretArray32 {
    type Target = [u8; 32];
    fn deref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8]> for SecretArray32 {
    fn as_ref(&self) -> &[u8] {
        &self.0[..]
    }
}

/// Compare against a bare array without copying the secret out — keeps
/// call-sites like `generation.key == expected` terse. Not constant-time
/// (neither is the derived `PartialEq`); none of the custody comparisons are
/// attacker-observable timing surfaces.
impl PartialEq<[u8; 32]> for SecretArray32 {
    fn eq(&self, other: &[u8; 32]) -> bool {
        *self.0 == *other
    }
}

impl PartialEq<SecretArray32> for [u8; 32] {
    fn eq(&self, other: &SecretArray32) -> bool {
        *self == *other.0
    }
}

/// Total order over the bytes, so a secret can be a `BTreeMap` key or a sort key
/// **without being copied out into a bare array first**.
///
/// Added for the mail merge, which associates each retired MSEK with its
/// retirement instant keyed by the MSEK value itself, never positionally
/// ([`crate::data::MailConfig::prior_msek_retirements`]).
/// Without an ordering the merge would have to key that map on `[u8; 32]`,
/// reintroducing exactly the bare `Copy` duplication the custody type exists to
/// prevent — for the whole life of the map.
///
/// Not constant-time, like the derived [`PartialEq`] above and for the same
/// reason: none of these custody comparisons are attacker-observable timing
/// surfaces (both sides are already-held local secrets, and the attacker who
/// could time them already holds the plaintext).
impl Ord for SecretArray32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self.0).cmp(&*other.0)
    }
}

impl PartialOrd for SecretArray32 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Redacted — never print the secret bytes (fixed length, for diagnostics).
impl std::fmt::Debug for SecretArray32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretArray32(<redacted 32 bytes>)")
    }
}

impl Serialize for SecretArray32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A byte string, byte-identical to an attributed `[u8; 32]` field.
        let arr: &[u8; 32] = &self.0;
        serde_bytes::serialize(arr, serializer)
    }
}

impl<'de> Deserialize<'de> for SecretArray32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let arr: [u8; 32] = serde_bytes::deserialize(deserializer)?;
        Ok(Self(Zeroizing::new(arr)))
    }
}

// UniFFI custom-type registrations for the secret newtypes. Because these types
// are defined *here*, the registration is the **blanket** (non-`remote`) form —
// `uniffi::custom_type!` emits `impl<UT> FfiConverter<UT>`, so the marshalling
// works for **every** consuming crate's `UniFfiTag` (`fauna-client-dns`,
// `fauna-client-mail-settings`, `fauna-onboarding-machine`, …). A per-consumer
// `remote` reg is tag-local (`impl FfiConverter<crate::UniFfiTag>`) and so can't
// be shared across crates that don't depend on each other — which is why these
// live in one place. `SecretBytes` marshals as `bytes` (`Vec<u8>`), `SecretString`
// as a `string`; the `lower` copy + the foreign-side buffer are un-zeroizable by
// design (the value-passing FFI boundary — see the module docs). Consuming crates
// forward `fauna-core/uniffi` from their own `uniffi` feature.
#[cfg(feature = "uniffi")]
uniffi::custom_type!(SecretBytes, Vec<u8>, {
    lower: |s| s.to_vec(),
    try_lift: |bytes| Ok(SecretBytes::from(bytes)),
});
#[cfg(feature = "uniffi")]
uniffi::custom_type!(SecretString, String, {
    lower: |s| s.as_str().to_owned(),
    try_lift: |s| Ok(SecretString::from(s)),
});

#[cfg(test)]
mod tests {
    use super::*;

    // ── fresh_secret_32 (the one CSPRNG minter behind every `fresh_*` key) ──

    /// The failure this catches is a minter that returns something other than
    /// live CSPRNG output — a fixed array, a zeroed buffer, a counter — which
    /// would silently make every key it mints predictable while every type,
    /// pin and call site still looked right. Cheap, and the only property a
    /// test *can* assert about a CSPRNG: the bytes move.
    #[test]
    fn fresh_secret_32_returns_live_random_bytes() {
        let a = fresh_secret_32();
        let b = fresh_secret_32();
        assert_ne!(*a, [0u8; 32], "minted an all-zero key");
        assert_ne!(*a, *b, "two mints returned identical bytes");
    }

    // ── constant_time_eq (shared secret/MAC/token comparison) ───────────────

    #[test]
    fn constant_time_eq_equal_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_differing_bytes() {
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn constant_time_eq_differing_lengths() {
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    /// The three tests above cannot witness the property that matters.
    ///
    /// Equal, unequal and differing-length behaviour all stay green through
    /// the entire defect class this function exists to prevent: a
    /// short-circuiting comparison returns exactly the same booleans as a
    /// data-independent one, and a test can only observe the boolean. Only
    /// codegen inspection or a dependency-carried barrier discriminates
    /// (which explicitly refuses a behavioural pin
    /// here). So the durable witness is a *mechanism* pin: this asserts the
    /// implementation still delegates to `subtle` and has not been "simplified"
    /// back to a hand-rolled accumulate loop whose data-independence would
    /// again rest on whatever the pinned nightly happens to emit.
    #[test]
    fn constant_time_eq_delegates_to_the_subtle_barrier() {
        let src = include_str!("secret.rs");
        let body = src
            .split_once("pub fn constant_time_eq")
            .expect("constant_time_eq went missing")
            .1;
        let body = &body[..body.find("\n}").expect("unterminated fn")];
        assert!(
            body.contains("ct_eq"),
            "constant_time_eq no longer delegates to subtle::ConstantTimeEq — a \
             hand-rolled comparison's data-independence is unpinned by anything \
             in this tree, and no behavioural test can catch it regressing"
        );
        assert!(
            !body.contains('^'),
            "constant_time_eq grew a hand-rolled XOR-accumulate loop again"
        );
    }

    #[test]
    fn debug_is_redacted() {
        let s = SecretBytes::from(b"hunter2".to_vec());
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("hunter2"), "Debug leaked the secret");
        assert!(rendered.contains("7 bytes"));
    }

    #[test]
    fn round_trips_identically_to_vec() {
        // The decode-only and FFI paths read a `SecretBytes` exactly as they
        // read a `Vec<u8>`: the serde shape is the same.
        let secret = SecretBytes::from(b"high-entropy".to_vec());
        let plain: Vec<u8> = b"high-entropy".to_vec();
        let a = serde_json::to_string(&secret).unwrap();
        let b = serde_json::to_string(&plain).unwrap();
        assert_eq!(a, b, "SecretBytes must serialize identically to Vec<u8>");

        let back: SecretBytes = serde_json::from_str(&a).unwrap();
        assert_eq!(back.as_slice(), b"high-entropy");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_canonical_encoder_refuses_secret_bytes() {
        // An integer array is no wire or at-rest shape: a secret field that
        // reaches the canonical encoder is a `SecretByteBuf`.
        let secret = SecretBytes::from(b"high-entropy".to_vec());
        let err = crate::encoding::canonical_encode(&secret).unwrap_err();
        assert!(
            err.to_string().contains("Variable-length byte fields"),
            "{err}"
        );
    }

    #[test]
    fn secret_string_debug_is_redacted() {
        let s = SecretString::from("hunter2");
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("hunter2"), "Debug leaked the secret");
        assert!(rendered.contains("7 bytes"));
    }

    #[test]
    fn secret_string_round_trips_identically_to_string() {
        // The text twin of `round_trips_identically_to_vec`: a `SecretString`
        // must encode byte-for-byte like a plain `String` (a CBOR text string),
        // so swapping a `String` field for `SecretString` leaves the at-rest
        // wire unchanged — not a wire-breaking "narrowing" (file-sync.md).
        use crate::encoding::{canonical_decode, canonical_encode};

        let secret = SecretString::from("dns-api-token-value");
        let plain: String = "dns-api-token-value".to_owned();
        let a = canonical_encode(&secret).unwrap();
        let b = canonical_encode(&plain).unwrap();
        assert_eq!(a, b, "SecretString must serialize identically to String");

        let back: SecretString = canonical_decode(&a).unwrap();
        assert_eq!(back.as_str(), "dns-api-token-value");
    }

    #[test]
    fn secret_array32_debug_is_redacted() {
        let s = SecretArray32::from([0xABu8; 32]);
        let rendered = format!("{s:?}");
        // The raw-array Debug would render the bytes (`[171, 171, …]`); the redacted
        // form must not.
        assert!(!rendered.contains("171"), "Debug leaked the secret bytes");
        assert!(rendered.contains("redacted"));
        assert!(rendered.contains("32 bytes"));
    }

    #[test]
    fn secret_array32_round_trips_identically_to_an_attributed_array() {
        // The fixed-array twin of `round_trips_identically_to_vec`: a `SecretArray32`
        // encodes byte-for-byte like an attributed `[u8; 32]` field — a 32-byte
        // CBOR byte string, as every serialized fixed-width byte field is
        // (serialization.md § "Fixed-size byte arrays") — so swapping such a
        // field for `SecretArray32` leaves the at-rest wire unchanged.
        use crate::encoding::{canonical_decode, canonical_encode};

        let seed = [0x5au8; 32];
        let secret = SecretArray32::from(seed);
        let a = canonical_encode(&secret).unwrap();
        let b = canonical_encode(&serde_bytes::ByteArray::new(seed)).unwrap();
        assert_eq!(
            a, b,
            "SecretArray32 must serialize as a 32-byte byte string"
        );
        assert_eq!(&a[..2], &[0x58, 0x20]);

        let back: SecretArray32 = canonical_decode(&a).unwrap();
        assert_eq!(back.to_array(), seed);
    }

    #[test]
    fn secret_byte_buf_debug_is_redacted() {
        let s = SecretByteBuf::from(b"hunter2".to_vec());
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("hunter2"), "Debug leaked the secret");
        assert!(rendered.contains("redacted"));
        assert!(rendered.contains("7 bytes"));
    }

    #[test]
    fn secret_byte_buf_round_trips_identically_to_a_byte_string() {
        // Unlike `SecretBytes` (which encodes as a CBOR *array* of integers,
        // identical to a plain serde `Vec<u8>`), `SecretByteBuf` must encode as a
        // CBOR *byte string* (major type 2) — byte-for-byte like a
        // `serde_bytes::ByteBuf` / `#[serde(with = "serde_bytes")]` field — so it
        // stays wire-compatible with a Go peer's `[]byte` and with a neighbouring
        // `ByteBuf` field it sits beside.
        use crate::encoding::{canonical_decode, canonical_encode};

        let secret = SecretByteBuf::from(b"one-shot-key-material".to_vec());
        let plain = serde_bytes::ByteBuf::from(b"one-shot-key-material".to_vec());
        let a = canonical_encode(&secret).unwrap();
        let b = canonical_encode(&plain).unwrap();
        assert_eq!(
            a, b,
            "SecretByteBuf must serialize identically to a ByteBuf"
        );
        // And decidedly NOT like a plain Vec<u8> (the array shape, major 4).
        assert_eq!(a[0] >> 5, 2, "SecretByteBuf must encode as a byte string");

        let back: SecretByteBuf = canonical_decode(&a).unwrap();
        assert_eq!(back.as_slice(), b"one-shot-key-material");
    }
}
