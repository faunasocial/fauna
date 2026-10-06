# Encrypted Spaces — target state

Owns: encrypted-spaces
Status: ratified — production dependency gated on ECS maturity (see ## Implementation status today)
Authority: owns the Encrypted Spaces product surface + trust property — what a Space is, the *verifiable-untrusted-server* property (clients cryptographically verify the nest applied every operation correctly), MLS coexistence, the `fauna.spaces.*` wire surface, Space-edit reconciliation, and where the integration lives across shared Rust / nest / the 7 apps. It does **not** own: ECS cryptographic mechanics → the upstream ECS whitepaper; integration mechanics (crate layout, wire struct shapes, handler registration) → tracked internally (design ratified 2026-06-24); page UX/IDs → `docs/goal/ui/spaces-documents.md` + ui.yaml; MLS DM/group channel setup → `docs/goal/behavior/direct-messages.md` / `docs/goal/behavior/groups.md`; the storage-mode axis → `docs/goal/architecture/nest/storage-modes.md`; the at-rest property → `docs/goal/architecture/encryption-at-rest.md`; sign-over-CID + transport trust → `docs/goal/architecture/security.md`; file-sync conflicts → `docs/goal/behavior/conflicts.md` § Conflicts. On conflict in those domains, raise it.

> **Audience:** rust, nest, all 7 apps. A verifiable, end-to-end-encrypted **collaborative shared-state**
> surface (shared documents / tables / calendars / "team spaces"), built on the third-party **Encrypted
> Spaces (ECS)** Rust SDK, running **alongside** — not replacing — the existing MLS conversation system.
>
> Sources: ECS whitepaper (2026-06-11, encryptedspaces.org), `libs/fauna-mls/`,
> `docs/goal/architecture/encryption-at-rest.md`, `docs/goal/architecture/version-compatibility.md`.

---

## Goal

Fauna will offer **Encrypted Spaces**: a surface for **shared, mutable, structured collaborative state** —
shared documents, collaborative tables/spreadsheets, shared calendars, and similar — with two properties that
Fauna does not have today:

1. **Verifiable-untrusted-server (ratified goal, 2026-06-24).** The nest holds only ciphertext + proof
   material for a Space and never decrypts it; **clients cryptographically verify that the nest applied every
   operation correctly** (membership change, write, delete) against a committed, hash-chained history. A
   malicious or compromised nest cannot silently corrupt, reorder, fork, or fabricate collaborative state
   without detection. This is **strictly stronger** than today's nest posture (§ Trust model).
2. **Shared mutable collaborative state with dynamic membership.** A general primitive — relational tables,
   ordered lists, collaborative text (CRDT), files — with cryptographically-enforced per-table access control
   and **selective retention** (cryptographic deletion; new members get access to non-deleted history only),
   none of which the existing channel/post/file-sync model provides.

Spaces are an **additive new surface**. They do **not** replace MLS conversations (DMs/groups), posts, mail,
or file sync; those stay exactly as specified in their own docs. Spaces is the home for collaboration that is
*mutable shared structured state*, where the existing model only offers append-only message logs, broadcast
posts, and single-user file sync.

---

## What an Encrypted Space is

An Encrypted Space is a shared, mutable application state over an untrusted server, provided by the ECS SDK
(`encrypted_spaces_sdk`, Apache-2.0, Rust, WASM-buildable). Its building blocks (per the ECS whitepaper,
2026-06-11):

- **Changelog** — an append-only, hash-chained log of operations; the authenticated history of the Space.
- **Verifiable database** — current state as an ordered key-value store over Merkle search trees, exposing:
  **tables** (typed schema, secondary indexes, SQL-style `Select` + one join + `LIMIT`), **lists**
  (order-statistic trees), **collaborative text** (a piece-table CRDT over lists), and **files** (a
  Cryptree-style directory-key tree).
- **Membership** — members invite (group key wrapped to the invitee's provisional update key), rotate keys,
  and remove (rekey of remaining members). Per-table access-control `rules`, enforced cryptographically.
- **Retention** — a two-tier retention key tree enabling cryptographic deletion and selective new-member
  access to non-deleted history **without re-encrypting the whole Space**.
- **Proofs** — **tracer proofs** (cheap, hash-only sparse-Merkle proofs binding each read/write to the data
  commitment) on every operation; optional **fast-forward proofs** (zkVM, for offline catch-up).

**Crypto posture (from ECS):** post-quantum except signatures — **X-Wing KEM (X25519 + ML-KEM-768)** for
multi-recipient verifiable encryption, Ed25519 signatures, Poseidon2 commitments.

---

## Trust model: verifiable-untrusted-server

This is the new property and the reason to adopt ECS over simply extending MLS. It sits **orthogonal to the
per-box capability-grant axis** (`nest/storage-modes.md` § What replaced each piece of the axis) and **does
not contradict** the existing trust framing — it adds a verification dimension the existing model has no
statement about.

**Today (baseline this extends):** every nest is trusted to *apply operations honestly* — content rests
sealed at rest uniformly, and what a box can read is the set of user-minted capability grants
(`nest/storage-modes.md` § What replaced each piece of the axis; the at-rest property: `encryption-at-rest.md`
§ The sealed posture). Clients verify payload **authorship/integrity** via sign-over-CID (`security.md` §
Goal) and authenticate the **nest's TLS identity** via `nest_actor_id` pinning (`security.md` § Transport
trust), but **no client today verifies that the nest stored, ordered, and delivered each operation without
tampering.** There is no changelog commitment, data commitment, or operation proof. (The account plane's own
statement of that seam — bounds, accepted status, revisit trigger — is
[`account-sync-plane.md`](account-sync-plane.md) § Ordering model, ratified 2026-08-11; this doc owns the
closure dimension.)

**Spaces add exactly that missing dimension.** For a Space:

- The nest is **untrusted for Space content** — Space content is sealed beyond the routing floor on every
  nest (no nest holds Space plaintext, regardless of what capability grants it holds for other content;
  verifiability against the box is the whole point). The nest's role
  for a Space is the ECS "backend": order operations, store ciphertext + proof material, relay to members,
  and (optionally) verify member-submitted proofs to enforce availability against malicious insiders.
- Clients verify **every** server response against their trusted commitment before accepting it
  (verifiable history + data commitment + tracer proofs).
- **Availability is the server's job** in the ECS model: the nest checks member-submitted ZK proofs, so a
  malicious insider can at worst cause an availability failure for others, never a confidentiality/integrity
  break. (How far Fauna runs the *proof-verification* side of this — vs. relying purely on client-side
  verification — is a design-time decision; see § Implementation status today.)

**Three verification axes now coexist, non-overlapping:** (1) *payload authorship* = sign-over-CID
(`security.md`); (2) *which nest you reached* = `nest_actor_id` transport pinning (`security.md`);
(3) *the nest applied operations correctly* = the Spaces verifiable-DB commitments/proofs (this doc). Axis (3)
is the new one.

---

## Coexistence with MLS (no replacement)

Spaces run **beside** the MLS conversation system, which keeps full ownership of DMs and groups
(`direct-messages.md` owns MLS channel setup / key-package / welcome delivery; `conversation-rooms.md` owns the room model — membership, classes, roles — and `groups.md` is the retired group plane's pointer stub;
at-rest seal authority: `encryption-at-rest.md` § Per-content-kind conformance → Conversation messages —
bodies are MLS-application-layer-sealed; `derive_blob_key` covers conversation attachments only).

- **Two group-keying systems will coexist:** MLS/TreeKEM for conversations; ECS's simpler linear-cost mVE
  rekey for Spaces. This is an accepted consequence of the Tier-1 decision (a new surface, not a re-base of
  MLS). ECS explicitly is *not* MLS — it provides "CGKA-like" functionality with a different protocol.
- **Identity is shared.** Fauna's password-less Ed25519-actor identity (`login.md`) maps directly onto ECS's
  per-user *update* + *authentication* keypairs, so a Space member is the same actor as everywhere else —
  no second identity system.
- **"Space" ≠ "folder".** A *folder* (`file-sync.md`) is per-user device-sync infrastructure; a *Space*
  is a new multi-user collaborative product concept. A Space may internally use shared-folder storage, but
  the terms are not interchangeable.
  - **Open design point — ECS-native files vs. folder-backed files (resolve when Spaces' file component is
    scoped, NOT now).** ECS ships its *own* files primitive — a Cryptree-style directory-key tree, exposed via
    a `fauna.spaces.file_*` kind pair once the wire set is fixed — so a Space's files can be **ECS-native** *or*
    **backed by the M2 shared-folder storage** (`mls-group-key-material.md` § Audience: an MLS group — the
    rotate-on-removal content-key mechanism is live). The "may internally
    use shared-folder storage" sentence keeps that option open but does **not** decide it. If a Space ever
    delegates its files to shared-folder storage, two seams must bridge because the two subsystems use
    **different** membership-crypto: (1) **rekey** — ECS membership rekeys via linear mVE while a shared file
    set rekeys via MLS epoch rotation; the `FolderGroupCrypto` trait in `libs/fauna-client-folders` is the
    natural adapter point, but bridging mVE-membership → MLS-rekey is real work, not free. (2) **deletion
    semantics** — ECS offers cryptographic *selective retention* (new members get non-deleted history only),
    whereas shared folders offer *forward secrecy on removal* (a removed member cannot read post-removal
    content) via fresh content-key **generations**; mapping ECS retention onto folder generations is a second
    bridge. **Until ECS is scoped, shared folders stay on the pure-MLS path with zero ECS coupling** — the two
    subsystems coexist additively (the dual MLS/mVE keying above is sanctioned), so neither blocks the other.

---

## Role in Fauna's storage model (ratified 2026-07-20, user-approved; review-scoping note advisory)

Written from the 2026-07-20 file-sync collaboration analysis,
which established the file plane's collaboration ceiling this section is the counterpart to
(`../behavior/conflicts.md` § Conflicts → *Format-aware merge drivers*: binary formats are
latest-wins forever; docx merge is decided-never; live merging belongs to a different plane).

- **The boundary rule (ratified).** After Spaces, Fauna has three storage primitives, each owning a
  distinct shape of state: **channels** (MLS, append-only sealed logs — messaging), **folders**
  (files at rest — the *interop plane*, state whose editors are third-party software), and **Spaces**
  (verifiable mutable structured state — the *collaboration plane*, state Fauna itself renders).
  The mechanical test any session can apply: **"Does a third-party app open it? → folder. Does
  Fauna render it collaboratively? → Space."** Corollary of the coexistence section above, stated as
  a routing rule: moving an *existing* content kind (calendar, contacts, the account-state plane
  kinds that replaced the retired `__config` rail, conversation state) onto Spaces is never a drift-style refactor — it is a major-gated migration needing its own
  design pass with an expand→migrate→contract path (`version-compatibility.md`).
- **Export is a v1 invariant of every Spaces surface, not polish (ratified).** *User always controls
  their data* (`principles.md`) binds here with extra force because a Space is not a file: no
  document bytes exist anywhere until a key-holding member device materializes them, so without an
  export affordance the data is structurally unreachable outside Fauna — the Apple Notes / OneNote
  lock-in shape, which Fauna's invariants forbid. The first Documents surface therefore ships with
  export-to-file (markdown into a folder, or plain download) from its first release; the concrete
  UX/IDs belong to `docs/goal/ui/spaces-documents.md`. Local *gateways* (a member device — plausibly
  the always-on sync agent — materializing Space documents as files in a synced folder, one-way
  mirror first) are the sanctioned future interop pattern, precedented by the mail/CalDAV bridges;
  a bidirectional text gateway is tractable (text diffs map onto CRDT ops) but is its own design
  pass: echo loops, attribution, offline divergence, and the mVE↔MLS bridge named in § Coexistence.
- **The at-rest format is a no-data-loss liability the moment real user data lands (ratified).**
  Once real notes live in a Space, the ECS changelog/state serialization becomes **user-irrecoverable
  at-rest data**, bound by the no-data-loss invariant — effectively forever — while upstream is a
  ~1-commit research preview. Mitigations required before any real-user-data flip: vendor + pin the
  SDK, require a documented changelog format upstream (or document it ourselves at
  pin time), and treat **op-log replayability as the migration escape hatch** — the replayable
  history is what makes "re-materialize into a successor engine" possible if upstream dies.
- **Dependency-review scoping (advisory to the dependency-review track — refute freely).** Fauna's tracer-only
  / FF-off runtime posture shrinks the *runtime* attack surface but **not the source-review
  surface**: risc0 + Plonky3 ride unconditionally in the SDK dependency tree (~360 crates, per the
  Track-1 spike). The injection-resistant review should be sized and metered against the full tree,
  not the lean runtime posture.

---

## Where the integration lives

Per priority #2 (maximize shared Rust), the ECS engine is consumed through a **shared Rust crate** (working
name `libs/fauna-spaces`) that wraps `encrypted_spaces_sdk`, exposing a Fauna-shaped API to all apps via
**WASM (web)** and **UniFFI (native)** — exactly as `libs/fauna-mls` wraps openmls today. The crate adapts
ECS's transport-agnostic `Transport` trait onto Fauna's WS-RPC substrate (`libs/fauna-ws-substrate`).

- **Nest** is the ECS backend, routing by plaintext structural IDs (`space_id`, `actor_id`) and **never
  decrypting Space content**. Note (spike, 2026-06-24): the backend is **heavier than the conversation-segment
  relay** — it is a stateful *verifiable-DB engine* that applies operations to Merkle search trees, executes
  verified queries, and serves group-key-delivery slots, not an oblivious sealed-bytes log. It never reads
  encrypted *values* (it routes/indexes on the plaintext tree structure only), so the untrusted-server property
  holds; but "same posture as conversation segments" understated it. Mechanics: integration spec + spike-findings doc §2.
- **Apps** get new collaborative UI on **all 7 apps** (priority #1) — the shared `ui.yaml` gains a
  Spaces surface; per-app shells consume the shared crate. **First surface ratified 2026-06-24: shared
  documents** (collaborative rich-text), chosen over collaborative tables / shared calendars as the cleanest
  first page exercising ECS's piece-table CRDT. The UI is a **two-level model** (ratified 2026-06-24): a list
  of **Spaces** (containers) → a Space's **documents** → a document editor — designed + ID-level user-approved.
  The per-page UX spec is `docs/goal/ui/spaces-documents.md` (owns the surface's behavior/IDs); it mirrors the
  conversations list+detail + reuses the `markdown-toolbar` component. IDs land in ui.yaml with the first
  app's TDD cycle.

---

## Wire surface (additive, alpha-compat-safe)

Spaces introduce **new** `fauna.spaces.*` WS-RPC kinds (create, op-apply, fetch, membership, …; exact set
TBD). New kinds are **alpha-compat-safe by construction**: `version-compatibility.md` § I4 ratifies that
*"new kinds are tolerated as `fauna.protocol.unknown_kind` / the `Unknown` push envelope; nothing is removed
or renamed in place within a major."* An old client that predates Spaces simply degrades on the unknown kinds;
nothing existing changes. This keeps the no-data-loss + bidirectional-compat invariants intact.

---

## Encryption at rest for Spaces

Space content is **sealed beyond the routing floor, unconditionally** (consistent with the
verifiable-untrusted-server property above — the nest is untrusted for Spaces). The plaintext floor for a
Space is the structural/routing metadata ECS already exposes to its server
(space IDs, membership-table structure, index entries on columns explicitly marked plaintext, ciphertext
sizes, timestamps) — the same floor shape `encryption-at-rest.md` already defines for other kinds. A
per-content-kind conformance row for Spaces will be added to `encryption-at-rest.md` when the byte shapes are
fixed (that doc owns the property; this doc states the intent).

**First content kind whose split is pinned — shared documents (Fork-2, validated 2026-06-28).** The documents
kind's per-column plaintext/encrypted split is owned by `docs/goal/ui/spaces-documents.md` § Persistence
(mechanics: integration spec § 9). It is one concrete instance of the floor above — the `encryption-at-rest.md`
Spaces conformance row lands once the remaining content kinds (tables / lists / calendars / files) fix their
byte shapes too.

---

## Conflict resolution

Spaces use **ECS's own reconciliation** (CRDT collaborative text via the piece table; tracer-proof-verified
merges of concurrent table/list writes) — distinct from file-sync's device-divergence conflicts
(`../behavior/conflicts.md` § Conflicts, which covers *one file with divergent local/remote versions across
a user's own devices*, auto-resolved with versions retained per the 2026-07-10 model). Spaces conflicts are
*concurrent multi-user edits to shared state*, reconciled by the SDK. `conflicts.md` § Conflicts stays the
owner of file-sync conflicts; this doc owns Space-edit reconciliation.

---

## Implementation status today

**Nothing is implemented, wire included. This is a ratified direction (2026-06-24;
evaluation tracked internally), not shipped behavior.** A
feature-flagged prototype spike with **no real user data** is the sanctioned next build step. The gap,
explicitly, so the next consumer treats it as scope constraint, not drift to discover:

- **ECS is a research preview.** Upstream README: *"DO NOT USE IN PRODUCTION."* Authentication is a
  placeholder; fast-forward proofs are non-cryptographic "fake receipts" unless built `--features
  real-proofs`; DoS hardening is incomplete; ~1 commit on `main`; no security audit. **The production
  dependency is gated on ECS reaching audited, hardened, non-research-preview maturity.** Until then: track
  the upstream repo; a feature-flagged prototype spike with **no real user data** is the only sanctioned build.
- **No `libs/fauna-spaces` crate exists.** Working name only.
- **No `fauna.spaces.*` wire kinds exist.** Track-2 Slice 1 (2026-06-24) landed eleven request/reply
  kinds (create / submit_change / fast_forward / select / add_member / remove_member / submit_retention /
  fetch_key_delivery / file_upload / file_download / list_for_actor) and a `fauna.spaces.updated` push,
  registered with no handler and with byte shapes still TBD-until-spike. They were **removed 2026-10-01**
  before the 2026-10 baseline (the 2026-10-01 survey's dead-and-reserved-shapes removal,
  [`compat-remnant-sweep.md`](compat-remnant-sweep.md)), so that no published build reserves a shape the
  design has not fixed. The design pass that fixes the shapes mints the kinds afresh, with their handlers.
  Their intended carriage stands: **opaque ECS-serialized bytes** routed by plaintext `space_id`/`actor_id`
  (the §8 re-scope: the nest is the verifiable-DB engine, not an oblivious blob log). **Absent:** the wire
  kinds, the nest handler surface (`spaces_handlers.rs` + per-Space `Db` registry — Slice 2), the
  `libs/fauna-spaces` client wrapper (Slice 3), app UI, ui.yaml entries, the ECS workspace wiring
  (Slice 0), and the ECS real-time ephemeral/broadcast sub-surface (`send_ephemeral`/`subscribe_*`).
- **No `encryption-at-rest.md` Spaces conformance row** yet (added when byte shapes are fixed).
- **Performance on Fauna's envelope — VALIDATED 2026-06-24 by the Track-1 spike**
  (findings tracked internally). The no-zkVM tracer-only posture **works on a
  cheap box**: the lean SDK builds (release 58s) and runs end-to-end with **no GPU and no rzup/RISC-V
  toolchain** (only stock nightly + `protoc`). Release, in-process: **table write ≈ 0.25 ms/op, collaborative-
  text edit ≈ 0.28 ms/op, verified read ≈ 1.7 ms/op**, and per-write cost rises only ~21% as the changelog/
  table grows 35× — confirming the whitepaper's hash-cheap `O(k·c·log n)`. Offline catch-up is the
  `FastForwardData{ proof: None, changes }` light-client replay branch, **natively supported** (no FF proof).
  **Two caveats the spike surfaced:** (1) risc0 + Plonky3 are **unconditionally in the SDK dependency tree**
  (not excludable by skipping `prove`/`real-proofs`) — they ride in as ~360 crates of compile-weight; the
  cheap-box-*fatal* parts (RISC-V guest compile, GPU `cuda`) are neutralized by `RISC0_SKIP_BUILD=1` at build
  **and** runtime (the server's eager FF prover checks that env on every change; Fauna's nest must hard-code the
  tracer-only/FF-off posture as a constant, not honor an env knob). (2) The **WASM/web leg is RESOLVED
  2026-06-24 (spike-findings §7) — web (priority #1) is feasible.** Both load-bearing crates compile to
  `wasm32-unknown-unknown`: the **full Plonky3 STARK stack** (`p3-uni-stark` + the p3 core — the client-side mVE
  membership crypto, needed on every platform) checks clean (6.2 s), and **`risc0-zkvm v3.0.5` checks clean too
  *once its unix-only `client` feature is dropped*** (with `client` it pulls `std::os::fd`/`UnixStream`, absent on
  wasm). Fauna never needs that feature — `client` is the host **FF-prover** transport, and Fauna runs FF off. So
  the earlier fear ("risc0 won't build to wasm → must strip the risc0/ffproof surface for web") is **refuted with
  nuance**: nothing must be stripped; the residual wasm blockers are mundane portability gaps the real
  `libs/fauna-spaces` carries once — wire `getrandom`'s `wasm_js`/`js` feature for all three versions behind a
  `js` feature (the spec §1.2 gap, confirmed), depend on risc0 **without** `client`, provide Fauna's own transport
  (so ECS's `native-tls`/`openssl-sys` WS layer is never pulled — it was the first wasm failure), and fix one
  `instant::SystemTime` vs `std::time::SystemTime` mismatch in ECS `changelog_core`. **Posture (Track-1.5
  decision, spike-findings §8):** keep risc0 as verify-only compile-weight, **never enable `client`**; a true
  risc0-ectomy from the *wasm bundle* is a deferred, measured **bundle-size** optimization behind a cfg seam (the
  tracer-replay verify Fauna actually runs lives in the pure-Rust `ffproof-tracer-shared`, no risc0/p3), not a
  correctness requirement.
- **Integration mechanics now fixed** (design ratified 2026-06-24, tracked internally): the durable code shape — the `libs/fauna-spaces` crate layout/features/`SpacesEngine`/`SpacesRpc`
  seam + `FaunaSpacesTransport` adapter, the additive `fauna.spaces.*` wire kind set, the `spaces_handlers.rs`
  backend + changelog store (spec §3 sketched it as an oblivious relay; superseded — the nest is the full ECS
  verifiable-DB backend, see the **Material correction** in the API-signatures item below), the WASM/UniFFI
  surface — is specified against verified in-repo
  prior art (fauna-mls wrap pattern, the WS-RPC kind/dispatch machinery, the segment-store sealed-payload
  routing). It does **not** ratify mechanics the spike should decide.
- **User decisions resolved 2026-06-24:** **(1) first surface = shared documents** (collaborative rich-text);
  **(2) nest proof-verification posture = pure relay first** — the nest stores proof material opaquely and does
  not verify; clients do all verification (cheapest on the cheap box; a malicious insider can at worst cause an
  availability failure, never an integrity break). The verifying-backend upgrade (insider-DoS hardening) is
  deferred behind a handler seam so it is a one-wiring-decision change once the spike supplies cost numbers
  (spec §3.2). **(3) The Spaces UI surface is designed + user-approved** — the two-level model (Spaces →
  documents → editor) and its net-new element IDs are approved at the ID level in the per-page authority
  `docs/goal/ui/spaces-documents.md`. The IDs are *not yet landed* in the shared `ui.yaml`: they land per the
  first app's TDD cycle (avoids an all-7-`nav_exceptions` CI break on the shared file), so "no ui.yaml
  entries exist" above is the landing gap, **not** an open approval gap.
- **ECS `Transport` / membership / proof / retention API signatures — PINNED 2026-06-24** against
  `encrypted-spaces/prototype@4cda0ae8` (full surface + wire types in the spike-findings doc §2). The real
  `Transport` trait is **15 methods** (submit_change / fast_forward / select-with-Merk-proof / add_member /
  remove_member / submit_retention / fetch_my_key_delivery / file_upload+download / ephemeral+broadcast), far
  richer than a 3-method op-relay. **Material correction this forces:** the nest is the **full ECS verifiable-DB
  backend** — it applies structured `Change`s to Merkle search trees, executes verified SELECT queries, and
  serves group-key-delivery slots / files — **not** the oblivious `(space_id, seq) → sealed_bytes` relay the
  integration spec §3 sketched (it never decrypts *values*, but it is a stateful query engine, not a blob log).
  This enlarges Track 2 and is detailed in the spike-findings doc §2 + the integration spec's appended
  spike-corrections note.
- **Still TBD:** a precise `Space::join`-at-depth-N replay number (derived estimate only so far: ~0.25–0.5 s per
  1000 changes — the WASM question above is now resolved); the wasm-bundle-size measurement that decides whether
  to cfg-strip risc0 from the web build; whether/when to adopt ECS's PQ KEM more broadly; and the **multi-device
  mapping** — an actor runs several devices while ECS serves one key-delivery slot per member, so two devices of
  one actor are either *one ECS member* (shared update/auth keys; local commitment state must sync or re-derive
  across devices) or *two members* (per-device slots; different key-delivery scoping and rekey cost) — pinned
  before the client wrapper's storage design (added 2026-07-12).

---

## Reading list (priority order)

1. **This doc** — what Spaces are + the verifiable-untrusted-server goal.
2. Integration mechanics (tracked internally): the
   `libs/fauna-spaces` crate shape, the `fauna.spaces.*` wire kinds, the nest handler surface, and what the
   spike must validate.
3. The minor→major evaluation, tiers, and SDK mechanics that produced this decision (tracked internally).
4. ECS whitepaper (encryptedspaces.org) + `github.com/encrypted-spaces/prototype` — upstream crypto + SDK.
5. `docs/goal/architecture/encryption-at-rest.md` + `nest/storage-modes.md` — the trust/at-rest baseline this
   extends.
6. `docs/goal/behavior/direct-messages.md` + `conversation-rooms.md` + `docs/goal/architecture/key-material-hierarchy.md`
   — the MLS system Spaces coexist with.
7. `docs/goal/architecture/version-compatibility.md` § I4 — why new Spaces kinds are alpha-compat-safe.
