# The protected share — public summary

> The full document records how the maintainers keep one of their private
> development machines responsive while the others build on the same host, and
> is not published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `fleet-protected-share.md` resolve
> here.

## What a contributor needs to know

Nothing in this repository requires any of it. The maintainers' machines share
one host's CPU through a small broker; this document is one rule of that broker.

Two facts from it describe the shape of the work rather than the private setup:

- A machine someone is working on may keep a floor of the shared CPU even when
  it has no build queued, so its interactive sessions are not starved by its
  siblings' builds. The floor is a constant, held only while a session is live
  on the machine, and the rest of the budget is shared as before.
- The floor is graded from the broker's own tick history, never assumed: the
  design names the figures that must move and the control window they are read
  against.

Everything else — the machines, the constant's value, the readings that
prompted it and the rulings it amends — is private.
