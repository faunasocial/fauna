# Release attempt — public summary

> The full document describes how the maintainers publish a version of this
> repository from their private tree — the checks a publication must pass and
> the steps of a release — and is not published. Other documents in
> `docs/goal/` that reference `release-attempt.md` resolve here.

## What a version must prove before it is published

Nothing is published here on the strength of a review alone. Before a version
becomes public, its commit message has been read and approved in full, the
dependencies it ships have been audited at the exact source being published,
this repository's own `.github/workflows/ci.yml` has been run green on the exact
tree being pushed, and its wire and storage formats have been checked against
the last published version.

## How a version is published

Each version is published in one attempt from a commit on the main branch: the
tree is graded once, the public CI runs on a draft pull request in parallel, and
a real defect is fixed on the main branch and the new commit graded again,
rather than patched on a side branch.

## What this means for a contributor

Nothing in your workflow. See [`publish-gates.md`](publish-gates.md) for how
this repository is curated, and [`release-integrity.md`](release-integrity.md)
for the trust model over released artifacts.
