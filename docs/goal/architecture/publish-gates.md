# Publish gates — public summary

> The full document describes how this repository is produced from the
> maintainers' private tree — the curation pipeline, the term lists it scans
> for, and where it is pushed from — and is not published. This abridged
> summary covers what a contributor needs; other documents in `docs/goal/` that
> reference `publish-gates.md` resolve here.

## Why this repository is a curated tree

Development happens in a private tree that carries things a public repository
should not: infrastructure detail, work-queue files, internal review logs, and
the maintainers' own operational notes. This repository is produced from that
tree by a transform that drops those paths and rewrites what remains, and it is
regenerated on every publish rather than edited in place.

That means a change published here was written against the same source files
you are reading — the transform removes, it does not rewrite behavior — but it
also means a file present privately may simply be absent here. Where a dropped
document is referenced by one that ships, an abridged stand-in like this one
takes its place, so every link in `docs/goal/` resolves.

## What is checked before anything is published

The publish path is default-deny: a set of independent checks runs over the
curated tree, and publication is blocked unless every one of them passes *and*
reports how much it looked at. A check that silently finds nothing is treated as
a failed check rather than a clean one, and the pipeline includes a self-test
that deliberately plants a finding to prove each check can still fail.

Between them the checks cover the classes a "search for bad strings" pass would
miss: content that survived an encoding, files that are not text and so are
never scanned at all, structural properties of the repository itself, real
high-entropy secrets as opposed to known vocabulary, links and build recipes
that point at something the public tree does not contain, and whether the
bundled third-party notices actually cover what ships.

## What this means for a contributor

Nothing in your workflow. You are working with the tree as published, and
`.github/workflows/ci.yml` is its self-contained buildability contract. The one
consequence worth knowing: a pull request that adds a link or a build step
pointing at a path that does not exist here will be caught, because the same
checks that guard publication also read what you added.

See also [`merge-gates.md`](merge-gates.md) for how checks are tiered, and
[`release-integrity.md`](release-integrity.md) for the trust model over
released artifacts.
