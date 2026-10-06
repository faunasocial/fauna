# Session protocol — public summary

> The full document specifies the mechanics of the maintainers' own development
> sessions — how a session starts on a shared work queue, how it hands work
> back when it stops, and the exact shape of the status message it ends on —
> and is not published. This abridged summary covers what a contributor needs;
> other documents in `docs/goal/` that reference `session-protocol.md` resolve
> here.

## What the protocol is

Development on this project runs as many parallel sessions, each working one
track at a time from a shared queue (see [`next-files.md`](next-files.md)).
Two moments in a session's life are fully specified so that no two sessions
collide and no work is silently lost:

- **Startup** — a fixed sequence: sanity, integrate against the shared branch,
  self-serve any red the asynchronous merge check has reported, pick a row
  mechanically rather than by reading the whole queue, have an independent
  reviewer vouch that the row is still wanted under the current specification,
  and only then claim it by landing a one-line commit before any work.
- **Halt** — a fixed-order status message that ends every session: a short
  fleet picture, what landed, what was handed to other sessions, and a
  machine-readable marker saying whether this line of work continues, is done,
  or is waiting on something named. Everything unfinished is written into the
  queue before the halt, because a piece that lives only in a status message
  has no session pointed at it and does not get done.

## The rule worth borrowing

The protocol's central idea is that **capture is free and losing work is the
cardinal failure**. A close moves the finished track's text verbatim into an
append-only archive and leaves a one-line record behind; a halt names every
open piece and where it now lives; a question only a human can answer is
written down as a track whose whole job is to ask it. Each of these is cheap
to write and expensive to skip.

## What a contributor needs

Nothing in this document affects how the product is built or run. The goal
documents under `docs/goal/` are the specification of target behavior; the
session protocol is scheduling and hand-off discipline around them. A
contributor proposing a change reads the relevant goal document and opens a
pull request — the public buildability contract is `.github/workflows/ci.yml`.
See also [`merge-gates.md`](merge-gates.md).
