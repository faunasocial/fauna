# Build target-directory layout, macOS — public summary

> The full document describes how the maintainers' private macOS development
> machine lays out and rations its build artifacts, and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `build-target-layout-macos.md` resolve here.

## What a contributor needs to know

**This repository needs no special target-directory setup on macOS.** A plain
`cargo build` or `just <recipe>` in a fresh checkout is the supported path. The
Apple-platform builds additionally need Xcode and the Swift toolchain; that
belongs to [`build-system.md`](build-system.md) and the Apple app
documentation, not here.

Three observations from the private setup generalize:

- **The Rust target directory is not the whole story on this platform.** Swift
  package build state and the generated FFI framework are comparable in size,
  and a disk-space plan that counts only `target/` will be wrong by roughly half.
- **APFS cloning makes a fresh checkout's build state nearly free to create but
  not to diverge.** The private fleet seeds a new checkout's target directory by
  cloning an existing one; the clone costs almost nothing at first and then
  grows as the two trees build different things. The same trick does *not* help
  for Swift package build state, which is bound to its absolute path and
  rebuilds in full when moved.
- **Free disk is a hard precondition.** Running out of space midway through a
  build on this platform can leave zero-filled binaries behind rather than
  failing cleanly, which is why the private tooling refuses to start heavy work
  below a disk floor rather than letting it begin and fail.

Everything else — the seeding mechanism, the cleanup tiers and their
thresholds, the divergence measurements, and the cached FFI build products — is
private infrastructure and out of scope for this repository.

See also [`build-system.md`](build-system.md) for the build system a contributor
builds with.
