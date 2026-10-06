# Serialization — canonical dag-cbor, CIDs, sign-over-CID — target state

Owns: serialization, wire-format, dag-cbor, fixed-size-byte-arrays
Status: ratified
Authority: owns the byte-level wire/hash/signature contract — canonical dag-cbor form, CID shape, embed-as-bytes, schema-evolution encoding rules, and the third-party verification recipe (security.md § Third-party verification recipe summarizes and defers here); sign-over-CID trust rationale → security.md; WS-RPC framing → transport.md; HTTP residue inventory → api-layers.md.

> **Audience:** every Rust/nest/client contributor working on the wire or
> on-disk bytes; third-party implementers writing a Fauna-compatible
> stack in any language with BLAKE3 + Ed25519 + a dag-cbor decoder.
> **Purpose:** the canonical reference for Fauna's serialization layer
> — canonical IPLD dag-cbor, the fixed CID shape, sign-over-CID
> envelopes with embed-as-bytes, schema-evolution discipline, and the
> language-agnostic verification recipe. Per-feature CDDL files in
> `libs/fauna-protocol/schemas/` author kind shapes; this doc enforces
> the *encoding* of any kind. `libs/fauna-cbor/` ships the canonical
> codec, CID type, pre-parse validator, and `SignedEnvelope`; the
> `ipld/codec-fixtures` conformance gate is green (commit
> `a312b720a4f8302c60f075aa3d33149967a4aa45`). See § Implementation
> status today.
> Sources: `libs/fauna-cbor/{src,tests}/`
> (design ratified 2026-05-15; tracked internally)

## Goal

Every byte that leaves Fauna — over the client↔nest WS-RPC connection,
the nest↔nest WS-RPC federation channel, the length-prefixed peer
channel riding the transport seam, the HTTP residue surfaces
(ActivityPub, byte transfer), on disk in CARv2 segments, in a CID URL,
or in a logged hash —
goes through one canonical encoder, one CID shape, and one signature
construction. Canonical bytes are reproducible bit-for-bit by any
conformant encoder; CIDs are content addresses with no alternate
representation; signatures cover the CID, never the bytes directly,
so verification needs only BLAKE3 + Ed25519 — never a CBOR encoder.

The load-bearing decision is **sign-over-CID with embed-as-bytes**:
the wire/disk shape carries the raw canonical bytes alongside the
envelope, so receivers verify by hash + signature without re-encoding.
This keeps every receiver — including third-party language
implementations — off the encoder-canonicality hook for verification.

## Canonical IPLD dag-cbor

Fauna implements the IPLD dag-cbor profile of RFC 8949 with the strict
restrictions called out by the IPLD spec. Every encoder/decoder on
the security path obeys all of:

| Rule | Encoding requirement |
|---|---|
| Map key sort | **Length-first, then bytewise ascending.** Not lexicographic. Two keys are compared first by encoded length; ties broken by bytewise compare on the encoded key bytes. |
| Map duplicates | Forbidden. Same key encoded twice in one map is non-canonical. |
| Integer encoding | **Shortest form.** Same rule applies to *every* length field — string lengths, array lengths, map lengths, byte-string lengths, tag IDs. A 1-byte extended length is only valid for value ≥ 24, 2-byte for ≥ 256, 4-byte for ≥ 65 536, 8-byte for ≥ 2³². |
| Floats | **Forbidden.** Major 7 with additional info 25/26/27 (half/single/double). NaN/Inf/finite all rejected. Fauna kinds never carry floats; integers + decimal-string + ratio shapes cover every numeric need. |
| Indefinite-length items | **Forbidden.** Major 2/3/4/5 with additional info 31. Encoders emit definite-length only; decoders reject indefinite. The major-7 break stop code is forbidden as a corollary. |
| Reserved additional-info | Values 28–30 are reserved across every major; rejected. |
| Tags | Only **tag 42** (CID link). Every other tag rejected on decode. |
| Trailing bytes | A complete top-level item is followed by EOF. Trailing bytes are non-canonical. |
| Nesting depth | ≤ 256 (matches the downstream decoder's depth guard). Hostile `[[[…]]]` shapes are rejected as non-canonical, not crashed-on. |
| Empty vs null containers | **An empty list / map / byte-string encodes as the canonical empty container (`[]` = `0x80`, `{}` = `0xa0`, byte-string `0x40`), never `null` (`0xf6`).** A `Vec<T>` / `Vec<u8>` / map field is *always* a container, so strict decode rejects `null` for it. Absence (`None`) is encoded *distinctly* — the field is omitted (`#[serde(skip_serializing_if)]`) or carries `null` — never an empty container. |

**Language-binding corollary (the nil-container `null` trap).** A
required list/map/byte-string field must never reach the wire as
`null`. The Go bridge's encoder sets `NilContainersAsEmpty`
(`bins/fauna-bridges/internal/dagcbor/codec.go`) so a nil Go
slice/map/`[]byte` encodes as the canonical empty container; Rust
encodes `Vec`/`BTreeMap` the same way. The corollary: a field whose
**absence is semantically meaningful** (`None` ≠ empty) must use a
*distinct optional encoding*, not a bare container that is "sometimes
nil" — in Rust an `Option<Vec<T>>` / `Option<ByteBuf>` (`None` →
omitted or `null`; see § Schema evolution rules), and in the Go bridge
a **pointer** (`*[]T`, `*[]byte`) whose nil still encodes as `null`
regardless of `NilContainersAsEmpty`. A bare nilable Go container
standing in for an `Option<…>` field is a wire bug: it would encode a
real `None` as an empty container. This was the `UID SEARCH ALL` /
plain-`EXPUNGE` failure class (a nil required list reaching nest as
`null` → strict-decode `ok=false`).

**Tri-state fields: `Option<Option<T>>` does not round-trip.** A nested
Rust `Option<Option<T>>` collapses to the same wire value for both
`None` (leave alone) and `Some(None)` (clear to null) — `serde` maps
both to a bare CBOR `null` with no discriminant to tell them apart, and
canonical dag-cbor has no second "absent vs. explicitly-null" signal to
recover it on decode (unlike JSON, where a custom deserializer can
distinguish a missing key from a present `null` key). Any RPC field
needing a genuine three-way choice — leave / clear / set — must NOT use
`Option<Option<T>>` on the wire. Two sanctioned shapes: split the
clearable knob into its own dedicated set/clear RPC (`Some(T)` sets,
`None` clears — there is no third "leave" state because the whole call
targets that one field; e.g. `fauna.bridges.set_catch_all_actor`,
`libs/fauna-protocol/src/bridge_routing.rs`), or model the tri-state
explicitly with a tagged enum (`enum FieldUpdate<T> { Leave, Clear,
Set(T) }`) when one RPC must carry several independently-clearable
fields at once. This is a wire-representation fact, not a schema-
evolution rule — it applies equally to a brand-new field, so it lives
here rather than in § Schema evolution rules.

**Fixed-size byte arrays: every serialized `[u8; N]` is a CBOR byte
string — the plain-derive array shape is a defect, never a wire shape
(re-ruled 2026-09-29).** A
32-byte actor id, a scope, entry or generation id, a hash, a nonce, a
key commitment: the Rust *field type* — never the field name — decides
what serde emits, and a bare `[u8; N]` or a plain-derive newtype over
one is a **tuple** to serde, so it lands as a CBOR array of N integers
(major 4: `0x98 0x20` + 32 shortest-form ints, 34–66 bytes for a real
key, whose bytes mostly need two) rather than the byte string (major 2:
`0x58 0x20` + 32 raw bytes, always 34) every `ByteBuf` field rides as.
The two shapes a raw-byte value may take:

| Rust field type | CBOR shape on the wire | Where |
|---|---|---|
| `ByteBuf`; `#[serde(with = "serde_bytes")]` (or the crate-local fixed-array module) on a `Vec<u8>`, a `[u8; N]`, an `Option<>` or `Vec<>` of one; a newtype whose serde impl goes through `serde_bytes` (`Cid`, `ActorId`, `ChannelId`, `SecretArray32`) | **byte string** (major 2) | every raw-byte field — the ~157 `actor_id: ByteBuf` protocol fields, the group plane's key material (2026-09-28), and every fixed-width id (2026-09-29) |
| `*Hex = String` alias (`push_events::ActorIdHex`) | **text string** (major 3): 64 lowercase hex chars | 8 push-payload fields — a JSON-bound push payload's spelling, not a dag-cbor one |

A bare `[u8; N]` with a plain derive is not a third shape but a
defect: its array encoding is refused at strict decode wherever a byte
string is expected (`schema mismatch: major type mismatch: expected
0x02 (byte string), got 0x98 (array)` — the codec names both shapes
and, for exactly this pair, points here: `libs/fauna-cbor/src/codec.rs`,
`map_decode_error`).

**Decision (2026-09-29, superseding 2026-09-21's "the divergence
stands"): one shape, tree-wide.** The
2026-09-21 ruling kept `ActorId` (and every sibling bare `[u8; 32]`)
on the array shape for one reason only: `ActorId` sits inside **signed
canonical payloads** — `ShareToken.author`
(`libs/fauna-core/src/share.rs`), `RecoveryKeyRegistration.actor_id`
and the succession statements (`libs/fauna-core/src/recovery.rs`), all
`canonical_encode`d and signed over their CID — and the account/group
planes' content-derived ids (`MintCore`, `GroupMintCore`,
`RosterEntryCore`) hash over their canonical encoding, so a flip
changes persisted canonical bytes, hence CIDs, hence every stored
signature and generation id, and no migration can re-mint one (a
succession statement exists precisely because its signer is gone):
`version-compatibility.md` § I1 / § I3. That reason is void once, and
now only: under `version-compatibility.md` § Dimension 2's fourth
ratified exception (the 2026-09-24 baseline reset) no Fauna
installation and no Fauna data exist anywhere, so none predates
the 2026-10 baseline. So the flip lands **before
the 2026-10 baseline** as an in-place change with no fallback — the
strict decoder refuses the array spelling, exactly as it refused the
byte-string spelling before — and the array shape is grandfathered
nowhere: **every `[u8; N]` — bare, `Option<[u8; N]>`, `Vec<[u8; N]>`,
or a newtype over one — that reaches a serde encoder on the wire, in a
signed canonical payload, in a plane-row value or at rest encodes as a
CBOR byte string.** The scope is the whole tree, not the account and
group planes alone: a per-plane flip would leave `ActorId`'s 22
protocol fields and the peer, segment and mail kinds as a permanently
frozen second family from the 2026-10 baseline on — the
two-families-forever outcome the 2026-09-21 rule (3) was written to
prevent for new types. The gains are the ones the group plane measured
on `Vec<u8>`: 34 B per id instead of up to 66, halving every id list
(the bounded mint's `member_ids`/`parents`, the seen-set's per-writer
elements, the roster), plus one shape for every caller — the Python
harness sends raw bytes everywhere (`list(raw)` retires), the Go
mirror's default byte-string encoding of a `[N]byte` is simply
correct, and the codec's array-vs-bytes hint becomes a one-directional
pointer. Decode-widening (accept both spellings) stays rejected on
this doc's own § Goal: one logical value would have two canonical byte
forms and therefore two CIDs. Out of scope: `Cid` (its own shape is
ruled at the end of this paragraph), the `*Hex` text form (a JSON push
payload's spelling), and a `[u8; N]` that never meets a serde encoder
(in-memory keys, `Zeroizing<[u8; 32]>` secrets, the CARv2 header's own
binary layout). **From the 2026-10 baseline on this ruling is frozen
like every other canonical byte:** a signed id's shape can never
change again, not at a major bump, because the stored signatures the
2026-09-21 ruling protected will then exist. **`Cid` encodes as an IPLD link (user-ruled 2026-10-01, dag-cbor everywhere): tag 42 over a byte string of `0x00` followed by the 36 CID bytes, with no byte-string fallback — the strict decoder refuses the bare byte string — so every content address in a Fauna record is a link a generic IPLD tool can follow.** It landed in place before the 2026-10 baseline under the same exception as the flip above and is frozen with it. It covers both codecs: `ContentHash` is `Cid` under the raw codec (§ CID shape), so every chunk, blob and video-segment content hash a record carries is a raw-codec link too — standard IPLD, where a link to an opaque byte stream carries codec `0x55`. Deliberately left as bytes, because none of them is a serde-encoded `Cid`: the signed envelope's flat 100-byte `[CID ‖ signature]` buffer (§ Embed-as-bytes for signed payloads — an opaque byte string whose first 36 bytes are a CID, not a field), the signature input (the 36 raw bytes `Cid::as_bytes` returns, § Sign-over-CID), and every boundary that stores or crosses only the 32-byte digest (DB columns, FFI/WASM hex — § Codec-parametric Cid).

**Variable-length byte fields: the same one shape (user-accepted 2026-10-01, the baseline survey's finding A2).** The decision above named `[u8; N]`; a plain-derive `Vec<u8>` is the same defect in a different serde costume — a *sequence* of `u8`s rather than a tuple, landing as a CBOR array of integers (every byte of 24 or more costing two) where the bytes belong in one byte string. So the rule is one sentence for every raw-byte field: **every `Vec<u8>`, `Option<Vec<u8>>` and `Vec<Vec<u8>>` — and any newtype over one — that reaches a serde encoder on the wire, in a signed canonical payload, in a plane-row value or at rest encodes as a CBOR byte string**, through `#[serde(with = "serde_bytes")]` on the field (`Vec<Vec<u8>>` through `fauna_core::byte_array::vec_of_bufs`), a `ByteBuf`, or a newtype whose serde impl goes through `serde_bytes`. Sealed ciphertexts (the calendar, contacts and conversation record envelopes, the key blob's `encrypted_key`), signatures (the recovery and succession statements, the nest-rotation proof, the moderation and scoring records), the chunk manifest's `sealed_hashes` and every federation record body are all in it. It lands in place before the 2026-10 baseline under the same exception and with the same strictness as the fixed-width flip — no decode-widening, the strict decoder refuses the array spelling — and is frozen with it, for the same reason: a signed or content-addressed byte field's shape can never change once stored signatures exist. Out of scope, as for the fixed-width rule: a struct that never meets the canonical encoder — a view model or FFI/WASM record that crosses to an app as a JS or Swift/Kotlin value (there `serde_bytes` would change a JS `Array` into a `Uint8Array` under the app's feet), a SQL row type whose bytes go to a BLOB column, a JSON-only payload (where the attribute is a no-op) — and text that happens to travel as bytes. The guard below enforces it in code.

**Mechanism and rules.** The attribute is the group plane's,
`#[serde(with = "serde_bytes")]` on the field — one spelling for every
raw-byte field, so a serialized `[u8;` without it is what a reviewer
greps for — wherever the pinned `serde_bytes` (0.11.19 in `Cargo.lock`)
accepts the field type; `Vec<[u8; N]>` and any form it refuses go
through one crate-local generic `with =` module in `fauna_core`. A
serde-deriving newtype over `[u8; N]` (`ActorId`, `ChannelId`) carries
the byte-string impl in its own `Serialize`/`Deserialize`, so every
field of that type inherits it without an attribute. **The guard:**
neither additive-evolution gate can see this class of change (the Rust
gate keys on the normalized type token, which an attribute or a
hand-written impl leaves untouched, and the CDDL files never spelled
these fields as arrays), so `fauna_cbor::encode_canonical` runs, under
`debug_assertions`, a no-op serde pre-walk that refuses a non-empty
tuple or sequence whose every element is a `u8` — the exact serde
signature of a plain-derive `[u8; N]` (a tuple) or `Vec<u8>` (a
sequence) — naming the field and the rule it broke; every test that
encodes a missed field fails by name, and rule (3) below is enforced in
code. An empty sequence says nothing about its element type and passes;
a genuine list of small integers in a serialized type takes a wider
element type. Rules for new fields collapse to one: **a raw-byte field
is a byte string** — `Vec<u8>` with the attribute, or `ByteBuf`, for
variable length; `[u8; N]` with the attribute, or a byte-string
newtype, for fixed — and a newtype over raw bytes that will ever reach
the wire or a signed payload ships with its byte-string impl from day
one. Language-binding corollary: a Go mirror
of a `[N]byte` field encodes it as the byte string the Go codec
(`bins/fauna-bridges/internal/dagcbor/codec.go`) already emits by
default (no mirrored kind carries such a field today); a Python caller
sends `raw`. Pinned by `libs/fauna-protocol/tests/wire_byte_array_shape.rs`
(the two shapes, the array spelling refused in both directions, for a
fixed-width id and a variable-length sealed payload alike),
`libs/fauna-cbor/tests/byte_array_guard.rs` (the guard, both shapes) and
`libs/fauna-core/src/identity.rs`'s test module (the frozen `ActorId`
bytes — a byte string). Built state: § Implementation status today.

### Conformance corpus

`ipld/codec-fixtures` at commit
**`a312b720a4f8302c60f075aa3d33149967a4aa45`** is the canonical-encoding
conformance corpus. The same corpus gates the Go mail bridge. Both
the Rust `fauna-cbor` and Go `dagcbor` packages must round-trip every
applicable fixture byte-for-byte; failure to do so blocks merge.

Fixtures classified `contains-float` or `contains-tag` (non-tag-42)
are skipped — the canonical profile forbids both.

Reference: `libs/fauna-cbor/tests/codec_fixtures.rs`,
`bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT`.

## CID shape

Every CID Fauna emits is exactly 36 bytes binary, with a fixed shape:

```
byte  0: 0x01    CID version 1
byte  1: <codec> 0x71 dag-cbor OR 0x55 raw (codec-parametric per Layer 3)
byte  2: 0x1e    multihash code = BLAKE3-256
byte  3: 0x20    multihash digest length = 32
bytes 4-35:      32-byte BLAKE3 digest
```

Two codecs are accepted, both fixed-shape and 36 bytes long:

- **dag-cbor (`0x71`)** — content addresses for canonical dag-cbor blocks. Use `fauna_cbor::Cid::DAG_CBOR` and the `*_dag_cbor` constructors (`Cid::of_dag_cbor` / `Cid::from_digest_dag_cbor`). Every signed kind, every CARv2 block CID, every wire/disk envelope `cid` field uses this codec.
- **raw (`0x55`)** — content addresses for opaque (non-dag-cbor) byte streams. Use `fauna_cbor::Cid::RAW` and the `*_raw` constructors (`Cid::of_raw` / `Cid::from_digest_raw`). `ContentHash` (the chunk / blob / video-segment content-address newtype in `libs/fauna-core/src/data.rs`) is `pub type ContentHash = fauna_cbor::Cid;` keyed under this codec — see § Codec-parametric Cid below.

Other hash codes or digest lengths are not accepted on decode. Other codecs (dag-pb, sha2-256) are not part of the Fauna contract — encountering one in a Fauna-typed slot is a hard decode error, not a fallback. `Cid::from_bytes` rejects every codec byte that isn't `0x71` or `0x55`.

The user-facing string form is **base32-lower multibase** (`b` prefix
+ base32 body, lowercase). All Fauna URLs, logs, UI cells, and admin
dumps render CIDs in this form. Receivers MUST accept the base32-lower
form on decode and MUST reject any other multibase prefix —
`Cid::from_base32` returns a decode error for any non-`b`-prefixed
string (`libs/fauna-cbor/src/cid.rs`).

The string starts with a multibase `b` followed by a base32 encoding
of the 4-byte header + 32-byte digest. The 4-byte header varies in
exactly one byte (codec, position 1: `0x71` dag-cbor or `0x55` raw),
so dag-cbor CIDs share one prefix shape (`bafyr4i…` — see the
worked example below) and raw CIDs share a different prefix shape.
A CID that doesn't match one of these two prefix shapes is
structurally not Fauna's.

### Worked example

Encode `{}` (the empty map) as canonical dag-cbor:

```
canonical bytes (1 B): a0
```

Compute the BLAKE3-256 digest of those bytes:

```
blake3 = 1f94cbf313b3ce23257a7251ea0fc95a24556ea611e4f8f475e549971baedb02
```

Assemble the 36-byte CID (`0x01 0x71 0x1e 0x20` header + digest):

```
36-byte CID:           01711e20 1f94cbf313b3ce23257a7251ea0fc95a24556ea611e4f8f475e549971baedb02
  header (4 B):        01711e20
  digest (32 B):       1f94cbf313b3ce23257a7251ea0fc95a24556ea611e4f8f475e549971baedb02
```

Multibase-encode as base32-lower:

```
bafyr4ia7stf7ge5tzyrsk6tskhva7sk2erkw5jqr4t4pi5pfjglrxlw3ai
```

A second example, encode `{ "msg": "hello" }`:

```
canonical bytes (11 B): a1636d73676568656c6c6f
36-byte CID:            01711e20febe5fc91b76890d69a8a1c90e9f2462073968cfd96e8b8d79eac50e242891c2
base32-lower string:    bafyr4ih6xzp4sg3wregwtkfbzehj6jdca44wrt6zn2fy26pkyuhcikeryi
```

Both examples are reproducible by `cargo test -p fauna-cbor cid_roundtrip`
and by any conformant dag-cbor encoder.

### Codec-parametric Cid (Rust API)

`fauna_cbor::Cid` is codec-parametric (landed in CBOR-DAG-everywhere
Layer 3 — Task 3.7). Public surface:

- Codec constants — `Cid::DAG_CBOR: u8 = 0x71`, `Cid::RAW: u8 = 0x55`.
- Paired constructors — `Cid::of_dag_cbor(bytes)` / `Cid::of_raw(bytes)`
  hash the input and assemble; `Cid::from_digest_dag_cbor(digest)` /
  `Cid::from_digest_raw(digest)` assemble from an existing 32-byte
  BLAKE3 digest.
- `Cid::digest(&self) -> [u8; 32]` returns the multihash digest portion
  (the canonical helper for the 32-byte boundary strip; replaces the
  prior `as_bytes()[4..]` pattern at the 6+ boundary sites Layer 2
  Task 2.8 surfaced).
- The 36 CID bytes have one layout regardless of codec; the codec byte
  at position 1 is the only field that varies. Under serde each rides
  as a tag-42 link (41 encoded bytes: the tag, the byte-string head,
  `0x00`, the 36 bytes — § Canonical IPLD dag-cbor, the raw-byte shape
  decision).

`ContentHash` is collapsed to `pub type ContentHash = fauna_cbor::Cid;`
(raw codec) in `libs/fauna-core/src/data.rs` — Layer 3 Task 3.8. Every
kind embedding `ContentHash` carries it as a raw-codec link, like any
other `Cid`. DB columns stay 32-byte
(boundary uses `cid.digest()` at write, `Cid::from_digest_raw` at read).
Client-facing FFI / WASM surfaces stay 32-byte hex / `Vec<u8>` on the
boundary.

## Sign-over-CID

The signature on a Fauna-signed payload covers `cid.as_bytes()` — the
36-byte CID — **NOT** the canonical content bytes. Verification is
two independent steps that DO NOT involve a CBOR encoder:

1. `blake3(received_bytes) == envelope.cid.multihash` (the digest portion).
2. `ed25519_verify(envelope.sig, envelope.cid.bytes, pubkey)`.

Either step failing rejects the payload. The bytes the receiver
actually decodes for application use are validated to hash to the
same CID the signer signed; the signature pins that CID to the
claimed key.

This is the "Camp A" model used by ATProto and Ceramic. Encoder
canonicality matters only on the *publishing* side — bytes that
aren't canonical hash to a CID nobody else recognizes, so the
publisher's content is unfindable. **It does not enter the
verification path.** Independent ATProto PDS implementations
interoperate without coordinating their CBOR libraries because
verification is hash + signature, never re-encode + compare.

Reference: `libs/fauna-core/src/encoding.rs::verify_envelope` — the production
door, which runs step 2 through the strict primitive
`fauna_core::identity::verify_detached` (`security.md` § Key management
invariants: the key here arrives *inside* the payload, so a
permissive verify would let a small-order key make an all-zero signature
binding). The raw two-step recipe lives at
`libs/fauna-cbor/src/envelope.rs::SignedEnvelope::verify_permissive`, whose name
says why production never calls it directly.

### Delegated authoring — signer ≠ author (ratified 2026-07-23; chain verify BUILT 2026-07-23, F2.2 slice 1; ALL SIX call sites wired — the four `Post`/`Tombstone` ones 2026-07-23, the two `Profile` ones 2026-07-29)

An authored kind (`Post`, `Tombstone`, `Profile`) is normally signed by its
author's identity key: the value's `signer_public_key()` returns the author
key, and the two-step recipe above verifies against it. A **delegated**
payload is instead signed by a sub-key `K` that a `DeviceAuthorization`
([`../behavior/devices.md`](../behavior/devices.md) § DeviceAuthorization —
identity-signed, capability-scoped) authorizes for that author. The
delegation cert **travels with the payload** (the optional `signer_auth`
field of the embed-as-bytes wire, § below) — verification stays
self-contained, never a network resolve. The chain (mirroring the shipped
`verify_key_blob_signature`):

1. The value's envelope verifies under the **author** key → accept as
   directly authored (an attached cert is ignored). Otherwise:
2. `signer_auth` is present, and the cert's own envelope verifies under
   `cert.actor_id` (two-step recipe, the cert is itself sign-over-CID).
3. `cert.actor_id == value.author`.
4. The kind's required capability (`Post` for Post + post-Tombstone;
   `UpdateProfile` for Profile) — or `All` — ∈ `cert.capabilities`.
5. `cert.expires_at`, when present, ≥ the value's `created_at`
   (signer-asserted; the residual is accepted and documented at the
   instantiation, `atproto-pds-full.md` D10).
6. The value's envelope verifies under `cert.device_key` (two-step recipe).

**Fail-closed by construction:** `Signed` impls for authored kinds keep
returning the *author* key, so a verifier that runs only the plain two-step
recipe — an old peer, an un-upgraded call site, a third-party implementation
that ignores `signer_auth` — **rejects** a delegated payload; it can never
accept one without validating the chain. The chain verify is one shared
`fauna-core` helper beside `verify_envelope`; call sites opt in explicitly.
**Built 2026-07-23:** `verify_authoring_envelope` +
`AuthoringOrigin::{Direct, Delegated}` (`libs/fauna-core/src/encoding.rs`),
the 6-step chain above verbatim, with the fail-closed property pinned by
test. **All four call sites now wired (2026-07-23):** `classify_encrypted_post`
(`bins/fauna-nest/src/storage/sealed.rs`), the federation
`post.forward`/`post.delete` receive verifies (`bins/fauna-nest/src/federation_handlers.rs`
— the delete leg via `decode_tombstone`, which calls the chain internally),
and the shared client read `fauna_client_core::post::decode_post`
(`libs/fauna-client-core/src/post.rs`) — detail at
[`../behavior/atproto-pds-full.md`](../behavior/atproto-pds-full.md) D10.
**The `Profile` sites followed 2026-07-29 (F2.3):** `fauna_core::encoding::decode_profile`
— the one read face `fauna-client-profile` on all 7 apps, the ActivityPub actor
serve and the ATProto projection all share — and the ingest gate
`profile_handlers::ingest_profile_core`. Two, not four: a profile has no
federation-forward path. Both require **`UpdateProfile`**, never `Post`, so a
cert minted for posting alone cannot rewrite an account's profile; and step 5's
operand is the profile's own `updated_at`, the field a `Profile` carries in
place of a `created_at`.

### Why not sign-over-bytes

Signing canonical bytes directly puts the encoder back in the security
path: every verifier must canonically re-encode the received content
to recompute the hash. Receivers whose CBOR library produces
slightly-different bytes silently fail verification on inputs every
other receiver accepts — an invisible bug class that reads like
cryptography and is actually encoder discipline. Sign-over-CID makes
multi-language compatibility a wire-spec property, not a per-library
discipline property.

## Embed-as-bytes for signed payloads

Wire and disk payloads for signed content carry the raw canonical
bytes as a CBOR byte string (major type 2), alongside the envelope:

```
{
  envelope: { cid: <Cid>, sig: <bytes 64> },
  bytes:    <byte string: canonical dag-cbor of the content>,
}
```

The diagram above is the *logical* shape. On the concrete wire, `envelope`
is **not** a nested map — it is a single CBOR byte string (major type 2) of
**exactly 100 bytes: the 36-byte CID followed by the 64-byte Ed25519
signature**. So the encoded map has two byte-string values, `bytes` and
`envelope`, and (canonical key order being length-first) `bytes` sorts before
`envelope`. The envelope is a flat buffer rather than a nested struct because
`fauna_cbor::SignedEnvelope` is not itself `Serialize` (its Cid + sig are raw
byte arrays) and the cross-language fixtures already use the
`[36-byte CID ‖ 64-byte sig]` layout. Reference impl + the authoritative wire
type: `libs/fauna-core/src/encoding.rs::EmbedAsBytes`
(`from_signed` / `into_signed`). Any non-Rust encoder (the native apps build
+ decode through shared Rust precisely to avoid re-deriving this) must match
those 100 bytes exactly.

**Optional third field `signer_auth` (ratified 2026-07-23; BUILT 2026-07-23,
F2.2 slice 1 — `EmbedAsBytes::signer_auth`, `libs/fauna-core/src/encoding.rs`):**
a delegated payload (§ Delegated authoring above) additionally carries the
authorizing `DeviceAuthorization` as `signer_auth` — itself an embed-as-bytes
value, nested. The field is
`#[serde(default, skip_serializing_if = "Option::is_none")]`-additive: a wire
without it is **byte-identical** to the two-field shape above (pinned by
test), and a reader that predates it drops the field and fail-closed-rejects
the delegated payload at signature verify. Canonical key order stays
length-first: `bytes`, `envelope`, `signer_auth`.

The receiver:

1. Reads `envelope.cid` and `envelope.sig` and `bytes`.
2. Verifies via the two-step recipe in § Sign-over-CID (or, when the
   envelope does not verify under the author key and `signer_auth` is
   present, the delegated chain in § Delegated authoring).
3. **After** verification succeeds, decodes `bytes` as dag-cbor per
   the kind schema using their language's decoder of choice.

Structural decode is post-verification. A malformed inner payload
that nonetheless passes hash + signature is a publisher bug, not a
receiver-side security problem — the receiver knows exactly which
key claimed which 36-byte CID.

### Why not embed-as-value

Embed-as-value (inner payload as a CBOR map *inside* the outer
envelope) would require every receiver to canonically re-encode the
inner map to recompute its CID — the encoder-back-in-the-security-path
problem applied to every signed kind on the wire. Embed-as-bytes is
the wire-shape choice that delivers on sign-over-CID's promise; the
cost is a few bytes per payload (byte-string length prefix +
structural overhead).

## Third-party verification recipe

Any language with BLAKE3 + Ed25519 + a dag-cbor decoder can verify
Fauna-signed payloads in four steps:

```
1. Receive (envelope, bytes) over the wire.
2. Compute h = blake3(bytes); require h == envelope.cid.multihash.
3. ed25519_verify(envelope.sig, envelope.cid.bytes, pubkey).
4. Decode bytes as dag-cbor per the kind schema.
   (Their decoder; needn't be canonical.)
```

Step 2 needs a BLAKE3-256 hasher. Step 3 needs an Ed25519 verifier
operating on the raw 36 CID bytes (not a wrapped or re-encoded form).
Step 4 needs a dag-cbor decoder *for that language's data model* —
canonical or relaxed both work for *post-verification* application
decode, since verification is already done.

Reference implementations:

- Python: `tests/common/envelope.py` (`canonical_dagcbor_bytes`,
  `cid_of_dag_cbor`, `sign_dagcbor_envelope` — the signing-side mirror
  of the four steps, used by the e2e harness)
- Go: `bins/fauna-bridges/internal/dagcbor/`
- Rust: `libs/fauna-cbor/src/envelope.rs::SignedEnvelope::{sign,verify_permissive}`

This recipe is the contract third-party implementers code against, and
this section is its **single owner**:
`docs/goal/architecture/security.md` § Third-party verification recipe
carries a summary and defers here for the byte-level steps and the
reference implementations.

## Schema evolution rules

Wire types evolve under three constraints that keep parallel
deployments interoperable through schema change:

1. **Optional fields only.** New fields use `#[serde(default)]` (or
   the language equivalent — `omitempty` in Go, defaulted Optional in
   Python). Decoders ignore unknown fields; encoders omit absent
   defaults. A field added to a kind in version N+1 is decodable by a
   version-N receiver as the default value.
2. **Never reuse field names or envelope integer tags.** A removed
   field's name is reserved; its integer envelope tag (if it had one)
   is reserved. Reuse means a version-N receiver decodes a version-(N+1)
   payload as a different shape — silent corruption.
3. **Wire-breaking changes require a NEW kind, not a version bump on
   the existing one.** Splitting a field into two, narrowing a type,
   reordering a tagged union — anything a version-N decoder would
   misinterpret — ships under a new kind string, with the old kind
   running in parallel until deprecation. (There is no per-kind version
   floor: within a major version, peers negotiate down and never refuse
   a kind by version — deprecation signaling is capability-set-based,
   owner: `version-compatibility.md` § Dimension 3.)

Per-feature CDDL files in `libs/fauna-protocol/schemas/` are the
authoring surface for kind shapes; this doc enforces the encoding +
evolution discipline that any kind file must respect.

## Forbidden in the security path

Code that participates in CID matching or signature verification —
in any language implementation of the Fauna stack — must NOT:

- Use `decode_relaxed` (or any equivalent permissive decoder). Relaxed
  decode is **debug-only**. It exists for inspection tools; it is
  forbidden in any path that hashes content, verifies a signature, or
  emits a CID downstream. (As implemented today `decode_relaxed` is an
  alias for `decode_strict` — `libs/fauna-cbor/src/codec.rs` — no
  permissive backend is wired; the *prohibition* is the contract, and
  an inspection tool needing true relaxed decode would add the backend
  behind the same hidden API.)
- Accept floats. Major 7 / additional info 25/26/27 is rejected on
  decode and never produced on encode.
- Accept indefinite-length items. Major 2/3/4/5 / additional info 31
  is rejected; the major-7 break stop code is rejected.
- Accept custom tags. Anything other than tag 42 is rejected.

### How decode-strictness is actually enforced (Rust)

In the Rust implementation, decode-strictness is enforced by the
**`fauna-cbor` pre-parse validator** at
`libs/fauna-cbor/src/canonical.rs`, NOT by the upstream
`serde_ipld_dagcbor` codec on its own. This is a deliberate
architectural choice with consequences for any third-party
implementation.

**What upstream catches on its own, per axis — measured, at the pinned
`serde_ipld_dagcbor` 0.6.4 / `cbor4ii` 0.2.14.** The table below is not read
off a changelog: each row is asserted by
`libs/fauna-cbor/tests/upstream_decoder_baseline.rs`, which calls raw
`serde_ipld_dagcbor::from_slice` with the validator deliberately *not* in
front of it. (Observing the dependency by *calling* it is the only sanctioned
way to learn this — reading its source is forbidden outside the pinned
containment venue, `release-integrity.md` § *Reviewing untrusted source
without being subverted*.) When a row here goes stale, that test fails.

| Canonical-form axis (`canonical.rs` header) | Upstream 0.6.4 alone |
|---|---|
| Non-shortest-form integer length-encoding | **accepts** — validator-only |
| Map keys not in length-first-then-bytewise order | **accepts** — validator-only |
| Duplicate map keys | **target-dependent** — see below |
| Floats | rejects |
| Tags other than 42 | rejects |
| Indefinite-length items | rejects |
| Reserved additional-info values (28–30) | rejects |
| Trailing bytes after a complete top-level item | rejects |

Two corrections this measurement forced, both previously stated the other way
here and mirrored into `security.md`:

- **Tags other than 42 are rejected, not "decoded as opaque tagged values".**
  At this pin a non-42 tag is an error on every target probed — untyped node
  and plain integer alike.
- **Duplicate map keys are not flatly accepted.** The outcome depends on the
  *Rust target type*, because part of the checking lives in the target's
  `Deserialize` impl rather than in the decoder: an untyped `Ipld` node
  rejects them, a derive-generated struct rejects them (serde's own
  duplicate-field check), and a **plain map target silently takes the last
  value**. So issue #61's reporter and this document's older text were each
  right about a different target type, and neither generalises.

**That target-dependence is the architectural point.** Upstream's strictness
varies with what you decode *into*; the pre-parse validator's does not — it
walks bytes before a target type exists. The tree's one plain-map decode
target, `libs/fauna-mls/src/engine.rs`'s snapshot read, sits on the security
path and is covered by the validator rather than by upstream — which is the
whole reason the validator, not the codec version, is the boundary.

The wider class is upstream's own public record: the DAG-CBOR spec
§ Strictness lets decoders relax key order and shortest-form rules by
default, and upstream closed the class itself — `ipld/serde_ipld_dagcbor`
issue #61 (filed against 0.6.4, 2026-06), the strictness PR series
#55/#57/#58/#64 (2026-06/07), and release 0.7.0 (2026-08-03), whose
changelog reads "deserialization is now as strict as possible" with a
`less-strict-decoding` opt-out. **For every decode routed through
`fauna-cbor`**, the pre-parse validator stays the security boundary
whichever codec version is pinned.

**Six production sites bypass `fauna-cbor` entirely** — they call
`serde_ipld_dagcbor::from_slice`/`from_reader` directly, with no validator
in front, so on those the two surviving axes (non-shortest-form integers,
map key order) have no defence at all. Each is safe today for a
site-specific reason, not because the validator covers it:

- `libs/fauna-carv2/src/v1.rs` — `V1Header` (remote CAR import): length-bounded
  by `MAX_RECORD_LEN` before decode, `version`-checked after, and no CID
  derives from it.
- `libs/fauna-bridge-atproto/src/reverse_translate.rs` — `PostRecord` and
  `ProfileRecord`: decodes bytes the bridge itself encoded, so canonicity of
  the wire form is moot.
- `libs/fauna-bridge-atproto/src/record_refs.rs` (two sites) and
  `permission_set.rs` — untyped `Ipld` targets: reject duplicate map keys per
  the table above, and are best-effort by contract — a miss lands only on the
  record's own author, never on another account.

A seventh raw `from_slice`/`from_reader` outside `libs/fauna-cbor/` must
re-argue safety the same way rather than inherit the boundary claim by
default; `scripts/check_dagcbor_bypass_ratchet.py` pins the current set so
one can't slip in unnoticed.

**Test-only sites in `libs/fauna-archive` (three files, four call sites,
argued 2026-09-07).** The reusable parser crate depends on crates.io only —
no `fauna-*` crate, `fauna-cbor` included (`behavior/archive-import.md`
§ Architectural rules) — so its round-trip tests (`src/model.rs`'s timestamp
wire-shape test, `tests/model_roundtrip.rs`, `tests/facebook_golden.rs`'s
summary and entity round trips) decode with raw `serde_ipld_dagcbor::from_slice`.
Every one decodes bytes the same test just encoded — the self-encoded case —
and the library itself never decodes anything: the archive folder's
`model/*.cbor` files are written and read by `fauna-archive-import-machine`
through `fauna_cbor::encode_canonical` / `decode_strict`, so the validator
stays in front of every at-rest read of that model. A production decode
added to `fauna-archive` would need its own argument here, or a
`fauna-cbor` dependency the crate deliberately does not have.

#### The 0.7.0 bump: gated, and not urgent (decision 2026-08-24)

**Decision: do not bump today.** Two independent reasons, either sufficient.

1. **Procedurally gated.** For a `0.x` crate, 0.6.4 → 0.7.0 is a
   Cargo-semver-**major** bump, and `release-integrity.md` § *Dependency
   currency* rules that major bumps are gated on the containment venue,
   producing "only a **candidate inventory** (manifest/index metadata —
   versions, never code)" until that venue is pinned. It is not pinned.
2. **It buys nothing on the security path for decodes routed through
   `fauna-cbor`.** Per the table above, the only axes upstream 0.6.4 misses
   are non-shortest-form encoding and key order — both already caught by the
   validator on that path, target-independently. The six sites enumerated
   above sit outside `fauna-cbor` and are unaffected by this reasoning; each
   is safe today for the site-specific reason given there, independent of
   the codec version. So on the validator-covered path, 0.7.0 is *redundant*
   defence, not new defence. It is currency, not a fix.

Against that, the bump carries real cost to schedule deliberately: the
`DecodeError` variants matched by name in `libs/fauna-cbor/src/codec.rs`
move with `cbor4ii` 1.x; 0.7.0 serialises `-0.0` as `0.0`, which touches the
no-floats rule's reasoning; and `canonical.rs`'s `MAX_NESTING_DEPTH`
justifies its 256 by matching `cbor4ii::SliceReader`'s `step_in` limit — a
premise a `cbor4ii` major re-opens.

**Candidate inventory** (index metadata only, gathered 2026-08-24):

- Target `serde_ipld_dagcbor` 0.7.0 (MSRV 1.81 — inert; the pin is nightly
  well past it), pulling `cbor4ii` 1.2.2 in place of 0.2.14.
- **No new transitive crates.** Both versions' runtime dependency sets are
  identical apart from the `cbor4ii` requirement itself (`ipld-core`,
  `scopeguard`, `serde`). `cbor4ii`'s only requirement change is its
  *optional* `half` (`^1` → `^2`), which is inert here: that feature is off,
  and `half` is already in the lock at 2.7.1 via another path.
- Edit sites when it is unblocked: one version-carrying declaration
  (`Cargo.toml` `[workspace.dependencies]` — every consumer now inherits it),
  two exact-version `cargo vet` exemptions to regenerate
  (`supply-chain/config.toml`, `serde_ipld_dagcbor` and `cbor4ii`), the error
  mapping in `codec.rs`, and the `MAX_NESTING_DEPTH` rationale.
- `less-strict-decoding` must stay **off** if the bump ever lands — it would
  re-open exactly the axes § *Forbidden in the security path* closes.

`fauna-cbor::decode_strict` runs `validate_canonical(bytes)` before
handing bytes to `serde_ipld_dagcbor::from_slice`, catching every
non-canonical axis the upstream misses. The validator walks the raw
byte stream once, no serde involvement, and returns a typed
`DecodeError::NotCanonical { reason }` for any violation.

**Implication for other-language implementations.** A language
implementation that wires a stock dag-cbor library directly into the
security path (without a pre-parse validator like `fauna-cbor`'s)
will have the same silent-acceptance gap. Two paths fix it:

1. Implement equivalent canonical-form checks themselves (port
   `validate_canonical` to that language's byte-walking idiom).
2. Rely on the pre-parse validator pattern: every decode goes through
   a validator-then-decoder pipeline, never raw decode.

Without one of those, the implementation is a *publishing-side
problem-creator*, not a *verification-side problem-detector*: it
silently accepts non-canonical bytes from peers, then re-publishes
non-canonical bytes that other receivers reject. The inter-op damage
is one-way and asymmetric — exactly the failure mode embed-as-bytes
+ sign-over-CID was designed to prevent on the verification side, now
re-introduced on the publishing side.

This is a documented architectural property of any IPLD-dag-cbor
deployment whose codec library is split across multiple
implementations. Fauna addresses it on the Rust side via the pre-parse
validator; equivalent measures are required wherever a Fauna-typed
byte stream is hashed, signed, stored, or forwarded.

## WS-RPC wire-conformance guardrails

The WS-RPC request/reply surface
(`bins/fauna-bridges/internal/wsrpc/methods.go` ↔
`libs/fauna-protocol/src/bridge_routing.rs`) is the widest *hand-mirrored*
wire boundary in the codebase — every field is written twice, once per
language — so the two invariants above (§ *Canonical IPLD dag-cbor*: the
**Floats** and **Empty vs null containers** rows, and the nil-container
`null`-trap corollary) are enforced there by an **executable harness**,
not by discipline:

- **Go encode side** — `internal/wsrpc/wsrpc_conformance_test.go` walks the
  full request/reply/nested/push type set. `TestWsrpcNoFloatFields` bans
  float fields at the *type* level (the codec already rejects float
  *bytes*; the only remaining break is a new `f64`/`f32` field
  definition). `TestWsrpcNonOptionalContainersEncodeEmptyNotNull` asserts
  every non-`Option`, non-`omitempty` list/map field encodes `[]`/`{}`,
  never `null` (it gates on the live encMode, so it became a hard
  assertion the moment `NilContainersAsEmpty` landed).
- **Rust decode side** — `libs/fauna-protocol/tests/wsrpc_nil_container_contract.rs`:
  `decode_strict` accepts `[]` and rejects `null` for a non-`Option` `Vec`.

Two WS-RPC-specific subtleties on top of the empty-vs-null rule:

1. **The Option/absence encoding has two correct shapes — do not assume one.**
   Both mirror a Rust `Option<…>`; a *bare* nilable Go container standing in
   for one is the wire bug the corollary above warns about.

   | Rust | Go | None on the wire |
   |---|---|---|
   | `#[serde(default, skip_serializing_if="Option::is_none")] Option<T>` | `*T` + `omitempty` | key **absent** |
   | plain `Option<T>` (no skip) | `*T` *without* `omitempty` | key present, value **`null`** |

2. **Latent asymmetry — `MailboxStateEvent.flags`** is a Go `[]string`
   tagged `omitempty` but mirrors a Rust *non-`Option`,
   non-`#[serde(default)]`* `Vec<String>`: an empty `flags` would be
   *omitted* by Go and then rejected by Rust strict decode. Latent today
   (only nest encodes this push; the Go side only decodes it) — if it ever
   becomes bidirectional, drop the `omitempty` (or make the Rust field
   `#[serde(default)]`) first.

`transport.md` owns the envelope framing; this guardrail covers the payload shape.

## Crate layout (Rust)

```
libs/fauna-cbor/                    # canonical codec + CID + envelope
├── Cargo.toml                      # serde_ipld_dagcbor, blake3, ed25519-dalek, multibase
├── src/
│   ├── lib.rs                      # public re-exports
│   ├── codec.rs                    # encode_canonical, decode_strict, decode_relaxed (hidden; aliases decode_strict today)
│   ├── canonical.rs                # pre-parse byte-stream validator (the strictness gate)
│   ├── cid.rs                      # Cid type — fixed v1+dag-cbor+blake3-256+32-byte shape
│   ├── envelope.rs                 # SignedEnvelope::sign / verify_permissive
│   └── error.rs                    # EncodeError, DecodeError, VerifyError
└── tests/
    ├── codec_basic.rs              # round-trip + canonical shape
    ├── codec_fixtures.rs           # ipld/codec-fixtures conformance gate
    ├── decode_strict_rejects.rs    # negative tests for every canonical-violation axis
    ├── upstream_decoder_baseline.rs # per-axis matrix of what raw serde_ipld_dagcbor rejects on its own
    ├── cid_roundtrip.rs            # CID byte layout + base32-lower string form
    ├── cid_golden_cross_language.rs # pinned CID strings shared with the Go byteplane test
    ├── cross_language_interop.rs   # Rust↔Go byte-parity on shared shapes
    └── envelope.rs                 # sign + verify positive/negative paths
```

`fauna-protocol`'s own `codec` module delegates `encode_canonical` /
`decode_strict` to `fauna-cbor` (landed in
`feat(rust,nest,plan): CBOR-DAG everywhere Layer 1 — foundations`). All other crates that touch the wire — `fauna-client`,
`fauna-peer`, the `bins/fauna-storage` binary (a reserved placeholder — see
below) — depend on `fauna-cbor` for canonical encode and `SignedEnvelope` for
signed content. ⚠ `bins/fauna-storage` is **not implemented**: it is a
workspace member reserved for the planned S3-compatible backup-storage daemon
whose `main` prints a notice and exits, so its "depends on `fauna-cbor`" row
is a statement about the target binary, not about shipped behavior.

## Test surface

| Layer | Where | What it covers |
|---|---|---|
| Canonical encoder round-trip | `libs/fauna-cbor/tests/codec_basic.rs` | encode → decode preserves; structural shape sanity |
| Canonical conformance vs. IPLD corpus | `libs/fauna-cbor/tests/codec_fixtures.rs` | byte-parity against `ipld/codec-fixtures` |
| Decode-strictness rejection | `libs/fauna-cbor/tests/decode_strict_rejects.rs` | every canonical-violation axis (sort, dup, shortest-form, float, indefinite, non-42 tag, …) returns typed `NotCanonical` |
| Upstream decoder baseline | `libs/fauna-cbor/tests/upstream_decoder_baseline.rs` | calls raw `serde_ipld_dagcbor::from_slice` (validator NOT in front) per canonical-form axis, pinning § *How decode-strictness is actually enforced*'s table against the live pinned dependency version |
| CID byte layout + string form | `libs/fauna-cbor/tests/cid_roundtrip.rs` | 36-byte layout, base32-lower round-trip, header-byte rejection on decode |
| `SignedEnvelope` sign + verify | `libs/fauna-cbor/tests/envelope.rs` | positive sign→verify; bytes-tampered → `CidMismatch`; sig-tampered → `SignatureInvalid`; wrong pubkey → `SignatureInvalid` |
| Cross-language conformance | `bins/fauna-bridges/internal/dagcbor/` (Go) | same `ipld/codec-fixtures` corpus runs against the Go encoder; Rust + Go must produce byte-identical output for shared kinds |
| Federation hello sign-over-CID, cross-language | `bins/fauna-nest/src/federation_sig.rs` (Rust unit) + `tests/e2e-unified/tests/test_federation_hello_envelope_verify.py` (tier_1) + `tests/e2e-unified/clients/ws_rpc_federation_client.py` (live door) | both directions of the `fauna.federation.hello` handshake envelope: the Python harness signs the request with `cbor2.dumps(canonical=True)` and the nest verifies it, and the client verifies the nest's **reply** envelope by re-deriving the bytes with the same independent encoder. Negatives pinned on both sides — a JSON-signed envelope, an untagged signature, and a signature made under another `sig_domain` tag must all fail. An in-process Rust handshake test cannot cover this: both its ends share one encoder and would drift together |
| WS-RPC nil-container & float invariants | `internal/wsrpc/wsrpc_conformance_test.go` (Go) + `libs/fauna-protocol/tests/wsrpc_nil_container_contract.rs` (Rust) | no wsrpc field is a float (type-level); every non-`Option` container encodes `[]`/`{}` not `null` (Go encode side, hard assertion since the `NilContainersAsEmpty` encMode landed) and Rust strict-decode accepts `[]`/rejects `null` for a non-`Option` `Vec` |
| WS-RPC reply-body Rust→Go field-rename/type drift | `internal/wsrpc/wsrpc_reply_cross_language_test.go` (Go) + `libs/fauna-protocol/examples/regen_go_wsrpc_reply_fixtures.rs` (Rust generator) | every reply body type (one fixture per `#[serde(tag)]` variant) is encoded by the Rust canonical encoder into `testdata/reply-*.cbor`; the Go mirror decodes + re-encodes and must reproduce the bytes — a Rust-side field rename/type-change drops the renamed key on the Go side and fails. A coverage test fails loudly if a reply type has no fixture. |
| WS-RPC request-body Go→Rust field-rename/type drift | `libs/fauna-protocol/tests/wsrpc_request_cross_language.rs` (Rust) + `internal/wsrpc/wsrpc_request_fixtures_gen_test.go` (Go regen + coverage) | the symmetric opposite: the Go encoder writes `testdata/request-*.cbor` (every request body; nested tagged unions like `SearchTerm` exercised one-per-variant), regenerated by `REGEN_WSRPC_FIXTURES=1 go test -run TestRegenerateRequestFixtures`; the Rust mirror `decode_strict`s + `encode_canonical`s and must reproduce the bytes — a Go-side rename/type-change fails the Rust strict decode or the byte-equality. Go coverage test fails loudly on a request type with no fixture; Rust test fails on an orphan fixture. |
| WS-RPC verdict/outcome/policy variant-name contract | `libs/fauna-core/tests/go_wire_variant_contract.rs`, `libs/fauna-mail/tests/go_wire_outcome_contract.rs`, `libs/fauna-client-mail-settings/tests/go_wire_policy_contract.rs` (Rust) + `internal/wsrpc/go_wire_variant_contract_test.go`, `internal/wsrpc/go_wire_outcome_contract_test.go` (Go) | pins the *variant-name strings* the Go bridge's hand-written `*ToWire`/`Parse*` switches must recognise — the mail-auth/content-scan verdicts, the MTA-STS/SRS decode-direction outcomes, and the admin-mail policy tokens — none of which the reply/request fixture tests above can catch, since Go decodes and re-encodes an unrecognised string byte-identically. Each Rust file derives its fixture live (serde / `as_str` / `as_wire`) and builds its instance list + exhaustiveness match from ONE macro-driven token list (`variant_set!`), so a variant addition cannot compile with only a match arm added — the fixture staying in sync is structural, not disciplined. |

CI gate: `cargo test -p fauna-cbor` plus the Go bridge's
`go test ./internal/dagcbor/...` — both must pass.

## Design decisions worth knowing

- **IPLD dag-cbor (not BARE/Protobuf/Cap'n Proto).** Spec-deterministic, multi-fork precedent (IPFS, Filecoin, ATProto), CID story for free. Replaced an earlier BARE decision — the CBOR-DAG-everywhere migration retired BARE from every at-rest, wire, and IPC path (see § Implementation status today).
- **Length-first then bytewise key sort (not lexicographic).** The IPLD-strict rule; `transport.md` defers here for the byte-level form.
- **BLAKE3-256 (not SHA2-256).** Faster on every platform, parallel-friendly, 32-byte digest. Multihash code `0x1e`.
- **Sign-over-CID, embed-as-bytes ("Camp A").** Verification is hash + signature, never re-encode + compare. Cross-language interop becomes a wire-spec property.
- **Pre-parse validator on the Rust side.** Upstream `serde_ipld_dagcbor` strict mode misses sort/dup/shortest-form/non-42-tag axes; `fauna-cbor`'s validator closes the gap. Other-language implementations need an equivalent.
- **Fixed 36-byte CID shape, two codecs (dag-cbor + raw), no negotiation.** Per CBOR-DAG-everywhere Layer 3, both `0x71` dag-cbor (every signed kind, every CARv2 block, every envelope `cid`) and `0x55` raw (the `ContentHash` alias keying chunks / blobs / video segments) are accepted on the same fixed 4-byte header + 32-byte digest layout. Any other codec, hash code, or digest length in a Fauna-typed slot is rejected outright.

## Implementation status today

**Built 2026-10-01: `Cid` as a tag-42 link.** § the raw-byte shape decision's ruling on `Cid` is in the tree: `fauna_cbor::Cid`'s serde impls carry it as an IPLD link through the `cid` crate's link type, so every `Cid` and `ContentHash` field encodes as tag 42 over `0x00` + the 36 bytes, and the strict decoder refuses the bare byte-string spelling (`libs/fauna-cbor/tests/cid_roundtrip.rs` pins both). The signed envelope's 100-byte buffer and the signature input stay raw bytes, as ruled.

**Built 2026-10-01: variable-length byte fields as byte strings.** § Canonical IPLD dag-cbor, "Variable-length byte fields" is in the tree: every `Vec<u8>`, `Option<Vec<u8>>` and `Vec<Vec<u8>>` that reaches the canonical encoder carries `#[serde(with = "serde_bytes")]` (a list of byte vectors `fauna_core::byte_array::vec_of_bufs`; the index manifest's `(kind, marker)` pairs a crate-local codec) — the segment kinds' record envelopes, placements and floors (calendar, contacts, conversations, mail), the signed moderation, scoring, subscription, recovery and nest-rotation records, the chunk manifest, the folder engine keys, the sync-agent IPC's file payload and every nest↔nest federation record. The encoder's debug guard refuses a plain `Vec<u8>` by field path; the nest's serve-page budget counts a record's raw length (the 2× array allowance it carried while federation records were integer arrays is gone). Deliberately left plain, because none meets the canonical encoder: the app-facing view-model and FFI records in the `fauna-client-*`, machine and wasm crates (to JS a byte string would arrive as a `Uint8Array`), the nest's SQL row types, and JSON-only payloads such as the content index's `IndexedDoc`.

The BARE→dag-cbor migration (CBOR-DAG-everywhere, Layers 1–6) is **complete**
for every at-rest and network-wire path. The main Rust workspace, the
`apps/fauna-windows` Rust workspace (named-pipe IPC framing flipped to
canonical dag-cbor), and the C# IPC codec all encode through the one canonical
dag-cbor path. The `fauna_core::encoding::bare_encode`/`bare_decode` helpers
were deleted at the Layer-6 close gate; `canonical_encode`/`canonical_decode`
(delegating to `fauna_cbor::encode_canonical`/`decode_strict`) are the sole
at-rest/wire serializers.

`serde_bare` was reintroduced as a `[workspace.dependencies]` entry on
2026-07-07, narrowly scoped to one boundary this migration never
covered: the sandboxed community-labeler `label()` WASM ABI (design §6,
`libs/fauna-labeler/src/lib.rs`, `libs/fauna-ffi/src/labeler.rs`, gated behind
the `labeler` feature). That boundary is a host↔WASM-module memory handoff —
not an at-rest row or a network-wire frame — and it specifically needs a
codec that carries floats: canonical dag-cbor forbids them (§ Canonical IPLD
dag-cbor, above), and `Label.confidence` is an `f64` every labeler author
needs to emit. The host BARE-encodes `LabelerPostInput` into the module's
linear memory and BARE-decodes the returned `Vec<Label>`;
`fauna_core::scoring::labels_to_score_entry` converts each label's confidence
to a per-mille integer at the `ScoreEntry` boundary
(`content-moderation-and-ranking.md` § The concrete registry) before anything
crosses onto the dag-cbor wire or reaches storage — the migration's at-rest/
wire scope is otherwise untouched, and this remains the one sanctioned direct
`serde_bare` dependency.

How that boundary's **output** evolves — a positional-BARE record is not
additive, so the module declares the `Label` record revision it emits on
its signed metadata and the runner reads it before the BARE decode — is
`content-moderation-and-ranking.md` § Tier-3 → *The output half of the
`label()` ABI*'s ruling (2026-10-02); this doc owns only the codec choice.

Other remaining `serde_bare` mentions in the tree are doc comments explaining
why strict canonical decode rejects pre-migration BARE bytes (the last live
non-labeler holdout, the `apps/fauna-windows` C# feed `PostContentBareCodec`,
was deleted with the feed-posts WS-RPC migration). Float-carrying in-memory
types that aren't the labeler ABI (engagement audit reports, user-context
weights) never had an at-rest dag-cbor path — floats are forbidden by
canonical dag-cbor and are scaled to ints at the WS-RPC boundary — so their
old BARE round-trip unit tests were dropped rather than flipped.

Guard against reintroduction on every path except the labeler ABI: there is no
stock lint that bans a crate dependency, so the realized guard is "no new
direct `serde_bare` (or `ciborium`) use outside its one sanctioned caller" — a
`use` appearing in an at-rest or network-wire path is a visible diff a
reviewer catches. (`ciborium` survives only transitively via `c2pa`; its
cargo-vet exemptions must stay, same as `serde_bare`'s.)

**Fixed-size byte arrays (re-ruled 2026-09-29) — BUILT 2026-09-29.** Every
serialized `[u8; N]` rides as a CBOR byte string: `ActorId` and `ChannelId`
carry hand-written byte-string serde impls (so `ActorId`'s protocol fields
inherit it), `SecretArray32`'s impl goes through `serde_bytes`, and every
bare or `Option<>` fixed-width field on a serde-deriving type takes
`#[serde(with = "serde_bytes")]` — the pinned 0.11.19 accepts both — while
`Vec<[u8; N]>` takes `fauna_core::byte_array::vec`. The debug guard lives in
`libs/fauna-cbor/src/byte_array_guard.rs` and runs inside
`encode_canonical` under `debug_assertions`; `libs/fauna-cbor/tests/byte_array_guard.rs`
pins its refusal by field path. The codec's hint is one-directional (an
array where a byte string is expected), `wire_byte_array_shape.rs` pins the
two shapes and the array refusal, `identity.rs` pins `ActorId`'s frozen
`0x58 0x20` bytes, and the Python harness sends raw `bytes` for every id.
`SEEN_SET_ELEMENT_BUDGET` was re-sized to 1000 at the same headroom, and the
bounded-mint pin measures 34 B per listed member and per parent. Every
at-rest golden, pinned segment sidecar and peer-channel corpus frame that
carried the array spelling was re-cut, with no fallback, under the
2026-09-24 baseline reset. Neither additive-evolution gate saw the change;
the guard is its witness.

## Reading list

In priority order:

1. `principles.md` — engineering priorities (long-term uniformity, shared Rust).
2. (design ratified 2026-05-15; tracked internally) — full design rationale for canonical dag-cbor + CID + sign-over-CID + embed-as-bytes.
3. `libs/fauna-cbor/src/{canonical,codec,cid,envelope}.rs` — the implementation this doc describes.
4. `docs/goal/architecture/transport.md` — WS-RPC framing that rides on top of this codec.
5. `docs/goal/architecture/api-layers.md` — HTTP residue inventory.
6. `docs/goal/architecture/data-flow.md` — CARv2 segment storage, the on-disk consumer of CIDs.
7. RFC 8949 §3 + §4.2.1 — CBOR data model + canonical encoding.
8. IPLD DAG-CBOR spec — additional restrictions on top of RFC 8949.
9. multibase / multihash specs — base32-lower prefix `b`, BLAKE3-256 code `0x1e`.
10. ATProto data model docs — prior art for sign-over-CID + embed-as-bytes; the design Fauna's serialization layer mirrors.
