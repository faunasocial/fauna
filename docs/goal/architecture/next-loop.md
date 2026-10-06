# The pickup loop — public summary

> The full document specifies the tool the maintainers use to start their own
> development sessions one after another on their private work queues, and is
> not published. This abridged summary covers what a contributor needs; other
> documents in `docs/goal/` that reference `next-loop.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. A contributor proposing a change
reads the relevant goal document and opens a pull request.

Two facts from it describe the shape of the work rather than the private setup:

- **Work is picked up one item at a time.** Each development session works a
  single queue item in a fresh context and stops; the next item starts in a new
  session, so nothing is carried from one item into the next.
- **An item is reviewed before it is started.** An independent check confirms
  that the item still matches the goal documents before any work on it begins,
  and an item that waits on other items is started only once those have landed.

Everything else — how sessions are launched and stopped, how a queue item is
chosen and reserved, and how usage limits are handled — is private
infrastructure and out of scope for this repository.

See also [`next-files.md`](next-files.md) for what the work queues are.
