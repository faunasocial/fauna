# e2e fleet load — public summary

> The full document records how the maintainers bound the end-to-end test load
> their private development machines carry, and is not published. This abridged
> summary covers what a contributor needs; other documents in `docs/goal/` that
> reference `e2e-fleet-load.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. Running the end-to-end tests your
change touches, on the app it changes, is the supported path.

Two facts from it describe the shape of the work rather than the private setup:

- **An end-to-end run is long and holds its resources throughout.** A single run
  on one app can take tens of minutes, and a whole-suite run on one app takes
  hours, so the maintainers run the tests a change touches before it lands and
  leave whole-suite runs for changes that reach code many tests share.
- **Cross-app parity is checked after landing, not only before it.** Running
  every test on every app for every change does not scale, so a scheduled run
  covers each app in turn and attributes a failure to the changes since that
  app's last passing run.

Everything else — the machines' measured queue waits and session counts, the
pool widths chosen from them, and the policy that orders the maintainers' own
work queue — is private infrastructure and out of scope for this repository.

See also [`testing.md`](testing.md) for how the tests themselves are organised.
