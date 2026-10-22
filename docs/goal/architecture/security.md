# Security — sign-over-CID, verification recipe, decode-strictness — target state

Owns: security, sign-over-cid, screen-capture-posture
Status: ratified
Authority: runtime crypto discipline (signed envelope, verification summary, decode-strictness, forbidden tags) + co-resident process-trust postures. Client-side transport trust (channel binding, identity/TOFU pinning, cross-connection SPKI pin, pin custody across processes) and its implementation status → [`transport-trust.md`](transport-trust.md) (**split 2026-10-14**); Byte-level encoding (canonical dag-cbor, CID shape, embed-as-bytes, conformance corpus, the verification recipe's byte-level steps) → `serialization.md`; the channel-binding handshake's *field layout* → `transport.md` (`transport-trust.md` owns *why* and *what is proven*); CARv2 segments / on-disk manifests → `data-flow.md`; key taxonomy + the sig-domain rule #8 → `key-material-hierarchy.md`; server-side cert policy → `nest/tls-certificates.md`; release / supply-chain / build-time trust → `release-integrity.md` (this doc is *runtime* crypto discipline; that doc is *who can ship code at all*); the post-quantum overlay → `security/post-quantum.md` (signatures are not HNDL-exposed so they sequence after the KEM work; the BLAKE3-256 CID is already quantum-adequate, so the verification recipe survives a signature-algorithm change unchanged); the third verification axis — *cryptographic proof the nest applied each operation correctly* (verifiable-DB commitments + tracer proofs), beyond this doc's two axes of *payload authorship* and *which nest you reached* — → `encrypted-spaces.md`.

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

**A media reference an author signs names a content address the client computed, never one a nest handed back.** The client hashes the bytes it uploads (`blake3` over the uploaded body, the function the nest ingests with) and refuses the upload when the nest's reply names a different hash, so a malicious or compromised home nest cannot put the author's signature on a blob they never attached; the nest's reply is an acknowledgement, not the source of the hash. Shared helper: `fauna_client::media_upload::upload_prepared_blob`.

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
  feed index never reads `content.payload`; `feed-read-model.md` § The read model). There is
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

Moved 2026-10-14 to [`transport-trust.md`](transport-trust.md), verbatim under this same heading,
with its status entries — the model and each of its children, every one under its original
`###` heading there: *Two independent axes*; *Connection-teardown rule*; *Cross-connection binding — pinning the bound SPKI onto the bearer connection*; *Post-auth surfacing — a mid-session identity change blocks, on the same surface (ratified 2026-07-23)*; *Pre-claim surfacing — the wizard's anonymous path carries the verdict too (ratified 2026-08-30)*; *Pin custody across processes — one install-scoped store, one interactive minter*.

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

**Hosted third-party code (ratified 2026-09-05, the third-party integration chain — TP3/TP11; what is built: § Implementation status today).** A plugin the nest hosts ([`third-party.md`](third-party.md) § Execution forms) is a co-resident process under exactly this posture and never a peer of trust: a curated-catalog **container** runs under its own UID with the kernel-enforced default-deny above (Landlock + seccomp), network-restricted to the hosts its document declares; a **WASM component** gets only the capability imports the host links (named outbound hosts, the nest API, a state scope, a clock) and the labeler runtime's fuel/memory caps. Neither ever holds a **certificate key**: the nest terminates TLS for any path or SNI name a plugin's document claims (`ingress`) and proxies plaintext to the plugin over the sidecar channel with a **nest-signed identity assertion** — the identity-aware-proxy shape, never a bare forwarded header (the same class of forgery the PROXY-header secret above closes). **The path half as built (ruled 2026-10-10; refutable until the first plugin declaring ingress is installed):** a declared prefix is served only on the nest's OWN host — the apex domain, or any non-custom host of a domainless box — never on a user's custom domain or `<handle>` subdomain, so a plugin never shadows a user's published site (the admin's apex site yields to the plugins the same admin installed); it is matched in the public router's fallback, so a path the nest routes never reaches a plugin, and install refuses a prefix under one of the nest's own subtrees (`plugin_ingress::NEST_RESERVED_INGRESS_PREFIXES`, pinned against every route literal) or overlapping another installed plugin's. The plugin receives the method, the full path, the query, the headers minus `authorization` and the hop-by-hop set, a body under the router's 10 MB limit, and `actor` — filled from the request's bearer through the one bearer validator (`auth::check_bearer_session`), none when absent, the nest's 401 when it does not validate — a record field the nest fills, so no header names an identity. The reply drops the hop-by-hop set and `set-cookie` and always carries `Content-Security-Policy: sandbox` and `nosniff`: the plugin answers on the nest's own origin, where the web app holds the user's secret, so nothing it serves may script that origin (a hosted plugin is headless — `third-party.md` § The runner contract, TP2). A fault, or a plugin not running, answers 502. The PDS bridge's own cert fetch (`apps/bridges.md` § Bridge-kind catalogue, `fetch_tls_cert_blob`) is the first-party exception TP11 migrates last. The sandbox profiles are owned here; the runner contract is `third-party.md`'s.

**The whole-name (`sni`) half as built (ruled 2026-10-10; refutable until the first plugin claiming a name is installed).** A plugin claims a whole name as **one lowercase DNS label under the installing nest's apex** — `{ "sni": "matrix" }` is `matrix.<apex>` on every nest that installs the document (the member's grammar is `third-party.md` § The manifest's): a publisher's document installs on many nests, so a name can only be relative to the nest, and an apex sub-name is exactly what the deployment already routes and resolves — `fauna-sni-router` is an L4 passthrough whose default backend is the nest, so **it needs no change**; the three labels it routes elsewhere (`mail`, `relay`, `pds`) are reserved labels no document may claim, and the admin's wildcard `*.<primary>` row (`../behavior/dns-management.md`) already points the name at the nest, with the name's own `A`/`AAAA` row surfaced beside `relay.`/`pds.` in the DNS matrix (`fauna_mail::dns::host`, `HostDnsInput::plugin_labels`) for an admin without the wildcard. **On a claimed name the nest answers nothing:** `plugin_ingress::whole_name_gate` is layered ahead of every route, merge and the credentialed CORS layer, so a request whose normalized `Host` is `<label>.<apex>` of an installed plugin is that plugin's on every path — no API, no `/app/`, no web content, no stacked CORS header — and a stopped plugin's name answers 502 like its paths. The name is therefore a whole origin holding no Fauna secret, the posture a user's `<handle>.<apex>` site already has (`../behavior/web-content-hosting.md` § Same-origin security model), so the reply **keeps `nosniff` and the dropped `set-cookie` but forces no `Content-Security-Policy: sandbox`** — the sandbox exists because the path half shares the web app's origin, and a separate name is a separate origin. **The certificate is the nest's, never the plugin's:** the name joins the per-domain web cert reconcile (`web_content::cert`, the sole authority over the per-SNI map — same HTTP-01 flow and account as a user's subdomain, issued within the loop's cadence, dropped after uninstall) and is served by `MultiDomainCertResolver` under the per-SNI floor rule (`nest/tls-certificates.md` § A); it stays out of the apex ACME order and out of the bridge fan-out by construction. **Exclusivity is first-come on the web name, in both directions:** install refuses a label whose `<label>.<apex>` is a live user site (an opted-in subdomain or a registered custom domain, `HostResolver::user_site_for`), one another installed plugin holds, and any label on a domainless box (no apex, no name to give — the install card lists each claimed name in full, `ConsentInstallInfo::ingress_names`); `fauna.web.set_subdomain_enabled` and `fauna.web.domain.set` refuse a name an installed plugin claims, and the boot seed of the host resolver skips such a subdomain. A handle equal to a plugin's label stays a valid handle — it is an address, not a web name — only its subdomain opt-in is refused, with the reason. Proven tier_3 in `bins/fauna-nest/tests/plugin_runner.rs` (the name reaches `ingress.handle` with the nest-asserted actor on a routed nest path too, host normalization, no sandbox on the name and the sandbox kept on the nest's host, the opt-in refusal, the install refusals) and unit-tested at the grammar, the gate's host match, the cert reconcile and the DNS row.

### Implementation status — UID isolation

**Decided 2026-06-24 (user): adopt the full UID/container split.** Resolves
loopback PROXY-spoofing, signature-free self-enrollment,
and the MDA-RCE-blast-radius items.

- **Slice 1 — distinct UIDs + filesystem isolation (single image): BUILT.** Each
  network-facing s6 service runs under its own non-root UID — nest `fauna`(1000),
  MTA `fauna-mta`(1001), MDA `fauna-mda`(1002), SNI router `fauna-router`(1003),
  P2P relay `fauna-relay`(1004), Bluesky PDS helper `fauna-atproto`(1005);
  `supervisor` stays root, `algorithm` is `fauna-sandbox`-confined.
  nest's sealed store (`/data/nest.db`, `/data/blobs`, `/data/acme`,
  `inbound-deliver-key`) is `0600`/`0700` nest-owned and **unreadable by a bridge
  UID**; each bridge's key lives in its own `0700` subdir
  (`/data/keys/{mta,mda}/`, bridge-UID-owned; the relay's and the PDS helper's
  likewise under `/data/keys/relay/` and `/data/keys/atproto/`), so no service
  can read another's key. `/data` is `0711` (traverse-only for the now-non-owner bridges);
  `operator-hatch.toml` (topology, no secret) is `0644`. The entrypoint creates and re-owns the per-role
  subdirs on every boot; no pre-split flat keyfile is migrated (the 0.1.x
  compat-free window and the 2026-09-24 baseline reset owe none). Authority: `installers/docker.md` § s6-overlay Services. Verified by
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
  3. **The deployed box** — the live nest self-reported
     `enrollment_strict {mta: true, mda: true}` over admin WS-RPC, i.e. the blessed
     registry really is provisioned in production and the silent-lenient failure
     mode is not live.
  A supervised read-only on-box pass (2026-07-22, user-approved; SSH stays
  debug-only per `testing.md` § Gap 3 and is **not** a supported verification
  path) additionally confirmed the slice-1/4 facts on that live nest itself, which
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
the hand-out the MTA already needs is the sign site. The residual: a
compromised MTA can still have a message signed by enqueueing it as a user
whose submission token it has unwrapped — a user who opted into
third-party-MUA submission and authenticated through it — until that
credential is revoked or the token expires; never as an app-only user, who has
no token. The nest proves the submitter with the token and holds the From: to
it at every door (`../behavior/mail-multidomain.md` § From: header ownership →
*Per door, nest-side* owns both, and the MDA's accepted residual).

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
update API is made a standing on-by-default path, revisit segmentation.) The posture is the socket holder's, whichever component that is: the Fauna updater, which replaced Watchtower in every shipped bundle, inherits it unchanged and has no update API at all — it takes no requests — [`nest/update-channel.md`](nest/update-channel.md) § The updater; its outcomes reach `admin-logs` by a write through that same socket into the nest container, which adds no mount, token or listener to the holder (that doc's § What the updater reports). The paragraph above stays true of a box installed before that change, which runs Watchtower until its compose is re-copied.

## On-screen secret exposure (screen capture)

**Ratified 2026-08-15.** Several app screens display a secret in plaintext *by
design*: the mail-credential reveal (`connected-apps-item-secret`, on the Connected apps
roster that lists the mail app passwords; rule 2 follows the reveal to
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
snapshot (`connected-apps-item-secret`'s own contract), and revealed only
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

- **Client-computed media hash (§ Signed-envelope shape, ratified 2026-10-12): shared Rust, web and android BUILT; the apple and windows shells OPEN.** `fauna_client::media_upload::upload_prepared_blob` compares the nest's upload reply with `blake3` of the primary bytes and refuses a mismatch (linux, tui, archive import, period-sealed media). The web SPA's `uploadBlobMultipart` (`apps/fauna-web/src/lib/api.ts`) does the same through `assertEchoedBlobHash` (`blob-upload-hash.ts`) and the wasm `blobContentHashHex` export (shared `fauna_media::pipeline::bytes_content_hash_hex`), covering every caller including thumbnail uploads. A sealed body's echo is compared with its `encrypted_ref` by `FeedManager::submit_gated_post` and the archive importer on every app. The shells that POST their own public (plaintext) uploads call the shared UniFFI export `verify_blob_upload_echo` (`libs/fauna-ffi/src/media_upload.rs`, the same `bytes_content_hash_hex`) on the reply: android `ApiClient.uploadPublicPostBlob` is BUILT (`ApiClientPublicUploadEchoTest`), returning the locally computed hash. Not built: the two shells that still return the reply's hash unchecked into a signed profile image or compose attachment — apple `APIClient.uploadBlob` / `uploadPreparedBlob`, windows `DirectNestClient.UploadBlobAsync` / `UploadPreparedBlobAsync`.

- **Hosted third-party code's sandbox profiles + nest-terminated ingress (§ Co-resident process trust boundary, ratified 2026-09-05): the WASM sandbox BUILT 2026-10-02 and the nest-side runner 2026-10-03, the rest unbuilt.** `libs/fauna-plugin-host` links exactly the capability imports the paragraph names (named outbound hosts refused before any dial, the nest API through the principal chokepoint, a state scope, a clock) under the labeler's fuel and memory caps, and the ingress record carries the nest-asserted actor as a field the plugin cannot forge; no plugin ever sees a certificate key because the host crate has no import that could carry one. The nest-side runner (`bins/fauna-nest/src/plugin_runner.rs`) starts each installed plugin under those caps and dials its outbound fetches through the nest's guarded fetcher (both SSRF seats over one parse, pinned addresses, no redirect) after the host policy admits the URL. The ingress proxy's path half BUILT 2026-10-10 and its whole-name (`sni`) half the same day (`bins/fauna-nest/src/plugin_ingress.rs`; tier_3 `bins/fauna-nest/tests/plugin_runner.rs`; the per-name cert in `web_content/cert.rs`, the DNS row in `fauna_mail::dns::host`). Not built: the container form's UID/Landlock profile; sequencing in `third-party.md` § Implementation status today.

The app-layer security path (§ Signed-envelope shape onward) is **shipping** —
`libs/fauna-cbor/src/envelope.rs` implements `SignedEnvelope::sign` /
`verify_permissive` over the 36-byte CID (the raw two-step primitive; every
production caller routes through `fauna_core::encoding::verify_envelope` →
`fauna_core::identity::verify_detached`, the strict-plus-weak-key-refusal
primitive — § Key management invariants), and the pre-parse validator
(`libs/fauna-cbor/src/canonical.rs`) gates every `decode_strict` call; per-area
embed-as-bytes adoption tracks separately under the CBOR-DAG-everywhere plan.
The transport trust model's implementation status — the WS-RPC leg and the HTTP content API,
graduation on both legs, the nest and client halves, and the per-app durability + web hardening
record — moved 2026-10-14 with § Transport trust to
[`transport-trust.md`](transport-trust.md) § Implementation status today.

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
Web's own rich-render body fetch (a second `posts.get` beside the manager's)
decodes through the bound shaped face
(`fauna_client_core::post::decode_post_shaped_fetched_as`, wasm
`decodePostFetchedAs`) and paints the body only when it is `valid` — else the
manager's snapshot body renders — so the body under the manager's badge is
never an unbound one; no other app runs a by-id decode into a rendered body.
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
