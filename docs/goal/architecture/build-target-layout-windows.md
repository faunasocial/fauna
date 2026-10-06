# Build target-directory layout, Windows — public summary

> The full document describes how the maintainers' private Windows development
> machine lays out and rations its build artifacts, and is not published. This
> abridged summary covers what a contributor needs; other documents in
> `docs/goal/` that reference `build-target-layout-windows.md` resolve here.

## What a contributor needs to know

**This repository needs no special target-directory setup on Windows.** A plain
`cargo build` or `just <recipe>` in a fresh checkout is the supported path.
Building this tree on Windows does have platform-specific requirements — the
MSVC toolchain and the WinUI build path — but those belong to
[`build-system.md`](build-system.md) and the Windows app documentation, not
here.

Three observations from the private setup generalize:

- **Target directories grow faster here than on the other platforms**, and
  Windows offers no per-directory quota to bound them, so the private fleet
  enforces a size cap in software and sweeps periodically instead. If you keep
  several checkouts of this tree on one Windows machine, budget disk
  accordingly.
- **A filesystem that supports block cloning is worth using for the source and
  build tree.** The private machine keeps the repository on a ReFS Dev Drive
  specifically so a new checkout's target directory can be cloned from an
  existing one rather than rebuilt from scratch.
- **An FFI-touching change costs a full cdylib relink on this platform**, where
  the equivalent change is close to free on Linux. It is a structural cost of
  the linker, not a regression: start the FFI build immediately after such a
  change rather than letting it land on a latency-sensitive path.

Everything else — the cap's values, the cleanup engine and its triggers, the
disk topology, and the seeding mechanism — is private infrastructure and out of
scope for this repository.

See also [`build-system.md`](build-system.md) for the build system a contributor
builds with.
