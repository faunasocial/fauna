# Compiling without the network — public summary

> The full document describes how the maintainers' private development
> machines and the project's CI runners keep every compiling build step off
> the network, and is not published. Documents in `docs/goal/` that reference
> `build-compile-boundary.md` resolve here.

**The CI workflows run every compiling `cargo` with the network taken
away**, through the helper `scripts/cargo-boundary.py` and the `cargo` shim
it places on the job's `PATH`; a plain `cargo build` or `just <recipe>` in a fresh checkout
works without either. The per-machine configuration, the sandbox profiles'
measurements and the rulings behind them are private infrastructure and out
of scope for this repository. See also
[`build-compile-cache.md`](build-compile-cache.md) and
[`build-system.md`](build-system.md).
