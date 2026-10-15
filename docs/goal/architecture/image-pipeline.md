# Image pipeline — public summary

> The full document describes how the maintainers build, stage and verify the
> nest container image on their private infrastructure and is not published.
> This abridged summary covers what a contributor or operator needs; other
> documents and code comments in this repository that reference
> `image-pipeline.md` resolve here.

## What you need to know

- **The nest image is published as a public package** (`ghcr.io/faunasocial/nest`),
  so it can be pulled anonymously.
- **It is built once per release and promoted, never rebuilt.** The same image
  that was built and verified is the one the release tags point at; a tag never
  names a different build than the one that was tested.
- **Tags are release channels.** A running nest follows one channel, which an
  admin can see and change in the app (see
  [`nest/update-channel.md`](nest/update-channel.md)). Pinned per-build tags
  also exist, for reproducing a specific build.
- **The public workflow builds on GitHub-hosted runners.** The image build
  workflow under `.github/workflows/` is self-contained and is the contract for
  how the image is produced from this tree. You do not need the maintainers'
  runners or staging boxes to build or run your own image.
- **Build the image from the repository `Dockerfile`.** Its build cache is a
  speed-up only; a cold build is correct, just slower.

## What is deliberately not here

The maintainers' private staging and acceptance flow, self-hosted runner disk
management, and the post-deploy checks run against their own boxes are private
infrastructure and out of scope for this repository. The build-if-stale
convention, toolchain pins and WASM chunking are summarised in
[`build-system.md`](build-system.md).
