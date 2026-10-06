# Work-queue files — public summary

> The full document specifies the maintainers' private work-queue files — the
> per-track queues their development sessions pick work from, the state
> vocabulary those files carry, and the tooling contract over them — and is not
> published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `next-files.md` resolve here.

## What the queues are

Development on this project is organized as many parallel sessions, each
working one track at a time from a shared queue. The queue files record, for
each track, what it is (a pointer to the goal document section it serves and a
concrete definition of done), who may pick it up, and its current state — ready,
claimed, blocked on something that must land first, blocked on a decision,
vouched for by an independent review, or a standing watch that asks for
nothing until a named event happens. Every row and every state entry carries a
full timestamp.

The design separates the **task** (written once by its author) from its
**state** (a closed vocabulary, written only by tooling), so that the question
"what can be worked on right now?" is answered mechanically rather than by
reading prose.

## What a contributor needs

Nothing in these files affects how the product is built or run. The
goal documents under `docs/goal/` are the specification of target behavior;
the queues are scheduling around them. A contributor proposing a change reads
the relevant goal document and opens a pull request — the public buildability
contract is `.github/workflows/ci.yml`. See also
[`merge-gates.md`](merge-gates.md).
