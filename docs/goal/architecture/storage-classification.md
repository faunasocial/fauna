# Storage classification — the total-classification law

Owns: storage-classification
Status: ratified 2026-08-11 (R17 (account-data-plane.md § The ratified decisions)/D2 — the user-ruled delta from the
greenfield storage derivation
(`2026-08-11-greenfield-storage-derivation.md` §§ 6+8, internal plans
tree — tracked internally, not shipped); put to
the user and ratified as a standing architectural rule). The law is
target state now; the per-store inventories and the lint are unbuilt
(§ Implementation status today).
Authority: owns the total-classification law — the three-way
classification of every locally-resting byte (canon / derived /
enumerated-local), the per-store inventory obligation, and the lint
contract. Defers: what *is* canon for account data and its primitives →
[`account-data-plane.md`](account-data-plane.md); the nest's at-rest
sealing posture + plaintext floor →
[`encryption-at-rest.md`](encryption-at-rest.md); the box scope's
dissolution of `nest.db` → [`account-data-plane.md`](account-data-plane.md)
§ The box scope (R16); install-scoped app-local state taxonomy →
[`apps/account-scoping.md`](apps/account-scoping.md).
Provenance: user ruling 2026-08-11 (delta D2 of the greenfield pass).

## The law

**Every byte resting locally on any device or nest is classifiable as
exactly one of three classes — and each store can list its members of the
third:**

1. **Canon** — a scope's truth: sealed content-addressed blocks,
   per-writer journal rows, merged state entries (the account-data
   plane's primitives 1–3, or their equivalents in stores not yet on the
   plane). Canon is never `ALTER TABLE`d, never destroyed by an upgrade,
   and is what sync, backup, export, and succession enumerate.
2. **Derived** — rebuildable from canon at any time: SQLite projections,
   indexes, caches, materialized views, thumbnails. Never truth, never a
   sync unit, never migrated — dropped and re-derived. Deleting a derived
   byte loses nothing.
3. **Enumerated local** — the honest exceptions, each argued and listed
   per store: roots (ceremony-only), MLS state (its own ratified plane),
   device-local keys (the device-only rung), the outbox (durable intents
   not yet canon — the one non-wipe-tolerant member), bounded operational
   ephemera (sessions, rate state, queues, locks — recreatable, TTL'd),
   and the nest plaintext floor (enumerated routing metadata).

A byte that fits none of the three — or fits the third without an
inventory row — is a classification bug, not a fourth class.

## Why this is a standing rule, not advice

One checkable property closes five recurring bug families at once:
**per-actor enumeration** ("everything belonging to actor X" becomes the
union of X's scopes' canon), **deletion orphans** (a delete that misses
derived state is harmless by construction; one that misses canon is
findable), **backup inventories** (back up canon, nothing else), **export
completeness** (export canon, nothing else), and **succession
re-pointing / erasure-on-signout** (enumerate and act on canon + the
third-class list). The account-data survey's nest-side findings — ~259
tables, ~10 actor-column spellings, three disagreeing hand inventories,
zero FKs to `users` — are what the absence of this law costs.

## The inventory + lint contract

- **Each store owes an inventory** naming its enumerated-local members
  (class 3) and its derived stores (class 2); canon needs no list — it is
  whatever the store's scopes hold. The inventory lives beside the
  store's owner doc or module header, greppable, one line per member with
  its argued reason.
- **The lint** (unbuilt) checks the inventory against the store's actual
  surface: a table/file/keyspace not classifiable as canon or derived and
  absent from the enumerated-local inventory fails. Cheap-tier candidate
  once inventories exist; never a synchronous-merge blocker before then.
- **New surfaces classify at birth**: a new table, file, or keyspace
  declares its class in the change that adds it — retrofitting a
  classification is exactly the archaeology this law exists to end.

## Implementation status today

**As of 2026-08-11 the law is ratified and nothing else exists**: no
store has a written inventory, no lint runs anywhere, and the nest side
is furthest from conformance (the `nest.db` state the box scope — R16,
[`account-data-plane.md`](account-data-plane.md) § The box scope —
dissolves is today unclassified and largely unenumerable). The
account-data plane's own store is the closest conformer by construction
(its four primitives are the canon/derived split already). Adopting the
law is incremental: inventories land per store as sessions touch them;
the lint lands once at least the nest and one app store carry
inventories worth checking.

**Re-checked 2026-08-23: still true, with one adjacent build worth
distinguishing.** `bins/fauna-nest/src/db/actor_tables.rs`'s `ACTOR_TABLES`
registry (built 2026-08-11 onward) now enumerates every actor-scoped nest
table against three axes — deletion (`Policy::Purge`/`Retain`), identity
succession (`Succession::Move`/`Burn`/`Stay`/`Partial`), and per-actor
export (`Export::Verbatim`/`Shaped`/`WithheldSecret`/`WithheldDerived`/
`WithheldOperational`/`Unreviewed`) — closing the same deletion-orphan and
export-completeness bug families § Why this is a standing rule names. It
is real per-table classification, but along axes this law does not
define, not a canon/derived/enumerated-local tagging of it: an
`Export::WithheldDerived` row names a re-derivable projection (this law's
class 2 in substance), but `Policy`/`Succession` answer different
questions than "is this byte canon" and the registry does not sort
`nest.db` into the three classes. No store yet carries the inventory this
doc's contract calls for, and no lint runs anywhere — the assessment
above still holds; `actor_tables.rs` is a promising adjacent asset for
whichever session first writes the nest's inventory, not a substitute for
it.
