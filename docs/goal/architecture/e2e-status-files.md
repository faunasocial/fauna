# e2e coordination log — public summary

> The full document specifies how the maintainers file and age out their own
> end-to-end test notes — the per-app coordination log their development
> sessions read and append to — and is not published. This abridged summary
> covers what a contributor needs; other documents in `docs/goal/` that
> reference `e2e-status-files.md` resolve here.

## What the log is

Running the end-to-end suite across seven apps produces a lot of hard-won
knowledge that belongs in neither the tests nor the specification: which failure
signatures are the harness rather than the product, which platform quirk cost a
day to diagnose, what a driver does that the code does not admit. The
maintainers keep that as a shared log, one file per app, which every session
reads before it starts and appends to before it finishes.

## The rule worth borrowing

A log like this has one failure mode, and it is not disorder — it is growth. It
is appended to by everyone and subtracted from by no one, so it eventually
exceeds what any reader will open, and from that moment the instruction to read
it is quietly unfollowable. That happened here: the log reached seven times a
readable size, and a prune five months earlier had not prevented it.

So the log carries a **retention rule with a tool behind it**: each file keeps a
fixed recent window of dated entries and never exceeds a stated size, older
entries roll verbatim into an archive, and undated reference material — how to
run the suite, a platform's standing quirks — is exempt because it has no age.
The move is verbatim and refuses to run if a single line would be lost.

## What a contributor needs

Nothing in these files affects how the product is built or run. The goal
documents under `docs/goal/` are the specification of target behavior; the
testing conventions themselves are [`e2e-conventions.md`](e2e-conventions.md)
and [`testing.md`](testing.md). The public buildability contract is
`.github/workflows/ci.yml`.
