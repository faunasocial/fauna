# `fauna-protocol` Schemas

CDDL files in this directory are the **canonical source** for Spec Y wire types. Rust types in `../src/` are hand-rolled and pinned by `tests/conformance.rs` reading binary test vectors in `test_vectors/`.

## Authoring rules (per spec § 2.3)

1. **Maps over arrays** for structured data. Arrays only for genuinely-list-typed data.
2. **Append-only fields with optional modifiers (`?`).** New fields are added only; never renamed or repurposed. Removing a field requires a kind-level deprecation cycle.
3. **`Unknown` variant in every tagged union.** Decoders try known variants in order; fall through to `Unknown { kind, payload }` on no match.
4. **Map values are decoded with key preservation.** Add `* tstr => any` to maps that should preserve unknown keys (default for all payloads). Strict-decoding payloads explicitly omit the catch-all.
5. **Canonical encoding is enforced.** RFC 8949 §4.2.1 + IPLD restrictions: shortest-form integers, lexicographic map key ordering, no float NaN/Inf, definite-length encoding only.

## Namespace policy (per spec § 2.4)

- **Project upstream:** `fauna.<area>.<verb>` for both RPC kinds and push event kinds. Examples: `fauna.bridges.link`, `fauna.account.update`, `fauna.knock`, `fauna.protocol.echo`, `fauna.protocol.resync_required`.
- **Forks:** prefix kinds with reverse-DNS (`com.acme.bridges.foo`) or bech32-pubkey (`npub1abc.<rest>`). Math-deterministic; no central registry.
- **`RpcError.code`** follows the same policy. Reserved infrastructure codes: `fauna.protocol.<error>` (`unknown_kind`, `cancelled`, `replay_too_large`, `timeout`, `frame_too_large`, `internal`, `disconnected`, `malformed_error`).
- **Routing/durability semantics MUST NOT be encoded into the kind string.** Kind answers "what is this?" only.

## Schema-evolution gate

`scripts/check-cddl-evolution.py` runs as the `cddl-evolution-check` cheap merge
gate in both merge scripts (both merge scripts, wired 2026-08-19 —
before that its only executor was the dispatch-only `ci.yml`, i.e. no automatic
path at all; there is no per-PR CI, merges land by direct fast-forward push).
Rationale and residuals: `docs/goal/architecture/merge-gates.md` § Wire-schema
evolution gate. Allowed changes:

- New optional field.
- New variant in a sum type that already has `Unknown`.
- New kind string.

Blocked changes:

- Removing a field, variant, or key.
- Renaming a key.
- Changing a field type.
- Making an optional field required.

For genuinely-breaking changes, introduce a new kind alongside the old one and deprecate the old one.

**The one override is `ratified-breaks.txt` in this directory** (added
2026-09-24): a user-ratified in-place break — one of
`docs/goal/architecture/version-compatibility.md` § Dimension 2's explicit
write-offs — is listed there as `cddl <Type> removed` / `cddl
<Type>.<field> <transition>` (this gate) or `rust
<module>::<Struct>.<wire_field> <transition>` (the struct gate below), and
both gates skip exactly the listed findings: each entry excuses only its own
transition (`removed` or `optional→required`) on its own key. The list only
grows (a removed name never comes back under the same name, and both gates
refuse one that does; each gate also independently refuses a head whose
own-gate entries drop one the merge base carried, catching a deletion even
without a revival), a line lands there only in the commit that carries the
ratification, and it is never a session's own call. The grammar is owned by
`docs/goal/architecture/transport.md` § Schema and forward-compat discipline;
the file's own header restates it for the reader editing it.
Without it a ratified break would red the asynchronous merge-gate check on
every pass — its base is the last all-green tip, which cannot advance while
the check is red.

Its Rust-struct analogue, `tools/check-additive-evolution`, applies the same
rules to the actual `Serialize`/`Deserialize` structs (this crate's wire
payloads + `fauna-segment-store`'s at-rest types) via a `syn` parser rather
than line heuristics — wired as the CHECK-tier `additive-evolution-check` gate
(added 2026-08-21; it runs `cargo`, so it sits
on the asynchronous heavy tier rather than beside `cddl-evolution-check` on
the cheap one). Rationale: `docs/goal/architecture/merge-gate-check.md` §
Merge-gate check.
