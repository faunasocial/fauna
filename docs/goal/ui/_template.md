# <Page or flow name> — target state

Owns: <comma-separated concept slugs from docs/goal/README.md § Registry; add a registry row in the same commit if the concept is new>
Status: <ratified | draft | partially-specified> — <non-ratified MUST add: resolved by <who/what settles it>; note what is ratified vs open>
Authority: ui.yaml (`<page>` page) owns element IDs + per-page element scope; this doc owns the page UX/behavior, data shape, and the Rust/app split; defers <each neighboring concept> → <its owner doc + §>. On conflict in the other doc's domain, raise it; don't silently diverge.

## Goal
1–3 sentences: what this page accomplishes for the user; what concept it owns.

## Implementation status today
Read-first section: the gap between this doc's target and current code. Current state FIRST — prefer a per-surface × per-app matrix + dated one-liners over narrative history (git owns history; never stack correction-on-correction archaeology). If everything is built, say "fully implemented" in one line.

## Load-bearing claims from referenced docs
Only when this doc's scope depends on another doc's claims: one entry per claim — owner doc file:line + commit hash + ONE sentence transcribing the claim. Omit the section if none.

## Layout & flow
The page's structure (sections, sub-pages, navigation in/out). Reference components from ui.yaml by name rather than re-listing their elements.

## Element IDs
ui.yaml owns the inventory — do NOT restate its element lists (they drift). Carry only the behavior notes this doc owns: visibility conditions, per-ID semantics, and pointers into the ui.yaml page block. New IDs need user approval (rule A) and land in ui.yaml WITH the first implementation.

## State & data shape
The Rust snapshot / struct types the page reads to render. Preferred shape: a single `*_snapshot()` getter on a shared Rust object, returning a typed struct. If no shared Rust type exists today, name what it should be. (TBD allowed; resolve before behavior-changing work — a TBD here is a hard stop: raise the gap before building against it.)

## Where logic lives
Explicit Rust-shared vs app-specific split. Default is "as much in shared Rust as feasible." For each non-trivial behavior, state: shared Rust (named crate/module), app glue (with reason), or undecided. Undecided counts as TBD.

## User actions
Per element / per gesture: what happens. Preferred shape: the app calls a named method on the shared object; method returns a typed result; the app renders the new snapshot. Avoid client-side decisions where the same decision would have to be made on 7 platforms.

## Persistence
What is written to the long-term store, when, in what shape. Cross-reference shared identity-store / settings-store contracts where they exist.

## Errors & edge cases
Empty states, loading states, error surfaces (the `error-message` ID), terminal states, retry/back behavior.

## Architectural rules
Page-specific constraints. Reference other docs' owned claims with a one-line scope description + pointer — never a paraphrase of the mechanism (docs/goal/README.md § Writing protocol).

## Don't do these
Negative rules — patterns previously rejected, per-app divergences this page must not absorb.

## Done definition
Checkboxes a session can use to mark this page complete on its app.

## Reading list
In priority order: `principles.md`, the registry (docs/goal/README.md), related owner docs, ui.yaml section, shared Rust crates, related test files, ui-actual-<app>.yaml.
