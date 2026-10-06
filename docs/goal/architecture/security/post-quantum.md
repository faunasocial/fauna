# Post-quantum cryptography — posture, crypto-agility, hybrid migration — target state

Owns: post-quantum
Status: ratified
Authority: owns the PQ posture + crypto-agility contract (suite discriminators, X-Wing hybrid KEM, suite capability negotiation, default-selection policy, upstream-blocked list). Consumes without redefining: key taxonomy → [`../key-material-hierarchy.md`](../key-material-hierarchy.md); plaintext/sealed split → [`../encryption-at-rest.md`](../encryption-at-rest.md); sign-over-CID + transport trust → [`../security.md`](../security.md); additive/capability machinery → [`../version-compatibility.md`](../version-compatibility.md). On conflict in those docs' domains, they win; this doc states only the quantum-specific overlay.

> **Audience:** every area that touches a KEM, a signature, or an at-rest/in-transit
> seal — nest, all 7 apps, and any shared-Rust work in `libs/fauna-{core,mls,protocol,cbor,pq-kem}`.
> **Purpose:** the destination for Fauna's quantum-resistance posture. States *which*
> quantum threat is real and which is not, *which* primitives are already adequate
> (so they are not churned), *which* surfaces are exposed, and the **two-step
> migration** — (1) make the crypto agile, (2) add a hybrid post-quantum suite —
> that reaches quantum-resistant confidentiality without ever breaking
> within-major compatibility.
> The design below is target state; § Implementation status today records, per item,
> what each slice built and what remains, and names the surfaces already shaped to
> receive the rest.
>
> Last verified: 2026-07-23 (docs-consistency sweep, code-accuracy re-check; fixed stale
> pre-tui-parity-flip "6 apps" phrasing, no other drift found) | Sources: `libs/fauna-mls/src/wrapped_blob/{format,mod,envelope,aead}.rs`,
> `libs/fauna-core/src/{crypto,identity}.rs`, `libs/fauna-core/src/subscription/{crypto,types}.rs`,
> `libs/fauna-mls/src/engine.rs:25`, `libs/fauna-cbor/src/cid.rs`,
> `Cargo.lock` (rustls/ring/hpke).

---

## Goal

Fauna's cryptography must reach **quantum-resistant confidentiality on every surface
Fauna itself controls**, by a path that **never breaks within-major compatibility**
([`../version-compatibility.md`](../version-compatibility.md) I2/I4) and **never
destroys data** (I1). The end state: a current sealed shape is *self-describing* about
the algorithm suite that produced it, both peers *negotiate* whether they can speak a
post-quantum suite, and new content is sealed with a **hybrid** (classical ∥
post-quantum) suite whose confidentiality survives the failure of *either* component.

This is deliberately a **hybrid**, not a post-quantum-only, target — matching NIST SP
800-227 / IETF guidance and every shipped deployment (Signal PQXDH, Apple iMessage
PQ3, TLS `X25519MLKEM768`): a hybrid suite stays as strong as X25519 if the
lattice scheme is later broken, and — crucially for Fauna — a hybrid suite is
**additive** (a new suite identifier alongside the classical one), so it lands as a
normal within-major minor-version change, never a major break.

The single load-bearing question for any sealed shape is the same as elsewhere in the
key hierarchy, with one clause added: **who may ever read this plaintext, and must it
stay unreadable to an adversary who records the ciphertext today and owns a quantum
computer in 2035?** If yes (long-lived user content), it belongs on the hybrid path.

---

## Threat model — harvest-now-decrypt-later is the only *live* quantum threat

There is no cryptographically-relevant quantum computer (CRQC) today; credible
estimates place one in the 2030s. So the design is driven by *which* of the two
quantum threats is already running, not by "Shor breaks X25519 eventually":

- **Harvest-now-decrypt-later (HNDL) — confidentiality — ALREADY RUNNING.** An
  adversary records ciphertext *now* and decrypts it once a CRQC exists. This is the
  only threat that bears on work-done-today, and it matters **exactly** for content
  Fauna's users expect to stay private for **decades**: mail, conversation history,
  audience-restricted posts, backups. For a privacy-first mail/messaging product this
  is the headline risk and it sets the migration priority.
- **Signature forgery / impersonation — authenticity — FUTURE-LIVE, NOT HARVESTED.**
  A 2026 signature cannot be retroactively forged in any way that matters (the signed
  content is already distributed and accepted). The risk is forging *new* signatures /
  impersonating an identity *from the moment a CRQC exists*. Because it is not a
  harvest threat, it is **lower near-term priority** than confidentiality — Fauna's
  Ed25519 identity keys are long-lived so the concern is real, but it is correctly
  sequenced *after* the KEM work (§ Signatures — deferred).

**Consequence for prioritisation:** KEMs (confidentiality) before signatures; and
within confidentiality, rank by *how long the data must stay secret*. The whole
design optimises the HNDL window, nothing else.

---

## What is already quantum-adequate — do NOT change

Two layers are already fine. Touching them is wasted churn and risks regressions.

### Symmetric / hash / KDF

| Primitive | Where | Why PQ-adequate |
|---|---|---|
| ChaCha20-Poly1305 (256-bit key) | every at-rest + app-layer seal | Grover halves the key search → effective 128-bit; ample. |
| BLAKE3-256, SHA-256 | CIDs, KDFs, fingerprints | 256-bit; quantum collision (BHT) needs ~2^85 *quantum* memory — infeasible. CID hash needs **no** change. |
| HKDF-SHA-256, BLAKE3 `derive_key`, Argon2id, PBKDF2 | all key derivation | symmetric; PQ-adequate. |

The one sub-256-bit AEAD — **AES-128-GCM in Web Push** (`bins/fauna-nest/src/push.rs`,
RFC 8291) — is *not* Fauna's choice (the RFC mandates it), is ephemeral per-notification,
and its real break is the underlying P-256 ECDH (a third-party push-service key Fauna
does not own), not Grover on AES-128. Out of scope.

### Seed-derived owner-only at-rest — already HNDL-safe (the precise property)

`BackupKey`, the content-index master key, and the nest-internal key-encryption key
(see [`../key-material-hierarchy.md`](../key-material-hierarchy.md) § Audience: owner
only / § deployment infrastructure) are each `BLAKE3::derive_key(context,
secret_seed)` — a symmetric key derived from a **secret seed that never transits a
public-key KEM and is never published**. A quantum adversary holding only public keys
+ harvested ciphertext **cannot** recover them.

The subtlety is worth stating exactly, because the *same* identity seed is the root
of *both* a safe and an unsafe derivation:

- **Safe:** `BackupKey = BLAKE3(seed)` derives from the **32-byte seed**. Shor run
  against the published Ed25519 public key recovers the *scalar* `s =
  clamp(SHA-512(seed)[:32])`, **not** the seed — and `seed → s` is a one-way hash, so
  the seed (hence `BackupKey`) is unrecoverable from any public value. `BackupKey`-sealed
  content (drafts, contacts, owner media, backups incl. held-for-friends, the content
  index; the `UserConfig` blob was here until it retired 2026-10-02 —
  [`../config-dissolution.md`](../config-dissolution.md) § The `__config` dissolution
  schedule → *The closure order*, step (6)) is broken **only** by classical device/seed compromise, not by a
  CRQC.
- **Unsafe (contrast, see § exposed surfaces):** the subscription KeyBlob's subscriber
  X25519 secret *is* exactly that scalar `s` (`ActorKeypair::to_x25519_secret =
  clamp(SHA-512(seed))`, `libs/fauna-core/src/identity.rs`), and the matching public
  X25519 is the published ActorId in Montgomery form — so Shor against the **public
  ActorId alone** recovers the long-term unwrap key.

So a large fraction of the data users most want kept for decades is **already
PQ-resilient against HNDL**, for free, by construction. Leave this layer as-is; the
version byte `BackupKey` already reserves (`[0x01]…`, `libs/fauna-core/src/crypto.rs`)
is sufficient headroom if a future *signature*-key compromise ever motivates a change.

---

## The exposed surfaces — HNDL-vulnerable, ranked

Every exposure is the same shape: a symmetric key delivered via an X25519 KEM/ECDH
whose public half is public. Ranked by sensitivity × longevity (this is the migration
order for § hybrid):

1. **Conversations & group-restricted posts (MLS).** Highest. Group secrets root in
   X25519 HPKE (KeyPackages/Welcome/commits). **Upstream-blocked** — see § 7.3 (MLS).
2. **Inbound mail at rest** (`MailRecordEnvelope`, HPKE to the recipient's standing
   key). High; decades-sensitive. *Fauna-controlled* → hybrid (§ surface A).
3. **Audience-restricted posts** (subscription `KeyBlob`, per-subscriber ECDH).
   Medium-high. *Fauna-controlled* → hybrid (§ surface B).
4. **TLS transport.** Medium — session-key HNDL exposes metadata,
   the short-lived bearer, plaintext-mode content, and the pairing leg. TLS is
   **upstream-blocked** (§ 7.3). Since the 2026-08-23 WireGuard deletion the P2P
   transport is iroh-QUIC — TLS too — so § surface C is the same upstream
   question rather than a second, Fauna-controlled one.
5. **TLS-cert bridge wrapped blobs.** Low — HNDL-vulnerable wraps, but the
   wrapped material is *infrastructure keys* long-rotated/expired before any CRQC;
   migrated for uniformity (they ride surface A's mechanism for free), not urgency.
6. **Ed25519 signatures everywhere.** Not HNDL. Future impersonation only — § Signatures.

> **Derived High surface — user-minted capability-grant wraps**
> (`GrantBlob.wrapped_keys` → each `WrappedScopeKey.hpke`,
> `libs/fauna-mls/src/wrapped_blob/format.rs`; the v1 capability-mediated
> content-processing build). A grant HPKE-wraps a **standing** content-opening
> key to a bridge service-user **holder** — for `content.read{mail,calendar}`
> the wrapped payload IS the recipient's MSEK-derived mail HPKE secret
> (`32 + 2400` B = `x25519_secret ∥ ml-kem-dk`). It is a **second-order**
> exposure (it adds no new plaintext; it re-wraps #2/#3's keys) but it
> **inherits #2's High ranking, NOT #5's Low**: the wrapped payload is a
> *standing* key (rotates only on MSEK hard-revoke, never on DKIM's ~90-day
> cadence) and the wrap rests in the nest's `capability_grants` table for the
> grant's whole auto-renewed life, so a classical wrap harvested today is a
> CRQC-openable **bypass** of #2's closed mail-at-rest seal (recover the
> standing key → open the X-Wing-sealed content anyway). *Fauna-controlled* →
> hybrid, riding surface A's mechanism (§ Implementation status → the
> capability-grant row).

---

## Design — crypto-agility first (step 1), hybrid suite second (step 2)

### This is an *application* of the existing additive machinery, not a new mechanism

[`../version-compatibility.md`](../version-compatibility.md) already gives Fauna every
primitive a PQ migration needs; the crypto-agility layer **reuses** them rather than
inventing a parallel scheme:

- **Self-describing-suite discriminator** = the same shape as the at-rest two-number
  version scheme (§ 2.2): a sealed blob already (or will) carry the algorithm-suite id
  that produced it, and a decoder *dispatches* on it instead of asserting one constant.
- **Suite capability token** = the existing `NestInfoReply.capabilities` +
  `fauna_protocol::discovery::capability` registry (§ Dim 3): absent ⇒ "peer can't do
  the suite" ⇒ the safe degrade, exactly the `mail_subsystem_ok`-omitted precedent. The
  first such token, `pq-hybrid`, is retired (§ Capability negotiation, 2026-09-24): every
  build does X-Wing, so its selection keys on the recipient's published ek alone; the
  mechanism stays for a future suite.
- **"Tolerate before any bump"** (§ 2.2): the discriminator/token lands *now*,
  defaulted to today's classical values, with **zero day-1 behaviour change** — so the
  later hybrid suite is an additive minor change, never a major break (I4). This is the
  identical pattern as `min_reader_format_version` shipping before any breaking
  segment-store change.
- **`tools/check-additive-evolution`** (CI, `protocol-checks`) is the guardrail: every
  shape change below must be additive (a new `#[serde(default)]` field or a new enum
  variant covered by the `Unknown` fallthrough), which is the correct constraint anyway.

### Per-surface discriminator state and the two steps

| Surface | Discriminator pre-S1 (the step-1 column has since landed for every row — see § Implementation status today) | Step 1 (agility) | Step 2 (hybrid) |
|---|---|---|---|
| **Mail-at-rest `MailRecordEnvelope`, TLS-cert blob** (`wrapped_blob`) | ✅ already self-describing: `{v:1, …, hpke:{ks:KemSuite{kem,kdf,aead}, enc, ct}}` (RFC 9180 IDs `0x0020/0x0001/0x0003`) | **decode-and-dispatch on `ks`** instead of asserting `KemSuite::STANDARD`; register the hybrid `kem` id constant | add the X-Wing `kem` id as an accepted suite; seal new mail to it when the recipient publishes a hybrid key |
| **Capability-grant `WrappedScopeKey.hpke`** (`fauna-mls`, derived surface A) | ✅ already self-describing — reuses the same `{ks, enc, ct}` `HpkeWire` as mail/TLS | **decode-and-dispatch on `ks`** via the shared `hpke_open_dispatch` (one dispatch point for every HPKE blob) | add `seal_capability_xwing` + `unseal_capability_hybrid`; the holder (bridge service-user) publishes a **seed-derived** ML-KEM ek; the client mint selects X-Wing per key when the holder published one |
| **Subscription `KeyBlob`** (`fauna-core`) | ❌ none — `encrypted_key` is hand-framed `[32 eph][12 nonce][ct][16 tag]` | **add** a `#[serde(default)] suite: KemSuiteId` to `KeyBlobEntry` (default = classical) + a self-test that the absent field decodes classical | add an X-Wing-wrapped entry variant; subscriber publishes an ML-KEM key |
| **WireGuard tunnel** (`fauna-wireguard`) — *row retired 2026-08-23 with the stack* | ❌ no PSK plumbed (`Tunn::new(.., None, ..)`, 4 sites) | thread an `Option<[u8;32]>` PSK through `WgPeer`/`WireguardPeerConfig`/`WgPeerRow` + an additive `preshared_key` DB column (no behaviour change while `None`) | **establish the PSK over the hybrid KEM** (surface A's channel) so it delivers real PQ benefit; rotate it |
| **BackupKey / index / nest-KEK at-rest** | ✅ `[0x01]…` version byte reserved | — (already HNDL-safe; no action) | — |
| **MLS, TLS, CID** | MLS `const CIPHERSUITE`; rustls/ring; BLAKE3 CID | upstream-blocked / not-needed — § 7.3 | § 7.3 |

### The hybrid KEM — X-Wing (ML-KEM-768 + X25519)

The hybrid primitive is **X-Wing** (`draft-connolly-cfrg-xwing-kem`): a single
IND-CCA2 KEM that internally runs **ML-KEM-768** (NIST FIPS 203, NIST security level 3)
and **X25519** and combines their shared secrets with a fixed hash. It is chosen
because it **drops directly into an HPKE KEM slot** — Fauna's mail/DKIM/TLS blobs are
HPKE, so X-Wing becomes "just another `KemSuite.kem` id," no envelope redesign. The
combiner makes the result as strong as X25519 even if ML-KEM is broken (and vice
versa), satisfying the hybrid goal.

Wire-size consequences (the only thing that grows): the HPKE `enc` field goes from
**32 B → 1120 B** (ML-KEM-768 ciphertext 1088 B ∥ X25519 ephemeral 32 B); a published
encapsulation key goes from 32 B → **1216 B** (ek 1184 B ∥ X25519 pk 32 B). Shared
secret stays 32 B, so all downstream AEAD/KDF is unchanged. These sizes are stated so
the segment-store / DB column sizing and the `actor_mls_pubkeys`-style registries are
provisioned correctly.

> **Refutable implementation note (the spec owns this):** the `hpke` crate (0.13) is
> classical-only and has no X-Wing KEM; the implementer evaluates a hybrid-KEM crate
> (e.g. an `x-wing` / `ml-kem` RustCrypto stack) vs. a hand-rolled combiner feeding
> the existing HPKE KDF. A non-IANA `kem` id in a documented Fauna-private `u16` range
> is used until/unless IANA assigns X-Wing an HPKE KEM id — sound because the suite is
> self-describing per-blob.

### Post-quantum key publication and derivation

Unlike X25519, an ML-KEM key **cannot be derived from an Ed25519 identity** — there is
no point-mapping trick. Two consequences, both additive:

- **Mail-at-rest (surface A):** the recipient's ML-KEM keypair is **derived from MSEK**
  (`seed64 = expand(MSEK, "fauna.mail.recipient-mlkem.v1", 64) → ML-KEM-768.KeyGen`),
  exactly mirroring today's MSEK-derived X25519 recipient key — so it stays
  **fleet-consistent** by construction (every device re-derives the identical key) and
  rides MSEK rotation. The public encapsulation key is **published** as new plaintext
  routing metadata alongside the existing X25519 pubkey
  (`actor_mls_pubkeys` / `provision_recipient_mls_pubkey`, a required sibling field:
  the provision request carries both halves, so no recipient has a key on file without
  its post-quantum half).
  **Scope — the message/event *body* AND its companion index hint (PQ-6 closed).**
  Surface A's hybrid seal covers the body `MailRecordEnvelope` (RFC 5322 message /
  iCalendar event content) and — as of **PQ-6** — the **encrypted index-hint** too, in
  every delivery path (the nest in-domain leg + the Go MTA/MDA legs). The hint is
  `tokenize(subject + body_text)`
  (`bins/fauna-nest/src/bridge_routing_handlers.rs`; `libs/fauna-mail/src/tokenizer.rs`),
  a deduplicated, sorted set of **plaintext** NFKC-lowercased words; left classical it
  would let a harvest-now-decrypt-later adversary who breaks the classical index key
  with a CRQC recover the **complete plaintext word-set (vocabulary) of every
  message/event body** even though the body is X-Wing-sealed — a meaningful HNDL leak
  for mail (this doc's own "High; decades-sensitive" class, § exposed surfaces #2), not a
  negligible one (the seal-side review, reviewed 2026-06-26 and tracked
  internally, § PQ-6 corrected the prior
  "small partial-content" wording). **PQ-6 closed it via option (a):** the hint now rides
  the body's X-Wing seal (the shared `seal_recipient_blob` selector in nest /
  `EncryptToRecipientHybrid` in the Go bridge) — sealed hybrid to the recipient's
  ML-KEM ek, classical only as the PQ-4b
  degrade (a recipient's key always carries its ek — § Capability negotiation and
  default-selection policy). Option (b) (subject-only / keyed-HMAC **blind** tokens) was **rejected**:
  encrypted-mode search is client/key-holder-side by design
  ([`../../behavior/content-index.md`](../../behavior/content-index.md) § Where queries
  run — the nest must never read or query the index), so blind tokens buy nothing here
  and would forfeit the client-side Tantivy BM25/phrase search. **Phase-E hand-off —
  RESOLVED 2026-08-03 (rollout S5, refutable-advisory ruling): the re-point is NOT owed,
  because Plan 5b's index key is not a *dedicated* one.** The conditional below was
  written before Plan 5b had specified its key. S0 ratified it 2026-08-02 as **MSEK-derived
  and symmetric** — `BLAKE3::derive_key("fauna.mail.index-seg.v1 2026-08-02", MSEK)`,
  `../owner-key-material.md` § Path B-sibling-4 — so there is no index *keypair*, no
  sibling ek to publish, and nothing that is "≠ the MLS pubkey" in the sense that matters:
  whatever can derive the index-segment key already holds MSEK, and MSEK is exactly what
  opens the hint. The harm the hand-off guards against — a search-only device holding only
  the index key and therefore unable to open in-domain-sent hints — is **unreachable by
  construction** in the ratified design, so today's behavior (hint sealed to the
  recipient's standing MSEK-derived key) is *correct*, not a deferred gap.
  Two supporting facts, both verified at code level this pass: the dedicated-index-key
  rail is **dark end to end** — `actor_index_pubkeys` has no production writer (its only
  caller is a nest unit test; `db/migrations.rs` says so in the schema comment), so
  `fauna.bridges.fetch_recipient_index_key` answers `None` for every actor and the Go
  legs' `indexPubkey` always takes its `mlsPubkey` fallback — and S5 proper *supersedes*
  the hint for the use case that motivated it, since the MDA will serve IMAP `SEARCH`
  from sealed tantivy segments rather than by linearly scanning per-message hints.
  **The original conditional stands for a future major that does mint a dedicated index
  key** (the rail is dormant, not deleted): it would then MUST also publish a sibling
  index ML-KEM ek and re-point the hybrid seal to it, else the hint seals classical —
  the safe default the `IndexHintMlkemEk` gate picks, which must not become permanent.
  That future pass must also cover the **nest in-domain leg**, which has no index-key
  indirection at all (`bridge_routing_handlers.rs::seal_and_persist_local` seals body and
  hint to the same recipient key), and not only the Go legs' `IndexHintMlkemEk` gate —
  that asymmetry is the surviving, correct half of a 2026-06-27 review advisory whose
  premise (that the capability-position index build would trigger it) this ruling refutes.
  **Paired publication (surface A) — the ek and its dk reach the box by two different
  routes, and every path that writes one MUST write the other in the same transaction.**
  The public **ek** is plaintext routing metadata on `actor_mls_pubkeys`
  (`provision_recipient_mls_pubkey`). The private **dk** reaches the MDA only inside the
  sealed **MLS snapshot blob** (`MlsSnapshotPlaintext.leaf_init_keypairs[].mdk`), which
  a *different* RPC writes. Both are MSEK-derived, so they are always derivable
  together — but they are not always *published* together, and the MDA seals to whatever
  ek it reads while opening with whatever dk the snapshot carries. Publish an ek whose
  dk is absent and the MDA seals collection metadata and CalDAV/CardDAV bodies it can
  never reopen — including the ones it sealed itself, seconds earlier. The failure is
  **silent**: the CalDAV read path logs `caldav: skipping calendar with undecryptable
  metadata` and drops the collection from the PROPFIND home set, so the user sees a
  calendar vanish rather than an error. It is also **permanent** for that actor, since
  nothing re-seals metadata and the snapshot is otherwise only rewritten at first-enable
  or MSEK rotation. Therefore: the client's three publish paths — first-enable, MSEK
  rotation, and the **connect-time epoch-schedule refresh** — all rewrite the snapshot
  before republishing the pubkey, through one shared recipe
  (`fauna-client-mail-settings::MailSettingsMachine::provision_snapshot_for`) so they
  cannot drift. The connect-time pairing is also what **heals** an actor stranded by an
  earlier unpaired publish: an MSEK never has to rotate for the dk to catch up. Pinned by
  `a_connect_refresh_that_publishes_an_ek_also_rewrites_the_snapshot`
  (`fauna-client-mail-settings/tests/state_machine.rs`), with the harm itself pinned at
  its mechanism by `a_classical_only_snapshot_cannot_open_hybrid_sealed_collection_metadata`
  (`fauna-ffi/tests/mail_record_opener.rs`).
- **Subscriptions (surface B):** the subscriber derives an ML-KEM keypair from their
  identity seed and **publishes** the encapsulation key (a new registry entry), because
  the author can no longer derive it from the public ActorId for free. The author wraps
  the period key to the published key.
- **Capability-grant holders (surface A, derived):** a grant's **holder** is an enrolled
  bridge service-user, not an actor, so — exactly like the DKIM/TLS wraps (§ exposed
  surfaces #5) — it cannot reuse a recipient's MSEK-derived ek. The holder derives its
  **own** ML-KEM keypair from the **bridge's identity seed**
  (`derive_mlkem768_keypair_from_ikm(seed, <bridge-service-user context>)`,
  context-separated from mail's MSEK context and subscriptions' identity-seed context)
  and **publishes** the ek at service-user enrollment (an additive
  `bridge_service_users.mlkem_ek` column + a `register_service_user` field). The client
  mint wraps each grant's standing key to `XWingPublicKey::from_parts(holder_mlkem_ek,
  holder_x25519)` when the ek is present, degrading to classical otherwise. This is the
  **same bridge-service-user ML-KEM infrastructure the DKIM/TLS hybrid needs** — built
  once, shared. **Implementation status: the seal/open primitives (PQ-CAP-1), the holder ek
  publication + seed derivation (PQ-CAP-2), the client mint selector (PQ-CAP-3), AND the tier_3
  drain proof (PQ-CAP-4) are all built — capability-grant wraps are COMPLETE (§ Implementation status).**

Both are governed by the no-data-loss + additive rules: an actor that publishes no
hybrid key simply isn't sealed-to with hybrid (the sender's per-recipient published-key
selection — data-dependent, no capability token since the 2026-09-24 ruling below), and
decrypt-side keeps both keys for grace windows exactly as MSEK rotation does today.

### Capability negotiation and default-selection policy

- **No `pq-hybrid` capability token, no advert gate (user-ruled 2026-09-24 under the
  compat-remnant sweep — [`../version-compatibility.md`](../version-compatibility.md)
  § Dimension 2, the fourth exception's rulings).** The token once advertised on
  `NestInfoReply.capabilities` and the client's absent-token classical degrade are
  RETIRED: the nest advertised it unconditionally, so the degrade arm served only a
  pre-sweep nest — and was a downgrade lever a hostile nest could pull by omitting the
  token. Every current build can do the hybrid suite; a client publishes its ML-KEM ek
  unconditionally and seals hybrid whenever the recipient/subscriber has published a
  post-quantum key. The `mail-epoch-schedule` token and its absent-token `None` degrade
  ([`../encryption-at-rest.md`](../encryption-at-rest.md) § Capability tiering →
  Content-sealing epochs) fell under the same ruling, being the same shape. A FUTURE
  suite gets its own capability token again — the advert shape is the upgrade mechanism
  the sweep kept; only these two consumer-less tokens went. **Implementation status:**
  ruled 2026-09-24 and LANDED: the constants are retired in
  `fauna_protocol::discovery::capability` (names never reused), the nest advertises
  neither, no client branches on either, and the `pq_hybrid: bool` is gone from
  `fauna-core::subscription::crypto` and its FFI/WASM bindings.
- **The suite is negotiated/constant, never a human knob.** Per the iron-clad product
  invariant (`principles.md` § One configuration surface, no config-file/flag theatre), the
  algorithm is **not** a user/admin choice and gets **no app UI** — it is selected
  by the recipient's published key material and otherwise a Rust constant. **Default
  policy (re-ratified 2026-09-24):** once the recipient/subscriber has published a
  post-quantum key, **new** sealed content uses the hybrid suite; classical remains
  accepted on the read path for content sealed to a key with no published post-quantum
  half (data-dependent, never a compat arm — the pre-sweep "within-major, old-peer
  content" reason is void) — pure additive, no flag day, no opt-out surface. **A mail
  recipient always has that half (2026-10-04):** the recipient-key provision requires
  the ML-KEM ek, standing and per-epoch, so the body of a recipient's mail is never
  sealed classically for want of a key. The classical seal keeps two producers on
  surface A — the index hint sealed to a recipient's separate index key, which no ek
  pairs with, and the degrade on an X-Wing seal *error* (PQ-4(b)) — and surface B
  still selects on what each subscriber has published. **Implementation
  status:** LANDED 2026-10-04, end to end. The wire field, the nest's provision door
  and the client seam require the ek; the `actor_mls_pubkeys.mlkem_ek` and
  `actor_epoch_seal_keys.mlkem_ek` columns are `NOT NULL`; the nest's seal-key seam
  (`RecipientSealKey`) carries the ek by value to every seal site; and
  `FetchRecipientMlsPubkeyReply` hands the bridge both halves as one optional value
  (`key`), absent only for a recipient with no key on file. In the nest, the classical
  seal is reached only through `seal_recipient_blob`'s two arms: no ek passed (the
  calendar-invite index hint sealed to a separate index key) and the degrade on a seal
  error. **The Go bridge refuses a present `key` that lacks either well-formed half** (a
  32-byte pubkey, a 1184-byte ek) at the one fetch every seal site's key comes through
  (`wsrpc.FetchRecipientMLSPubkeyHybrid`): an error — the MTA tempfails the recipient, an
  IMAP/CalDAV/CardDAV sign-in fails, an APPEND keeps the two-half key cached at sign-in —
  never a recipient whose body is then sealed classically. No nest sends such a reply,
  and a misbehaving one cannot select a classical body seal by leaving the ek out; the
  bridge's classical seal keeps the nest's two producers (LANDED 2026-10-04, pinned by
  `FetchRecipientMLSPubkeyHybrid_RefusesAKeyWithoutBothHalves`).

### A PSK is only PQ-meaningful if its DELIVERY channel is (general rule)

*Written for the WireGuard PSK (deleted 2026-08-23 with the stack); kept because
the rule is about PSK provenance, not about WireGuard, and the next mechanism
that reaches for a pre-shared key will need it.*

Mixing a PSK into a handshake means an adversary must break **both** the
classical ECDH **and** obtain the PSK. That only yields *quantum* protection if
the PSK reached both endpoints over a channel a CRQC **cannot** harvest. A PSK
distributed over today's classical TLS/WS hands a HNDL adversary the PSK along
with everything else, adding nothing against the quantum threat — though it does
still harden against a *classical* passive attacker, which is defense in depth,
not PQ.

So: **a PQ-meaningful PSK must be established over a hybrid-KEM channel**
(surface A's, client↔nest at enrollment or nest↔nest at pairing) and rotated. Do
not accept a design that plumbs a PSK first and sources it later — the plumbing
is harmless, but the PQ claim is false until the delivery channel is hybrid.
This sequencing is stated so a future session does not ship a TLS-distributed
PSK and mistake it for quantum protection.

---

## Tracked but upstream-blocked — § 7.3

These two surfaces are real HNDL exposures Fauna **cannot** fix unilaterally today;
they are tracked, not scheduled, and re-evaluated when the upstream lands.

- **MLS (highest-sensitivity surface).** `libs/fauna-mls/src/engine.rs:25` pins one
  classical `const CIPHERSUITE`; `openmls` ships only RFC 9420 classical suites. A
  hybrid/PQ MLS ciphersuite is an IETF draft (`mls` WG hybrid-KEM /
  `draft-ietf-mls-combiner`) with no production openmls support. **Trigger to revisit:**
  openmls gains a hybrid ciphersuite — then Fauna changes (or negotiates) the const and
  the agility token already in place carries it. Watch item, owned by this doc.
- **TLS transport.** rustls 0.23 supports the `X25519MLKEM768` named group **only via
  the `aws-lc-rs` provider**; Fauna is on **ring**, which cannot. **Path when scheduled:**
  migrate the rustls provider `ring → aws-lc-rs` (cost: a C toolchain; verify the
  native-app + nest-serving paths; the **web** app uses the browser's own TLS,
  which already ships `X25519MLKEM768`, so web needs nothing). Note this couples to
  the channel-binding SPKI logic in [`../security.md`](../security.md) (provider swap
  must not change SPKI computation). Watch + scoped-migration item, owned here.
  **Also carries the P2P leg (Leg C) under iroh:** iroh is QUIC over TLS 1.3 (quinn + rustls), and
  **iroh is now the adopted P2P substrate** (2026-06-28, reversible second impl behind the
  `fauna-transport` seam — design ratified 2026-06-27; tracked internally),
  so PQ-P2P folds into *this* `aws-lc-rs` + `X25519MLKEM768` migration instead of the bespoke
  WireGuard PSK (S5b–d dropped 2026-06-28; the stack itself deleted 2026-08-23, making this
  the ONLY PQ-P2P path rather than the chosen one of two). The `fauna-iroh` impl defaults to a ring provider and exposes the
  crypto provider as an injection point, so this migration is a one-line provider swap (proven
  SPKI-neutral by a library review dated 2026-06-28, tracked internally). Background:
  design ratified 2026-06-27; tracked internally.

## Signatures — deferred (not HNDL; revisit after confidentiality)

Ed25519 underpins sign-over-CID authorship, the deployment identity, the channel
binding, DKIM, and submission tokens. Because forgery is not harvestable (§ threat
model), the PQ-signature migration (ML-DSA / SLH-DSA, almost certainly itself hybrid)
is sequenced **after** the KEM work and after stable Rust MLS/identity PQ-signature
support exists. The CID hash (BLAKE3-256) is already PQ-adequate, so the
sign-over-CID *recipe* survives a signature-algorithm change — only the signature
primitive and the published-pubkey shape would evolve, additively, under the same
suite-discriminator discipline. Bridge curves (secp256k1 Nostr, P-256 ACME/VAPID,
ATProto) are dictated by external protocols and move only when those ecosystems do —
permanently out of Fauna's unilateral scope.

---

## Implementation status today

**Step 1 (crypto-agility) is built (S1); the shared X-Wing primitive is built (S2); the
mail surface-A hybrid seal is LIVE (S3 complete — S3a–S3f): clients publish their
MSEK-derived ML-KEM ek (unconditionally per the 2026-09-24 ruling — the `pq-hybrid` advert
and the client's absent-advert degrade are retired and removed from the code, § Capability
negotiation), and new in-domain/inbound mail
+ CalDAV event-body content seals X-Wing to the recipient's published ek, which the
recipient-key provision requires since 2026-10-04 (§ Capability negotiation and
default-selection policy) — classical is retained on the read path, for what is sealed
to a key no ek pairs with. Subscriptions (surface B) hybrid is LIVE in
**both plaintext (S4c-2/S4d) and encrypted (S4a/S4b/S4c-1) mode** — the author client-side
mint wraps hybrid per subscriber and the nest accepts it; the per-app
subscribe-publish UI is now wired **and build-verified on all 7 apps** (apple on macOS,
android + web on a Linux dev machine, windows on Windows, linux the template, tui native against
the same shared crate) — surface B COMPLETE; the P2P transport (surface C) has no
fauna-authored PQ mechanism of its own any more and rides § 7.3's TLS provider migration
(the WireGuard PSK it used to carry went with the stack 2026-08-23 — see the surface-C
item below); DKIM/TLS-cert blobs (S6) remain step-1-only; and the **capability-grant wraps** (surface A,
derived) are COMPLETE — X-Wing seal/open primitives (PQ-CAP-1), holder ML-KEM publication +
seed derivation (PQ-CAP-2), the client mint selector (PQ-CAP-3), AND the tier_3 drain proof
(PQ-CAP-4).** This doc emits the backlog (tracked internally; implementation)
plus the closed design track (tracked internally) and the companion design
spec (ratified 2026-06-24; tracked internally).
Each item below records
what landed and what remains — so the next consumer sees the gap as a scope constraint,
not drift to discover:

- **X-Wing hybrid-KEM primitive (`libs/fauna-pq-kem`) — DONE (S2).** A standalone shared
  crate provides `encapsulate`/`decapsulate` (1216-B ek, 1120-B ct, 32-B shared secret),
  the deterministic ML-KEM-from-seed derivation `derive_mlkem768_keypair_from_ikm(ikm,
  context)` (HKDF-SHA-256 → 64-B FIPS 203 `KeyGen_internal` seed), and the X-Wing
  combiner `SHA3-256(ss_M ∥ ss_X ∥ ct_X ∥ pk_X ∥ \.//^\)`. ML-KEM-768 is Cryspen's
  formally-verified `libcrux-ml-kem` (already vendored); X25519 is the bare RFC 7748
  `x25519()`; the combiner is hand-rolled (the `x-wing` crate's monolithic seed cannot
  accept Fauna's independently-derived halves). Builds for `wasm32-unknown-unknown`
  (getrandom-free; caller supplies the RNG). **Now consumed by the mail surface (S3):**
  `fauna_mls::wrapped_blob::seal_to_recipient_xwing` + the FFI `seal_to_recipient_xwing`
  export seal to it, and the `hpke_open_dispatch` X-Wing arm decrypts it (no longer
  `InvalidFormat`); the read path opens both suites. The seal callers are live as of S3e
  (keyed on the recipient's published ek; the `pq-hybrid` gate they first shipped behind
  is retired).

- **Mail/DKIM/TLS HPKE blobs (surface A) — agility DONE (S1); mail hybrid LIVE (S3); DKIM/TLS hybrid NOT (S6).** The
  `wrapped_blob` HPKE shapes already serialize a per-blob `KemSuite`
  (`libs/fauna-mls/src/wrapped_blob/format.rs`); S1 added `KemSuite::is_standard()` + the
  Fauna-private `FAUNA_KEM_XWING` id and a single dispatch point `hpke_open_dispatch`
  (`wrapped_blob/mod.rs`) that routes the unseal sites
  (`unseal_tls_cert`/`unseal_mail_record`; a third, `unseal_dkim`, was retired 2026-10-04) on `ks`: classical → today's path;
  X-Wing → the hybrid opener (S3b), unknown → typed `InvalidFormat` (no silent mis-decrypt).
  Classical wire bytes unchanged. **Step 2 mail is LIVE (S3a–S3f):** the recipient ML-KEM
  keypair is MSEK-derived + published (S3c `actor_mls_pubkeys.mlkem_ek`), the reader threads
  the hybrid opener across every surface (S3d step 1–2), the seal side selects X-Wing on the
  client-publish (leg A), nest in-domain (leg D1), Go MTA inbound (leg C), CalDAV-MDA +
  IMAP-append (leg D2a/b) and first-party-CalDAV event body (leg D2c) paths (**S3e** flipped
  them on; the `pq-hybrid` token it advertised is retired since 2026-09-24), and the S3f
  tier_3 round-trip gate (`bins/fauna-nest/tests/conformance_email_send_in_domain.rs`:
  hybrid → hybrid, a recipient's key always carrying its ek) is green. Every seal degrades to classical
  on an X-Wing seal *error* (PQ-4b), never failing closed. **Residual (deferred, not a
  blocker):** the first-party CalDAV *collection-metadata* seal (`seal_calendar_metadata`)
  stays classical — making it hybrid needs a cross-app provision-flow refactor; the
  MDA path already seals collection metadata hybrid (leg D2a) and every reader opens both.
  DKIM/TLS HPKE blobs stay classical until S6. **PQ-6 (CLOSED — option (a)):** the
  companion `encrypted_index_hint` now seals **X-Wing (hybrid)** alongside the body in
  every delivery path (a recipient's key always carries its ML-KEM ek) — the nest in-domain leg
  via the shared `seal_recipient_blob` selector (`bridge_routing_handlers.rs`) and the Go
  MTA/MDA legs via `EncryptToRecipientHybrid` + the `IndexHintMlkemEk` gate
  (`mta/server.go`, `mta/fauna_recipient.go`, `mda/caldav/put.go`, `mda/imap/append.go`).
  Classical only as the PQ-4b degrade / Phase-E dedicated-index-key
  fallback; the read path is unchanged (`OpenMailRecord` / `unseal_mail_record_hybrid`
  already dispatch on the envelope suite). Proven by the extended tier_3 round-trip
  (`conformance_email_send_in_domain.rs`: the hint seals X-Wing, mirroring the body) +
  Go `TestAppendSealsHintHybridOnFallbackWhenEkPublished` / `TestIndexHintMlkemEkGate`.
  Option (b) (subject-only / blind tokens) was rejected (search is client-side —
  § surface-A scope above). **Mail's surface-A HNDL exposure is now closed**
  (same seal-error degrade residual as the body). Phase-E hand-off:
  **RESOLVED 2026-08-03 — index-key separation is NOT owed by Plan 5b**, whose key is
  MSEK-derived and symmetric rather than dedicated; the hint rides the recipient's
  standing key by design, not as a fallback awaiting replacement. Grounds + the
  conditions under which the separation would return: § surface-A scope above.
- **Capability-grant wraps (surface A, derived) — COMPLETE: agility reused (DONE); X-Wing seal/open
  primitives DONE (PQ-CAP-1); holder ML-KEM publication + seed derivation DONE (PQ-CAP-2);
  client mint selector DONE (PQ-CAP-3); tier_3 drain proof DONE (PQ-CAP-4).** The v1 capability-mediated content-processing build
  (design ratified 2026-07-04; tracked internally)
  HPKE-wraps a **standing** content-opening key to a bridge service-user holder in each
  `GrantBlob.wrapped_keys[*].hpke` (`libs/fauna-mls/src/wrapped_blob/`). Because that payload
  is a standing key (not DKIM's short-lived one), a classical wrap is a **High-HNDL bypass**
  of the closed mail-at-rest seal (§ exposed surfaces → the derived-High note), so it joins
  the X-Wing overlay. **PQ-CAP-1 (DONE, this pass):** `seal_capability_xwing` +
  `unseal_capability_hybrid` (`libs/fauna-mls/src/wrapped_blob/mod.rs`) mirror mail's
  `seal_to_recipient_xwing`/`unseal_mail_record_hybrid` — the wrap self-describes its suite
  (`FAUNA_KEM_XWING`, 1120-B `enc`), the holder opens with its X25519 secret + ML-KEM dk, and
  the shared `hpke_open_dispatch` already routes both suites; unit-proven (round-trip,
  canonical-wire round-trip, classical-superset-open, needs-hybrid-opener typed error,
  AAD-bound holder/scope tamper). **PQ-CAP-2 (DONE, this pass):** the holder (bridge
  service-user) derives its **own** seed-derived ML-KEM keypair
  (`fauna_mls::wrapped_blob::derive_bridge_service_user_mlkem768` under
  `fauna.bridge.service-user-mlkem.v1`, surfaced by the `fauna-ffi`
  `derive_bridge_service_user_mlkem768` export) and publishes its 1184-B ek at enrollment (additive
  nullable `bridge_service_users.mlkem_ek` column, reconcile-added so it lands on existing DBs after
  the role-widen rebuild; the additive `register_service_user` `mlkem_ek` field, set-once-frozen like
  x25519 via `BridgeMlkemEkFrozen`). The Go bridge derives dk+ek from its keyfile Ed25519 seed each
  boot, publishes the ek at `register_service_user`, and threads its dk through
  `unseal_capability_grant` so the MDA capability holder opens **both** classical and hybrid wraps with
  one path (the dk zeroized at holder Close) — the **identical infra the paused S6 DKIM/TLS hybrid
  needs**, built once, shared. Proven: fauna-ffi hybrid round-trip + classical-superset + wrong-length
  dk + deterministic-seed derivation; nest DB/handler set-once-freeze + wrong-length gate + additive
  reconcile no-data-loss; Go client mlkem_ek-in-request + cross-language fixture parity.
  **PQ-CAP-3 (DONE, this pass):** the client-side mint selects the X-Wing wrap per
  key-bearing tuple. `build_grant_blob` + its FFI `build_capability_grant_blob` + the shared
  `mint_grant` (`libs/fauna-client-capabilities`) grew a `holder_mlkem_ek` param; the new
  `seal_capability_selecting` (`wrapped_blob/mod.rs`, mirroring the nest's `seal_recipient_blob`)
  wraps X-Wing when the holder published a valid 1184-B ek, degrading to classical on a seal
  *error* (PQ-4b). **The gate is holder-ek-present** — no capability token (§ Capability
  negotiation): a bridge publishes its seed-derived ek at enrollment (PQ-CAP-2), which is why
  the § key publication/derivation prose gates the mint on ek-presence. The holder ek reaches the mint via
  the additive `FetchBridgePubkeyReply.mlkem_ek` (nest reads the stored `bridge_mlkem_ek`) →
  `HolderInfo.mlkem_ek` → `LinkedNestsMachine::mint`. **The mail/calendar payload is now the
  `32 + 2400`-byte X-Wing superset** (`x25519_secret ∥ ml-kem-dk`, `derive_scope_payload`) — its
  X25519 half is byte-identical to the classical secret, so it opens both classical and hybrid
  mail records, which is what makes hybrid-sealed mail drainable under the grant. **The 6-app
  *machine* mint path (`dispatch(Mint)` → `LinkedNestsMachine::mint`, below the WASM boundary)
  picks up X-Wing for free** — no per-app change; the FFI signature growth is native-only
  (`fauna-ffi` + `cmd/seal-helper-testonly`). Unit-proven: X-Wing-when-ek + classical-when-none +
  malformed-ek-degrades (`fauna-mls`); the 2432-B superset + classical-superset-open
  (`fauna-client-capabilities`); FFI hybrid mint → holder-drains (`fauna-ffi`); the reply ek
  round-trip (nest). **PQ-CAP-4 (DONE, this pass):** the tier_3 drain proof. The
  `cmd/seal-helper-testonly` `mint-grant` mode now mints an X-Wing-wrapped grant (holder ek)
  carrying the `32 + 2400` mail key — via the new shared
  `fauna_mls::wrapped_blob::derive_recipient_mail_capability_secret` (the single source of truth for
  the byte contract, which `derive_scope_payload` also calls) surfaced by the `fauna-ffi`
  `derive_recipient_mail_xwing_material` export — and the enrolled MDA holder opens it with its
  seed-derived ML-KEM dk and drains a hybrid (X-Wing) sealed mail record
  (`open_mail_record_with_key`, 32-vs-2432 length dispatch). Proven end-to-end on real nest + MDA
  binaries by `test_capability_rescore_drain.py`'s hybrid variant
  (`test_rescore_drain_hybrid_mail_under_xwing_grant`), and in-process by `seal_test.go`'s
  `TestMintGrantXwingRoundTrip` + `TestMintGrantXwingDrainsHybridMail` — the latter with the
  **negative control** that a 32-byte classical key CANNOT open the X-Wing record, so the drain is
  proven to service a genuinely-hybrid record, not a classical fallback the superset key would also
  open.
- **Subscription `KeyBlobEntry` (surface B) — agility DONE (S1); hybrid LIVE in plaintext
  (S4c-2/S4d) AND encrypted mode (S4a/S4b/S4c-1) — the per-app subscribe-publish UI is now
  wired **and build-verified on all 7 apps** (apple on macOS, android + web on a Linux dev machine,
  windows on Windows, linux the template, tui native against the same shared crate). Surface B is
  COMPLETE.**
  `libs/fauna-core/src/subscription/types.rs` has `KemSuiteId` (Classical default, Xwing) +
  the `#[serde(default, skip_serializing_if = "KemSuiteId::is_classical")] suite` field — a
  classical entry is byte-identical to a pre-agility one (self-tested) (S1). **A third suite
  (ruled 2026-10-01, arm built 2026-10-02):** `KemSuiteId` has an unknown arm
  (`KemSuiteId::Unknown`, collapsing, never written back), so an entry of a suite a reader does
  not know makes that one entry unopenable (`DecryptError::UnknownSuite` from
  `decrypt_key_blob_entry_for`) and never fails the `KeyBlob` for every subscriber, and an older
  nest stores the upload instead of refusing it — [`../transport.md`](../transport.md) § Schema and forward-compat discipline
  → *Rule 3 in full* owns the rule; `#[serde(default)]` on the field covers only an absent
  suite, never an unknown one. The shared
  X-Wing wrap core `fauna_core::subscription::crypto` (**S4a**) derives the subscriber's
  identity-seed ML-KEM keypair (`SUBSCRIBER_MLKEM_DERIVE_CONTEXT`, distinct from mail's MSEK
  context), publishes its 1184-B ek (`subscriber_mlkem_encaps_key`), wraps via
  `create_key_blob_entry_xwing`, and reads either suite via `decrypt_key_blob_entry_for`; the
  shared selector `create_key_blob_entry_auto` (X-Wing iff the subscriber published an ek;
  PQ-4b degrade-to-classical on wrap error) is the one gate both wrap sites
  call. Additive wire carriers `SubscribeRequest.mlkem_encaps_key` +
  `SubscriberEntry.mlkem_encaps_key` + `PendingRequest.mlkem_encaps_key`
  (omitted-when-`None`, forward-compat) ship the ek (**S4c-1**).
  **The nest carries the ek, the author's client wraps — tier_3-proven (S4c-2/S4d):** the subscriber publishes its ek on
  the `subscribe` request (handler length-gates `== 1184`); the nest persists it on the
  `subscribers` and pending `subscribe_requests` rows (nullable, reconciler-added columns) and
  serves it on `requests.list` / `subscribers.list`, where the author's client mint selects X-Wing per
  subscriber (the nest-side wrap paths left with the nest-held period-key plane, 2026-09-27) — `bins/fauna-nest/tests/
  conformance_subscription_keyblob_pq.rs` proves published-ek ⇒ `Xwing` + subscriber unwrap,
  no-ek ⇒ `Classical` degrade (mixed-suite roster), malformed-ek ⇒ rejected before any DB
  write. **Late-publish upgrade (SUB-1, CLOSED):** an already-subscribed subscriber who first
  joined classically and *later* re-subscribes carrying a freshly published ek is upgraded
  classical → hybrid rather than silently stranded — `add_subscriber` upserts the ek
  (`ON CONFLICT … COALESCE(excluded, existing)`, so the auto-approve cascade re-touching a
  held lower tier upgrades it without ever clobbering an ek with NULL), and both idempotency
  early-returns (`is_subscriber` and `has_pending_subscribe_request`) persist the late ek
  across all the subscriber's rows (`update_subscriber_mlkem_ek` /
  `update_subscribe_request_mlkem_ek`), so the author's next rotation re-wraps an `Xwing`
  entry — proven by `a_late_published_ek_upgrades_a_classical_subscriber_at_the_next_rotation`. **Encrypted mode — author mint DONE (S4b):** the **author's client** mints hybrid
  blobs — `mint_key_blob` / `mint_key_blob_from_bytes` (+ the WASM `1184·N` flat-bytes and FFI
  `Vec<Vec<u8>>` ek marshalling) take per-subscriber eks and map each subscriber through
  `create_key_blob_entry_auto`; the orchestration `mint_upload`
  (`fauna-client-subscriptions`) reads each member's ek from `subscribers.list` (and a
  brand-new subscriber's ek from `requests.list`), so an encrypted-mode approve/remove mints
  X-Wing per member who published one. The nest's
  `verify_encrypted_upload` accepts a hybrid upload identically to classical (it never
  inspects the suite) — proven by `subscription_ws_rpc.rs::
  verify_upload_accepts_client_minted_hybrid_blob` + the orchestration hybrid tests. The
  **subscriber-side publish** is shared (`SubscriptionsClient::subscribe_publishing_ek` derives
  + publishes the ek unconditionally); **all 7 apps now adopt the
  publishing variant** at their subscribe/follow button — linux (`offers.rs`/`mod.rs`) and tui
  (`profile.rs`) call the shared `fauna-client-subscriptions` method directly (no FFI/WASM
  boundary); apple (`APIClient.swift`), android (`ApiClient.kt`), web (`rpc.ts` +
  `profile/+page.svelte`) and windows (`NestRpcClient.cs`) go over the
  `subscriptions_subscribe_publishing_ek` FFI free fn / the WASM
  `subscriptionsSubscribePublishingEk` export. **All seven build-verified**
  (apple on macOS; android `:app:compileDebugKotlin` + web `deno task check` + tui native
  `cargo build -p fauna-tui` all on a Linux dev machine; windows `FaunaApp.Core` MSBuild on
  Windows). **Surface B is COMPLETE.**
- **Reserved but HNDL-safe (no action):** `BackupKey` version byte
  (`libs/fauna-core/src/crypto.rs`).
- **P2P transport PQ (surface C) — now wholly the § 7.3 `ring → aws-lc-rs` question.**
  With the WireGuard stack **deleted 2026-08-23** (user-directed; owner
  [`../../behavior/p2p.md`](../../behavior/p2p.md)), iroh-QUIC is the only P2P
  substrate, so PQ-P2P is TLS 1.3 hybrid KEM (`X25519MLKEM768`) and nothing
  else. **This surface no longer has a fauna-authored mechanism of its own** —
  it rides the provider migration tracked in § 7.3, and there is nothing
  substrate-specific left to build, park, or watch here.

  *History, kept because it explains why the S-numbers skip and why no bespoke
  PQ handshake exists in-tree.* The bespoke WireGuard impl carried a
  PSK-over-X-Wing design: `WgPeer::new` took `preshared_key: Option<[u8; 32]>`
  into `Tunn::new`, `wireguard_peers` had an additive `preshared_key TEXT`
  column (S1), and **S5a was LIVE** — the registering peer published an X-Wing
  encapsulation key on `fauna.wireguard.peer.register`, the responder nest
  encapsulated a PSK to it (the 32-byte X-Wing shared secret *was* the PSK, no
  extra AEAD wrap), persisted it, and returned the ciphertext for the initiator
  to decapsulate. A peer publishing no ek degraded to classical, non-erroring
  (PQ-4b); a classically-registered peer that later published one was upgraded
  classical → hybrid. **S5b–d were DROPPED** when iroh was adopted (2026-06-28):
  the mechanism is substrate-dependent, so building the initiator/rotation half
  for a substrate already scheduled for replacement would have been throwaway.
  The 2026-08-23 deletion removed S5a with its substrate.

  **Two findings from that work survive it, and both still bind:**

  1. **WG-1 owner-binding — the vulnerability class, not the door.** The peer
     control plane originally let any `User` register *any* WG public key (a
     public value) with an *attacker-chosen* encapsulation key, re-keying a
     victim's live tunnel — and unregister any peer at all. The fix was to
     check `registered_by` ownership on every mutating path. Generalized: **a
     late-publish/upgrade branch keyed on a non-secret identifier is an
     authorization door**, and a "re-key" path that skips the ownership check
     is a DoS primitive even when it leaks nothing. Apply it to any future
     enrollment surface.
  2. **A nest is not an actor on a peer nest.** Any future nest↔nest peer
     enrollment must ride the **federation channel** (mutual nest-key), never a
     bearer client kind (`federation.md:37-45`). This was recorded here because
     the dead nest↔nest initiator (`auto_peer_with_nest`) had violated it; it
     holds unchanged under iroh, where the question is likely moot — a `NodeId`
     is the key itself.

- **Capability token — RETIRED (2026-09-24).** `pq-hybrid` was advertised from S3e until
  the compat-remnant sweep removed it with its client publish gate (leg A) and the nest's
  seal-side self-check (§ Capability negotiation); `conformance_discovery` pins that
  `nest.info` no longer carries it (nor `mail-epoch-schedule`).
- **Upstream-blocked:** MLS single-suite const, rustls-on-ring — **not started by
  design** (§ 7.3).
- **No app UI exists or is planned** — by the product invariant the suite is never a
  human knob; this is intentional, not a gap.

## Reading list

1. [`../version-compatibility.md`](../version-compatibility.md) — the additive /
   two-number-version / capability machinery this doc applies (read § 2.2 + Dim 3 first).
2. [`../key-material-hierarchy.md`](../key-material-hierarchy.md) — the audiences and
   roots each sealed surface belongs to.
3. [`../encryption-at-rest.md`](../encryption-at-rest.md) — the per-content sealed/plaintext split.
4. [`../security.md`](../security.md) — sign-over-CID + transport trust (the
   signature-agility and TLS-provider couplings).
5. (design ratified 2026-06-24; tracked internally) — the
   per-shape byte-level construction recipes and the implementation slicing.
6. NIST FIPS 203 (ML-KEM); `draft-connolly-cfrg-xwing-kem` (X-Wing); RFC 9180 (HPKE);
   RFC 9420 (MLS); TLS `X25519MLKEM768` (`draft-ietf-tls-ecdhe-mlkem`).
