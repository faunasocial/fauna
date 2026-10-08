# Principles — product invariants and engineering principles

Owns: product-invariants, engineering-principles
Status: ratified
Authority: Level 1 of the goal-doc hierarchy (README.md § The hierarchy) — the never-violate product rules and the engineering principles every design in this tree assumes. Domain mechanisms stay owned by their Level-2 docs; this doc owns the principles themselves, not any implementation.

Every design decision in `docs/goal/` assumes the rules below. A change that
would break one of them gets redesigned until it doesn't; the invariants are
never weighed against convenience.

## Product invariants

### One configuration surface: the apps

The only humans in a Fauna deployment are **users and admins, and they
configure exclusively through the apps**. Nobody ever reads or edits a
config file, environment variable, or command-line flag to express a
preference or change a feature, policy, or behavior — that person does not
exist ("operator" is not a role in Fauna's model). Anything a user or admin
*chooses* — bridges, DKIM keys, TLS/ACME, spam thresholds, federation peers,
mail submission, capability grants, a serving/CalDAV port someone would pick — is
exposed as UI in **all 7 apps** (identical features save a few absent on
web) and persisted in **nest state**.

Every other value is **not chosen by any human**: it is either **hard-coded
in Rust** (a constant that can never change) or **internal wiring the
deployment artifact sets / the binary auto-detects** — pure inter-process
communication: how the Docker image / installer / entrypoint and co-located
processes wire themselves up (data dir, bind addresses, the
nest↔MDA↔SNI-router loopback split, log level). Such a file or variable is
IPC written by the artifact, never configuration a human touches, and is
minimized toward the works-out-of-the-box invariant below. (A bridge's *role*
is IPC of exactly this kind — discovered from its nest service-user
enrollment, not a `--mode=` flag: the supervisor runs the binary with the
right keypair and the binary asks the nest who it is.)

So every value sits in exactly one of **two buckets** — **(1) not chosen by
any human** (a hard-coded Rust constant, or artifact-set / auto-detected IPC
wiring), or **(2) a user/admin choice → all-7-app UI + nest state**. There
is no third "hand-edit a file to change behavior" tier. The test before
adding any non-app knob: *would a user or admin ever want to choose this?*
Yes → it belongs in the app UI, persisted in nest state. No → it is a Rust
constant or artifact-set IPC. The banned anti-pattern is
**configuration-file theatre**: a config-file or env-var knob standing in for
hard-coding a constant or building the app UI.

This invariant is what makes the uniform-apps principle (#1 below) the
literal whole truth: if a capability isn't in the app UI, it is not
configurable — it is a constant or artifact wiring.

### Works out-of-the-box

Fresh nest + fresh app → a working system with no manual config-file
editing and no terminal commands beyond starting the binaries. Features ship
with defaults correct for the most common deployment, or are configurable in
the app UI.

### The user always controls their data

User content rests sealed on every nest — there is no storage mode and no
claim-time trust question
([`architecture/nest/storage-modes.md`](architecture/nest/storage-modes.md)).
What a specific box can read is the set of capability grants its users have
minted for it: per-scope, time-bounded, revocable, and audited from the
user's own app (the grant primitive:
[`architecture/encryption-at-rest.md`](architecture/encryption-at-rest.md)
§ Capability tiering). The widest such grant — per user, own scopes, own
nest — additionally lets that box keep readable *derived views*
(projections, the user's search index) at rest; canonical stores stay
sealed everywhere, and revoking deletes the views (the materialization
tier, ratified 2026-08-10:
[`architecture/encryption-at-rest.md`](architecture/encryption-at-rest.md)
§ Readable classes class 4).
Read/share/export/delete affordances live in the user's app, never behind
server-side commands. New data shapes default to "user-revocable from the
user's own app". A **third-party principal** — an approved external app,
device app, or nest-hosted plugin — is a grant holder like any other:
per-scope, revocable, audited from the user's app; and a *remote* one never
holds keys to a scope it did not create (ratified 2026-09-05). Mechanism
owner: [`architecture/encryption-at-rest.md`](architecture/encryption-at-rest.md)
§ Capability tiering → *Third-party holders*; the principal itself:
[`architecture/third-party.md`](architecture/third-party.md).

**One deliberate exception (ratified 2026-08-13, the folders re-model):** a
folder whose owner sets its **audience** to `public` is world-readable by
design — its content, names, and paths (they are URLs) rest unsealed,
integrity-protected only. The flip to public is always an explicit per-folder
owner confirm naming that consequence, and flipping back re-seals only future
content (rotation cannot reclaim past secrecy). Everything else stays sealed —
in particular, **paywalled web content is *not* this exception**: it stays
sealed under the per-folder content key, decrypted at serve time by the nest's
web-serve holder via a scoped, revocable capability grant. Concept owner:
[`behavior/folders.md`](behavior/folders.md) § Target re-model.

### No user-data loss; additive evolution

No schema or data evolution may destroy data a user cannot recreate, and
clients and nests interoperate bidirectionally within a major version —
evolution is additive everywhere (wire and at-rest). Owner:
[`architecture/version-compatibility.md`](architecture/version-compatibility.md)
(the I1–I4 invariants and the compatibility mechanics).

**Alpha carve-out — user-approved deletion may be preferred to a costly
migration (user directive, 2026-07-21).** While the product is in closed alpha,
a cumbersome migration or backwards-compatibility shim can cost more —
in complexity, bloat, and the development speed a thousand-session marathon
depends on — than the data is worth preserving. So during alpha, **deleting
at-rest data is a legitimate option instead of expand→migrate→contract**, under
one non-negotiable gate:

- **Every deletion is explicitly surfaced to the user AND approved by them
  before it lands.** No silent drop, no deletion buried in a commit body. Name
  what is destroyed, on which clients/nests, and what the user must redo. If
  you cannot get approval, you do the migration.
- **Client-only-resident key material stays protected.** Most client state is a
  re-derivable replica of nest-held data — that is what makes this safe. An
  identity secret with no backup is not re-derivable, and destroying it destroys
  the account, not a cache. Treat that class as still-iron-clad unless the user
  says otherwise for a specific store.
- **The bidirectional client↔nest wire-compatibility corollary is UNCHANGED.**
  This carve-out is about at-rest data only; it is not licence to break the wire
  within a major version.
- **Expires with alpha.** The justification is the closed-alpha phase, not a
  property of the design. Re-ratify before it ends — the default returns to
  no-loss.

**The 2026-09-24 baseline reset — one-time, user-ruled, and the last of its
kind.** No Fauna installation and no Fauna data existed anywhere on that date,
and the public repository's first commit — the 2026-10 baseline — follows the
compat-remnant sweep, so every installed version descends from it. So compat kept only for anything predating
the sweep — at-rest read-fallbacks and aliases, schema floors and migration
steps for pre-sweep shapes, older-peer wire arms — is *removed*, not migrated:
there is nothing to migrate and no deletion to approve. Scope, what stays, and
the per-program record:
[`architecture/version-compatibility.md`](architecture/version-compatibility.md)
§ Dimension 2, the fourth ratified exception.

**The 0.1.x compat-free window (2026-10-08, user-ruled) extends that reset
through every 0.1.x product version.** Nothing had been released or installed
anywhere when the user ruled it, so while the product version is 0.1.x no
wire, IPC or at-rest backwards compatibility is owed: an alpha tester's 0.1.x
install may need a fresh start after any 0.1.x upgrade, and that is no
deletion needing the carve-out's per-case approval. **The window closes at
0.2.0**: from the commit raising the product version to 0.2.0 this section
applies in full and unchanged, and the alpha carve-out above is the only
remaining latitude. Scope, what stays and the gates' mechanism:
[`architecture/version-compatibility.md`](architecture/version-compatibility.md)
§ Dimension 2, the fifth ratified exception.

### Client-recoverable nest state

Every nest state a client can reach — including a client crash at any point
of any operation, factory reset included — must be recoverable by a client,
with no shell access or manual DB surgery. A transition that can strand the
nest in an off-box-only-fixable state is a bug, not a deferred feature.
Owner: [`architecture/nest/common.md`](architecture/nest/common.md)
§ Client-state recoverability (the authoritative statement + the
per-transition verification question).

## Engineering principles

Referenced across this tree as "priority #1"–"#5" or by name.

1. **Minimize per-app divergence.** Same UI, behavior, code shape, and
   concepts on all 7 apps. Before adding a per-app shape, ask whether
   the other six can adopt it; new deviations are exceptional and explicit.
2. **Maximize shared Rust** — and shared code within each platform family's
   own shared layer. Business logic, validation, parsing, state machines,
   and protocol code live in the shared `libs/fauna-*` crates (WASM for web,
   UniFFI for the native apps); the same holds for each family's shared
   layer (e.g. `FaunaKit` for macOS+iOS, the web SPA's shared core).
   Per-app files are for genuinely platform-divergent *shells*, never the
   leaf components inside.
3. **Same concepts and architecture everywhere** — names, data shapes,
   flows, error handling, screen structure. Find prior art in another app
   before designing fresh.
4. **Resolve drift; don't match it.** Found per-app divergence? Lift to
   the unified shape on all apps rather than replicating the divergence
   on a new surface — and pick the richest existing pattern, not the
   simplest. If neither is a strict superset, say so and raise it.
5. **Test-first whenever feasible.** Failing test first (cross-app
   behavior in `tests/e2e-unified/`, shared logic in the owning Rust crate),
   confirm it fails for the expected reason, then implement.

## Implementation status today

The invariants are design-and-review rules, not a single enforced mechanism;
each owner doc's `## Implementation status today` section tracks its own
domain's gaps against them.
