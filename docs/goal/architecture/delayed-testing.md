# Delayed testing — public summary

> The full document designs how the maintainers re-check tests that are
> probably still green in the background on their private development
> machines, and is not published. This abridged summary covers what a
> contributor needs; other documents in `docs/goal/` that reference
> `delayed-testing.md` resolve here.

## What a contributor needs to know

Nothing in this repository requires any of it. Running the end-to-end tests your
change touches — the test files you edit, the tests whose modules import a
harness file you edit, and the witnesses of the feature pages your change
names — on the app it changes is the supported path.

Two facts from it describe the shape of the work rather than the private setup:

- **Every test run records when each test was last green.** The record is per
  test and per platform, and names the commit, so a failure found later can be
  attributed to the commits since that test last passed rather than to
  everything that landed in the meantime.
- **Breadth is checked after landing, in small pieces.** A scheduled runner
  re-checks the tests furthest behind the tree in short shards, one feature
  page's tests at a time, and stops itself at once when the machine is needed
  for something else; a failure it finds is filed with its suspect commits for
  the next session to fix forward.

Everything else — the machines' budgets, the runner's scheduling and how it
yields, and the policy that orders the maintainers' own work queue — is private
infrastructure and out of scope for this repository.

See also [`testing.md`](testing.md) for how the tests themselves are organised
and [`feature-catalog.md`](feature-catalog.md) for the feature matrix those runs
feed.
