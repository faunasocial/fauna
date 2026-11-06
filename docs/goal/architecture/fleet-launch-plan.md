# The launch plan — public summary

> The full document records how the maintainers' private development machines
> decide what one working session is handed before it starts, and is not
> published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `fleet-launch-plan.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers' machines are
handed their work from one scheduler; this document is one rule of it.

Two facts from it describe the shape of the work rather than the private setup:

- Several small, related pieces of work may be handed to one session together,
  and that session reports on each piece as it lands it, so a small piece never
  pays a whole session's fixed cost on its own.
- A session is given a plan before it starts — which pieces are independent of
  each other, what each needs, how it should finish — and verifies that plan
  against the code before acting on it; the plan is a reading, never authority.

Everything else — the machines, the figures, the scheduler's mechanics and the
rulings it amends — is private.
