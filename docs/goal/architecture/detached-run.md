# Outliving a supervisor's time budget — public summary

> The full document describes how the maintainers' private development
> machines keep a long verification alive when the tool that started it
> enforces its own time budget, and is not published. This abridged summary
> covers what a contributor needs; other documents in `docs/goal/` that
> reference `detached-run.md` resolve here.

## What a contributor needs to know

Nothing in this repository's build requires it. A plain `cargo build`,
`just <recipe>`, or a single end-to-end run started from your own terminal is
the supported path, and it runs to completion on its own.

One observation from the private setup generalizes:

- **A job started by a supervisor dies with the supervisor's process tree.**
  If whatever launched a long build or test suite enforces a time limit and
  kills its children when the limit runs out, the job dies with them, however
  healthy it was. Start such a job in a session of its own, write its exit
  status to a file, and read that file afterwards, rather than relying on the
  launcher staying alive.

Everything else — the maintainers' own launcher, its status format and the
per-machine measurements behind it — is private infrastructure and out of
scope for this repository.

See also [`build-slot-pool.md`](build-slot-pool.md) for the queue such a job
usually waits in, and [`build-system.md`](build-system.md) for what the builds
themselves do.
