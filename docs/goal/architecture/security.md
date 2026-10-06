# Security — sign-over-CID, verification recipe, decode-strictness — target state

Owns: security, sign-over-cid, channel-binding, screen-capture-posture
Status: ratified
Authority: runtime crypto discipline (signed envelope, verification summary, decode-strictness, forbidden tags) + client-side transport trust (channel binding, identity/TOFU pinning, cross-connection SPKI pin) + co-resident process-trust postures. Byte-level encoding (canonical dag-cbor, CID shape, embed-as-bytes, conformance corpus, the verification recipe's byte-level steps) → `serialization.md`; the channel-binding handshake's *field layout* → `transport.md` (this doc owns *why* and *what is proven*); CARv2 segments / on-disk manifests → `data-flow.md`; key taxonomy + the sig-domain rule #8 → `key-material-hierarchy.md`; server-side cert policy → `nest/tls-certificates.md`; release / supply-chain / build-time trust → `release-integrity.md` (this doc is *runtime* crypto discipline; that doc is *who can ship code at all*); the post-quantum overlay → `security/post-quantum.md` (signatures are not HNDL-exposed so they sequence after the KEM work; the BLAKE3-256 CID is already quantum-adequate, so the verification recipe survives a signature-algorithm change unchanged); the third verification axis — *cryptographic proof the nest applied each operation correctly* (verifiable-DB commitments + tracer proofs), beyond this doc's two axes of *payload authorship* and *which nest you reached* — → `encrypted-spaces.md`.

> **Audience:** every Rust/nest/app contributor working on signed
> content; third-party implementers writing a Fauna-compatible stack
> in any language with BLAKE3 + Ed25519 + a dag-cbor decoder.
> **Purpose:** the canonical reference for Fauna's *cryptographic
> discipline* — what signed-envelope shape covers, how a third party
> verifies a signed payload, what stays in and out of the security
> path, and why decode-relaxed is forbidden anywhere a CID match or
> signature verification happens.

## Goal

Verifying a Fauna-signed payload is a hash check plus a signature
check — never a re-encode. Any third party with BLAKE3 + Ed25519 + a
dag-cbor decoder can verify content end-to-end without coordinating
CBOR libraries with anyone else. Encoder bugs become *publishing-side*
problems noticed immediately (the publisher's content doesn't dedupe
with the rest of the network), not *verification-side* failures that
silently break peers. This invariant — **encoder canonicality is out
of the verification path** — is the single load-bearing security
property of the Fauna stack.

## Signed-envelope shape

A `SignedEnvelope` is a `(cid, sig)` pair. The signature covers
`cid.as_bytes()` — the 36-byte CID — **NOT** the canonical content
bytes themselves.

```rust
struct SignedEnvelope {
    cid: Cid,        // 36 bytes: v1 + dag-cbor + blake3-256 + 32-byte digest
    sig: [u8; 64],   // ed25519 signature over cid.as_bytes()
}
```

The 36-byte CID layout (`0x01 0x71 0x1e 0x20` header + 32-byte BLAKE3
digest) and the user-facing base32-lower string form are specified by
`docs/goal/architecture/serialization.md` § CID shape. Wire and disk
payloads carry the raw canonical bytes alongside the envelope (the
embed-as-bytes pattern, `docs/goal/architecture/serialization.md`
§ Embed-as-bytes for signed payloads), so receivers verify by hash
plus signature without ever re-encoding the inner content.

Reference: `libs/fauna-cbor/src/envelope.rs::SignedEnvelope`.

## Third-party verification recipe

Any language with BLAKE3 + Ed25519 + a dag-cbor decoder verifies a
Fauna-signed payload in four steps: receive `(envelope, bytes)`;
require `blake3(bytes) == envelope.cid.multihash`; verify the Ed25519
signature over the raw 36 CID bytes (never a wrapped or re-encoded
form); decode `bytes` per the kind schema with any decoder — canonical
or relaxed both work for *post-verification* application decode, since
verification is already done. Verification never re-encodes: it is a
hash check plus a signature check over the bytes as received.

The byte-level authority for this recipe — the exact steps, the CID
layout they operate on, and the reference implementations (Python, Go,
Rust) — is `docs/goal/architecture/serialization.md` § Third-party
verification recipe.

### App display of unverified content

When a client **decodes a post (or other signed payload)** and the recipe's
hash or signature check **fails** — notably content fetched from a **non-home
nest** in a federated feed
— it must neither silently render it as authentic nor silently drop it.
**Decided 2026-06-24 (user): show an "unverified source" indicator.** The client
renders the content but marks it visibly unverified (a badge / muted styling, the
DKIM-fail analogue), so the user sees both the content and the caveat, and a
transient key-rotation-lag false-negative does not make a legitimate post vanish
with no signal.

**Where the indicator can fire — and where it deliberately cannot.** The signal
keys off *this client's own* envelope verification, so it exists only where the
client holds a **raw signed envelope** to verify. Two surfaces, two findings:

- **Decode paths.** Wherever the client fetches a raw
  signed body via `fauna.posts.get` — post-detail, the
  quoted-post fallback (`resolve_quoted_post`), and media-resolve
  (`resolve_media`) — the validity flag is real. It was being *computed and
  discarded* (`let (post, _valid) = …`); it is now threaded into the render model
  (below) and drives the badge. A body fetched **by id** verifies only if it is
  both validly signed **and** the post asked for — its wire id (hex `blake3` over
  the served bytes, the nest's own id derivation) equals the requested id
  (`fauna_client_core::post::decode_post_fetched_as`). A signature alone proves
  who wrote a body, not which post it is: a hostile nest answering `posts.get(X)`
  with another genuinely signed post `Y` renders `Y` `Failed` (badged, never
  dropped), with no authoring-origin claim. A **bridge-translated** post is the
  one body with no envelope: the nest stores it as a bare canonical `Post` (its id
  `blake3` of those bytes), so it decodes only when its bytes hash to the id asked
  for and stays **`Unchecked`** (no badge — nothing was signed for this client to
  check; its `protocol-badge` carries its trust class), never `Failed`
  (`fauna_client_core::post::decode_bare_post_fetched_as`, ruled 2026-10-02 when
  the render-model.md § D6c witness found every bridged body undecodable).
- **The feed-list projection (by design).** A
  feed-list card is mapped from a `fauna_protocol::feed::FeedPostItem` — the home
  nest's **trusted index projection**, which carries **no signed envelope** (the
  feed index never reads `content.payload`; `feed.md` § The read model). There is
  therefore nothing for the client to verify on the list path, and a card is
  **`Unchecked`** (no badge) until the post is actually decoded. This is the
  explicit cost of the home-nest trust model (§ Transport trust — the client
  trusts its connected nest's *content projections*); a **hostile/lured non-home
  nest** can serve a forged `author`/`body` in the list and it renders without a
  badge until opened. A nest-projected "verified" bool would be **worthless**
  here — the same hostile nest controls the projection — so the only real defense
  is the client decoding the raw envelope itself, which is exactly what the decode
  paths above do. `resolve_media` is the one list path that *does* fetch + decode
  the raw body, so a media post whose body fails verification surfaces the badge
  on its list card too.

**The shared signal.** Verification status is a single shared enum,
`fauna_core::render::VerificationStatus` — `{ Unchecked, Verified, Failed }`,
default `Unchecked` — **not** a bool (a bool conflates "never checked" with
"checked and authentic", the precise conflation this indicator exists to break).
The badge renders **iff** `Failed`. Every app reads it from one place
(`fauna_feed::PostSummary::verification` for a list card / focal post;
`fauna_feed::QuotedPostView::verification` for a quoted embed) instead of each
re-deriving validity (priorities #1/#2).

**Implementation status:** fully built on all seven apps — focal-post +
quoted-embed badges, list + detail, with the iff-`Failed` rule e2e-proven on
six of seven apps (web, linux, windows, macOS, iOS, tui —
`tests/e2e-unified/tests/test_feed_unverified_source.py`; android is
compile-verified only, runtime e2e gated on its emulator setup).
Rollout record: `## Implementation status today` § Unverified-source
indicator rollout.

## Encoder canonicality is OUT of the security path

What's **IN** the security path:

- BLAKE3-256 of the wire bytes.
- Ed25519 verification of `(envelope.sig, envelope.cid.bytes, pubkey)`.
- The 36-byte CID itself (fixed v1 + dag-cbor + blake3-256 + 32-byte
  digest shape; `docs/goal/architecture/serialization.md` § CID shape).

What's **NOT** in the security path:

- The dag-cbor encoder. Receivers never re-encode for verification.
- Re-encoding for hash matching. Bytes ride along the envelope per
  embed-as-bytes (`docs/goal/architecture/serialization.md`
  § Embed-as-bytes for signed payloads); the receiver hashes the
  bytes it received, not bytes it produced.

### Why this split (Camp A reasoning)

Consider a third-party publisher with a buggy CBOR encoder. The
encoder produces some bytes `B` that hash to `CID-A`; the publisher
signs `CID-A`. Receivers get `(envelope=CID-A+sig, bytes=B)`, compute
`blake3(B) == CID-A` (true), verify `ed25519(sig, CID-A.bytes, pk)`
(true) — content verifies correctly.

But the CID is "wrong" from a deduplication standpoint: a *canonical*
re-encoding of the same logical content produces different bytes `B'`
that hash to a different `CID-B`. The publisher's content is
inter-op-broken — nobody else's content with the same logical shape
shares its CID, so the publisher can't dedupe with the rest of the
network. The publisher's encoder bug surfaces immediately as a
publishing problem the publisher's admin notices ("our content
isn't merging with anyone else's").

**Encoder bugs become publishing-side problems noticed immediately
(content doesn't dedupe with everyone else's), not verification
breakage that propagates.** This is the load-bearing property the
Camp A model (sign-over-CID + embed-as-bytes) was designed to
deliver. Sign-over-bytes — the alternative — would put the encoder
back in the verification path, because every receiver would have to
canonically re-encode the received content to recompute the hash, and
encoder bugs would silently fail verification on inputs every other
receiver accepts. See `docs/goal/architecture/serialization.md`
§ Sign-over-CID for the wire-spec consequence.

## Decode-relaxed prohibition

`fauna_cbor::decode_relaxed` exists only for **debug and inspection
tools** (admin dumps, panic-handler payload extraction, ad-hoc CLI
introspection). It is **forbidden** anywhere a CID match or signature
verification is performed. The Rust crate marks it `#[doc(hidden)]`;
security-path code uses `decode_strict` exclusively.

### Why decode-strict is structural, not advisory (Rust)

`decode_strict` runs the canonical-form pre-parse validator
(`libs/fauna-cbor/src/canonical.rs`, landed with the crate itself in
the CBOR-DAG-everywhere Layer-1 squash) **before** handing
bytes to `serde_ipld_dagcbor::from_slice`. The validator walks the raw byte
stream once, no serde involvement, and returns a typed
`DecodeError::NotCanonical { reason }` for any canonical-form
violation.

This is a deliberate architectural choice: the upstream codec does not
catch every canonical-form axis on its own, and what it *does* catch
varies with the Rust type being decoded into, whereas the pre-parse
validator walks bytes before a target type exists. **Which axes fall to
which, at the pinned version, is owned by `serialization.md` § *How
decode-strictness is actually enforced (Rust)*** — a measured, test-backed
table (`libs/fauna-cbor/tests/upstream_decoder_baseline.rs`), together with
the standing decision on upstream's 0.7.0 strictness release. It is not
restated here: this section carried a copy until 2026-08-24, and the copy
inherited two claims the measurement refuted.

Any such axis slipping into a security-path decode would mean the
receiver hashes bytes whose canonical form the publisher's encoder
should have rejected — exactly the publishing-side problem-creator
posture that breaks the Camp A model from the publisher's side. That is
why the validator, and not the codec version, is the boundary.

### Implication for other-language implementations

Other-language implementations need either:

1. **Equivalent canonical-form checks themselves** — port
   `validate_canonical` to that language's byte-walking idiom.
2. **A trusted decoder that rejects non-canonical input on its
   own** — verified against the `ipld/codec-fixtures` corpus
   (`docs/goal/architecture/serialization.md` § Conformance corpus).

Without one of those, the implementation silently accepts
non-canonical bytes from peers, then re-publishes non-canonical bytes
that other receivers reject — breaking the publishing-side
problem-creator detection model from the previous section. The
inter-op damage is one-way and asymmetric; it reads like a
verification-side bug and is actually a missing strictness gate.

See `docs/goal/architecture/serialization.md` § How decode-strictness
is actually enforced (Rust) for the same gap analysis from the
encoding-spec angle.

## Forbidden tags

Only **tag 42** (IPLD CID link) is accepted by the decoder. Every
other tag is rejected as
`DecodeError::NotCanonical { reason: "tag other than 42" }`.

- **Producers** must never emit a non-42 tag in any byte stream that
  will be hashed, signed, stored, or forwarded.
- **Receivers** must never accept a non-42 tag in any byte stream
  participating in CID matching or signature verification.

The pre-parse validator enforces this on every `decode_strict` call;
language implementations without an equivalent validator must add the
check (see § Decode-relaxed prohibition).

## Key management invariants

The full key taxonomy — per-audience roots, derivation chains,
rotation policy, AEAD framing — lives in
`docs/goal/architecture/key-material-hierarchy.md`. This section
calls out only the security-path invariants that bear on
`SignedEnvelope` verification.

- **Ed25519 keypair shape.** 32-byte seed + 32-byte public key.
  `VerifyingKey::from_bytes(...)` validates structural shape (point
  on the curve, valid length) but not provenance — it does not tell
  the caller *who* the key belongs to or whether the holder is
  authorized to sign for the actor in question. Provenance is the
  caller's responsibility (look up the actor's published pubkey, walk
  the device-key registry, etc.) and outside the scope of
  `SignedEnvelope::verify_permissive` and its `fauna-core` wrappers.
- **A wire-supplied verifying key is verified strictly, and small-order
  keys are refused.** Wherever the public key arrives *from the wire*
  beside the signature — every "prove you hold the key you name"
  ceremony — the key is
  attacker-chosen, and the permissive `VerifyingKey::verify` is then
  **not a signature check at all**: with a small-order key a forged
  signature can satisfy the verification equation on a large fraction of
  messages (ruled 2026-08-17). Such ceremonies MUST use
  `verify_strict` and MUST refuse
  weak keys, so that a signature never admits an identity nobody holds a
  key to. Code: `fauna_core::identity::verify_detached`, the single
  primitive for this class (the relay carries an equivalent local copy).
  Honest signers are unaffected — a real keypair is never small-order and
  a real signature never carries a small-order `R`. ⚠ The two refusals
  overlap on the key axis (`verify_strict` already rejects a small-order
  `A`), so the property is pinned, not either half; do not drop
  `verify_strict` on the grounds that the weak-key check subsumes it.
  This is orthogonal to *provenance* (previous bullet) and to
  *domain separation* (rule #8, below): it says a signature is binding at
  all, not who signed or in which context.
  ⚠ **Conformance to this rule is claimed only by a walk, never by an
  enumeration** — `libs/fauna-core/tests/one_ed25519_verification_shape.rs`
  walks **the whole Rust tree** (every `libs/*/src/` and `bins/*/src/` source;
  89 of them name `ed25519_dalek` as of 2026-08-17), and
  `bins/fauna-nest/src/state.rs::nest_has_one_ed25519_verification_shape` is the
  nest's in-crate twin so a nest regression fails the nest's own suite. Each
  fails if a source names
  `ed25519_dalek`'s permissive `Verifier` trait without a
  `verify-ok(<class>)` marker in the preceding 8 lines. The marker IS the
  per-site ruling — recorded where the next reader looks, not in a review
  doc: `verify-ok(test)` for a module holding both halves of its own
  keypair, `caller-supplied key` where the expected key arrives as a
  parameter, `tofu-identity` where the key IS the identity being
  established. The relay carries the rule against its own local copy.
  The guards exist because the enumerated form of this rule was wrong three
  times: the sweep that
  introduced it listed the ceremonies by name (auth / register / invite /
  claim / lockout / device) and so missed `storage_mode_core.rs`,
  `nat_mode_core.rs` and `share_routes.rs`, which are the same shape under
  different names — then recorded "all ten sites" as settled fact here and
  in `verify_detached`'s doc; then a hand census of `libs/` listed "~a dozen"
  sites and was overtaken **within a day** by group-machinery landings that
  copied the shape into four more (`group_generation.rs`, `group_scope.rs`).
  **Any remedy whose value is its completeness
  ships with a mechanical re-runnable census, or the next reader inherits
  the gap as closed.** ⚠ **A walk certifies only what it can SEE — direct
  verification.** The nest guard read green while every nest sign-over-CID
  ceremony verified permissively one crate away, through
  `fauna_core::encoding::verify_envelope`: a ceremony that
  *delegates* its verification is outside a source-text population defined by
  naming `ed25519_dalek`. What closes that gap is not a wider grep but the
  shared door itself running the strict primitive — `verify_envelope` /
  `verify_authoring_envelope` (through the private `verify_envelope_under`)
  route straight to `crate::identity::verify_detached`, never through
  `fauna_cbor::SignedEnvelope::verify_permissive` at all, so that method has
  **zero production callers** — every remaining caller is a fixture or
  negative-path test holding its own key, a claim the same walk file pins (`the_permissive_envelope_verify_has_no_production_caller`) — which is why it carries
  `_permissive` in its name.
- **`VerifyError::SignatureInvalid` semantics.** Means *the
  signature does not match `(sig, cid_bytes, pk)`*. The cause is one
  of: tampering with `sig` or `cid_bytes` after signing; the wrong
  `pk` (e.g. looked up the actor's *previous* device key after
  rotation); content signed by a *different actor* than the one whose
  key was supplied. The verifier cannot disambiguate these — the
  caller decides which interpretation applies given the surrounding
  context (which actor was expected, which key registry was queried,
  whether rotation is in flight).
- **`VerifyError::CidMismatch` semantics.** Means
  `blake3(bytes) != envelope.cid.multihash` — the bytes were tampered
  after signing, or the envelope and bytes were swapped between
  signed pairs (an `(envelope_A, bytes_B)` mix-up). The signature
  itself may still validate against the CID; the receiver MUST
  reject the payload regardless.
- **Shared-signing-key domain separation.** One shared Ed25519 key
  signing in more than one protocol context ⇒ every signed message
  carries a distinct constant domain-separation tag, so a signature in
  one context can never be reinterpreted as valid in another. For the
  **deployment Ed25519 signing key** this binds six shipped contexts
  (cert-binding, outbox, federation, subscription + archival KeyBlob,
  the deployment-seed rotation statement). It equally binds the **actor
  (user) key**: the no-nonce login handshake and the
  claim-admin verifier built byte-identical bytes, so a login signature
  was a valid `claim_admin` signature until the tag — every actor-key
  context is tagged-only now (login, challenge-verify, registration,
  lockout, claim-admin, invite submit/cancel, the NAT-mode commit and,
  until it left the wire 2026-09-24, the storage-mode one), with no untagged accept path left anywhere (audit COMPLETE
  2026-08-17).
  The tag registry, per-context status, the actor-key audit, the KeyBlob
  exemption, and the pre-external-peer audit are owned by
  `key-material-hierarchy.md`
  § Architectural rules #8 (code: `fauna_protocol::sig_domain`). Never
  add a new un-tagged signer with a shared key — including a second
  bare-CID signer (`key-material-hierarchy.md` § Don't do these).

Both error variants come from
`libs/fauna-cbor/src/error.rs::VerifyError`; the positive and
negative paths are exercised in `libs/fauna-cbor/tests/envelope.rs`.

## Transport trust — authenticating a nest's TLS without a public CA

Everything above is *app-layer* (sign-over-CID): it proves a payload's
authorship once it's in hand. This section is the orthogonal *transport*
question: when a Fauna **client** opens the WS-RPC TLS connection to a
**nest**, how does it know it reached the real nest and not a
man-in-the-middle — for a self-hosted, LAN, or `.local` nest that has no
publicly-trusted (CA-issued) certificate?

A public-domain nest answers this the boring way: it serves an ACME /
CA-issued cert and WebPKI validates it. The hard case is the
**Pi-on-the-LAN / `.local` / bare-IP** nest, which serves a **self-signed**
cert (`bins/fauna-nest/src/self_signed_cert.rs`,
`fauna.bridges.provision_self_signed_cert`; the nest also auto-self-signs at
bootstrap before ACME can run). The **server-side** policy for *which* cert the
nest serves per connection — the always-live self-signed floor, the tiered
trusted-cert acquisition (HTTP-01 / client-published DNS-01), and how a cert
stays alive and trusted — is owned by
[`nest/tls-certificates.md`](nest/tls-certificates.md); this section is its
**client-side** counterpart (how a client authenticates whatever cert it
receives). The two are additive: the server keeps a cert always live and as
trusted as obtainable, the client pins the *identity* the channel binding
proves. The client *was* all-or-nothing — WebPKI-valid, or the
`FAUNA_INSECURE_TLS=1` escape hatch that accepted *any* cert (**MITM-open**,
dev/dogfood only). That flag is **removed** and the path this section
specifies is **implemented** on every bearer-carrying connection (see
`## Implementation status today`); the prose below is the normative
statement of what now runs.

> **Scope — the *Fauna app* ↔ *nest WS-RPC* leg only.** A generic MUA
> reaching a nest's IMAP/CalDAV listener cannot verify a Fauna identity
> signature and gets the weaker network-trust posture — owner:
> `nest/deployment-home-with-public-relay.md` § MUA reach. The Fauna
> client *can* (it knows the nest's `actor_id`), so it gets the strictly
> stronger guarantee below. The two are additive, not contradictory.

> **A third-party principal's session is outside this section as well (ratified 2026-10-01; built 2026-10-02).** It authenticates the nest by ordinary WebPKI on the issuer's domain — the trust the client already placed when it fetched discovery there; a nest with no claimed domain has no issuer and so no principal sessions at all — and authenticates *itself* with a DPoP-bound access token at the upgrade, never a bearer. Owner: [`transport-connection.md`](transport-connection.md) § Connection lifecycle → *The principal session*.

> **The login signature binds the nest it is addressed to (2026-09-23).** The channel binding below authenticates the *nest to the client*; the complementary direction — that the client's login signature cannot be relayed by the nest it signs into to mint a bearer elsewhere — is owned by [`../behavior/login.md`](../behavior/login.md) § Binding the nest: every `fauna.auth.{handshake,verify,device_handshake,custody_handshake}` signature names the identity `fauna.auth.nest_handshake` proved on the same connection, SPKI-compared on native TLS and pin-checked before signing on web.

### Two independent axes

Trust here is **not** a bind-vs-TOFU either/or. It is two orthogonal axes
that always both apply:

**Axis 1 — Cert ↔ identity binding (the mechanism; always on).** A
**channel binding** that fuses the TLS channel to the nest's Fauna identity,
RFC 5929 `tls-unique` in spirit:

1. The client accepts the self-signed cert **provisionally** — encrypt-only,
   *not yet authenticated* — and records the **SPKI fingerprint** of the cert
   it actually received (`SHA-256` over the cert's
   `SubjectPublicKeyInfo` — the *public-key* fingerprint, **not** the
   whole-cert fingerprint).
2. In the WS-RPC handshake the client sends a fresh random `client_nonce`.
   (The same nonce is also folded into the client's own auth signature —
   `handshake_signed_message(actor_id, timestamp, client_nonce)` — so two
   legitimate same-actor clients signing in the same millisecond don't collide
   on the nest's deterministic-signature replay guard; `login.md` § direct auth,
   auth-handshake finding #1. One field, two uses.)
   The nest signs `(SPKI_fp_of_the_cert_the_nest_itself_serves ‖
   client_nonce)` with its `nest_signing_key` (the stable Ed25519
   deployment-identity key, which survives a factory reset so the pinned
   `nest_actor_id` stays stable — custody, reset survival, and off-box
   recovery: `key-material-hierarchy.md` § Roots → Deployment Ed25519
   signing key + `nest/box-recovery.md`) and returns
   `(signature, nest_actor_id)`.
3. The client verifies the signature over
   `(SPKI_fp_of_the_cert_THE_CLIENT_received ‖ client_nonce)` against the
   public key of `nest_actor_id`.

   **Load-bearing subtlety:** the nest signs the SPKI of the cert *it* serves
   (known locally), never an SPKI the client reports. The client checks that
   signature against the SPKI *it* saw. The two SPKIs are equal iff no
   middlebox substituted the cert. A LAN MITM terminates TLS with its own
   consistent cert, so plain TOFU-on-cert is fooled — but the MITM saw a
   *different* SPKI than the nest serves and **cannot forge the nest's
   Ed25519 signature over the SPKI the client saw**. So the substitution is
   exposed exactly when it matters. This is why the binding is *necessary*,
   not optional, even when an out-of-band fingerprint was confirmed.

This axis is a pure mechanism — it proves "the encrypted channel I'm on
terminates at the holder of `nest_actor_id`'s key." It says nothing about
whether `nest_actor_id` is the nest I *meant* to reach. That is Axis 2.

**Axis 2 — Identity root of trust (where the *expected* `nest_actor_id`
comes from; differs by deployment).**

| Deployment | Root of trust | First-connect UX |
|---|---|---|
| **Public domain** (`alice@example.com`) | DNS `_fauna.{domain}` TXT `self=<actor_id>` (`libs/fauna-core/src/resolve.rs`; DNSSEC is the ideal hardening). The expected `actor_id` is fully resolved before connecting. | **None** — fully verified, no warning. The login records the verified identity as the host's pin (*The login's pin*, below), so the escrow-holder set is never empty here; a later identity change the zone and a rotation chain do not carry warns like the TOFU row. |
| **LAN IP / `.local`** (`alice@192.168.1.57`, `alice@pi.local`) | **TOFU** — there is no DNS authority to ask. Pin `nest_actor_id` on first successful channel binding; on a *later* change, warn loudly (the SSH `known_hosts` model). | First connect pins silently; optional out-of-band hardening (the Pi prints its fingerprint / a QR at setup → the user confirms it, closing the first-connect trust window). |
| **Client-provisioned box** (first contact, pre-DNS) | the client's own **injected `deployment_seed`** — the client minted+injected the box's identity at provision, so it knows `nest_actor_id` a priori. This is the LAN-IP row with the *optional* out-of-band hardening made **mandatory and automatic** (the client is the out-of-band channel). | **None** — verified against the injected identity from first connect; **no TOFU window**. |
| **Self-hosted public domain, pre-claim** (hand-deployed internet nest — `nest-internet-setup.md`; domainless until the claim names it, so no DNS `self=` exists yet and no seed was injected) | the **console-printed `fauna://claim` URI** — the boot banner prints the claim code beside a URI carrying `nest_actor_id` (`fauna_core::claim_code::claim_uri`; banner `bins/fauna-nest/src/claim.rs`), and pasting the URI makes the onboarding machine hold that identity as the first-contact root for the claim host *before the code is sent* (`wizard_submit_claim_code` → `hold_first_contact_identity`). The console is the out-of-band channel the code already travels on — the LAN-IP row's optional hardening made available here, where it guards a claim-time admin-takeover secret. | **Native apps:** paste the URI → verified from first connect, **no TOFU window**, and every later wizard call to that host self-verifies too — `WsNestApi::core`'s native arm runs `graduate_first_contact` against the held root before any wizard call rides the connection. Type the bare code → **no root** (the pre-fix state, kept working for old consoles/transcription): the claim rides the provisionally-accepted channel — a documented residual, closed for the account's later life by the post-claim DNS `self=` TXT. **Web consults the pin too, but proves less** (built 2026-08-16): the pasted identity is held the same way (`hold_first_contact_identity` is not cfg-gated), and the wasm arm of `WsNestApi::core` now verifies it before any wizard call rides the connection — `fauna_client_core::nest_trust::prove_first_contact_identity_possession` makes the box sign a fresh **client-chosen** nonce as the pasted identity, and a wrong identity, an absent binding, or a refusal of the handshake kind each hard-fail with nothing sent (as every row does since 2026-09-24 — the admin pasted *this* box's identity, so a refusal is hostile-or-broken). **It remains strictly weaker than native, and the difference is structural, not a gap to close later:** a browser exposes no received certificate to WASM, so there is no SPKI to compare the signature against and Axis 1's channel binding cannot be completed — the proof establishes *possession of the key*, not that *this channel* terminates at its holder. Concretely, web now refuses any box that cannot sign as the pasted identity (it previously accepted every box: the pin was stored and never consulted, a silent no-op), while a **compromised or coerced CA** stays exploitable by an attacker who both mints a browser-trusted certificate for the domain **and** relays the handshake to the real nest. Native's SPKI compare defeats that relay; matching it on web needs a received-certificate primitive browsers do not offer. |
| **Federation-granted** (cross-nest byte plane) | the **home nest's `nest_actor_id`, delivered WITH the routing** — a cross-nest reader dials the folder's home nest's HTTPS byte plane directly (chunk/manifest transfers), holding no account there, so it cannot graduate a pin the ordinary way. Whatever carried `home_nest_url` also carries the identity, and the row has **two arms of different provenance — never generalize one onto the other** (the *Mechanism* paragraph below splits them): **(shared set)** the inviter's grant — the MLS Welcome relay + every `caller_access` federated read reply stamp `home_nest_actor_id` beside the URL (`../architecture/federation.md` § Cross-nest → *Recipient-side access discovery*; `../behavior/file-sync.md` § Multi-writer shared sets); **(followed public folder)** no grant and no inviter — the follower typed the owner's address, the client derived `home_nest_url` from that domain, and the home nest stamps its own `nest.info` identity on the `fauna.folders.public.fetch` reply, relayed through the follower's own nest (`../behavior/folders.md` § Publicly-synced follow; `../ui/media.md` § Followed public folders), anchored by the domain→key binding TLS gives the relaying nest's dial (`../architecture/federation.md` § Security → *Domain↔key binding rides TLS*) and held against no prior expectation. `IdentityRoot::PreResolved` on both arms. | **None — no TOFU window on either arm**: the identity arrives before the first byte-plane contact, which verifies against it (a self-signed home works; a WebPKI home is unchanged and the root is belt-and-braces). The follow arm's residual is the absence of any expectation *older than the reply*: an attacker who owns the typed domain's resolution *and* a WebPKI-valid certificate for it (the bar the relaying nest's dial enforces since 2026-09-25 — `../architecture/federation.md` § Peer-auth model → *Discovery trust rule*) owned the whole follow by construction, and beneath the pin that arm has no seal (its content is public by the owner's choice) — CID verification still runs, rooted in the same relayed listing. |

**The login's pin — a login graduation leaves the pin store naming the identity it verified, whatever root proved it (ruled 2026-10-05; built the same day).** The pin is not only the TOFU row's record: it is *this machine's statement of which nest stands behind an authority*, and the escrow-holder set the account runtime trusts is defined as exactly that pin ([`account-data-taxonomy.md`](account-data-taxonomy.md) § The generation machinery → *A holder change re-receipts and never mints*, clause (1): "the one identity pinned for the bound nest … never the nest's own claim about itself"). So the rule is one sentence with three consequences, each closing a path on which a login verified the nest's identity and recorded nothing — which left every GenerationTip write (`fauna.state.mail`, `.backup`, `.folder-keys`, …) refused for good with *the escrow receipt is signed by a holder this account does not trust*, on exactly the deployments the project ships (a public-CA cert plus a published `self=`), for every user who signed in without having CLAIMED the box through onboarding (the claim ceremony's own seed, `seed_claimed_identity_pin`, hid the gap from every admin). **(1) A binding already in hand is never waived.** The WebPKI waiver (`trust::webpki_waives_binding`, the one decision point) answers "may a public-CA nest go *unbound*?" — the pre-identity first contact's question, asked before spending a round trip on `fauna.auth.nest_handshake`. A login never asks it: `read_login_binding` demanded a binding before anything was signed ([`../behavior/login.md`](../behavior/login.md) § Binding the nest), and every TLS nest's bearer-mint reply carries one. WebPKI says the client reached the name it dialed; the binding says *which nest* that is, and the stronger statement is kept — the full core runs (SPKI compare, the DNS `self=` root where the zone publishes one, else TOFU), the bound SPKI is pinned and re-pinned at every graduation, which is the mechanism a claimed § B-IP box already rides and what carries the app across an ACME rotation. **(2) The pre-resolved rows record the root they verified as the pin, authoritatively** (`trust::record_login_identity`, run by the two login wrappers after the core): a root outranks any TOFU pin, the same reasoning as the claim seed, and on the public-domain row "the client accepts whatever head the zone names" ([`nest/box-recovery.md`](nest/box-recovery.md) § Client acceptance) — so a pin that differs from a verified DNS root is replaced, not warned about; recording the identity the pin already names is a no-op, so an accepted rotation's seq survives every login. The core itself still never TOFU-pins a pre-resolved root. **(3) The scope is the LOGIN — the bearer-mint handshake and the silent-challenge launch wrappers — and nothing else.** `graduate_first_contact`'s callers record nothing new: the wizard's claim ceremony seeds its own pin; the byte-plane's grant-carried root stays on its own row (the federation-granted row's arms are fenced — *never generalize one onto the other* — and a durable pin for a home nest would be exactly that generalization); the consumer's never-minting heal and a read-only store stay never. **What this is, and is not.** It is native matching web: the SPA has always pinned the possession-proved identity on first contact over any origin (`check_web_nest_identity`, the NT-1 analysis below) and read its escrow trust from that pin. It is not a weakening: a pin adds the binding requirement, the SPKI pin and change detection *on top of* WebPKI, never in place of it; the first-connect UX on a public-CA nest stays silent. The one visible change is the public-domain row's "no pin": a box rebuilt with a new identity that neither the zone's `self=` nor a committed rotation chain carries now warns on native as it always has on web — which is the generation machinery's holder-change signal (clause (1): *trust follows the pin, every pass*), not a regression. Standalone e2e never saw the gap because its plaintext rig is seeded (`e2e-automation-surface-gating.md` § The e2e trust seed); live mode deliberately is not, which is how the first tui sweep against a real public-CA box exposed it. Witnesses: `trust::tests::{a_webpki_login_with_a_binding_in_hand_pins_the_identity_it_proved, a_login_records_the_pre_resolved_root_it_verified_as_the_pin, both_login_wrappers_pin_a_never_claimed_public_ca_nest}` (red first against the waiver), beside the unchanged waiver table and the § B-IP bridged-box test.

**Mechanism for the client-provisioned row — the pre-identity nest-identity
handshake `fauna.auth.nest_handshake`.** The bearer handshakes can't serve this
window (`fauna.auth.handshake` requires a *registered* actor; nothing is
registered before the claim), so the nest proves its identity on demand,
pre-identity: the request carries a fresh `client_nonce`; the reply carries the
same Axis-1 `cert_binding` shape as `HandshakeReply` — the nest signs
`served_SPKI ‖ client_nonce` with `nest_signing_key` (no actor, no DB;
replay-moot since each signature is nonce-unique and nothing mutates). The
onboarding machine holds the injected seed's **derived public identity** for the
provisioned domain (`hold_first_contact_root`; the seed itself stays under
box-recovery custody rules), persists that public identity — never the seed —
beside the claim code in the pending-provision slot so a Retry or a relaunch
re-holds the same root for the same box (`../behavior/onboarding.md` § 6 *The
pending-provision slot*), and runs the handshake as the **opening step of
every fresh pre-identity connection** to that host — probe, claim,
NAT-mode, mail-enable each self-verify (the wizard opens a fresh connection
per call, so a graduate-once model would leave the later calls blind). The
verification core is shared with the bearer path
(`graduate_handshake_with_root`; `IdentityRoot::PreResolved` covers both DNS
`self=` and the injected seed). A mismatch on a box we just provisioned is
MITM/bug, never benign → hard-fail per § Connection-teardown rule — **and that
hard-fail includes a nest that *refuses the kind or withholds the binding***:
with an injected root held the client knows the box it just provisioned (from a
current image) answers the handshake, so a rejection is a first-contact MITM's
cheapest move (terminate TLS, refuse the kind, ride the downgrade), not a benign
old nest (safe
failure direction — re-provisioning from the current `:latest` clears it). The
"no TOFU window" cell above is therefore unconditional on this row — **and since
2026-09-24 on every row: a kind-rejection hard-fails everywhere.** Past the
WebPKI waiver the binding is demanded outright, and a box that will not prove
its identity is not spoken to (`trust::TrustError::BindingRequired`, nothing
sent). The pre-Track-2 compat fallback that once let a rejection on the pin-less
DNS/TOFU ladder — and on a pinned host on the *self-signed floor* — proceed
ungraduated (design + A/B analysis ratified 2026-07-07; the "held covers a root
*or* a pin" tightening ruled 2026-09-02) was a backwards-compatibility remnant
for nests predating `fauna.auth.nest_handshake` (2026-07-07), and the
compat-remnant sweep removed it in place with no accept path
([`version-compatibility.md`](version-compatibility.md) § Dimension 2, the
fourth user-ruled write-off). With it went the residual that fallback carried
(an active MITM could mimic the rejection) and the *open population question* it
raised (a nest that minted a TOFU pin over the bearer handshake but predated the
kind, and had since acquired a WebPKI-valid cert — a population the user confirmed
empty on 2026-09-26, no such alpha nest existing): every nest a current client
meets answers the kind, and a box that does not is updated, not tolerated. The
failure direction is safe (refuse, nothing sent).

**Mechanism for the federation-granted row — the cross-nest byte plane.** A
cross-nest reader's byte plane (direct HTTPS chunk/manifest transfers to the
folder's **home** nest) has no other trust root: the reader holds no account on
the home nest, so no bearer handshake runs there, and the control plane is
relayed through the reader's own nest, not dialed. The same pre-identity
`fauna.auth.nest_handshake` fills the gap — `graduate_first_contact` runs it
against the home nest with `IdentityRoot::PreResolved(home_nest_actor_id)` (the
identity the record carries — whose provenance differs per arm, below) and pins
the bound SPKI, which the byte-plane reqwest
client (`store_pinned_reqwest_tls`, read **per-handshake**) then accepts. Run
**before** the byte plane is dialed, by one shared body —
`fauna_client::graduate_home_nest_pin` — from both native byte-plane dials:
`fauna-sync-agent`'s bind (`engine_driver`), and every app's Media download of a
foreign set or a **followed public folder** homed elsewhere
(`fauna_client::ForeignPublicChunkFetcher::for_home`, fed the record's
`home_nest_actor_id` through the media machine's foreign-fetcher factory —
built 2026-09-22; before that the Media dial was plain WebPKI and a self-signed
home was simply refused). `https` homes only — a plain-`http` loopback has no
TLS to pin. **Refusal after a root was delivered is a hard-fail
→ the folder is left inert (fail-closed + loud), retried on the next bind**: a
home nest serving cross-nest sets postdates Track 2 and answers the kind, so a
rejection is the MITM's cheapest move, not a benign old nest — and a WebPKI home
graduates by short-circuit, so a refusal is never a benign-old-nest case here.
**Absent** `home_nest_actor_id` (a relay-unaware home) skips graduation and keeps
the `RequireWebPki` floor — never weaker (a WebPKI home works, a self-signed one
fails loud at TLS). **The root's provenance and what sits beneath the pin differ
per arm — a sentence true of one arm is not licensed on the other.** *Shared
set:* the root is the **grant-delivered** identity (Welcome relay /
`caller_access` reply). Why it may travel the (own-nest-mediated) grant relay
rather than inviter-authenticated E2E material: the member's own nest is
**already routing-trusted** (it controls `home_nest_url` today), so relaying adds
no adversary; and content stays sealed E2E (the content key came over MLS) +
CID-verified regardless, so the pin is a second line behind the seal. A
strictly-separable future hardening moves `home_nest_url` + `home_nest_actor_id`
inside the MLS Welcome payload — both fields or neither. (Design ratified
2026-07-23.) *Followed public folder:* there is no grant and no inviter. The
root is the home nest's own `nest.info` identity, **stamped by the home nest on
the `fauna.folders.public.fetch` reply** and relayed by the follower's
already-routing-trusted own nest, which resolved that identity over
authenticated TLS to the domain the follower typed — anchored by the domain→key
binding TLS provides (`federation.md` § Security → *Domain↔key binding rides
TLS*, enforced at that dial by § Peer-auth model → *Discovery trust rule* since
2026-09-25: a handle domain is a request-named global target, so neither of the
rule's carve-outs reaches this arm) and held against **no prior expectation**
(`federation_pool::originate`'s
own caveat: the dial proves the peer holds the key it advertises, not that it is
the peer the caller intended). That missing older expectation is the honest
residual, not a missing anchor — the arm is not the LAN-IP row's TOFU, since a
CA's domain→key binding, not blind first contact, is what the stamp rests on.
And beneath the pin there is **no seal**: the owner's own `audience=public`
choice rests the content unsealed (`../principles.md` § The user always
controls their data), and the followed read is structurally keyless
(`../ui/media.md` § Followed public folders). CID verification still runs
(manifest against the address asked for, file bytes against the manifest's
hash), but its root is the same relayed listing — it defends against byte-plane
substitution, not against a lying home nest. So on the follow arm the graduated
pin is the byte plane's only transport line, with no seal beneath it to fall
back on; a session reasoning about what a compromised home nest can do to a
follower starts from that, not from the shared-set arm's sentences.

**Key principle: pin the *identity* (`nest_actor_id`), never the cert.**
Certs rotate (90-day self-signed expiry, reinstall, ACME self-heal), so
raw-cert pinning would false-alarm on every benign rotation. The
per-connection channel-binding signature (Axis 1) re-binds *whatever cert is
current* to the stable identity each time, so the only thing worth pinning
is the identity the binding proves. "Bind cert→Ed25519" is the always-on
mechanism; "DNS / TOFU / out-of-band" is the per-deployment identity root.
A *deliberate* change of the pinned identity is the deployment-seed rotation
ceremony — a pinned client re-pins on a verified rotation chain instead of
surfacing the identity-changed warning, and refuses superseded ancestors
afterward; owner: `nest/box-recovery.md` § Deployment-seed rotation (ratified
2026-08-11; client acceptance BUILT 2026-08-12 in the shared
`fauna_client_core::nest_trust` core — an updated client re-pins silently; a
pre-acceptance client still surfaces the warning, the § Version skew arm).

**The mail/CalDAV serving path honors the LAN-IP row too.** A domainless / bare-IP nest serves IMAP + CalDAV at the same locator a Fauna app uses; Fauna apps authenticate that endpoint by Axis 1 exactly as above (address/CA-independent), while a third-party MUA — which cannot do channel binding — falls back to plain TOFU-on-cert with the out-of-band fingerprint as optional hardening. Owner: `../behavior/caldav-server.md` § Authentication + § Network exposure.

### Connection-teardown rule

TLS completes **before** the WS-RPC handshake, so the cert is accepted only
*provisionally* and is *retroactively* authenticated by the in-band
signature. If channel-binding verification fails — bad signature, wrong
`actor_id` vs. the DNS/TOFU root, or a TOFU pin-change the user rejects — the
client **tears the connection down immediately** and never sends the bearer
or any request over it. A provisional connection that fails to graduate is
indistinguishable from an attacker's connection and is treated as one.

**Tearing down is dropping the client, so dropping the client must actually
close the socket.** The rule is carried out by releasing the connection object
(`AnonymousNestClient`), which is the only teardown affordance the pre-identity
path has. That makes the type's `Drop` load-bearing for this rule rather than
mere hygiene: it aborts the task driving the connection, because the driver owns
the whole adapter and **dropping a `JoinHandle` detaches its task instead of
aborting it**. While that abort was missing the rule was silently a no-op — a
refused connection stayed open and `ESTABLISHED` for the life of the process,
and since every bearer mint opens one such client per refresh, a client
reconnecting in a loop accumulated one unreleased socket per attempt until the
host could open no new outbound connection at all. Pinned by
`libs/fauna-anon-client/tests/connection_teardown.rs`.

### Cross-connection binding — pinning the bound SPKI onto the bearer connection

Axis 1 verifies the binding on the connection that carried
`fauna.auth.handshake`. But a Fauna app's **bearer** rides a *different* TLS
connection — the authenticated `GET /api/v1/ws/{actor_id}` (the bearer is a
subprotocol header at connect time), and historically a separate HTTPS
`POST /auth/token`. Binding connection *A* while sending the bearer over
connection *B* would leave *B* MITM-able: an attacker who passes the
pre-identity handshake through cleanly but intercepts the authenticated WS would
capture the bearer. **The binding must therefore extend to every connection
that carries the bearer.** It does so by *pinning the SPKI it just authenticated*:

1. A successful handshake binding yields a trusted pair `(nest_actor_id,
   bound_spki)` — the channel binding proved `bound_spki` belongs to
   `nest_actor_id`, and Axis 2 proved `nest_actor_id` is the nest meant.
2. The client **pins `bound_spki`** and requires every bearer-carrying TLS
   connection to that host to present a leaf cert whose SPKI equals
   `bound_spki`. This is sound *on its own*, without a second signature: TLS
   requires the server to prove possession of the cert's private key, so a MITM
   **cannot complete a handshake as a cert whose SPKI it doesn't hold the key
   for**. The per-connection signed binding's role is purely to *bootstrap*
   which SPKI to pin (via DNS or TOFU); ordinary SPKI-pinning secures the rest.
3. **Cert rotation** (90-day self-signed expiry, ACME self-heal) changes the
   served SPKI, so a pinned-SPKI mismatch is *expected and benign*. On mismatch
   the client re-runs `fauna.auth.handshake` to re-bind; if the freshly-bound
   `nest_actor_id` still matches the pin, it accepts the new SPKI and re-pins.
   This is why the durable pin is the **identity** (`nest_actor_id`), never the
   cert (the key principle above): the SPKI pin is a per-session cache the
   binding refreshes, the identity pin is permanent.

A direct consequence: for a self-hosted-trust nest the bearer is obtained over
the **WS `fauna.auth.handshake`** (the binding-verified path that mints the
token), not a separate unauthenticated HTTP `POST /auth/token`. That legacy
HTTP token-fetch — exactly what `FAUNA_INSECURE_TLS` made accept-any — has been
**deleted** (the last `deprecated_http` control-plane twin, dropped at the
WS-RPC-everywhere endgame), so bearer acquisition now rides the bound WS
connection as its sole path: both the secure shape and the realized
`docs/goal/architecture/transport.md` "WS-RPC everywhere" end-state.

**The dialers — one implementation, every bearer-carrying WS (2026-08-02).**
This policy is code exactly once — `fauna_anon_client::tls_dial` (graduated-pin
lookup → SPKI-pinned or strict-WebPKI dial, plus the § Pin custody
graduate-and-retry arm) — and every bearer-carrying native WS dial consumes it:
the WS-RPC bearer channel (`fauna-client::ws_adapter`), the leaf-crate bearer
connect (`fauna-anon-client`), and — until the daemon that dialed it was
removed 2026-10-02 — the **sync `/sync/ws` data plane** (`bins/fauna-sync`;
the route itself was removed the same day). The sync plane is named deliberately: it shipped for weeks
as a fourth hand-rolled dial that never adopted the policy (plain strict
WebPKI), so a fresh nest serving the self-signed floor was undialable by the
headless daemon — first-boot LAN file sync broken exactly when a new user first
tries it. The residual HTTP legs share the same policy via
`store_pinned_reqwest_tls`. A new bearer-carrying connection must consume
`tls_dial`, never re-derive the policy inline. A WebPKI-valid graduation that
holds **no root** also **clears** any SPKI a prior self-signed graduation
cached for that host — the floor→ACME rotation otherwise strands a long-lived
process on a stale pin no re-bind refreshes (rotation rule 3 above; the pin is
a session cache, and a WebPKI cert with no root needs none). Once a root *is*
held the clearing never runs: the graduation verifies the binding in full and
**re-pins** the served SPKI, which serves the same rotation — and the
[`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP bridge cert's
~4-day rotation — without ever downgrading a pinned host.

### Post-auth surfacing — a mid-session identity change blocks, on the same surface (ratified 2026-07-23)

Identity verification does not end at launch; three structural re-check
points keep running for the life of a session, so a nest whose identity
changes *mid-session* is detected without any new mechanism:

1. **Bearer re-mint.** Bearers are short-lived (the nest's TTL, ~1 hour,
   with a 60 s pre-expiry refresh), and every native re-mint opens a fresh
   anonymous connection and runs the full Axis-1 graduation over the silent
   challenge (`fauna_anon_client::mint_bearer_over_silent_challenge`), consulting the
   identity pin. Web's re-mint — wasm `challengeVerify` since 2026-09-23, a
   possession-checked handshake from 2026-08-19 (closing the residual
   this bullet used to record) — possession-verifies the reply's
   `cert_binding` over the challenge nonce folded with its own client nonce
   and rejects before the token is returned.
2. **SPKI-pin mismatch → re-handshake.** Every bearer-carrying connection
   requires the pinned SPKI (§ Cross-connection binding); a served-cert
   change forces a re-bind, which re-runs the full identity verdict.
3. **Background silent challenges / token refresh.** A post-auth
   `fauna.auth.challenge`/`verify` re-check (e.g. a client's background
   account-data refresh) yields the same
   `SilentChallengeOutcome::IdentityChanged`, and the launch machine's
   token-refresh seam the same `TokenRefreshOutcome::IdentityChanged`.

The *security* invariant therefore already holds mid-session: a changed
identity fails graduation, the connection is torn down, and no bearer is
sent over it (§ Connection-teardown rule). What this section ratifies is
the **surface**. A post-auth `IdentityChanged` verdict, from *any* of the
three channels, is routed to the **same blocking `launch_identity_changed`
surface the launch path renders** — the warning, the explicit re-trust
affordance (`trust_nest_identity()` — forget the pin, re-TOFU), and the
fallthrough, with **no retry CTA** — and the session's bearer is dropped;
the session blocks until the user chooses (the launch-surface row in
`../behavior/onboarding.md` already states this for the machine's
token-refresh path: a mid-session identity change is the same MITM signal).
No app invents a second, softer per-app shape — no banner, badge, or
toast: a possible-MITM signal gets the one uniform surface all seven
apps already render (priority #1/#3; and the session is already
de-facto dead — its connections can no longer graduate — so a soft surface
over a broken session would be strictly worse, a generic-error mystery
instead of the honest verdict). Background refreshes stay silent for every
*other* failure class: transient/network errors remain logged-and-swallowed;
only the identity verdict escalates.

The plumbing rule that makes the routing implementable: **the typed verdict
must survive every seam between detection and surface.** The shared error
taxonomy on the bearer/connection path carries the identity verdict as a
distinguishable variant end-to-end; a client's glue is then only "on that
variant, drop the bearer and navigate to the existing surface."

The failure mode this rule exists to prevent, kept here because it is the
one a future seam will re-introduce by accident: a verdict flattened into a
stringly transport error reaches clients as a *generic* failure, which
reconnect supervisors treat as **retryable** — a transient-retry loop on a
MITM signal, the launch path's pre-2026-07-13 dead-Retry bug reborn one
layer down. So a new seam on this path either carries the variant or is a
bug; "it still compiles" is not the test, because the flattening arm is
usually a catch-all that compiles fine. Corollary, learned from the four
seams the 2026-08-15 leg fixed: **a side channel hung off one concrete
bearer cannot serve this**, because linux and tui supply their own
`BearerSource` — the property that makes the taxonomy the right carrier and
the `SupersededLatch` shape the wrong one here.

### Pre-claim surfacing — the wizard's anonymous path carries the verdict too (ratified 2026-08-30)

§ Post-auth surfacing's plumbing rule is scoped post-auth; this section
extends it to the **pre-identity wizard path** — every `WsNestApi` seam
(probe, silent challenge, claim, invite, register, age-nonce, storage/NAT
commits, escrow restore), whose fresh per-call connections each run
first-contact graduation (§ Transport trust). The *security* half already
held (teardown, nothing sent — § Connection-teardown rule); what this
ratifies is the *telling*: **the typed identity verdict must survive every
pre-claim seam.** A graduation failure the trust layer classifies as an
identity verdict is never mapped to a per-endpoint transient/retryable
variant; a seam that flattens it is a bug — the § Post-auth surfacing
failure mode one layer earlier. The measured shape was exactly that: eleven
of twelve seams mapped `AnonClientError::Trust` into per-endpoint
`Transient`s, so the "Almost ready" surface rendered a detected mismatch on
a just-provisioned box as an indefinite, error-free "waiting for DNS"
spinner.

The classification lives in ONE place — `WsNestApi::core`'s
`classify_core_failure` — never re-derived per seam:

1. **Pin-related failures** (via the shared `classify_identity_changed`
   table: a changed or forked TOFU pin, or a withheld binding while a pin
   exists) → the verdict.
2. **ANY graduation failure while a pre-resolved first-contact root is held
   for the host** (the injected deployment seed, or a pasted `fauna://claim`
   URI) → the verdict — the § Transport trust client-provisioned row's
   unconditional hard-fail, refusals and withheld bindings included.
3. **Rootless, pin-free trust trouble** (a first-contact binding failure on
   the TOFU ladder) stays transient: nothing was pinned or injected, so
   there is no identity to have mismatched, and a false-terminal is worse
   there.

The surface is deliberately NOT the post-auth blocking surface: pre-claim
there is no session to block, no bearer to drop — and **no re-trust
affordance**: with a held root the remedy is re-provisioning (§ Transport
trust: safe failure direction), never forget-the-pin, so
`launch_identity_changed`'s re-trust must not be offered anywhere in the
wizard. Instead each wizard consumer routes the verdict to its **existing
machine-shared terminal error state** — one shape per page, rendered
identically by all 7 apps; this is plumbing into pages the machine already
owns, not the "second, softer per-app shape" § Post-auth surfacing forbids.
In particular the two consumers that retry *without a human in the loop*
STOP: the "Almost ready" poll lands on the terminal error instead of the
DNS resting message, and the provisioning run's claiming substep fails the
run (surface spec: `../behavior/onboarding.md` § "Almost ready" surface;
witness:
`awaiting_manual_dns.rs::recheck_probe_identity_mismatch_is_terminal_not_the_dns_resting_message`).

### Pin custody across processes — one install-scoped store, one interactive minter

An app installation can put several **processes** on the wire to the same
nest: the interactive app, and headless helpers with no user attached — the
apple File Provider extension, the desktop background sync agents. Pin custody
across them follows two rules (ratified 2026-07-22):

1. **The identity pin store is scoped to the app *install*, not to any
   process.** The pinned identity is a fact about "the nest this installation
   trusts" — the `known_hosts` of the host — so every process of one install
   reads the **same** store, rooted wherever that platform's processes share
   client state (iOS: the app-group container's `trust/` dir, beside the
   credentials and sync state the extension already shares; **macOS, since
   2026-08-25: the user-domain `~/Library/Application Support/Fauna/trust`**
   — the ONE Rust derivation `install_scoped_trust_home` the sync agent and
   fauna-tui read, because a launchd-spawned process is TCC-prompted for the
   app-group container on every instance with no user decision ever binding
   the next one (`installers/macos.md` § Identifier domain, item 5) — with
   the app keeping a **read replica** at the container's `trust/` for the
   sandboxed File Provider extension, which can reach nothing else; the
   replica is the writer's own file, copied on every persist by the store
   itself (`DiskPinStore::open_in_dir_with_mirror`), so rule 2 is untouched
   and a stale replica fails closed exactly like a missing pin; platforms
   whose processes share a plain config dir — linux, windows — already
   satisfy this with the dir itself). A per-process store is a bug twice over: the helper
   starts empty (it can never authenticate a TOFU-rooted nest the user already
   trusts), and any pin it *did* learn would drift from the app's.
2. **Only the interactive app mints or removes pins; every other process is a
   read-only consumer.** First-trust and re-trust are user decisions, made
   where a user is present — the SSH model: an interactive session prompts,
   batch mode (`StrictHostKeyChecking=yes`) refuses unknown hosts. A consumer
   process reaching a TOFU-rooted nest with no pin **fails the connect and
   retries** (`IdentityError::PinRequired` — benign: the app just hasn't pinned
   yet) rather than silently trusting whatever it reached; a writable store in
   a background process would let a MITM be pinned with no user in the loop and
   would forfeit the pin-change warning this section promises. Consumer stores
   read the file per lookup (uncached), so a pin the app mints after the
   consumer launched is honored on its next retry without a process relaunch.

Mechanism: `NestIdentityPinStore::read_only()` marks a consumer backend
(`cert_binding::ReadOnlyDiskPinStore`); graduation then takes the strict TOFU
arm (`IdentityRoot::TofuStrict` — verify/warn exactly like the interactive arm,
but never mint). The store relocation once shipped with a one-time
**copy-never-destroy** adoption of the legacy per-process pin file into the
install-scoped home (`cert_binding::adopt_legacy_pin_file`); it was retired
2026-09-24 by the compat-remnant sweep
([`compat-remnant-sweep.md`](compat-remnant-sweep.md) § Program
4) — no pin file predating the relocation exists anywhere.

**The consumer's dial needs its own graduation step — and that step never
mints.** The § Cross-connection binding SPKI pin is an ephemeral
**per-process** cache populated by a handshake graduation *in that process* —
and a consumer process holds a bearer, not the identity key, so it never runs
the signed `fauna.auth.handshake` mint that graduates one; a shared identity
pin alone still leaves its bearer dial failing strict WebPKI against a
self-signed nest. The shared trusted dial (`fauna_anon_client::tls_dial`,
consumed by `fauna_client::ws_adapter`, and by the removed `/sync/ws` plane until 2026-10-02)
therefore carries a **graduate-and-retry fallback**: a failed dial with no
SPKI pin over `wss` runs one pre-identity `fauna.auth.nest_handshake`
graduation (`AnonymousNestClient::graduate_transport_trust`) and retries once
with the SPKI it cached. That graduation is **always the strict arm above**
(`PinMinting::Never`), whatever pin store the process installed: it verifies
a held identity pin or a DNS `self=` root and refuses everything else, so an
unpinned TOFU nest stays refused until the interactive path pins it. The
strictness cannot be left to the store, because the fallback runs on a
*failed* dial with a bearer in hand and no user in the loop, and every dialer
reaches it, not only consumers: a public-CA nest is unpinned until this
machine's first login completes (since the login's pin ruling above; before
it, a public-CA nest's own bearer mint took the WebPKI waiver and pinned
nothing, for good), so an interactive app (or, until its removal
2026-10-02, the standalone daemon) meets any dial error against such a nest
before that login with no pin at all. There, a
store-decides graduation TOFU-minted whichever box answered the pre-identity
handshake with a binding-valid self-signed cert — an on-path attacker needs
no CA compromise to be that box, only to make the first dial fail — and the
retry then carried the real bearer to it. Witness: tier_3
`bins/fauna-nest/tests/tls_bearer_dial_fallback_never_mints.rs` (a
writable-store process with no pin refuses and sends no retry; a held pin
still heals), beside the consumer-store twin `tls_pin_consumer_fallback.rs`.

**The derivation of the trust home is shared code, and tui is a writer
(2026-08-02).** The per-platform install-scoped home resolves through one
function — `fauna_anon_client::cert_binding::install_scoped_trust_home` (unix
`$XDG_CONFIG_HOME/fauna`; macOS the user-domain trust dir rule 1 names;
windows `%LOCALAPPDATA%\Fauna`) — consumed by writers (the linux app, tui) and
consumers (the sync agent's read-only store) alike, which makes rule 1's
alignment structural rather than a table each crate transcribes. It exists
because tui drifted: it wrote a per-app `$XDG_CONFIG_HOME/fauna-tui` store no
consumer read, so a tui-provisioned sync agent could never authenticate a
TOFU-rooted (self-signed) nest — its graduate-and-retry ran `TofuStrict`
against a store the app never wrote and failed `PinRequired` forever. (The
one-time adoption of pins pre-fix tui builds minted into the per-app dir was
retired 2026-09-24 with the adoption above.)

**The standalone headless daemon was its own install (ratified 2026-08-02;
the daemon was removed 2026-10-02 — `apps/sync-agent.md` § Headless
deployment — and this paragraph is kept as the record).**
`bins/fauna-sync` — the legacy NAS/server deployment (`file-sync.md`
§ deployments) — had no interactive app beside it, so rule 2's "only the
interactive app mints" resolved to the daemon itself: it installed a
**writable** `DiskPinStore` beside its hand-edited config (the same
seed-custody carve-out that config's `secret_key` already lives under) and
TOFU-pins on first connect — the SSH `accept-new` model, first start being the
provisioning moment. A later identity change hard-fails the graduation;
recovery is deleting the pin file beside the config and restarting the daemon.
Wherever the daemon meets that verdict — its start-up mint, its register hold
(`file-sync.md` § 1 Device Registration), a `/sync/ws` redial — it **parks**:
it stops attempting the nest, logs the verdict and the pin file's path at
`error`, and stays up until stopped. Not a retry: that is § Post-auth
surfacing's retry loop on a possible MITM. Not an exit either: the daemon's own
service units restart an exited process within seconds, the same loop only
faster. The prior state — the process-default in-memory store — was not a
lesser version of this but rule 2's exact failure: an empty, writable store at
every start silently re-TOFUs whatever answers, with no user in the loop and no
pin-change warning ever able to fire.

## Co-resident process trust boundary (UID isolation)

A single-box deployment co-locates `fauna-nest` with the processes that parse
hostile internet input — the Go MTA and MDA (`fauna-mail-bridge` roles) and the
`fauna-sni-router` — under s6 supervision in one Docker image. nest extends a
**loopback trust** to those neighbours: it accepts a PROXY-v2 header conveying
the real client IP only from a loopback peer (`read_optional_proxy_header`,
trusting `tcp_peer.ip().is_loopback()`), and it auto-approves a bridge
self-enrollment presented over a loopback connection
(`bridge_blob_handlers::request_enrollment`).

**Target: the co-resident processes are isolated, not mutually trusting.** Each
runs under a **distinct UID**, and `/data/keys/*` plus the sealed store are
filesystem-isolated so one process cannot read another's key material; the
hostile-MIME-parsing bridges run under their own UIDs with **kernel-enforced
filesystem default-deny (Landlock + seccomp — the ratified + built Slice 4
shape below)**; a literal separate container remains optional future hardening.
The trust nest extends to a loopback peer is then **earned, not assumed**:

- **every backend the router fronts** — nest, and each Go bridge that peels the
  header itself — distinguishes the SNI router (the one legitimate PROXY-header
  writer) from every *other* loopback peer by a **shared secret the router holds
  and the bridges' UID cannot read**, appended to the PROXY-v2 header as a custom
  TLV that the backend verifies — so a compromised MTA/MDA/PDS bridge cannot forge
  a PROXY header to spoof a source IP (evading per-source rate limits / the MDA
  AUTH-lockout key). Each backend has its **own** peel point, so each must verify:
  a nest-only check would leave the harm this bullet names — the MDA lockout key,
  which is keyed off the *MDA's* peel — wide open.
  (`SO_PEERCRED` is *not* usable here despite each role now having its own UID:
  the router→nest hop is **TCP loopback**, where peer-credential checks are
  unavailable — they need a Unix-domain socket, a transport change not worth
  perturbing the delicate `serve_tls` path for. The shared secret is the
  sanctioned alternative.)
- a bridge proves possession of an **artifact-provisioned key** owned by its UID
  (`/data/keys/{mta,mda}.key`) at enrollment, instead of any fresh loopback-
  presented pubkey being auto-approved — and because the key file is UID-isolated,
  a co-resident attacker cannot read it to forge the proof.

This is **artifact-set IPC** (the deployment image/installer wires the UIDs,
ownership, and container split — no human config knob, per `principles.md` § One
configuration surface), the same bucket as the existing `caldav_listen_https=127.0.0.1:8444`
loopback split.

**Hosted third-party code (ratified 2026-09-05, the third-party integration chain — TP3/TP11; unbuilt).** A plugin the nest hosts ([`third-party.md`](third-party.md) § Execution forms) is a co-resident process under exactly this posture and never a peer of trust: a curated-catalog **container** runs under its own UID with the kernel-enforced default-deny above (Landlock + seccomp), network-restricted to the hosts its document declares; a **WASM component** gets only the capability imports the host links (named outbound hosts, the nest API, a state scope, a clock) and the labeler runtime's fuel/memory caps. Neither ever holds a **certificate key**: the nest terminates TLS for any path or SNI name a plugin's document claims (`ingress`) and proxies plaintext to the plugin over the sidecar channel with a **nest-signed identity assertion** — the identity-aware-proxy shape, never a bare forwarded header (the same class of forgery the PROXY-header secret above closes). The PDS bridge's own cert fetch (`apps/bridges.md` § Bridge-kind catalogue, `fetch_tls_cert_blob`) is the first-party exception TP11 migrates last. The sandbox profiles are owned here; the runner contract is `third-party.md`'s.

### Implementation status — UID isolation

**Decided 2026-06-24 (user): adopt the full UID/container split.** Resolves
loopback PROXY-spoofing, signature-free self-enrollment,
and the MDA-RCE-blast-radius items.

- **Slice 1 — distinct UIDs + filesystem isolation (single image): BUILT.** Each
  network-facing s6 service runs under its own non-root UID — nest `fauna`(1000),
  MTA `fauna-mta`(1001), MDA `fauna-mda`(1002), SNI router `fauna-router`(1003);
  `supervisor` stays root, `algorithm` is `fauna-sandbox`-confined.
  nest's sealed store (`/data/nest.db`, `/data/blobs`, `/data/acme`,
  `inbound-deliver-key`) is `0600`/`0700` nest-owned and **unreadable by a bridge
  UID**; each bridge's key lives in its own `0700` subdir
  (`/data/keys/{mta,mda}/`, bridge-UID-owned), so neither bridge can read the
  other's key. `/data` is `0711` (traverse-only for the now-non-owner bridges);
  `operator-hatch.toml` (topology, no secret) is `0644`. A redeploy migrates a
  pre-split flat keyfile into the per-role subdir, preserving the enrolled
  identity. Authority: `installers/docker.md` § s6-overlay Services. Verified by
  tier_4 `tests/e2e-unified/tests/platform/docker/test_uid_isolation.py`
  (cross-UID read denied; process UIDs distinct; all services still serve).
- **Slice 4 — isolate the hostile-MIME parsers from the sealed store: BUILT, via
  Landlock (user-chosen 2026-06-25).** The "own container" goal — *a MIME-parser RCE
  must not reach the sealed store even on in-container privilege escalation past the
  DAC UID* — is realized by running BOTH bridges under `fauna-sandbox`'s
  `bridge`/`bridge-imap` profiles (Landlock filesystem default-deny + seccomp)
  rather than a literal separate container. The Landlock profiles grant each bridge
  only `/data/keys/{mta,mda}` (RW, its own key) + `/data/operator-hatch.toml` (RO);
  `nest.db`/`blobs`/`acme` are **kernel-denied**, not merely DAC-denied. A separate
  container was evaluated and rejected as higher-complexity for the same goal (it
  must share the netns to stay a loopback peer for enrollment, then needs a
  dual-mode image + a new cross-container enable path + a Go idle-not-exit change +
  a netns-restart hazard); the existing `fauna-sandbox bridge-imap` profile was the
  prior-art signal. The bridges keep their privileged binds under the sandbox's
  `no_new_privs` because `fauna-sandbox` carries `cap_net_bind_service` itself and
  raises it into the ambient set before exec. Authority: `installers/docker.md`
  § s6-overlay Services; verified by tier_4 `test_uid_isolation.py` — since
  2026-07-22 with **in-domain kernel-LSM probes**, not inference:
  `fauna-sandbox <profile> -- cat/ls` of `nest.db`/blobs/acme **as root** (so
  DAC cannot be the denier) must fail EACCES inside a real bridge Landlock
  domain, with unconfined-success + allowed-path-success negative controls and
  an enforcement-status assert per probe — fully OR partially enforced, never
  `NotEnforced` (`Seccomp: 2` alone cannot detect the `NotEnforced`
  warn-and-continue fallback `landlock::apply` takes on a kernel without
  Landlock — that check proves only the seccomp half; current kernels report
  "partially enforced" with the denials provably working, so the behavioral
  probes are the authority and the status line only rules out silent-off). The
  literal separate-container option remains available as a future hardening if
  a stronger isolation boundary is ever wanted.
- **Slice 3 — router-distinguished PROXY trust: BUILT on all three
  peel points (code + tier_3); tier_4 + deploy bundled.** The router appends a
  shared-secret TLV (artifact-provisioned `/data/keys/router/proxy-secret`,
  root-only 0600) to **every** backend it proxies, not just nest
  (`fauna-sni-router` `encode_v2_authed`). Each peel point verifies it:
  - **nest** — `read_optional_proxy_header` honours a PROXY header only with a
    matching TLV on a router-fronted box (`is_fronted_by_router`), else falls
    back to the genuine loopback peer; shared `fauna_proxy_protocol` carries the
    TLV encode/parse.
  - **the Go bridges** — the MDA's shared **DAV-443** listener and the ATProto
    **PDS bridge's XRPC** listener both peel PROXY-v2 on their own loopback
    binds, so both apply the same check via `internal/proxyproto`'s
    `WithRouterAuth` (constant-time compare, fallback to the genuine loopback
    peer on a missing/wrong TLV). Without it the MDA's CalDAV **AUTH lockout +
    `report_auth_event` audit** — which key on the MDA's *own* peel, not nest's —
    and the PDS bridge's per-IP XRPC rate limits stayed spoofable by any
    co-resident UID (closed 2026-08-14; the asymmetry was graded in a security
    review tracked internally).

  All four run-scripts (nest, router, MDA, PDS bridge) read the secret **as root**
  before their `s6-setuidgid` drop and `export` it as `FAUNA_ROUTER_PROXY_SECRET`
  — via env not argv, so a bridge UID reads neither the file nor another process's
  environ. Uniformly permissive when unprovisioned: no secret ⇒
  trust-any-loopback, because failing closed would collapse every external client
  onto the router's loopback address (worse than the spoofing risk, and against
  works-out-of-the-box). tier_3 unit tests prove the rejection logic on both sides
  (`fauna-nest` `proxy_header_tests`, `fauna_proxy_protocol::tests`, and Go
  `internal/proxyproto` + the `davauth` end-to-end pin that a forged header cannot
  reach the lockout key or the audit); tier_4
  `tests/.../docker/test_proxy_router_auth.py` proves provisioning +
  UID-isolation + serving on the real image. Code-complete and merged; the
  tier_4 run + deploy **bundle into the mail deploy-verification work's slice-1+4 image
  build** — slice 1 is not yet deployed, and nest+router+bridges+secret ship in
  one image (no version skew). Until that image deploys, the live box keeps the
  unconditional loopback trust.
- **Slice 2 — key-possession enrollment (+ the x25519-PoP
  fold-in): ALL THREE halves BUILT (nest + Go-bridge + artifact); awaiting the
  bundled image deploy.** `request_enrollment` (`bridge_blob_handlers.rs`) now requires, when
  the artifact has provisioned a **blessed registry** for the role
  (`FAUNA_BLESSED_{MTA,MDA}_PUBKEY` — hex of the role's blessed Ed25519 pubkey,
  read from a root-owned bridge-UID-unwritable file by the run-script, slice-3
  env pattern), that the enroller (i) *be* the blessed key and (ii) prove
  possession of it with an Ed25519 signature over
  `enrollment_signed_message(role, ed25519_pubkey, x25519_pubkey)`
  (`check_enrollment_authorization`). The key file is UID-isolated (slices 1+4),
  so a co-resident attacker that cannot read it cannot forge the proof; a fresh
  attacker-generated pubkey is not blessed and is rejected outright. The **same
  signature covers the x25519 pubkey**, which nest binds set-once at enrollment
  (`upsert_bridge_x25519`) — so the x25519 proof-of-possession half folds into this
  one attestation and the later `register_service_user` only confirms the
  already-blessed value. Strictness is **gated on the registry's presence**
  (a binary-only / dev nest with no artifact mint stays lenient → deploy-safe);
  a router-fronted box that lacks one logs a provisioning-gap warning. tier_3:
  `bridge_blob_handlers::tests` (the `enrollment_auth_*` + `parse_blessed_pubkey_*`
  + `bind_enrollment_x25519_*` set) + the `fauna-protocol`
  `enrollment_signed_message`/round-trip tests. The **Go-bridge challenge-signing
  + artifact keypair-mint + blessed-registry write** (the mail deploy-verification work's
  domain) are now BUILT in the same image: the artifact mints each role's keypair
  AS ROOT at entrypoint (`fauna-mail-bridge --print-pubkey`, mint-if-absent →
  stable across reboots), writes the private half to the slice-1
  `/data/keys/<role>/<role>.key` (0600, bridge-UID-owned) and the public half to
  the root-owned, bridge-UID-unwritable registry `/data/keys/blessed/<role>.pub`,
  and the nest run-script points nest at the registry dir
  (`FAUNA_BLESSED_KEYS_DIR`, re-read live per enrollment since the 2026-07-09
  re-keying build; the earlier `FAUNA_BLESSED_{MTA,MDA}_PUBKEY` value envs are
  read by nothing — the registry dir is the one source); the Go
  bridge is now LOAD-ONLY and signs the enrollment (`wsrpc.EnrollmentSignedMessage`
  / `wsrpc.SignEnrollment`, the single-source byte mirror), sending the additive
  `x25519_pubkey` + `enrollment_sig` wire fields. Covered by `just mail-bridge-test`
  (the Go signer + `--print-pubkey` mint) and a tier_4 test
  (`test_bridge_enrollment_pop.py`: legit bridges enroll + auto-approve on the
  real image; a forged fresh-pubkey enrollment over loopback is rejected; the
  registry is root-owned + bridge-unwritable + matches the keyfile + the nest env
  — and, since 2026-07-22, the **lenient-box negative control**: with the
  registry root-removed the SAME forged enrollment is auto-approved + the
  provisioning-gap warning fires, proving the strict test's rejection is
  registry-caused and would go red, not green, on an unprovisioned box).
  Because strictness silently degrades when the artifact fails to provision the
  registry, nest **self-reports** it: the admin `fauna.bridges.list_service_users`
  reply carries an additive `enrollment_strict {mta, mda}` diagnostic
  (admin-class callers only; read live per call, same registry semantics as
  enrollment) — the sanctioned **no-SSH observable** for verifying a *deployed*
  box is not silently lenient (a provisioned box carries no ssh key —
  `testing.md` § Gap 3), asserted live on the installer-provisioned VPS by
  `tests/live/test_private_relay_hetzner.py::_assert_enrollment_strict`. The
  installer half (a compose override could defeat every slice from outside the
  image — `privileged`, `user:`, entrypoint override, seccomp `security_opt`,
  `FAUNA_BLESSED*` env, a bind over `/data/keys`) is pinned by
  `fauna-provisioning` `cloud_init::tests::test_cloud_init_does_not_defeat_image_hardening`.
  **The deploy gate cleared 2026-07-22 — the hole is now closed in production,
  verified on three independent surfaces** (release,
  `build-nest-image.yml` run `29914697508`, digest `sha256:9d0f5d76…89f562`, the
  same manifest on `:latest` and `:dev`), so the version skew above is gone:
  1. **The manifest** — the `self_contained_docker` pair re-run against the
     retagged production image: `18 passed` (both formerly version-gated asserts
     green).
  2. **A freshly installer-provisioned box** —
     `test_private_relay_hetzner.py::_assert_enrollment_strict` green on a real
     Hetzner VPS the installer stood up end-to-end, which is what proves the
     *artifact* wires the hardening rather than a hand-built image.
  3. **The deployed box** — `example.com` self-reports
     `enrollment_strict {mta: true, mda: true}` over admin WS-RPC, i.e. the blessed
     registry really is provisioned in production and the silent-lenient failure
     mode is not live.
  A supervised read-only on-box pass (2026-07-22, user-approved; SSH stays
  debug-only per `testing.md` § Gap 3 and is **not** a supported verification
  path) additionally confirmed the slice-1/4 facts on `example.com` itself, which
  matter there because it is hand-managed rather than installer-provisioned, so
  the `cloud_init` static asserts do not cover it: four distinct non-root UIDs
  (`fauna-nest` 1000, MTA 1001, MDA 1002, `fauna-sni-router` 1003; only s6
  supervision runs as root), and an in-domain `fauna-sandbox bridge -- cat
  /data/nest.db` **as root** denied `Permission denied` while the same read
  unconfined succeeds — the negative control that attributes the denial to the
  Landlock ruleset rather than DAC. The wrapper logged `landlock: partially
  enforced`, the same status the tier_4 assert accepts (see `_assert_landlock_enforced`).
  Wire/signing contract: § Enrollment proof-of-possession contract below.

Tracked internally (slices 2–3) — slices 1+4 are
done (tracked internally, archived). Related: moving DKIM signing
nest-side to shrink the stolen-MTA-key blast radius is a sibling "shrink the
co-resident blast radius" call to make alongside.

### Confinement self-probe (the no-SSH observable for slices 1 + 4)

Slices 1 and 4 above are verified **on the image** by tier_4
(`test_uid_isolation.py`). They were not, until this, verifiable **on a deployed
box**: a provisioned box carries no ssh key (`testing.md` § Gap 3), so the only
way to learn its actual UID split and Landlock status was a supervised on-box
pass — which is what the 2026-07-22 verification chain had to do, and which does
not scale past the one hand-managed host. Slice 2 already solved the same problem
for the blessed-registry half by having nest self-report `enrollment_strict`; this
is that shape applied to the process-isolation half.

**The mechanism.** The bridge **probes its own confinement at startup** and
reports the result as an additive `confinement` field on the
`fauna.bridges.register_service_user` call it already makes on every cold boot.
Nest bounds the values, stores them on the bridge's `bridge_service_users` row
with a nest-clock `reported_at`, and projects them on
`fauna.bridges.list_service_users` **to admin-class callers only** — beside
`enrollment_strict`.

**What reads them (ruled 2026-10-01).** The verification that has no shell — the live e2e and the image
suite — and nothing else: no app renders the four facts or the strict flag, and
none owes it. A healthy read-out gives an admin nothing to choose and nothing
to act on, so it is no page's content; what an admin is owed is to be told
when the walls are *down*, and that is the degraded warning below, on the
Logs page every app already has. (This paragraph said the facts were "read by
the same admin surfaces" until this date; no such surface ever existed.)

Four facts, split by **who measured them** — the distinction is load-bearing:

| Field | Values | Measured by |
|---|---|---|
| `uid` | the process UID | the bridge (`getuid`) — the slice-1 fact |
| `sealed_store` | `denied` / `readable` / `absent` / `unknown` | the bridge, by attempting to open `/data/nest.db` **from inside its sandbox** |
| `landlock` | `fully` / `partial` / `off` / `unknown` | **relayed** by `fauna-sandbox` across `execvp` |
| `seccomp` | `filter` / `strict` / `off` / `unknown` | the bridge, from `/proc/self/status` |

`sealed_store` is the fact that actually bounds the blast radius — but it
**attributes nothing**: the DAC UID alone produces the same `EACCES`, so a denial
here is not evidence Landlock is doing anything. Only `landlock` attributes it to
the kernel LSM, and only the wrapper can know it (`RulesetStatus` exists exactly
at `restrict_self`, and a restricted process cannot ask the kernel about its own
domain) — hence the hand-off through the environment
(`fauna_sandbox::LANDLOCK_STATUS_ENV`), which is **artifact-set IPC**, one
process telling the next what the kernel just did, with no human in the loop
(`principles.md` § One configuration surface, bucket 1). The wrapper sets it
unconditionally, so a value injected from outside the image loses to what
actually happened. Read together: `denied` + `partial` is the healthy production
shape (current kernels report *partially* enforced with the denials provably
working — which is why `landlock` is three-state and not a boolean);
`denied` + `unknown` says the confinement holds but the wrapper never ran;
`readable` says it does not hold at all.

**The `landlock` token set has one owner, and the asymmetry with nest is deliberate.** The three tokens the wrapper can emit are `fauna_sandbox::LANDLOCK_STATUS_{FULLY,PARTIAL,OFF}`; the Go reader names them `internal/confinement.Landlock{Fully,Partial,Off}` and switches on those constants in both places that matter — the admission parser and `Report.Confined`, which is what decides whether a box is *reported* as sandboxed. `internal/confinement.ConfinementStates()` is the closed set of every token a report field can hold, and `internal/logplane`'s interpolation allowlist derives from it rather than restating it. The cross-language pin (`TestLandlockTokensMatchTheRustWrapper`) covers the token **values**; the older pin next to it covers the env-var **name**, and covering only the name was the gap — a renamed token leaves the name pin green while every deployed bridge silently reports `unknown`, which then reads as a broken sandbox rather than a broken contract. ⚠ **Both pins read a file outside the Go module, and Go's test cache does not track it**: measured 2026-08-23, renaming a token with no Go file touched returns `(cached)` — green while wrong, the exact failure the pins exist to prevent. `just mail-bridge-test` therefore runs them uncached (`-count=1 -run 'TheRustWrapper'`) before the cached suite; that line is load-bearing, not tidy-up-able. **Nest deliberately does NOT mirror the set**: `bound_confinement_token` applies a charset bound, not an allowlist, so a newer bridge reporting a state this nest build predates survives (additive-everywhere, `version-compatibility.md`) — which is why this vocabulary has an owner on the emitting side and no twin on the receiving one.

**⚠ These are PROVISIONING DIAGNOSTICS, never a security attestation.** A
self-report from a compromised bridge is untrustworthy by definition — it can
claim whatever it likes, and nothing in nest may gate a security decision on it.
The value is catching the **honest misconfiguration**: a compose that bypasses
`fauna-sandbox`, a kernel without Landlock, a docker seccomp policy blocking the
landlock syscalls, an image that lost its per-role UIDs. The trust-bearing proof
stays tier_4, where the probes run against the image from outside the sandboxed
process. Same framing as the sidecar log plane's trust posture
(`apps/observability.md` § Trust posture).

**Two shape decisions worth not relitigating.** (1) The report is stored
**last-write-wins**, deliberately unlike the set-once `x25519_pubkey` /
`mlkem_ek` bindings on the same row: those freeze because a changed value means
an attacker redirecting a seal target, whereas this describes the *currently
running* process, so each boot must overwrite the last — freezing it would pin a
report from an image that is no longer deployed. (2) The status fields are a
small **open vocabulary**, not enums: a newer bridge may report a state an older
nest predates, and additive-everywhere (`version-compatibility.md`) says that
must survive rather than be rejected — so nest bounds each token (charset +
length, collapsing an illegal one to `unknown`) and stores it verbatim, never
failing an enrollment over a diagnostic string.

The wire also has a **secondary** surface: when the probe comes back degraded the
bridge emits a catalogued `confinement_degraded` log-plane warn, so a
misprovisioned box is visible to an admin who never thinks to open a service-user
row. The wire field stays the primary record — the plane is a bounded ring, not
queryable state. **Residual, stated:** because the ring is bounded, the warning
can scroll out of the admin's log while the condition still stands; the bridge
emits it again at every start. A standing indicator for the degraded state
would be a new element on an admin page and is not ruled owed.

### x25519 binding & DKIM-signer posture

**x25519 binding is set-once: freeze half BUILT.** At
`register_service_user` a bridge attests the x25519 public key the nest seals its
TLS cert blob to. That binding is now **set-once**: the first
attestation binds it, an idempotent re-attestation of the *same* key is accepted,
and a *different* key is rejected (`CacheDb::upsert_bridge_x25519` →
`BridgeX25519Frozen`, mapped to a permission-denied at the RPC boundary). So a
co-resident attacker holding the bridge's ed25519 identity still cannot swap the
sealing target after enrollment. The complementary **x25519
proof-of-possession signature** half is now folded into the key-possession enrollment
signature (slice 2, nest side BUILT) rather than a parallel mechanism: the
enrollment signature covers `ed25519 ‖ x25519`, so a valid signature both proves
possession of the artifact-blessed key *and* binds the x25519 pubkey, which nest
records set-once at enrollment — the later `register_service_user` then only
confirms it. The freeze is the standing protection until the Go-bridge signing +
artifact key-mint halves (now BUILT — see § Implementation status, slice 2)
**deploy** in the same image (§ Enrollment proof-of-possession contract).

**DKIM-signer blast radius: the nest signs; the MTA holds no key.** The
per-domain DKIM signing key is the nest's own, sealed under its key-encryption
key, and the nest signs each outbound message as it hands it to the MTA
(`../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic) owns the
custody decision; the MTA-side signing this section accepted for alpha was
superseded 2026-10-03 and its sealed-blob path removed 2026-10-04). A stolen MTA
key therefore signs nothing on its own, and signing costs no extra round-trip:
the hand-out the MTA already needs is the sign site. The residual is that a
compromised MTA can still have a message signed by enqueueing it, until the
nest-side From-ownership check at `fauna.bridges.enqueue_outbound_mail` lands
(same owner, *What the move does not yet buy*).

**TLS-cert blast radius: shared apex cert accepted,
documented.** Every TLS-fetching sidecar — the MTA, the MDA, and now the iroh
relay — is sealed the **full multi-SAN apex cert + private key**
(`Storage::seal_current_tls_cert_for_{bridge,x25519}`), not a cert scoped to the
one name it serves (`mail.<primary>` / `relay.<apex>`). So an RCE in any one of
them puts the whole-deployment TLS key in that process's memory → full-domain
impersonation, not just its own subdomain. **Posture for alpha: keep the shared
apex cert.** Dedicating per-service certs (least-privilege) would need a
**per-name/per-role ACME-order subsystem that does not exist today** (the
deployment issues one all-or-nothing multi-SAN order — `tls-certificates.md`
§ HTTP-01), a cost disproportionate to a **mitigated** residual: the
distribution is already hardened (each sidecar opens an HPKE blob sealed to its
**own** attested x25519; `/data/acme` is UID-isolated + Landlock-kernel-denied;
the relay never persists the key). The mitigations bound the *probability* of
obtaining the cert, not the *consequence* once a process is popped — so this is an
explicit accepted residual, consistent with the DKIM-signer posture above. The
**relay** is the first candidate to dedicate (broadest, internet-facing surface);
the cheap moment is when a per-name ACME-order capability lands for another reason
(custom-domain issuance over the existing per-SNI `MultiDomainCertResolver`).
(Full analysis + revisit triggers + the refutation path ratified 2026-06-30;
tracked internally — refutable; the user or a future session may elect
dedicated certs now.)

### Enrollment proof-of-possession contract (slice 2)

The single source of the slice-2 cross-language contract — the bytes are
assembled by `fauna_protocol::wrapped_blob::enrollment_signed_message`, used by
nest's verifier and the Go bridge's signer so they cannot drift (mirrors
`auth::handshake_signed_message`). Three artifact-coupled halves that must ship in
**one image** (no version skew — same discipline as slice 3):

- **Blessed registry (artifact mints + writes; nest reads).** The deployment
  artifact (entrypoint, as root) **mints** each role's keypair once
  (mint-if-absent — the key must be stable across reboots, like slice 1's keyfile)
  and writes: the **private** half to `/data/keys/{mta,mda}/{mta,mda}.key` (the
  slice-1 path/owner/mode — `0600`, bridge-UID-owned) and the **public** half into
  a nest-readable, **bridge-UID-unwritable** registry. nest reads the per-role
  blessed pubkey **live from the registry file on every enrollment** — the nest
  run-script exports the registry *dir* as the IPC env `FAUNA_BLESSED_KEYS_DIR`
  and nest reads `<dir>/<role>.pub` fresh per `request_enrollment`
  (`blessed_pubkey_from_dir`); with the dir unset (a binary-only nest)
  enrollment stays lenient. The value is *public*
  (integrity, not confidentiality, is the property: a bridge UID must not be
  able to *substitute* a pubkey, which the root-owned file + root-owned 0755
  dir ensure — nest only reads). The live read exists for **service-user
  re-keying** (`mail-bridge-lifecycle.md` § Service-user re-keying): after an
  admin revoke the bridge archives + regenerates its keypair, and the role's s6
  run-script (root, pre-drop) re-derives the blessed pubkey from the keyfile at
  the UID-isolated path (`docker/rebless-bridge-key.sh`) — the running nest
  must see the re-bless without a restart. This keeps the trust anchor "the key
  at the root-verified, UID-isolated `/data/keys/<role>/<role>.key`": a
  co-resident non-role UID cannot write that 0700 subdir, so it still cannot
  get a rogue key blessed; the role UID already holds the role's private key,
  so a re-derive grants it no capability it lacks. The artifact owns the
  on-disk registry file path/format; nest's contract is only the env vars. The
  bridge is **load-only** on the image path (it self-generates only at the
  § Service-user re-keying step-7 regeneration, inside its own key subdir).
- **Signed message (the bytes both sides assemble).** Domain tag
  `b"fauna.bridges.enroll.v1"`, then `ed25519_pubkey (32) ‖ x25519_pubkey (32) ‖
  role` (`role` ∈ `"mta"`/`"mda"`, the canonical strings). A **static,
  context-bound** signature — no nonce, no second round: replay of `(pubkey, sig)`
  only re-asserts the same blessed identity, and the private half stays
  UID-isolated, so replay is harmless; the domain tag + role bind it to this
  purpose. The Go bridge signs this with its keyfile Ed25519 seed
  (`keyfile.SigningKey()` → `ed25519.Sign`); nest verifies with the presented
  pubkey after checking it equals the blessed key for the role.
- **Wire (additive, forward-compatible).** `RequestEnrollmentRequest` carries two
  optional fields — `x25519_pubkey` (bstr 32) and `enrollment_sig` (bstr 64,
  Ed25519). Both **absent** on a legacy/transition bridge (lenient path);
  **required** when nest has a blessed registry for the role. Verification:
  presented Ed25519 == blessed key, signature valid over the message above; on
  success nest binds the x25519 set-once at enrollment. Reject (`permission_denied`)
  on any miss.

### Watchtower (auto-update) trust — accepted posture

The auto-update sidecar (`watchtower`) mounts `/var/run/docker.sock`, which is
**host-root-equivalent**: it is a fully-trusted component of the deployment image
set, in the same trust class as the base images themselves, and **no network
placement changes that** (the socket, not the network, is its privilege). Its
`POST /v1/update` instant-update API — deployment IPC, a redeploy hook for the
nest/an SSH tunnel, *not* a user/admin feature — is therefore the only inbound
attack surface, and it is **OFF by default** (`WATCHTOWER_HTTP_API_UPDATE=false`
in `docker-compose.yml`, published only on host-loopback `127.0.0.1:8080`); when
disabled nothing listens and a connection from a co-resident container (a
compromised clamd/rspamd/nest) is refused. **Accepted posture:** keep
the update API off in production; if a deployment ever enables it, it MUST set a
high-entropy `WATCHTOWER_HTTP_TOKEN`. We do **not** put watchtower on its own
compose network — segmentation is defense-in-depth only for the already-off API
path while leaving the load-bearing socket privilege untouched, so it adds compose
complexity without shrinking the trust. (Refutable by the deployer: if the
update API is made a standing on-by-default path, revisit segmentation.)

## On-screen secret exposure (screen capture)

**Ratified 2026-08-15.** Several app screens display a secret in plaintext *by
design*: the mail-credential reveal (`mail-settings-credential-item-secret` — and,
on an app whose Connected apps roster lists the mail app passwords, the same reveal
under its roster id `connected-apps-item-secret`; rule 2 follows the reveal to
whichever page paints it), the
Bluesky app-credential reveal (`atproto-app-credential-reveal`), the one-time
Nostr bunker connect string (`nostr-bunker-connect-string`), the identity secret
(`secret-key-display`) and the recovery kit (`recovery-kit-secret-display`). Until
this section, no app suppressed screen capture on any of them — `FLAG_SECURE`,
`NSWindow.sharingType`, `SetWindowDisplayAffinity` and every equivalent were absent
tree-wide. The gap was **uniform across all 7 apps**, which is exactly why parity
work never surfaced it: a gap where every app matches every other app produces no
divergence signal.

**The posture, in one sentence: capture suppression is defense-in-depth applied to
*minted, revocable* credentials only — never to the user's own root secrets, and
never a control this security model relies on.**

Three rules follow, and they are ordered — rule 1 wins where they collide.

1. **Never suppress capture on a root-secret surface.** `secret-key-display` and
   `recovery-kit-secret-display` show key material that is **client-only-resident**:
   a user who loses it loses the account, with no recovery path (`principles.md`
   § No user-data loss). Users legitimately screenshot a recovery kit because it is
   the copy that saves them. Blocking that trades a shoulder-surfing risk for an
   account-loss risk, and account loss is the irreversible one. These screens stay
   capturable on every app, deliberately.
2. **Suppress capture while — and only while — a minted credential is actually
   revealed.** Mail credentials, Bluesky app credentials and the bunker connect
   string are *minted capability credentials*: revocable, re-mintable, and never the
   only copy of anything (`atproto-pds-full.md` § F1 detail;
   `mail-credentials.md`). Losing one costs a revoke-and-re-add, so suppressing
   capture costs the user nothing they cannot redo. Scope it to the **reveal
   window**, not the page: capture suppression is a *per-window* property on every
   platform that has it, so "on while revealed, off on hide or navigate-away" is the
   whole contract — and getting the clear-path wrong silently leaves the entire app
   unscreenshottable, which users experience as a broken phone, not as security.
3. **Where the platform has no per-window API, this is a declared absence, not a
   bug.** linux (GTK — neither X11 nor the Wayland screencopy protocols expose a
   per-window opt-out to an application), web (no browser API exists) and tui (the
   terminal emulator and its scrollback own the pixels) cannot implement rule 2.
   They implement the parts they can: hidden-by-default, explicit reveal, and a
   reveal that ends. **Do not simulate suppression** with a blur, an overlay or a
   screenshot-detector on these platforms — a control that looks like protection and
   is not is worse than its stated absence.

**Consequently, capture suppression is never load-bearing.** Three of seven apps
can offer it, so no part of the threat model may assume it: a secret revealed on
screen is treated as disclosed to anything with display access on that device. What
*is* uniform, and what the design actually leans on, is **bounded exposure** —
secrets are hidden by default, fetched on demand rather than carried in the passive
snapshot (`mail-settings-credential-item-secret`'s own contract), and revealed only
by an explicit act the user can reverse.

⚠ **One platform-API correction worth not re-deriving.** SwiftUI's
`.privacySensitive()` does **not** suppress screenshots or recording — it drives
redaction placeholders for widgets and Always-On Display only, and an apple leg
that reaches for it will ship a control that does nothing. The per-platform
mechanisms that actually work are: android `WindowManager.LayoutParams.FLAG_SECURE`
(also removes the recents-screen thumbnail — the concrete harm this finding named),
windows `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`, macOS
`NSWindow.sharingType = .none`. iOS has no per-window equivalent; its honest
options are `UITextField.isSecureTextEntry` for a field's own content, or observing
`UIScreen.isCaptured` and blanking the revealed value while a recording is live —
an apple leg should state which it took.

## Implementation status today

- **Hosted third-party code's sandbox profiles + nest-terminated ingress (§ Co-resident process trust boundary, ratified 2026-09-05): the WASM sandbox BUILT 2026-10-02 and the nest-side runner 2026-10-03, the rest unbuilt.** `libs/fauna-plugin-host` links exactly the capability imports the paragraph names (named outbound hosts refused before any dial, the nest API through the principal chokepoint, a state scope, a clock) under the labeler's fuel and memory caps, and the ingress record carries the nest-asserted actor as a field the plugin cannot forge; no plugin ever sees a certificate key because the host crate has no import that could carry one. The nest-side runner (`bins/fauna-nest/src/plugin_runner.rs`) starts each installed plugin under those caps and dials its outbound fetches through the nest's guarded fetcher (both SSRF seats over one parse, pinned addresses, no redirect) after the host policy admits the URL. Not built: the TLS-terminating proxy that feeds `ingress.handle`, and the container form's UID/Landlock profile; sequencing in `third-party.md` § Implementation status today.

The app-layer security path (§ Signed-envelope shape onward) is **shipping** —
`libs/fauna-cbor/src/envelope.rs` implements `SignedEnvelope::sign` /
`verify_permissive` over the 36-byte CID (the raw two-step primitive; every
production caller routes through `fauna_core::encoding::verify_envelope` →
`fauna_core::identity::verify_detached`, the strict-plus-weak-key-refusal
primitive — § Key management invariants), and the pre-parse validator
(`libs/fauna-cbor/src/canonical.rs`) gates every `decode_strict` call; per-area
embed-as-bytes adoption tracks separately under the CBOR-DAG-everywhere plan.
The transport trust model (§ Transport trust) is now **implemented for the
WS-RPC client↔nest leg** (and, from 2026-08-02 until its removal, the sync `/sync/ws` data
plane: `bins/fauna-sync` adopted the shared trusted dial + a writable
per-install pin store; before that the headless daemon dialed plain strict
WebPKI and could not reach a self-signed-floor nest at all; the daemon was
removed 2026-10-02, and the route with it) — both axes
(including the DNS `self=` root for self-signed *public* domains), the
cross-connection pin, and the **complete removal of `FAUNA_INSECURE_TLS`**:
not just the WS paths but the residual HTTP content API + `POST /register`
reqwest leg now share the WS handshake's pinned SPKI (below). The disk-backed pin store is now installed at startup on every
app (linux / windows / android / macOS / iOS all now build-verified — the
mac binding regen landed and iOS is additionally live-run-verified against a
real self-signed dev nest, see below).

- **Graduation now covers BOTH legs.** The *bearer-carrying* leg
  (`fauna_anon_client::graduate_handshake` on `fauna-launch-machine` +
  `fauna-client::ws_challenge_bearer`, via `fauna.auth.handshake`) and — since
  2026-07-07 (Track 2, Option B ratified + built) — the **pre-identity
  onboarding leg**: `fauna-onboarding-machine`'s `WsNestApi::core` runs
  `AnonymousNestClient::graduate_first_contact` (the `fauna.auth.nest_handshake`
  mechanism, § Two independent axes above) on every fresh pre-claim connection,
  graduating against the injected-seed root on the client-provisioned path and
  the DNS-`self=`/TOFU ladder otherwise. Since 2026-07-29 the same held-root
  seam has a **third feeder** — the self-hosted pre-claim row above: pasting
  the console's `fauna://claim` URI holds the printed identity via
  `hold_first_contact_identity` before the claim call opens its connection
  (machine-level, so all apps get it through their existing claim input; the
  graduation is native — web cannot reach a self-signed nest anyway). Guarded
  by the tier_3 `tls_channel_binding_roundtrip.rs` (`nest_handshake_*` +
  `graduate_first_contact_*` tests: no-actor answer, injected-root exact match,
  wrong-root + substituted-SPKI hard-fail, and — since the compat-remnant sweep
  retired the legacy-nest fallback 2026-09-24 — a kind rejection hard-failing
  on every host) and the
  machine/codec units (`claim_code::` URI round-trip + loud malformed-pin
  refusal; `machine::` pin-before-send, bare-code-no-pin). **Live-validated
  2026-07-07** on a real Hetzner box (`just e2e-live-provision`, production image): the wizard's bridge-driven claim rode the graduated first-contact
  path against the injected-seed root — self-signed pre-ACME cert +
  handler-answering nest make exact-match graduation the only green path — with
  every downstream gate (storage-mode, mail-enable, ACME TLS, firewall,
  25/587/465/993 banners, DKIM) green.

The macOS/iOS binding-regen verification (below) has since landed too.

- **Implemented (nest half):** the nest self-signs (`self_signed_cert.rs`) and
  holds a stable `nest_signing_key`; DNS publishes `self=<actor_id>` for public
  domains (`resolve.rs`); the WS-RPC auth challenge proves the *client* to the
  nest (`auth_core.rs`). The nest→client channel-binding leg signs the SPKI of
  the cert *it itself serves* (read live via `acme::ServedCertSpki`, which reads
  the resolver's **default/apex** cert — the cert a Fauna *client* connects with;
  the serve path installs `MultiDomainCertResolver`, whose per-SNI custom-domain
  certs serve *web visitors*, not the channel binding —
  [`nest/tls-certificates.md`](nest/tls-certificates.md) § A) over
  `spki_sha256 ‖ client_nonce` with `nest_signing_key`
  (`auth_handlers::build_cert_binding`).
- **Implemented (client half — Axis 1 + Axis 2 + cross-connection):**
  - A **capturing TLS verifier** (`fauna_anon_client::tls_verify`) provisionally
    accepts the cert (encrypt-only), records its SPKI + WebPKI validity, and
    (for a bearer connection) hard-fails unless the served SPKI matches a pin.
  - The bearer-minting silent challenge on **both** native bearer paths
    (`fauna_client::ws_challenge_bearer::WsChallengeBearer` for the UniFFI
    apps — and `bins/fauna-sync` until its removal 2026-10-02 — and
    `fauna-launch-machine` for linux + tui)
    sends `client_nonce`, then `fauna_anon_client::trust::graduate_verify_path`
    verifies the binding against the *received* SPKI, checks the identity against
    its Axis-2 root (`cert_binding::{verify_cert_binding,check_identity_root}`),
    and pins the bound SPKI. WebPKI-valid certs take the boring path (no binding
    required) **only when no Axis-2 root is held and no binding was offered**
    (the second condition since 2026-10-05 — § Transport trust → *The login's
    pin*: the login paths always have one in hand, so on them the full core
    runs and the identity it verifies is recorded as the pin, pre-resolved
    roots included); non-WebPKI (self-signed/LAN)
    certs require the binding, and so does *any* cert once a root is held
    (tightened 2026-09-02 — WebPKI answers "is this the
    address I dialed", the root answers "is this the nest I provisioned", so the
    stronger authenticator is always used when the caller has one; before this,
    the WebPKI arm returned before the root was read, which was safe only while a
    domainless box served an untrusted floor — the premise
    [`nest/tls-certificates.md`](nest/tls-certificates.md) § B-IP deletes).
    **"Held" covers both a root the caller pre-resolved and an identity pin the
    install-scoped store already names for the host** — the claim-seeded pin on
    a client-provisioned box (`seed_claimed_identity_pin`), or an ordinary TOFU
    pin — so the posture outlives first contact on the bearer-mint and launch
    paths, which never see the seed; `trust::webpki_waives_binding` is the one
    decision point all four graduation entrypoints share (ruled 2026-09-02, after the bridged-box pin was found to last exactly
    one connection: the wrappers' unconditional WebPKI arm cleared it on the
    very next bearer mint). **`graduate_first_contact`'s rejection arm hard-fails
    on every host** (since 2026-09-24; the intermediate `trust::rejection_is_hostile`
    predicate, ruled 2026-09-02, had first made that
    arm read the waiver's own "held" — it previously hard-failed on an explicit
    root alone, so one function carried two definitions of "a root is held"
    twenty lines apart and the box on the wire chose which ran — and the
    compat-remnant sweep then removed the pre-Track-2 fallback the predicate's
    other arm still allowed, § *Compat* above). The
    Axis-2 root is resolved by `trust::resolve_dns_self_root` (the only async step,
    delegating to the sync `graduate_handshake_with_root` core): a registrable
    **public** domain that publishes `_fauna.{host}` TXT `self=` gets the strong
    DNS root (exact identity match, no pin — `lookup_fauna_txt` +
    `probe::public_dns_host`, which shares `resolve_handle_domain`'s `.local`/IP
    classification); LAN/`.local`/IP hosts and public domains that publish no
    `self=` fall to TOFU-on-host.
  - **The login's pin (ruled and built 2026-10-05).**
    The two login wrappers (`graduate_handshake`, `graduate_verify_path`) never
    waive a binding they hold, and record the identity the core verified
    (`trust::Graduated::Verified` → `trust::record_login_identity`) as the
    host's pin, pre-resolved roots included; `graduate_first_contact` and the
    never-minting fallback are unchanged. Unit witnesses in `trust::tests`
    (named at the ruling). **Live verification against a public-CA box is
    PENDING**: a tui run of `test_backups.py::test_backup_destination_crud`
    and `test_archive_import.py` against the staging box with no e2e trust
    seed set is queued behind the running full tui live sweep — until it lands, the ruling is
    unit-proven and live-unproven.
  - **The TOFU-mint decision is atomic** (`NestIdentityPinStore::pin_if_absent`,
    fixed 2026-08-17): `check_identity_root`'s `Tofu` arm used to check-then-act
    (`get()` a pin, decide, then a separate `set()`) — a genuine race between two
    concurrent graduations of the *same* host (a client's synchronous
    login-time bearer mint racing its own fire-and-forget background silent
    challenge, both TOFU-checking the same host — tui's
    `session::spawn_domain_refresh` is one such caller). Under load, one
    graduation's stale, still-in-flight mint could land **after** a second,
    newer pin decision and silently clobber it back with no warning —
    `test_nest_identity_pin_post_auth.py` caught it intermittently. Every
    `NestIdentityPinStore` backend a multi-threaded native runtime can call
    concurrently (`MemoryPinStore`, `DiskPinStore`) now performs the whole
    check-and-mint under one lock acquisition; a concurrency unit test
    (`nest_trust::tests::pin_if_absent_is_atomic_under_concurrent_first_contact`)
    races 32 threads against the same host and pins the winner-agreement
    property.
  - **Every client decision keyed on "which nest is this" reads the identity
    the connection is bound to, never the nest's own `fauna.nest.info` claim.**
    `LinkedNestsNest::bound_nest_id` (the origin's pin, else a possession proof
    over the connection) surfaces as `LinkedNestsMachine::bound_nest_id()`,
    which also **refuses** a nest whose claim disagrees with it — a
    disagreement is hostile or broken, and a silent substitution would hide it.
    The blessing decisions moved first; the
    deployment-seed custody comparand (BR-2), the seed-map fan-out's owned-box
    guard, the co-admin self-heal, the predecessor entry a rotation supersedes,
    both ids a both-ends link writes and the DNS cert `target_nest_id` followed
    on 2026-09-27 — on all 7 apps
    through that one resolver (linux/tui `resolve_this_nest_id`, the `fauna-ffi`
    and wasm twins). Before it, a box the user was a mere user of could answer
    `nest.info` with an owned sibling's id and receive the admin's whole sealed
    custody map past the fan-out's blast-radius guard, or have that sibling's
    custody entry marked superseded by a rotation it ran. Per-consumer
    mechanics: [`nest/box-recovery.md`](nest/box-recovery.md) § Implementation
    status today (the BR-2 bullet).
  - **Cross-connection propagation:** `ws_adapter` requires the pinned SPKI on
    the authenticated WS that carries the bearer; absent a pin it falls to strict
    WebPKI. The **residual HTTP content API + `POST /register` reqwest leg** shares
    the same pin: its reqwest client (`fauna-client`'s `AuthClient::new`, the linux
    `build_http_client`, the windows hydration host) is built with
    `trust::store_pinned_reqwest_tls(nest_url)`, whose verifier reads
    `pinned_spki(authority)` **per-handshake** — so it pins the moment the WS
    handshake graduates one (and re-reads it across a cert rotation), refusing a
    non-WebPKI cert with no pin rather than accepting any. `FAUNA_INSECURE_TLS` no
    longer touches **any** path (WS or reqwest); the env var is gone.
  - **Windows `DirectNestClient` — the *second* residual-HTTP leg (C#), pinned
    via shared-Rust FFI**
    (`apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/DirectNestClient.cs`).
    The "windows hydration host" above is the Rust `fauna-sync-agent` (its reqwest
    client *is* `store_pinned_reqwest_tls`-built); but the desktop app's own health /
    blob / snapshot calls go through a .NET `HttpClient`, which the Rust rustls pin
    store cannot govern. That leg shares the same pinned identity via a
    `ServerCertificateCustomValidationCallback` that accepts a served cert iff it is
    WebPKI-valid, OR the host is loopback (same-box install — the sanctioned
    `danger_accept_invalid_certs` prior art, also covering the unauthenticated health
    check that can precede the pin-graduating handshake), OR its SPKI matches the pin.
    The loopback and pin carve-outs are the *configured nest's*, so the callback grants them
    only to a request for that nest's own host and port (`NestCertTrust.IsForNest`, host
    compared case- and bracket-blind, port defaulted per scheme): `HttpClientHandler`
    follows a 30x by default and calls back for the redirected request, and a redirect
    anywhere else — or a request naming no host — falls to strict WebPKI alone, so a
    loopback nest's redirect target is never accepted unverified. The pin key stays the
    port-as-written authority Rust pinned; the guard compares host and port numerically and
    never re-derives it. The Apple leg below grants the same.
    The SPKI computation, pin lookup, and loopback classification are the *same shared
    Rust* the handshake uses, exported as `fauna_ffi::{spki_sha256_of_cert_der,
    pinned_spki_for_host, authority_of, is_loopback_authority}` (gated `nest-trust`), so
    the C# value is byte-identical to the pin — no .NET re-encoding divergence — and the
    `host[:port]` loopback test (including the IPv6-bracket parse) is one tested impl
    rather than a hand-rolled per-app parser. This is what lets a fresh same-box app reach its installed
    nest at `https://127.0.0.1:443` and a remote `test@<ip>` self-signed nest serve it
    media, with no public CA.
    - **The authority is the host a URL parser would dial — userinfo is never part
      of it.** `authority_of` drops any `userinfo@` (`fauna_core::web::strip_userinfo`,
      splitting on the *last* `@` per WHATWG) before the string becomes either the pin key
      or the loopback classifier's input, and `is_loopback_authority` strips again on its
      own input because it is a UniFFI export any app's cert callback can reach. Both
      matter: the *same-box* carve-out above must be granted only when the connection
      really goes to the local box (granting it to a remote nest would disable WebPKI
      **and** the pin at once), and a TOFU pin must be keyed on the host actually dialed,
      or that host escapes the identity-change warning. Malformed
      authorities stay non-loopback in **both** the bracketed and unbracketed camps
      (`[::1]:junk` no more loopback than `localhost:junk`); an unparseable port suffix is
      never discarded to yield a clean literal. Landed 2026-08-01 with the
      `is_loopback_authority` fix; the same `strip_userinfo` backs
      `fauna_onboarding_machine::helpers::nest_host` so the two host extractors that read a
      nest URL cannot disagree about which side of an `@` is the host.
    - **Every nest-URL host extractor splits the authority the same way** —
      `strip_userinfo` then `split_host_port`, keeping IPv6 brackets and stripping them only
      where an `IpAddr` parse follows (`fauna_core::resolve::strip_ipv6_brackets`). The last
      hand-rolled splitter, `fauna_core::resolve::parse_node_address`, joined the family
      2026-08-01, replacing a private splitting rule of its own that could misclassify a
      host and so let it through `is_public_dns_name`. That parser is load-bearing in three
      user-visible places — the SRV **reconnect** self-heal, `fauna_client_dns`'s dial
      classification, and the IMAP/SMTP/CalDAV endpoints shown for a user to paste into
      their MUA — so all three now read a nest URL's authority through the one shared
      splitter.
      - **The authority itself ends at the first of four bytes — `/` `\` `?`
        `#` — never just `/`, or `/` and `\`.** A raw
        `?`/`#` in a nest URL is not a metacharacter these extractors used to
        stop at, so a phished URL like `https://nest.example.com?@[::1]`
        read the query's `[::1]` as the host while every WHATWG parser — the
        real dialer included — resolves `nest.example.com`. This is the
        *third* variant of the same drift, after userinfo and `\` alone: each fix landed in
        one extractor and left the others narrower. The terminator set now
        lives in exactly one place, `fauna_core::web::authority_len`, behind
        two public callers: `authority_of` (its own fixed
        `http`/`https`/`ws`/`wss` scheme set, for the TLS-trust pin key and
        loopback classifier above) and `fauna_core::web::generic_authority`
        (any `scheme://`, for the display/resolve extractors —
        `fauna_core::format::url_host_opt`,
        `fauna_onboarding_machine::helpers::nest_host`,
        `parse_node_address` below, `fauna_core::data::url_host` (the
        peer-anchor nest-URL extractor),
        `fauna_launch_machine::auth::hint_authority_url` (the wasm reach-hint
        dial, joined 2026-09-14, which also composes
        `split_host_port` to keep the URL's own port when substituting the
        hint IP), and `fauna_provisioning::probe::split_host_port` (joined
        2026-09-16, cutting at the terminator before
        `strip_userinfo` — below) — which never restricted the scheme the way
        `authority_of` does).
      `fauna_provisioning::probe::split_host_port` joined the family
      2026-09-03: it classified `split_host_port(domain)` without
      stripping userinfo first, so `resolve_handle_domain` read one authority (the
      unstripped host) but composed its `base_url` from a *different* one (the whole
      `domain` string) — a userinfo-carrying domain like
      `nest.example.com@attacker.example` classified by the wrong half of the `@` and
      then composed the URL from the un-stripped whole. Both are fixed: the classifier
      strips userinfo first, and the public-domain branch composes `base_url` from the
      same `host`/`port` it just classified, never from the raw input. The one
      caller-reachable through this path — the Bluesky OAuth `client_id` derivation
      (`bins/fauna-nest/src/bluesky/mod.rs::oauth_public_url`) plus the claim-time and
      admin add-domain identity doors — additionally reject a domain that fails
      `fauna_core::web::is_hostname_syntax` (userinfo, a path, a query, a fragment, or
      whitespace) before it ever reaches the classifier, so a malformed candidate is
      refused rather than silently normalized.
      **Fixed 2026-09-16, filed:**
      `probe::split_host_port` used to read to the end of the
      WHOLE string handed to it, not to the end of an authority the way a URL
      parser ends one — so a string carrying a URL metacharacter *past* a
      `.local`/loopback-looking suffix (`x.example#.local`) misclassified as
      non-public while `base_url` composed an authority a URL parser would
      resolve differently, the same terminator-set gap a sibling finding fixes
      for `authority_of`. Fixed by joining the `generic_authority`
      family above rather than special-casing `.local` inside the classifiers:
      `split_host_port` now cuts at the first authority terminator *before*
      `strip_userinfo` — not after, since a terminator preceding the last `@`
      would otherwise be read as part of the userinfo half — so classify and
      compose both name the authority a URL parser would actually dial
      (`probe.rs::a_terminator_suffixed_local_string_names_one_authority`).
      `fauna_core::resolve::is_public_dns_name` and `is_private_network_target`
      are unchanged; the terminator set still lives solely in
      `fauna_core::web::authority_len`. Not reachable before the fix, same as
      stated at filing: every identity-writing door gates on `is_hostname_syntax`
      first, and the un-gated federation/peer-domain resolvers
      (`ConversationsClient::actor_by_handle_remote`,
      `AnonAttendeeDiscovery::resolve_actor`) consult only `base_url`, never this
      bool — the real TLS trust decision is keyed off the dialed connection's own
      parsed authority, not this classifier.
    **Live-verified end-to-end on BOTH branches — no residue.** Until this work no automated
    test opened a socket on this path at all; the unit suites (`NestCertTrustTests`,
    `NestCertTrustFfiTests`, and `DirectNestClientCertCallbackTests` — the callback driven
    with a hand-built request for another host) only pin the decision function. Two tier_3 suites now drive a real
    FaunaApp against a nest serving its self-signed floor over real HTTPS, and prove the same
    C# legs land on each branch: **health** (`IsAvailableAsync`) plus **blob upload *and*
    download** (media).
    - **Loopback branch (2026-07-11)** — `test_self_signed_nest_client_legs.py`, via the
      `self_signed_nest` fixture (the one e2e nest that drops the process-wide plain-HTTP
      escape). This is the same-box install: `https://127.0.0.1:443`.
    - **SPKI-pin branch (2026-07-12)** — `test_spki_pinned_nest_client_legs.py`, via
      `spki_pinned_nest`: the same self-signed floor, dialled on the box's own **LAN IP**. The
      authority is therefore non-loopback (`is_loopback_authority` classifies IP literals and
      never resolves DNS) and the cert is not WebPKI-valid for it — a negative-control test
      asserts a *verifying* TLS client is rejected — so `spki == pin` is the **only** term of
      the decision that can accept it. A green is a proof of the pin branch **by elimination**.
      This is the remote `test@<ip>` case, and it also proves the bearer mint graduates the pin
      *before* the unauthenticated health check runs (on loopback that ordering is masked by
      the loopback term, which exists partly to cover a pre-pin health check).
  - **Apple `APIClient` — the *third* residual-HTTP leg (Swift `URLSession`), the same
    policy through the same shared-Rust FFI (2026-09-24)**
    (`apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/NestCertTrust.swift`). The macOS and
    iOS apps' blob upload / download / `HEAD`, chunk and manifest calls go through one
    `URLSession` per `APIClient`; it used to be `URLSession.shared`, which makes **no** trust
    decision — the OS trust store refused the self-signed floor cert
    (`NSURLErrorServerCertificateUntrusted`, -1202) and media failed against a same-box nest
    while every Rust leg accepted it. The session now carries a `URLSessionDelegate` whose
    server-trust answer is windows' `NestCertTrust.ShouldTrust` member for member — accept
    iff `SecTrustEvaluateWithError` passes (WebPKI), OR the nest authority is loopback, OR
    the served leaf's SPKI equals the pin — with the SPKI, pin lookup and loopback
    classification read from the same four `fauna_ffi::trust` exports the C# callback uses.
    The carve-outs are granted only to a challenge for the configured nest's own host and
    port (`challengeIsForNest`, the twin of the C# leg's `NestCertTrust.IsForNest`, pinned
    by the same eight rows on both sides — a redirect anywhere else falls to the system
    default, strict WebPKI). The one refinement over the C# leg, also fail-closed: a refusal
    answers `.performDefaultHandling` rather than cancelling — the system re-evaluates the
    same trust it just failed, so the request still dies, but as -1202 rather than a
    `.cancelled` the UI would misread as the user's own cancellation. Pinned by `NestCertTrustTests.swift` (the pure decision, the
    real FFI classifiers, and the delegate's verdict over a real self-signed certificate:
    accepted on loopback, refused off loopback with no pin) and live by
    `test_self_signed_nest_client_legs.py` on `--app macos` and `--app ios`.
  - **Android `ApiClient` — the *fourth* residual-HTTP leg (Kotlin OkHttp), the same
    policy through the same shared-Rust FFI (2026-09-26)**
    (`apps/fauna-android/app/src/main/java/com/fauna/app/core/NestCertTrust.kt`). The
    android app's blob upload / download / `HEAD` calls go through one OkHttp client; it
    used to be the plain injected one, which makes **no** trust decision beyond the OS
    trust store and so refused the self-signed floor cert (`SSLHandshakeException`).
    `ApiClient` now derives its client with `NestCertTrust.makeClient`, whose verdict is
    windows' `NestCertTrust.ShouldTrust` member for member over the same four
    `fauna_ffi::trust` exports, the carve-outs granted only to a peer at the configured
    nest's own host and port (`NestCertTrust.isForNest`, the same eight rows as the C# and
    Swift legs). JSSE splits what the C# callback and the Swift delegate answer at once, so
    the one verdict runs on both halves: an `X509ExtendedTrustManager` answers for the
    chain (its socket and engine variants name the peer; the two-argument variant names
    none and gets strict WebPKI alone) and a `HostnameVerifier` answers for the name — a
    connection stands iff the chain AND the name are valid, OR the peer is the nest and a
    carve-out holds. The nest URL is read at handshake time (`ApiClient` is one instance
    across sign-ins, and signed out it names no nest, so no carve-out exists), a refusal
    rethrows the platform's own exception, and the injected base client
    (`AppModule.provideOkHttpClient`) stays strict WebPKI for any other consumer. Android
    carries no Kotlin WebSocket: the bearer-carrying dial is shared Rust's alone. Pinned
    by `NestCertTrustTest.kt` (the pure decision, the real FFI classifiers, the trust
    manager over a real self-signed certificate — accepted on loopback, refused off
    loopback with no pin, accepted on a matching pin and refused on a disagreeing one —
    and what a peer that is not the nest is granted). **Not yet live-verified:**
    `test_self_signed_nest_client_legs.py` carries `pytest.mark.android`, but no android
    e2e run exists until the run venue's harness lands (`testing.md` § Default app and
    nest mode → *Android's run venue*).
  - A disk-backed identity pin store (`cert_binding::DiskPinStore`) survives
    restarts; the in-memory `MemoryPinStore` is the default until an app
    installs the disk store (`trust::install_pin_store`). **Linux installs it**
    at startup (`apps/fauna-linux/src/client.rs::install_disk_pin_store`, called
    from `main.rs`'s `connect_startup` before the first authenticated connect)
    rooted at `~/.config/fauna/nest_identity_pins.json` — the XDG config dir its
    MLS / P2P / window-state stores already use, *not* the nest sidecar's
    `service_watcher::resolve_data_dir()` (which is `None` for a pure client, so
    pins would never persist on a laptop). `fauna-client` re-exports
    `fauna_anon_client::trust` so every app installs it the same way.
  - **Apple pin custody is install-scoped (closed 2026-07-22 — was the "appex
    installs an EMPTY pin store" KNOWN GAP, measured on an iOS simulator as
    `ws connect failed: invalid peer certificate: UnknownIssuer` retrying
    forever).** Per § Pin custody across processes: the app's
    `NestTrust.installPinStore()` roots the store at the app-group
    `trust/` dir on iOS, and — **since 2026-08-25, on macOS** — at the
    user-domain `~/Library/Application Support/Fauna/trust` taken from the
    `install_scoped_trust_home` FFI export (the one-time adoption of the
    per-process file and the 2026-07-22..2026-08-25 container primary was
    retired 2026-09-24 by the compat-remnant sweep, § Dimension 2 program 4 of
    [`version-compatibility.md`](version-compatibility.md))
    with the container's `trust/` maintained as the extension's read replica
    (`install_nest_identity_pin_store_with_mirror`; tier_1-pinned in
    `fauna-anon-client`'s `cert_binding` tests); the extension installs the
    container dir **read-only** on both OSes
    (`NestTrust.installPinStoreReadOnly()` →
    `install_nest_identity_pin_store_read_only`) — it consumes the pin the
    user's onboarding accept minted and can never TOFU-mint on its own. E2E
    launches keep the legacy per-process path (the app-group container is
    machine-global state a test launch must never touch — e2e-launch-isolation.md
    § conventions point 10, same branch as `FaunaClient.syncStateDir`).
    The pin store alone was necessary but not sufficient: the appex mints no
    bearer in-process, so nothing graduated a bound SPKI and its dial failed
    strict WebPKI regardless — closed by the `ws_adapter` graduate-and-retry
    fallback (§ Pin custody across processes, *the consumer's dial needs its
    own graduation step*). Consumer-side strictness + live pickup proven by
    `libs/fauna-anon-client/tests/read_only_pin_store_consumer.rs`;
    the full consumer dial (refuse-unpinned → connect-once-the-app-pins, over a
    real self-signed-TLS nest) by tier_3
    `bins/fauna-nest/tests/tls_pin_consumer_fallback.rs`; the fallback's
    graduation never minting in a *writable*-store process either
    (`PinMinting::Never`, 2026-09-28) by its twin
    `tls_bearer_dial_fallback_never_mints.rs`.
  - **The desktop background sync agents are read-only consumers too (closed
    2026-07-22 — was the "installs no pin store at all" gap).**
    `bins/fauna-sync-agent` ran on the writable `MemoryPinStore` default, which
    is wrong twice over: empty at every start (a TOFU-rooted nest was simply
    unreachable from an agent process) and *writable* (it would mint a pin for
    whatever it reached, with no user in the loop). It now installs
    `ReadOnlyDiskPinStore` at startup (`crate::trust::install_consumer_pin_store`,
    called from `service::run_agent_with_store` before the capability restore
    can dial), rooted at the install-scoped trust dir — which per rule 1 is a
    *sibling* of the agent's own root, never under it: `<app-group>/trust` on
    macOS (the same dir `NestTrust.sharedTrustDir()` returns),
    `$XDG_CONFIG_HOME/fauna` on linux and `%LOCALAPPDATA%\Fauna` on windows (the
    dirs those apps already pass to `install_nest_identity_pin_store`). An
    explicit `--data-dir` keeps the store inside that root, so a test launch
    never touches the box's real one (e2e-launch-isolation.md § conventions point 10). One
    call covers all three platforms: the windows `fauna-sync-agent.exe` shim is
    a thin `main()` over the same `run_main`. Proven by
    `bins/fauna-sync-agent/tests/agent_pin_consumer.rs` (reads the app's pin,
    sees a later mint with no relaunch, cannot mint or remove one) plus the
    per-platform dir derivations in `trust::tests`.
  - Proven end-to-end by tier_3 `bins/fauna-nest/tests/tls_channel_binding_roundtrip.rs`
    (genuine self-signed nest connects over `wss://`; substituted cert rejected
    before the bearer).
- **Per-app durability + web hardening record (remaining gap: the macOS/iOS
  binding regen):**
  1. **Per-app durability wiring — wired on every app.** Each disk-backed
     app installs the store at startup so TOFU pins survive restarts.
     - **Linux** (native Rust): `client::install_disk_pin_store` →
       `~/.config/fauna/`. **Verified** — `libs/fauna-anon-client/tests/disk_pin_store_durability.rs`
       (a TOFU pin survives a simulated restart; a changed identity is then
       rejected).
     - **tui** (native Rust): `session::install_disk_pin_store` → the
       install-scoped trust home (`install_scoped_trust_home`, § Pin custody
       across processes rule 1 — never a per-app `fauna-tui/` dir), called once
       from `main.rs` before `launch::start` runs the silent challenge. Same
       shared seam as Linux (`DiskPinStore::open_in_dir` →
       `trust::install_pin_store`, canonical `NEST_IDENTITY_PIN_FILE`), so on
       Linux the two apps read and write the one pin file — the install's, not
       the app's. **Verified
       end-to-end** by tier_3 `tests/e2e-unified/tests/test_nest_identity_pin.py`
       `[tui]` (a seeded pin survives a real force-quit + relaunch over
       `self_signed_nest` TLS and drives `LaunchPhase::IdentityChanged`). Until
       this landed tui alone kept the volatile `MemoryPinStore`, so a nest-identity
       change across a restart — the pin's whole point — was never caught on tui.
     - **UniFFI apps (windows / macOS / iOS / android):** one shared exported
       Rust fn `fauna_ffi::install_nest_identity_pin_store(data_dir)` (which calls
       `DiskPinStore::open_in_dir` → `trust::install_pin_store`), called once at
       each native startup with that app's config dir — macOS/iOS
       `FaunaKit.NestTrust.installPinStore()` (the app-group `trust/` dir since
       2026-07-22, with the File Provider extension as a read-only co-reader —
       § Pin custody across processes; e2e launches keep application-support
       `Fauna/`), Windows `App.OnLaunched` (`%LocalAppData%\Fauna`), Android
       `FaunaApp.onCreate` (`filesDir`). The canonical pin filename
       (`cert_binding::NEST_IDENTITY_PIN_FILE`) lives in one place, so every
       app's on-disk layout is identical. **Windows compile-verified**
       (2026-05-31, on Windows): `just windows-ffi` regen → generated
       `FaunaFfiMethods.InstallNestIdentityPinStore(string dataDir)` → WinUI
       MSBuild builds `FaunaApp.dll` clean against it. **Android compile-verified**
       (2026-06-01, on a Linux dev machine): `just android-ffi` regen → generated
       `fun installNestIdentityPinStore(dataDir: String)` (`fauna_ffi.kt`) →
       `just android-debug` builds the debug APK clean against `FaunaApp.onCreate`'s
       call (NDK 28 — see the internal dev-setup notes § Android NDK on the Linux dev machine; the verify
       runs here, not on the emulator host). **macOS / iOS compile-verified on macOS**
       (`just apple-ffi` regen + Xcode build, both targets — the shared
       `FaunaKit.NestTrust.installPinStore()` builds clean into both the app
       and the File Provider extension, each `rc=0`). **iOS additionally
       live-run-verified**: the pin store install runs during a real
       onboarding flow against a self-signed dev nest (2026-07-20 iOS
       pixel-check session, tracked internally), and — after
       the 2026-07-22 app-group relocation — its pin is read back by the File
       Provider extension read-only (§ Pin custody across processes). Adding
       the export changes the UniFFI contract checksum, so every native
       app must regen its bindings.
     - **Web** is exempt from the *disk* pin store — the browser validates TLS
       itself and wasm has no `DiskPinStore` (it keeps a localStorage pin instead;
       part a-ii below). The **residual** of that exemption is
       hardened by review: the SPA now refuses to navigate
       (`window.location` / `window.open`) to a non-`https:` **nest-supplied**
       destination — the bridge OAuth `redirect_url` and the subscription
       `payment_url` are scheme-allowlisted via `$lib/safe-url.ts` `isSafeNavUrl`
       (`routes/bridges/+page.svelte`, `routes/profile/[[actorId]]/+page.svelte`),
       so a spoofed/compromised nest can't relay the user to an attacker scheme
       (the anti-phishing-redirect class, not XSS — the browser already
       blocks `javascript:` here and `noopener` is set on the payment open).
       **The `payment_url` half of this guard is cross-app, not web-only**
       (native apps have no OAuth `redirect_url` flow, only the `payment_url`
       open): `fauna_core::subscription::is_safe_payment_url` (UniFFI-exported)
       is the single native-app definition, found 2026-08-10 to have drifted —
       tui duplicated it at two call sites instead of sharing it, linux and
       windows each guarded only the feed-side `payment_url` open and missed
       the profile-side one, and apple had no guard at all — fixed the same day
       across tui/linux (verified) and apple (native-toolchain build-verified
       2026-08-10 on macOS: `just apple-ffi` regen + `mac-debug`/`swift-test`
       360/360 green, the generated `isSafePaymentUrl(url:)` signature matching
       the call site exactly, no toolchain delta); windows' leg stays
       source-verified against generated UniFFI bindings pending its own
       native-toolchain build on Windows.
       **part (a-i) — done:** the `fauna_node_url` localStorage override is
       validated by `$lib/safe-url.ts` `isSafeNodeUrl` before `$lib/api.ts`
       `nodeUrl()` honours it — accepted iff it parses as an absolute `https:` URL
       (the production / LAN-TLS nest scheme) or an `http:` URL to a loopback host
       (`localhost`, `*.localhost`, 127.0.0.0/8, `::1` — the browser
       secure-context dev/e2e carve-out: `just web-dev` and the e2e suite serve the
       nest at `http://127.0.0.1:<port>`). Any other value — notably `http:` to a
       non-loopback host (the phishing/MITM-relay residual), or a malformed /
       relative / `javascript:` / `data:` string — is ignored and `nodeUrl()`
       falls back to `window.location.origin`, so a spoofed override can't silently
       steer the client to an attacker-controlled origin.
       **part (a-ii) — done (2026-06-26):** web adopts the SSH `known_hosts`
       model. The silent-challenge launch path (`fauna.auth.verify`) now carries the
       same Axis-1 proof the direct handshake does — `VerifyReply.cert_binding`, the
       nest signing `served_SPKI ‖ challenge_nonce ‖ client_nonce` (reusing
       `auth_handlers::build_cert_binding`; the `client_nonce` is the NT-1 hardening
       described below), symmetric with `HandshakeReply`. On each
       launch the web SPA possession-verifies that proof — **key-possession only**,
       since a browser can't read the served cert's SPKI to do the native channel
       binding — and TOFU-pins `(origin → nest_actor_id)` in localStorage, **warning
       loudly** when a later connect's possession-verified identity *differs*
       (`Changed`) **or** when the nest can no longer present a valid proof for an
       origin it already pinned (`Withdrawn` — downgrade protection, so an attacker
       can't bypass the pin by simply omitting the binding). The warning surface
       (`launch_identity_changed` → `nest-identity-changed-warning` + a "trust
       this nest" re-pin button that forgets the pin and re-TOFUs) is the
       **uniform all-app model**, carried by the shared `LaunchMachine`: a
       changed/withdrawn pin is `SilentChallengeOutcome::IdentityChanged` /
       `TokenRefreshOutcome::IdentityChanged` →
       `LaunchPhase::IdentityChanged { pinned_hex, seen_hex }` (auto-entry
       blocked, bearer dropped, retry refused), and the explicit recovery is
       `LaunchMachine::trust_nest_identity()` — forget the pin through the
       `AuthConnector` trust seam, then re-challenge and re-TOFU. *(Ratified
       2026-07-13; supersedes the earlier "hard connection error, no soft UI,
       web-exclusive warning" stance — the hard error routed natives to a
       retry spinner on a MITM signal with no recoverable affordance.)*
       Native detection stays the connect-time channel binding (full SPKI
       compare — strictly stronger than web's possession-only check); since
       2026-07-13 the native **silent-challenge (launch) path graduates too**
       (`fauna_anon_client::graduate_verify_path` over `served_SPKI ‖
       challenge_nonce ‖ client_nonce` — that exact message and no other since
       2026-09-24, when the compat-remnant sweep removed the challenge-nonce-only
       retry and the downgrade-harvest residual it carried) — before
       that, the launch path minted and *used* a bearer with **no identity
       check at all**, so an impersonated self-signed/LAN nest was spoken to
       until the first token refresh or content connect caught it. The
       pin/compare + possession-verify logic is **shared**
       (`fauna_client_core::nest_trust`: `check_web_nest_identity`,
       `verify_cert_binding_possession`, the `run_pinned_silent_challenge` ceremony the machine's wasm
       connector runs, the `NestIdentityPinStore` trait — `get`/`set`/`remove`,
       `remove` being the user-approved re-trust seam — + `MemoryPinStore` —
       lifted there from `fauna-anon-client` so native and wasm call one core,
       priority #2); only the pin-store backend diverges (web's
       `LocalStoragePinStore` over localStorage — also in `nest_trust`, ONE
       store shared by the SPA's `challenge_verify_inner` path and the
       machine's wasm connector, keyed by the nest URL — vs. native's
       `DiskPinStore`). **The reach hint keys on the domain, never on the
       dial (built 2026-08-30; supersedes the 2026-08-29 carry-over target).** A
       session that reaches a freshly-provisioned box through its reach hint
       (`behavior/onboarding.md` § Reach hint — `https://<ip>` on web, the
       resolve override on native) pins under the **domain**, exactly as a
       domain dial does: natively for free, since `connect_resolving` moves the
       socket address alone and leaves the URL — hence SNI, `Host`, the demanded
       cert and the pin key — the domain's; on wasm by keeping the dial URL out
       of the identity argument, since there the *authority* is what moves. So
       the hint dial and the later domain dial resolve the **same** pin: the
       switch opens no TOFU window and can never read as `IdentityChanged`,
       there is only ever one key per box, and a stale hint pointing at a
       recycled address is refused by the pin the account already holds.
       **The earlier target — copy the pin from the hint authority to the domain
       authority — is retired as unsafe, not merely unnecessary:** it promotes
       whatever the IP dial TOFU-accepted into the pin that guards every later
       domain dial, which is strictly weaker than pinning the domain in the
       first place. **The claim seeds the pin from the wizard's
       possession-proven first-contact root (ratified + built 2026-08-30).** Where a hint dial is the *first*
       contact with a box (the common case at wizard exit, since the domain
       does not yet resolve), it used to TOFU under the domain key — the
       ordinary NT-1 first-contact residual — even though the client had
       possession-proven that box's identity minutes earlier (every pre-claim
       dial graduates against the injected-seed / pasted-URI root) and then
       discarded the proof at wizard exit. Now **every claim success persists
       the held root as the domain's durable pin** through the shared
       `NestIdentityPinStore` seam (`OnboardingMachine::seed_identity_pin_at_claim`,
       called from all three claim-success arms: `claim_provisioned_box`'s
       fresh claim, its already-claimed-ours recovery edge, and
       `wizard_submit_claim_code`'s URI-pinned claim), each arm writing the key
       its launch reader resolves — native `authority_of(nest_url)` into the
       installed store (`fauna_anon_client::trust::seed_claimed_identity_pin`),
       wasm the nest URL verbatim into `LocalStoragePinStore` — so the first
       post-claim dial, hint or domain, **verifies instead of TOFU-ing**, on
       both arms. Native is seeded too, deliberately: the SPKI channel binding
       defeats a *relay*, not a *stranger* — a first-contact hint dial at a
       recycled address would TOFU-pin whatever box answers with a valid
       self-binding on native exactly as on web. The write is authoritative
       (`set`, never `pin_if_absent`): the claim is a user-initiated ceremony
       over a connection that just possession-proved the root — evidence
       strictly stronger than any TOFU pin it replaces — so a "start over onto
       a new box, same domain" installs the new box's root rather than wedging
       the first launch on an `IdentityChanged` the stale pin would force (the
       wizard's own pre-claim dials never consult the pin — a pre-resolved
       root is exact-match — so a stale pin cannot wedge the wizard either).
       Rotation is untouched — and, since the 2026-08-30 same-root-guard
       hardening, that is a
       property of the **write path**, not only the value: a seed whose root
       the pin ALREADY NAMES is a no-op — `set` replaces the whole entry, so
       an unguarded re-seed on the repeat-claim path would erase a
       chain-accepted `rotation_seq` and silently disarm both of
       `evaluate_rotation_bridge`'s fork clauses, downgrading a hard
       `PinForked` to a re-trustable `IdentityChanged`. A fresh seed lands an
       ordinary pin (`rotation_seq = None`) that `try_rotation_repin` bridges
       like any other, and a DIFFERING root still overwrites authoritatively —
       the old chain's seq belongs to the old box. Pinned by
       `fauna-anon-client`'s
       `a_reseed_of_the_same_root_keeps_the_chain_accepted_seq`
       (red-verified against the guard removed) on native; the wasm arm's
       identical guard property is witnessed separately, through a real
       browser claim rather than `FakeNestApi`, by
       `tests/e2e-unified/tests/test_web_claim_pin_wasm_witness.py`
       (red-verified 2026-09-13 by making its `store.set` unconditional,
       which reddens exactly the same-root case's `rotation_seq`
       assertion). What remains TOFU is exactly
       a claim with **no** held root — a
       bare typed claim code, or a resumed slot from before `nest_actor_id`
       existed — where nothing was proven and nothing may be invented. Pinned
       by `fauna-onboarding-machine/tests/claim_seeds_identity_pin.rs` (seed,
       no-root, stale-overwrite, and recovery-arm cases; red-verified against
       the seeding removed).

       **What the web pin does and does NOT catch (NT-1; reviewed 2026-06-26,
       tracked internally).** Web does
       **possession-only** verification — a browser can't read the served cert's SPKI,
       so there is no received-cert compare (the leg that gives the *native* channel
       binding its anti-MITM strength). The pin therefore rests entirely on nonce
       freshness, and it does **not** catch an attacker who has broken WebPKI for the
       origin (the spec's own premise — without a browser-valid cert the TLS handshake
       fails and no wasm runs) **and** is willing to obtain one genuine binding: such
       an attacker presents the **real** `nest_actor_id` via a binding **live-relayed**
       from the real nest (the **offline-harvested** binding — any one registered
       actor yielding a reusable, actor-independent, non-expiring binding — was the
       *legacy* nest's shape, one that signed over the server-chosen `challenge_nonce`
       alone; a client that folds its own nonce accepts no such binding since
       2026-09-24) — so the pin matches and does *not* warn. The earlier draft of
       this section claimed the pin "catches a DNS-hijack + attacker-valid-WebPKI-cert
       where the attacker lacks the nest deployment key"; that is **false** — an
       attacker lacking the deployment key still presents the real `nest_actor_id` via
       a harvested or relayed genuine binding. The pin's genuine residual value is
       narrow: (1) **benign** identity-change detection — it warns on a legitimate
       redeploy / key rotation (a different `nest_actor_id` → `Changed`); and (2) it
       catches only the **lazy** attacker who breaks WebPKI but presents *no* binding
       (`Withdrawn` — downgrade protection) or *their own* key (`Changed`). Against a
       network attacker willing to relay one binding it adds **no** meaningful
       protection over plain WebPKI. This *broadens* — does not contradict — the
       phishing/MITM-relay residual below: the uncaught case is a live
       TLS-terminating relay (an offline harvest sufficed only against the retired
       legacy shape).

       **A refused proof and an absent one collapse to the same outcome when
       unpinned — declared, accepted (reviewed 2026-08-26, tracked internally).**
       `check_web_nest_identity`'s `(pinned=None, seen=None) → Unprovable, proceed`
       row does not distinguish *why* `seen` is `None`: a plaintext/dev nest that sent
       no `cert_binding` at all reads identically to a nest that sent one whose
       signature failed to verify (`fauna_client_core::nest_trust`'s three web call
       sites all discard that `Err` into `None` via `.ok()` before it reaches the
       check). Once a pin exists this does not matter — `(pinned=Some, seen=None)`
       always warns (`Withdrawn`), refused or absent alike — the gap is narrow to
       **first contact with no prior pin**. This is the same reasoning as the
       former DNS/TOFU kind-rejection residual above (§ *Compat*, retired
       2026-09-24 with the rest of the transition remnants): with no root to authenticate
       *any* first-contact signal against, a corrupted proof is exactly as cheap for
       an active attacker to produce as an absent one, so refusing on it buys nothing
       against this section's own threat model while growing web a new hard-fail
       surface on a connection that is unauthenticated by construction. Accepted as a
       residual rather than closed; pinned by
       `fauna_client_core::nest_trust::tests::a_refused_verify_path_binding_collapses_to_unprovable_when_unpinned`.

       **Per-site witness status (added 2026-09-02).** The pin above covers the *rule* in isolation — it
       calls `verify_cert_binding_possession`/`check_web_nest_identity` directly and
       never drives a production call site, so it cannot see a regression at any
       of the three. One of the three now has a call-site witness; two are
       **declared absences**:
       - **`fauna_client_core::nest_trust::run_pinned_silent_challenge`** (the
         launch machine's wasm connector's entry point) — witnessed by
         `nest_trust::rotation_tests::the_web_launch_path_pins_the_identity_the_opening_read_proved`,
         which drives the real function against a mock nest serving a genuine
         opening proof and a present-but-forged verify-reply `tagged_sig`, and
         asserts the `Success` outcome and that the pin names the identity the
         **opening read** proved. **Since the login nest binding (2026-09-23,
         [`../behavior/login.md`](../behavior/login.md) § Binding the nest) the
         web verdict rides that opening read, not the verify reply:** the
         identity is possession-verified over a fresh client nonce and checked
         against the origin's pin *before* the login is signed, and a corrupted
         or absent opening proof is a hard refusal — no identity, no login
         signature — so the residual above is structurally closed on the login
         path; the verify reply's own proof is no longer consulted on web (it
         still is natively, where `graduate_verify_path` pins the served SPKI
         for the bearer connection).
       - **`fauna-wasm`'s `challenge_verify_inner`** (`libs/fauna-wasm/src/lib.rs`; its
         former sibling `mint_bearer_inner` was deleted 2026-09-23 when the SPA's re-mint
         moved onto `challengeVerify`) — **declared absence, not oversight.** It calls a
         live `fauna_rpc_wasm::AnonymousWsRpcClient`
         concretely (unlike `run_pinned_silent_challenge`, which is generic over
         `RpcRequester` precisely so a mock nest can stand in); reaching the
         refused-proof arm through either wasm32-only function needs a live
         WS-RPC mock reachable from a `wasm-bindgen-test` browser run (`libs/fauna-wasm`
         already runs pure-logic `#[wasm_bindgen_test]`s, e.g. `mint_key_blob_inner_roundtrip`
         in `tests/upload_sidecar.rs`, but none of them drive a live connection —
         no such harness exists in this crate today) or a refactor to inject the
         requester the way the native path does. Either is real, follow-on-worthy
         infrastructure work, not a witness — out of scope for this residual.
         Cross-referenced inline at both call sites.

       **NT-1 hardening (client-nonce fold) — done.** To shrink the residual to the
       handshake path's already-accepted *live-relay* case, `VerifyRequest` carries a
       fresh **client**-chosen `client_nonce` and the nest folds it into the binding —
       `served_SPKI ‖ challenge_nonce ‖ client_nonce` (symmetric with the handshake
       path's client-chosen nonce; `auth_handlers::build_cert_binding`). A binding
       harvested under one client nonce no longer possession-verifies against a
       victim's fresh nonce, so the *offline* harvest is closed against a current nest
       — only a *live* relay remains. A client that folds its nonce verifies the
       3-part message and nothing else: the challenge-only retry that once accepted a
       legacy nest's `served_SPKI ‖ challenge_nonce` binding (the
       verify-path fallback) and its
       residual *downgrade*-harvest (an attacker harvesting a 2-part binding by
       omitting its own client nonce) were removed 2026-09-24 by the compat-remnant
       sweep. The master secret
       never leaves the browser regardless (`challengeVerify` signs in
       wasm; only the signed bearer is sent), so the residual is impersonation, not
       raw-key theft. (The bearer re-mint runs the same possession check — see
       § Post-auth surfacing, channel 1. Since 2026-09-23 it IS this ceremony
       (`getAuthToken` → `challengeVerify`, `login.md` § When to use which); from
       2026-08-19 until then it was a direct handshake verifying over that
       ceremony's own client nonce. The *launch* identity pin rides the same
       challenge/verify, where the warning can surface on the launch screen
       before the app mounts.)
       **Implementation status today:** built — the identity pin runs on the web
       silent-challenge ceremony (`challenge_verify_inner`,
       `libs/fauna-wasm/src/lib.rs`) — launch and bearer re-mint alike — through one verdict
       helper (`fauna_wasm::nest_identity::check_pin_and_maybe_repin`) and all
       backed by the shared `fauna_client_core::nest_trust` core; tier_2 web e2e
       covers the warn-and-recover wiring, and the tier_3
       `test_nest_identity_pin_post_auth.py` covers the mid-session verdict. The NT-1 client-nonce hardening is also built —
       `VerifyRequest.client_nonce` (additive wire) + the nest fold in
       `auth_handlers::build_cert_binding` + the native client's `fauna_anon_client::graduate_verify_path`
       (one leg per request), pinned on that production door by
       `a_two_part_verify_path_binding_is_refused_by_the_production_door` (a 2-part
       binding, and a binding harvested under another client nonce, are refused by a
       client that folded a nonce). The transition residual
       — a *downgrade*-harvest against the legacy fallback — closed 2026-09-24 when
       the fallback was dropped. The **shared `LaunchMachine` identity-pin seam is built**
       (2026-07-13): `SilentChallengeOutcome::IdentityChanged` /
       `TokenRefreshOutcome::IdentityChanged` → `LaunchPhase::IdentityChanged`
       + `trust_nest_identity()`, native launch-path graduation
       (`graduate_verify_path`) and the wasm pinned ceremony
       (`nest_trust::run_pinned_silent_challenge` over the shared
       `LocalStoragePinStore`), `fauna_ffi::forget_nest_identity_pin`, all
       transition-tested in `fauna-launch-machine/tests/{silent_challenge,
       token_refresh}.rs`. **Consumed by web (2026-07-13)**: web's
       silent-challenge launch row now runs through `LaunchMachine::start()`
       (`routes/onboarding/+page.svelte`), so `launch_identity_changed` renders
       off `LaunchPhase::IdentityChanged` and the trust button calls
       `trust_nest_identity()`. Web's own pre-machine classifier is deleted; the
       pin store is unchanged (`LocalStoragePinStore`, `fauna_nest_pins`, keyed by
       nest URL), so what is pinned and what verdict a pin produces are the same
       before and after — the tier_3 `tests/test_nest_identity_pin.py`
       warn-and-recover journey (its `[web]` arm) is the regression pin. The `mintBearer` re-mint
       that used to follow the launch is gone too: the machine's already
       pin-verified bearer is handed to the SPA's token cache
       (`api.ts primeTokenCache`), so the app no longer mints a second, *unpinned*
       bearer on its first request.
       **Native, today:** `linux` + `tui` render the **full** surface for
       `LaunchPhase::IdentityChanged` — the localized
       `onboarding.launch.identity_changed_warning` in its own
       `nest-identity-changed-warning` element, the re-trust button
       (`nest-identity-changed-trust-button` → `trust_nest_identity()`) and
       `launch-fallthrough-button`, with **no retry CTA**
       (`apps/fauna-linux/src/views/launch.rs` § `IdentityChanged`;
       `apps/fauna-tui/src/launch.rs`). *(Until 2026-07-13 both fell into their
       launch-phase catch-all and rendered a `TransientRetry` surface: a **dead**
       Retry button — `retry_silent_challenge()` no-ops outside
       `Offline{transient:true}` — over a raw Rust debug dump. That is fixed; a
       retry CTA on a possible-MITM signal is precisely what this section
       forbids.)*
       **`android` (2026-07-13): the full uniform surface**, matching web — a
       new `AppLaunchVM.NavTarget.IdentityChanged` arm (`ui/viewmodel/AppLaunchVM.kt`)
       + `LaunchIdentityChangedScreen.kt` render the localized warning
       (`nest-identity-changed-warning`), the re-trust button
       (`nest-identity-changed-trust-button` → `AppLaunchVM.trustIdentity()` →
       `machine.trustNestIdentity()` on the *same* `LaunchMachine` instance that
       produced the verdict, not a fresh one) and `launch-fallthrough-button` — no
       retry CTA. Before this landed, `LaunchPhase.IdentityChanged` had **no**
       `when` arm at all in `AppLaunchVM.navTargetFor`, a non-exhaustive-`when`
       *compile error* that blocked all of android from building (discovered
       2026-07-13 rebasing the android Nests-trust-facet development branch
       onto the already-widened ui.yaml + shared enum).
       **`macOS` + `iOS` (2026-07-13): the full uniform surface**, completing the
       seven-app set. Apple was the last app whose launch never called the
       shared machine at all — it hand-rolled the four-case routing table in Swift,
       so `LaunchPhase::IdentityChanged` was not merely unrendered but
       *unreachable*, and a changed pin fell into the bespoke classifier's
       catch-all and painted the **transient retry** surface: the same dead-Retry-on-a-
       MITM-signal shape linux and tui had. Both targets now construct
       `LaunchMachine` over the shared `RegistryLaunchPersistence`
       (`FaunaAccounts.bootLaunchPersistence()`, from `FaunaMacApp.runLaunch()` /
       `FaunaApp.runLaunch()`) and render the phase through one **shared FaunaKit**
       `LaunchIdentityChangedView` — warning, re-trust button
       (→ `trustNestIdentity()` on the *held* machine, not a fresh one) and
       `launch-fallthrough-button`, no retry CTA. The trust button deliberately
       carries no `.keyboardShortcut(.defaultAction)` either: making "trust this
       nest" the Enter-key default on a possible-MITM warning is the same silent
       re-pin this section forbids, by another name. The fallthrough does **not**
       forget the pin — walking away from an untrusted nest must not un-pin it.
       **E2E, today:** the warn-and-recover journey is pinned by **one cross-app
       module** `tests/test_nest_identity_pin.py` (web + tui + linux + **macOS +
       iOS**; the separate
       web module merged in on 2026-07-15 once web grew a `common.launch_harness`
       leg — web has no `app_path`, and its only state-preserving relaunch is a
       `hard_reload()`, so a `teardown()` discards the whole browser profile). The
       arm each app reaches follows the nest's TLS posture: **tui** + **linux** +
       **macOS** + **iOS**
       ride `self_signed_nest`'s real binding → the stronger **Changed** arm (a pin
       the binding *disagrees* with); **web** rides its plain-HTTP origin, which
       serves no binding → the **Withdrawn** arm (a pin that can't be *confirmed*).
       **web, tui, macOS and iOS pass; linux skips headlessly** — the seeded pin must
       survive a
       relaunch (the native drivers otherwise hand a *fresh* store, so the harness
       calls `driver.preserve_state_across_relaunch()`), and linux's real-keyring
       backend needs an unlocked desktop Secret Service, whereas tui's, macOS's and
       iOS's file backend and web's localStorage are headless-safe.
       **apple joined 2026-08-01**, and needed no new harness *mechanism*: the
       `AppleFileCredStore` adapter, `make_launch_harness`'s native leg, both apple
       drivers' `preserve_state_across_relaunch()`, and the value-returning
       `machine_method_result` reader path the pin *reader* half needs were all
       already built — joining was the module's client list plus iOS's `ios_setup`
       fixture shape. Only the self-signed nest was new ground for apple, and it
       needed no cert plumbing: `self_signed_nest` binds `127.0.0.1`, so both apple
       clients accept it on the **loopback** branch of the trust posture above.
       ⚠ **One real harness bug surfaced, and it is the durability trap to know:**
       iOS's `preserve_state_across_relaunch()` pinned only the *credential dir*
       while `drivers/ios.py::launch()` kept `simctl uninstall`-ing the app data
       container — so the identity trio survived a relaunch but everything rooted in
       `Library/Application Support` did **not**, including the E2E pin store
       (`FaunaKit` `NestTrust.installPinStore()` roots the e2e launch at
       `<Application Support>/Fauna`). The method returned `True` while keeping half
       its promise, which reads as a product bug at the assertion. Fixed by
       `_preserve_container` (skip the uninstall when a test pinned state; `simctl
       install` upgrades in place and keeps the container) — the iOS counterpart of
       macOS's `_preserved_home`. Any iOS test asserting at-rest durability across a
       relaunch (MLS db, SwiftData, pins) depended on this.
       ⚠ **The same trap a third time, on tui-on-macOS (found + fixed
       2026-09-21).** tui's `DiskPinStore` rests at `install_scoped_trust_home()`
       (§ Pin custody across processes rule 1): `$XDG_CONFIG_HOME/fauna` on linux,
       under the `xdg_base` its `preserve_state_across_relaunch()` always pinned —
       but `<HOME>/Library/Application Support/Fauna/trust` on macOS, where every
       launch relocates HOME into a throwaway dir the pin never named. The seed
       assertion passed (same process, same HOME); the relaunch met an empty trust
       dir, TOFU-pinned afresh and entered as if nothing had changed, so the
       missing **Changed** arm read as a launch-routing bug (`count=0`, never
       rendered). The leg's only recorded green was linux's, and on macOS it could
       not have passed since the 2026-08-02 store move — until the driver pinned
       `home` too (`drivers/tui.py` `_relocates_home` / `_remember_launch_store`,
       the tui counterpart of macOS's `_preserved_home`).
       ⚠ The native cases **must** ride the TLS-serving `self_signed_nest` fixture:
       both native graduation points are scheme-gated
       (`fauna-launch-machine/src/auth.rs` and `fauna-anon-client/src/bearer.rs`,
       each `if nest_url.starts_with("https://")`), so against a plain-HTTP nest a
       pin is **never consulted** and this surface is simply unreachable. Both
       modules seed the pin through the one shared E2E-bridge name
       `set_nest_identity_pin_for_test`, which writes whichever pin store the client
       installed (`onboarding.md` § E2E bridge contract).
       **`windows` (2026-07-13): the full uniform surface, completing all seven
       apps.** `App.xaml.cs:DispatchLaunchSnapshotAsync`'s `LaunchPhase.IdentityChanged`
       case navigates to `Views/LaunchIdentityChangedPage.xaml` — the localized warning
       (`nest-identity-changed-warning`), the re-trust button
       (`nest-identity-changed-trust-button` → `machine.TrustNestIdentity()` on the same
       machine instance that produced the verdict) and `launch-fallthrough-button`, no
       retry CTA — the same lift linux/tui/android/apple already did. Rendering-wise every
       app is done; **e2e-verification-wise the `cred_store.py` adapter gap is CLOSED**
       (`tests/common/cred_store.py` wires `WindowsFileCredStore` since 2026-08-09 and
       `AppleFileCredStore` for macos/ios — `make_cred_store` covers six of seven apps
       (web, linux, tui, macos, ios, windows; `cred_store.py:389`'s `raise ValueError`
       is the unhandled-client fallthrough); android has no host-side adapter by
       design — its credential file is written on-device by the bridge with no
       host-readable path (`attach_cred_store`'s `NotImplementedError`,
       `cred_store.py:422-429`) — the "only linux/tui/web are wired" framing here was
       stale by the time row 87 read it, 2026-08-26).
     - **Post-auth surfacing (§ Post-auth surfacing, ratified 2026-07-23):
       the shared taxonomy is DONE (2026-08-15); linux (2026-07-23), tui
       (2026-07-30), macOS/iOS (2026-08-01), web (2026-08-19), android
       (2026-08-21) and windows (2026-08-26) route the verdict — all 7
       apps.** All seven apps render the launch surface and now route a
       *post-auth* verdict to it. Windows' leg is
       `App.RunTtlRefreshLoopAsync` — the
       app's one post-auth "keep the bearer fresh" cadence, the same universal
       hook other apps' legs route through: its per-iteration phase check now
       catches `LaunchPhase.IdentityChanged` (parked there by
       `LaunchMachine.RefreshToken()` rather than thrown) and re-enters
       `DispatchLaunchSnapshotAsync` on the SAME `LaunchMachine` instance that
       produced the verdict — never a synthesized one, since `TrustNestIdentity()`
       reads secret + nest_url off that specific instance and no-ops on any
       other. **windows' e2e leg is CLOSED (2026-08-31)**: `TestAgent.cs`'s `call_machine_method` case falls back to a
       throwaway `OnboardingMachine` for `set_nest_identity_pin_for_test` /
       `nest_identity_pin_for_test` specifically (the two names the shared
       `call_machine_free_method` dispatcher answers with no machine at all —
       every instance's dispatcher tries them first regardless of `self`) when
       `OnboardingViewModel.Current` is null post-auth, and a new `silent_sign_in`
       command drives `App.RunSilentSignInForTestAsync` — `LaunchMachine.RefreshToken()`
       on the SAME live machine `RunTtlRefreshLoopAsync` holds, then the identical
       phase-check-and-dispatch the loop's own tick performs.
       `test_nest_identity_pin_post_auth.py`'s `_POST_AUTH_APPS` now includes
       windows. Android's leg is
       `AppLaunchVM.performPostAuthSilentSignIn()`, called at the same
       universal post-auth hook `FaunaNavHost` fires the mail/CalDAV/backup
       glue from: catches `FfiException.NestIdentityChanged` from
       `silentChallenge`, tears the session down without erasing credentials
       (`ApiClient.clearAuth()`), and re-enters the launch flow via
       `appState.isOnboarding = true` — the same reconnect-no-relaunch path
       `AccountSettingsVM.switchAccount` already uses, so the SAME
       `LaunchMachine` singleton re-derives the verdict and the trust button
       has a live machine behind it. ⚠ **e2e is not wired**: android's
       `androidTest` bridge has neither the launch-time
       `set_nest_identity_pin_for_test` seam nor a `silent_sign_in` agent
       command yet (`test_nest_identity_pin_post_auth.py`'s `_POST_AUTH_APPS`
       still excludes android), and android e2e is emulator-host-gated on this
       fleet regardless — the code leg is done, the test leg is tracked
       separately.
       **web is the one app that covers TWO channels, and the only one whose
       leg is not a port of the native glue.** `fauna-anon-client` is
       native-only (its wasm twin is `fauna_rpc_wasm::AnonymousWsRpcClient`),
       so none of the four seams the shared taxonomy leg fixed exist on wasm;
       web instead runs the check at both of its own re-check points, over the
       browser-side possession primitive (`verify_cert_binding_possession` —
       no received-cert compare, since a browser cannot read the served SPKI):
       the **bearer re-mint** (channel 1, the handshake then — closing the
       "the `mintBearer` path ignores its `cert_binding` reply"
       residual, which is why web could not detect a re-mint-channel change at
       all) and the **background silent challenge** (`fauna.auth.verify`,
       channel 3). Both share one verdict helper
       (`fauna_wasm::nest_identity::check_pin_and_maybe_repin`, rotation-repin
       bridge included) so the two channels cannot drift. Web's handler is the
       apple shape rather than linux's: drop the bearer cache, count the
       teardown, and re-enter the real launch flow by navigating to the
       onboarding route — never a surface painted in place, because the
       re-trust button drives `trustNestIdentity()` on the `LaunchMachine` that
       produced the verdict and a synthesised phase would render an identical
       surface whose button did nothing.
       **e2e coverage of web's channel 1, closed 2026-08-22
:** a
       new web-only `bearer_force_refresh` command drives the real
       `getAuthToken(secret, undefined, true)` re-mint over the SAME
       Withdrawn-arm plain-HTTP harness `test_nest_identity_pin_post_auth.py`
       already uses for web's channel 3 — plain HTTP serves no
       `cert_binding` at all, so a poisoned pin is *unconfirmable* rather
       than *contradicted*, but that is the same verdict class reaching the
       same surface, not a weaker test. **Ruling on the Changed arm (real
       TLS) for web e2e: not pursued, and not needed.** Reaching it would
       need a TLS-terminating upstream leg in the e2e harness's SPA proxy
       (`tests/e2e-unified/conftest.py`'s `_proxy_websocket`/
       `_proxy_to_nest`, today a bare `socket.create_connection` + an
       unverified `urllib` context) — real harness work, not a browser-cert
       question, since the browser only ever talks to the plain-HTTP proxy.
       It buys no new coverage: `check_pin_and_maybe_repin` is the same code
       for both arms (only the re-TOFU tail differs, and the Withdrawn arm's
       own `repinned is None` assertion in
       `test_nest_identity_pin_post_auth.py` already covers that tail), and
       the Changed arm itself is already proven — over the real
       `self_signed_nest` fixture — by the other four wired native apps.
       **The classification rule is SHARED** — the identity verdict is the
       ONE outcome that escalates, every other failure class stays swallowed
       — as `fauna_launch_machine::classify_silent_challenge` →
       `SilentSignInVerdict`, with its unit pins beside it. It was app-local
       on linux until tui became the second consumer; the remaining five
       consume the shared fn rather than re-deriving the rule, which is what
       stops it drifting per-app. `SilentSignInVerdict` is a four-variant
       enum rather than a `Result` on purpose: the identity verdict must be
       reachable only through its own arm, since folded into an error it is
       indistinguishable at the call site from a network blip — exactly how
       it used to be swallowed.
       **Both legs then share one shape:** the verdict reaches the app loop
       as a dedicated payload-free `DataMessage::NestIdentityChanged`, never
       the error/toast path; the handler tears the session down **without
       erasing credentials** (nothing is wrong with the identity; the *nest*
       changed — and both exits need it in hand) and **re-enters the real
       launch flow** rather than painting the surface in place, because the
       re-trust button drives `trust_nest_identity()` on the machine that
       produced the verdict and a synthesized phase would render an
       identical surface whose button silently did nothing.
       Per-app detail: **linux** classifies in `client.rs` and tears down via
       `settings::trigger_nest_identity_changed` (pump, backup, sync-agent
       capability, windows destroyed, client `shutdown()` — which is what
       drops the bearer). **tui**'s channel is `session::silent_refresh`, the
       body behind its post-auth background refresh — which until 2026-07-30
       was a `let Success(..) else { return }` that swallowed the verdict
       along with everything else; its handler is `session::sign_out` +
       `launch::start`. **macOS/iOS** go over UniFFI instead of native Rust:
       `APIClient.silentSignIn(secret:)` throws a distinguishable
       `FfiError.NestIdentityChanged` (`libs/fauna-ffi/src/auth.rs`'s
       `silent_outcome_to_ffi`, the same classification linux/tui's
       `classify_silent_challenge` performs, just surfaced as a Swift error
       rather than a Rust enum); `performPostAuthSilentSignIn()` (one-shot at
       authenticated launch, plus the `#if DEBUG` test command's trigger)
       catches only that case and calls the same `tearDownSessionForSwitch()`
       + `runLaunch()` pair account-switch already uses, so the fresh
       `LaunchMachine` re-challenges the still-poisoned pin and lands on
       `LaunchPhase::IdentityChanged` with a live machine behind the trust
       button (avoiding the dead-button trap the design guards against).
       ⚠ **e2e:** `test_nest_identity_pin_post_auth.py` (tier_3) drives all
       three. The machine-free bridge arms (the nest-identity pin seed/read)
       are a shared free dispatcher
       (`fauna_onboarding_machine::call_machine_free_method`) each agent
       consults *before* requiring an onboarding machine (absent post-auth, so
       the pin CAN be re-seeded on a live session), and a `silent_sign_in`
       bridge command triggers the production refresh — the real one, not a
       faked verdict. ⚠ **Not every arm is equally observable:** linux's GREEN
       run is gated on an unlocked Secret Service (its launch injects the
       identity into libsecret), so it SKIPS on a headless box — run it on a
       Linux desktop session or via the real-session path; **tui's and
       apple's stores are file-backed and those arms run green headless**, so
       they're the arms that actually verify this path in CI-shaped
       environments.
       `test_nest_identity_pin.py` covers the *launch*-time surface.
       **The shared taxonomy leg is DONE (2026-08-15).** The bearer re-mint's
       verdict is no longer flattened: `ApiError::NestIdentityChanged` and
       `NestClientError::NestIdentityChanged` carry it — with the same
       `host` + two-fingerprint field set `FfiError::NestIdentityChanged`
       renders — through all four seams that used to stringify it
       (`ws_challenge_bearer::map_anon_err`, `AuthClient::map_api_err`,
       `ws_adapter`'s bearer arm, and `LaunchMachineBearer`, the last being
       the one linux and tui ride). Two consequences worth naming:
       `ClientChannel::connect_error_is_terminal` now stops the reconnect
       loop on it, ending the transient-retry-on-a-MITM-signal the plumbing
       rule describes; and `fauna-ffi`'s single `stringify` helper raises the
       verdict instead of `FfiError::General`, so every authenticated FFI
       call routes it without a per-call-site decision. **Which failure IS
       the verdict is one shared table**, `fauna_anon_client::
       classify_identity_changed` (pin changed / rotation-chain fork / a
       binding withdrawn on a host that has a pin) — lifted out of the launch
       machine when the mint became its second consumer, so the mint and both
       launch-machine channels cannot drift apart.
       **Still open:** per-app glue ×1 — windows — on that variant (or the
       machine's `TokenRefreshOutcome::IdentityChanged`), drop the bearer and
       navigate to the existing `launch_identity_changed` surface; linux's,
       tui's and android's legs are the reference, and the step is now mostly
       the handler, since both the classifier and the taxonomy are shared.
       (Web and android are both done — web's leg additionally had to add the
       possession check its direct `mintBearer` re-mint was skipping, before it could route the
       verdict at all.) Tracked internally as a cross-app track.

  (The former "residual HTTP surface" gap — the reqwest content API + `POST /register`
  still honouring `FAUNA_INSECURE_TLS` — is **closed**: that leg is now SPKI-pinned
  to the WS handshake's bound identity, see the Cross-connection-propagation bullet
  above. Byte-bulk content stays HTTP/1.1 (Bucket C — `transport.md` § HTTP residue;
  it is *not* migrated to WS-RPC, just secured), but it is no longer MITM-open.)

### Unverified-source indicator rollout

(§ App display of unverified content is the rule; this is its rollout record.)
The **shared-Rust foundation is landed** — the `VerificationStatus` enum, the
`PostSummary`/`QuotedPostView` fields, the consume-the-flag fix at both
`manager.rs` decode sites, and the `RenderBlock::QuotedPost.verification` field
folded by `build_post_document`. The `ui.yaml` `unverified-source-badge` element
(user-approved 2026-06-24), the **Slice-2a focal-post badge** (list card +
post-detail), and the **Slice-2b quoted-embed badge** are landed on **all seven
apps**: linux (`post_list.rs` `build_unverified_badge`, `post_detail.rs`,
`build_quoted_post_card`), web (`UnverifiedSourceBadge.svelte` in `PostCard`;
`QuotedPost.svelte`, reused by the detail pane), android
(`UnverifiedSourceBadge.kt` in `FeedScreen.kt` + `PostDetailScreen.kt`;
`QuotedPostEmbed` — compile-verified via host-`.so` bindgen on a Linux dev machine; runtime
APK + emulator e2e stay emulator-host-gated), windows (focal badge +
`FeedPostItem.QuotedPostIsUnverified` on list card + post-detail dialog,
2026-06-26), apple (macOS/iOS: `UnverifiedSourceBadge` in
`MacPostCardView`/`PostCardView`, detail panes off
`livePost.verification`, shared `QuotedPostCard` across all four feed surfaces,
2026-06-26 — mac-debug + swift-test green), and tui (`feed::post_card` in
`apps/fauna-tui/src/feed/mod.rs`, both the focal card and the quoted-embed
badge keyed off `RenderBlock::QuotedPost::verification`, landed
— the same commit also added tui to the cross-app tier_2 e2e module below).
Each surface renders the badge iff `Failed`, reading the one shared field.
The **tier_2 state-injection e2e**
(`tests/e2e-unified/tests/test_feed_unverified_source.py`) proves the
iff-`Failed` rule for both surfaces, green on six of seven apps — web, linux,
windows, macOS, iOS, tui — via the shared feed-injection seam
(`FeedManager::set_feed_snapshot_for_test`; web
`WasmFeedManager::injectPostsForTest`, linux/windows/tui `feed_inject_posts`).
Android stays compile-verified only per the app list above. The track is
complete (tracked internally).

**§ On-screen secret exposure (screen capture) — ratified 2026-08-15, android
built the same day, apple 2026-08-16, windows 2026-08-24; every app that can now
does.** Before that date the posture existed nowhere and **no app implemented any
part of rule 2** — capture suppression was absent tree-wide, which is the finding
that produced the section. Today:
**android implements rule 2** (`SuppressScreenCapture`, a Compose effect that sets
`FLAG_SECURE` on the host Activity window while composed and clears it on dispose,
mounted from the mail- and Bluesky-credential reveals only); **apple implements
rule 2 on all three minted surfaces** (mail credential, Bluesky app credential,
Nostr bunker connect string) through one shared `FaunaKit` modifier,
`.suppressScreenCapture()`; **windows implements rule 2 on the same three**
(below); **linux, web and tui are rule-3 declared absences** with no platform API
to reach for.

**windows, and the release path that is not the obvious one.** The mechanism is the
one the section names — `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` on
the app window, behind the same **refcounted per-window guard** apple's macOS half
needs and restoring the *remembered previous* affinity when the last holder leaves
(`FaunaApp.Core/Services/ScreenCaptureGuard.cs`; each call site owns one idempotent
`ScreenCaptureHold` so a double-acquire — a permanently uncapturable app — is
unrepresentable). It lives in `FaunaApp.Core`, not the WinUI project, because
`FaunaApp.Tests` is plain `net10.0` and references Core only; the WinUI half is one
file that resolves `App.MainWindow`'s HWND. **The Bluesky reveal has no hide path**
— `atproto-app-credential-reveal`'s own Button text *becomes* the secret and the
view-model never clears its revealed-secrets map — so a hold keyed on "the secret
went away" would never release; its release is `OnNavigatedFrom`, which is rule 2's
"or navigate-away" arm doing the whole job rather than half of it. **Rule 1's
windows coverage is one screen, stated rather than implied:** windows renders
`secret-key-display` (`Views/Onboarding/IdentityCreatedView.xaml`) and has **no
`recovery-kit-secret-display` surface at all**, so the pin covers the identity
secret today and gains the recovery kit when that screen lands. Witnesses:
`FaunaApp.Tests/ScreenCaptureGuardTests.cs` for the refcount arithmetic against a
fake seam, and `ScreenCaptureAffinityTests.cs` for the **real** platform bit, read
back with `GetWindowDisplayAffinity` off a window the test process owns
(`SetWindowDisplayAffinity` accepts no other kind).

**apple's two halves, and the iOS option it took.** *macOS* is the real thing:
`NSWindow.sharingType = .none`, reached from SwiftUI through a zero-size
`NSViewRepresentable` in the reveal's background, behind a **refcounted per-window
guard** that restores the *previous* sharing type when the last holder leaves —
refcounted because several credential rows can be revealed on one window and a
navigation transition can briefly hold two screens, and a leaked hold leaves the
whole app unscreenshottable, which users experience as a broken machine rather
than as security. *iOS* has no per-window equivalent, and this leg took the
section's **`UIScreen.isCaptured` option**, extended to the foreground transition:
the revealed value is blanked while a capture is live **and** while the app is
leaving the foreground, the second arm covering the app-switcher snapshot — iOS's
analogue of the recents-screen thumbnail android's `FLAG_SECURE` removes, which is
the concrete harm the original finding named. **Stated rather than implied away: a
still screenshot on iOS is not prevented**, because the platform offers no way to.
The alternative option — hosting the value inside a secure `UITextField`'s
excluded canvas layer — would cover screenshots too, and was declined on purpose:
it reparents content into a layer whose exclusion UIKit documents nowhere, so its
failure mode is *silently ceasing to protect* on a future iOS, and since
suppression here is never load-bearing, a documented partial control beat an
undocumented total one. The decision half is a pure function
(`ScreenCaptureBlankPolicy.shouldBlank`) precisely so the iOS mechanism has host-
runnable tests: `swift test` builds for macOS, so an iOS-only binding would
otherwise ship with no test at all. **Rule 1 is satisfied
everywhere by construction and must stay that way:** no app has ever suppressed
capture on `secret-key-display` or `recovery-kit-secret-display`, so the rule
documents a property to preserve rather than one to build — the failure mode is a
future session "completing" the feature by extending suppression to those screens,
which is why the rule is ordered first and why android's leg pins the surface list
rather than the mechanism alone. Since 2026-08-16 that preservation is enforced
**without any platform toolchain** by tier_1
`tests/e2e-unified/tests/test_screen_capture_posture.py`: a source rendering a
root-secret id may not also call its app's suppression, every minted reveal must
call it, and both directions carry vacuity guards. Toolchain-free is the point —
the artifact-level witnesses are per-platform (android's needs Robolectric,
apple's needs a Mac), so without it the rule is enforced only on whichever machine
happens to own that app. It is red-verified against the exact defect it forbids
(planting a suppression call on `secret-key-display` fails it, in both the apple
and windows arms), and each app's row is added there in the same commit as its leg.

⚠ **A grep-shaped pin decays in ways its own green does not show, and this one had
already decayed twice by the time windows joined it (2026-08-24). Both are worth
not re-learning.** *(1) Spelling.* It matched the literal kebab-case id; the
element-id constant sweep the day after it was written moved apple's
views to `Ids.mailSettingsCredentialItemSecret`, leaving the literal only in the
generated id map the module deliberately excludes — so apple's arms scanned zero
real files from that commit on. The **vacuity guard is what caught it**, which is
the entire argument for carrying one. It now resolves each app's own generated
`UiIds.*` map and accepts either spelling, so the next app to adopt constants
inherits the coverage instead of losing it. *(2) Selection.* `conftest.py` splits a
parametrization id on `-` and deselects any case naming an app outside `--app`, so
an id of `apps/fauna-android` read as "an android test" and never ran in a default
run at all. Toolchain-free coverage that only runs on the machine owning the app is
not toolchain-free coverage; the ids carry no app token now. *(3)* windows also
forced the scan unit to widen from a file to a **screen**: it keeps ids in `.xaml`
markup and calls in `.xaml.cs` code-behind, so a file-level scan would have found
no windows file that both paints a root secret and suppresses — vacuous in both
directions while green.

## Reading list

In priority order:

1. `principles.md` — engineering priorities (long-term uniformity, shared Rust).
2. (design ratified 2026-05-15; tracked internally) — full design rationale for canonical dag-cbor + CID + sign-over-CID + embed-as-bytes.
3. `docs/goal/architecture/serialization.md` — byte-level CID layout, canonical encoding, embed-as-bytes wire shape, conformance corpus.
4. `libs/fauna-cbor/src/{envelope,canonical,codec,cid,error}.rs` — the implementation this doc describes.
5. `docs/goal/architecture/key-material-hierarchy.md` — long-lived key taxonomy and per-audience root mapping.
6. `docs/goal/architecture/transport.md` — WS-RPC framing that rides on top of signed payloads.
7. `docs/goal/architecture/data-flow.md` — CARv2 segment storage, the on-disk consumer of signed CIDs.
8. RFC 8032 — Ed25519 signature algorithm.
9. BLAKE3 spec — multihash code `0x1e`, 32-byte digest.
10. ATProto data model docs — prior art for the Camp A sign-over-CID model Fauna's security path mirrors.
