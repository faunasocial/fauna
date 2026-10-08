# The Fauna build. Every recipe in this file is one you can run against a clone
# of this repository: the app and server builds, the generators whose output is
# checked in, the test suites, and the lints CI enforces. `just --list` is the
# index, and each recipe's own comment says what it is for.
#
# The build's architecture -- what is generated from what, which artifacts each
# platform produces, and why the toolchain is pinned -- is described in
# docs/goal/architecture/build-system.md.

# ── The recipe shell on Windows ───────────────────────────────────────────────
# just's default shell is `sh -cu`, looked up on PATH. Git Bash has `sh`; a
# PowerShell or cmd.exe terminal does NOT — Git for Windows only puts
# `C:\Program Files\Git\cmd` (git.exe, gitk) on the global PATH, never its
# POSIX bin. So from PowerShell EVERY recipe in this file died on
#   error: Recipe `X` could not be run because just could not find the shell `sh`
# and win sessions had to remember "just only from Git Bash". Naming Git's sh.exe
# outright fixes every non-shebang recipe in both terminals — it is the same
# interpreter Git Bash itself runs, so behaviour is identical, and the setting
# is inert on Linux/macOS. `-cu` reproduces just's own default flags.
#
# RESIDUAL, deliberately not papered over: the 59 `#!/usr/bin/env bash`
# shebang recipes bypass this setting entirely — just translates their
# interpreter path with `cygpath`, which lives in `C:\Program Files\Git\usr\bin`
# and is likewise off the PowerShell PATH ("Could not find `cygpath`
# executable"). No justfile setting reaches that lookup. Putting that directory
# on the global PATH is NOT the fix: it also carries MSYS `link.exe`,
# `find.exe` and `sort.exe`, which shadow the MSVC linker and the Windows
# built-ins — the very trap `scripts/cargo-win.cmd` exists to dodge. Run a
# shebang recipe from Git Bash, or scope the PATH prepend to the one call:
#   $e=$env:PATH; $env:PATH="C:\Program Files\Git\usr\bin;$e"; just <recipe>; $env:PATH=$e
# The Windows dev-setup notes carry the same guidance under
# "Running `just` on Windows".
set windows-shell := ['C:\Program Files\Git\bin\sh.exe', '-cu']

# Force UTF-8 stdout/stderr in Python subprocesses so build scripts that
# print non-ASCII characters (e.g. "→" in build-if-stale.py) work on
# Windows, where Python defaults to the legacy cp1252 code page. No-op on
# Linux/macOS where UTF-8 is already the default.
export PYTHONUTF8 := "1"

# Rosetta (build 367.4 — the Linux dev VM's x86_64 translator) AOT-translates
# every x86_64 binary it runs, and AOT has broken the Android toolchain two ways:
# NDK clang-19 (135 MB) dies with "rosetta error: Failed to map AOT header: 22"
# (a ~128 MiB AOT size cap), and AAPT2's `link` dies silently ("AAPT2 process
# unexpectedly exit", empty stderr) on inputs that had linked before. JIT-only
# translation is unaffected by both, so it is forced for EVERY recipe: each Gradle
# recipe runs AAPT2, and a new one must not have to remember it. Only the Rosetta
# interpreter reads the variable, so it is a no-op wherever the toolchain binaries
# run natively.
export CAMBRIA_DISABLE_AOT := "1"

# Cross-checkout reproducibility of the wasm layer (release-integrity.md
# § Release signing → Web-app verifiability, piece 1). A registry or git crate
# compiles with its ABSOLUTE `$CARGO_HOME/registry/src/…` path in panic
# locations, and a path dependency outside the workspace members (the vendored
# forks) with the checkout's absolute path — so a verifier rebuilding a tagged
# commit in any other checkout or `CARGO_HOME` got different `.wasm` bytes.
# Remapping both roots to fixed placeholders makes the output a function of the
# source alone; the Dockerfile's `rust-wasm` stage sets the SAME placeholders
# for its own roots, so the image's chunks and a `just web` build agree. The
# cargo target dir is the third root: build-script `OUT_DIR` output lives under
# it, and although none of those paths survives into the final `.wasm`, it
# feeds the LLVM module hash that names promoted symbols (`….llvm.<hash>`) —
# measured 2026-09-27, one commit built into two different target dirs gave a
# `fauna_wasm_bg.wasm` differing only in one such suffix. Scoped to the wasm32
# target through cargo's per-target variable: native and host units (build
# scripts, proc-macros) are untouched, and every wasm32 cargo run under `just`
# shares one fingerprint. Order matters — rustc applies the LAST matching
# prefix, so a target dir inside the checkout still maps to `/fauna/target`.
# Not `trim-paths`: its `cargo-features` line would make the workspace manifest
# unloadable by stable cargo (build-target-layout-linux.md, Track C-2).
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS := "--remap-path-prefix=" + justfile_directory() + "=/fauna --remap-path-prefix=" + env("CARGO_HOME", home_directory() / ".cargo") + "=/cargo --remap-path-prefix=" + env("CARGO_TARGET_DIR", justfile_directory() / "target") + "=/fauna/target"

# ── The interpreter: ALWAYS uv, never bare `python3` (user ruling 2026-08-23) ──
# Bare `python3` resolves to a DIFFERENT interpreter per machine — 3.14 on the
# Linux and Windows VMs, but Apple's /usr/bin/python3 3.9.6 on the macOS one,
# because nothing ever symlinked the venv's python next to the `pytest` the same
# provisioning step does symlink. Nothing announced it: the failure surfaces as a
# SyntaxError in a file nobody edited (a `just` recipe was dead at parse on macOS
# on the very day the machine-pool checkouts were created to use it), or as a
# TypeError on a PEP 604 annotation in a driver nobody touched, or — worst — as a
# subtly different stdlib behaviour that never errors at all.
#
# `uv` is a bare command on all three machines and already the repo's Python
# policy, so it is the one resolver that agrees everywhere. The version it
# resolves lives in `.python-version` at the repo root — the SINGLE home for that
# pin, replacing the copies that had accumulated in three dev-setup docs and the
# CI workflows. `--no-project` keeps this an interpreter request and nothing more
# (there is no root pyproject.toml, and this must not start inventing one).
#
# Cost is not a reason to avoid it: measured 33 ms against bare python3's 32 ms.
#
# ⚠ Use `{{py}}` for any stdlib-only script. A script needing third-party
# packages declares them in a PEP 723 `# /// script` header and is invoked as
# `uv run scripts/foo.py` instead — see `lint-ui-elements.py` and friends. The
# two deliberate `/usr/bin/python3` pins in the SessionStart hooks are NOT this
# variable: they are pinned to the system interpreter on purpose.
py := "uv run --no-project python"

# Scratch-tree root for every recipe that builds a throwaway tree (the
# store-safe checks, the android bundle/FOSS checks, publish-heavy-checks).
# `$TMPDIR` when the environment sets one (the Linux dev box pins it to the
# /work/tmp ZFS dataset in /etc/environment; macOS sets a per-user dir);
# otherwise the large-temp dataset where it exists, else /tmp — the one root
# Git Bash on Windows always has. Never a bare `/work/tmp` fallback in a
# recipe body: on Windows mktemp fails on the missing directory before the
# check runs, and the async merge-gate check reports that as a code red.
# The probe is gated on linux for the same reason, and the gate is what makes
# the sentence above true. `just` evaluates `path_exists` with Windows-native
# path rules, where `/work/tmp` is DRIVE-relative — so a stray `D:\work\tmp`
# (2026-09-07: two unit tests hard-coded that box's path and created one) makes
# the probe true on Windows and hands every recipe below the bare `/work/tmp`
# this comment forbids, which the recipe's own bash then resolves under the
# MSYS root instead.
tmp_root := env_var_or_default("TMPDIR", if os() == "linux" { if path_exists("/work/tmp") == "true" { "/work/tmp" } else { "/tmp" } } else { "/tmp" })

# How `mail-bridge-ffi` and `mail-bridge-ffi-check` OBTAIN the Go bindings —
# `regenerate` (the default: run the pinned uniffi-bindgen-go) or `tracked`
# (consume the committed libs/fauna-mail-go as-is, building no bindgen).
#
# This is an ENVIRONMENT CAPABILITY switch, not a preference: the bindgen is a
# single rustc process compiling askama's compile-time templates, and on the
# 2-CPU / 8 GB runner a GitHub-hosted job gets, that process is shut down for
# memory before it finishes — measured three times on 2026-08-28, at 11, 27 and
# 9 minutes in, in both profiles. A machine that cannot build the generator can
# still consume its committed output, which is exactly what the bindings being
# TRACKED is for (build-system.md § UniFFI app bindings).
#
# `regenerate` stays the DEFAULT so the dev loop keeps its self-heal: the
# bindgen tail is keyed on the cargo-built .so, so a rebase or a doc-comment
# edit under libs/fauna-ffi/src/ (which moves the UniFFI checksum) regenerates
# on the next build instead of panicking at run time. CI sets `tracked` because
# its checkout's bindings are already gated — by `mail-bridge-ffi-check` on the
# dev side's merge-gate check, before the commit CI is testing ever landed.
go_bindings := env_var_or_default("FAUNA_GO_BINDINGS", "regenerate")

# The FLAVOR-PRIVATE cdylib slot `mail-bridge-ffi` owns, relative to the cargo
# target dir — and the only `libfauna_ffi` anything mail-bridge-side is allowed
# to read (bindgen input, cgo link, cgo rpath, the macOS installer's staged
# payload). Spelled once here (the `mail-ffi-slot` recipe below) because
# eleven-plus recipes plus the e2e conftest and the macOS installer all derive
# it.
#
# `mail-bridge-ffi` builds fauna-ffi IMPLICIT-HOST (no `--target`), which is
# correct — asking for the host's own triple buys a second artifact tree and no
# sharing (build-machine-resources.md § Cargo target dir layout). But it puts
# this flavor's cdylib in `target/$PROFILE/libfauna_ffi.{so,dylib}`, ONE slot
# that every host build of fauna-ffi AT THAT PROFILE in the workspace writes:
# mac's `_apple-ffi-host-flavor release` (default / test-helpers / store-safe),
# any `cargo build --release -p fauna-ffi` or `--workspace --release`. Sharing
# the BUILD is the win; sharing the PATH is the "whichever built last owns the
# file" trap that `test_mac_host_tree_is_shared.py` names — and it named THIS
# recipe as the other side of the collision while isolating only the apple
# side.
#
# The failure was silent and fail-dangerous, and it is measured, not theorised
# (2026-08-29): `mail-bridge-ffi`'s cargo step is SOURCE-keyed (`--stamp`), so on
# a tree with no `libs/` change it reads fresh and cargo is skipped — leaving the
# foreign cdylib in place — while the bindgen gate below is ARTIFACT-MTIME-keyed
# (`--source "$LIB"`), so the foreign write makes it stale and it regenerates
# from the WRONG flavor. Running the pinned bindgen over a default-features
# `libfauna_ffi.so` emits 26 namespaces against this flavor's 10, and
# `sync-generated-tree.py` then writes all 26 into the committed
# `libs/fauna-mail-go` — the 16 stray crate directories and ~75 k-line diff a mac
# installer build produced from a clean tree that touched no `libs/` source
# .
#
# So: a private slot, written only by a build these recipes just ran. The copy is
# a `cp` inside the gated build command rather than `--artifact-dir` (apple's
# shape) for one reason — `--artifact-dir` would also copy fauna-ffi's 404 MB
# staticlib, which nothing on this path consumes, into a 40 G-capped dataset.
# The flavor is in the PATH, not just a comment: a second mail-bridge flavor gets
# its own directory instead of silently sharing this one.
#
# The slot is PROFILE-KEYED (installers/macos.md § Size & build profile): `mail-bridge-ffi`/`mail-bridge-build` accept a
# `profile` parameter (default `release`; `dist` is the macOS installer's
# shipping build), and cargo's own output slot moves with it
# (`target/$PROFILE/...`), so a bare `release/...` string can no longer carry
# the path for every caller. That is also why this is a RECIPE and not, as it
# used to be, a bare `just` VARIABLE: a `:=` variable is evaluated once per
# `just` PROCESS, before any recipe parameter exists, so it could never read a
# caller's `profile`. Every consumer calls `just mail-ffi-slot [profile]`
# (default `release`, so an unchanged call site is byte-for-byte the same path
# as before) rather than re-deriving the mapping.
mail-ffi-slot profile="release":
    @case "{{profile}}" in dev) echo "debug/mail-bridge-ffi/labeler" ;; *) echo "{{profile}}/mail-bridge-ffi/labeler" ;; esac

# The macOS SDK every Unix cgo link uses — a shell line each cgo recipe
# interpolates beside its CGO_LDFLAGS export (`:` off macOS). Without it cgo's
# `clang` takes the OS-default SDK, which can belong to a NEWER Command Line
# Tools than the selected Xcode's `ld` can parse: mac, 2026-09, CLT 27.0's
# `arm64e.x1` .tbd slices under Xcode 26.5's ld-1267 → "tapi error: malformed
# file", and every mail-bridge e2e test errored at setup. rustc already pins
# its SDK through `xcrun --sdk macosx`; this makes the Go link agree with it.
# A string, not a backtick: nothing runs until a cgo recipe runs it.
cgo_sdkroot := if os() == "macos" { 'export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"' } else { ':' }

# The executable suffix cargo gives a binary on this platform. A build-if-stale
# `--target` naming a cargo binary appends it: on windows an extensionless path
# never exists, so the gate reads "missing → stale" on every call and takes a
# `build` slot even on a warm tree.
exe_suffix := if os() == "windows" { ".exe" } else { "" }

# ── Optional machine-wide slot helper ─────────────────────────────────────────
# Heavy recipes (workspace cargo builds, wasm-pack, the Android and Apple builds,
# docker images) run through `scripts/build-slot.py` when that script is present,
# so several checkouts on one machine queue instead of competing for it. The
# script is the maintainers' own and is not part of this tree: without it every
# `slot_*` variable below is the empty string and the recipe line runs directly.
slot_build :=if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { py + " scripts/build-slot.py --pool build --" } else { "" }
slot_build_wide := if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { py + " scripts/build-slot.py --pool build --cores 4-8 --" } else { "" }
slot_tier_1 :=if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { py + " scripts/build-slot.py --pool tier_1 --" } else { "" }
slot_focus :=if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { if os() == "linux" { "" } else { py + " scripts/build-slot.py --pool focus --" } } else { "" }
slot_build_body :=if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { '[ -n "${FAUNA_SLOT_HELD_BUILD:-}" ] || exec ' + py + ' scripts/build-slot.py --pool build -- bash "$0"' } else { ":" }
slot_tier_1_body := if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { '[ -n "${FAUNA_SLOT_HELD_TIER_1:-}" ] || exec ' + py + ' scripts/build-slot.py --pool tier_1 -- bash "$0"' } else { ":" }
# The display-deadlock guard for the Apple FFI recipes (`_apple-ffi-bindgen` has
# the why). `gpu_check` is a whole statement: it fails the recipe, naming the
# cause, when the dev fleet's macOS guest has a deadlocked display. `gpu_watch`
# prefixes one command, running it under the same check. The detector is private
# dev-fleet machinery like build-slot.py (the fault is the guest's paravirtual
# GPU driver), so both degrade to a bare run where the script is absent.
gpu_check := if path_exists(justfile_directory() / "scripts/mac-gpu-deadlock.py") == "true" { py + " scripts/mac-gpu-deadlock.py --check" } else { ":" }
gpu_watch := if path_exists(justfile_directory() / "scripts/mac-gpu-deadlock.py") == "true" { py + " scripts/mac-gpu-deadlock.py --watch --" } else { "" }

# The win-only MSBuild.exe path, discovered from THIS machine's own Visual
# Studio / Build Tools install (build-system.md § Windows toolchain
# location) rather than one box's pinned build number — a contributor whose
# install differs even slightly used to fail on step one of the documented
# windows build. `just` variables are evaluated EAGERLY on every invocation,
# for every recipe, on every machine sharing this justfile (measured directly:
# a failing backtick here breaks `just <anything>` fleet-wide, not just the
# windows recipes that use it) — so the backtick is gated behind
# `os() == "windows"`, whose `if`/`else` DOES short-circuit (also measured):
# a non-Windows session never touches this at all. `just` runs a backtick on
# windows under `windows-shell` above — Git's sh, measured 2026-10-03, not
# cmd.exe as this comment said until then — and that sh runs a `.cmd` it is
# handed by itself (measured: a `cmd //c` wrapper here double-invokes cmd and
# fails), so the backtick just names the script directly. `vs-install-path.cmd` always succeeds (falls back to this
# box's last-known-good path internally), so the branch windows itself takes
# never fails either.
msbuild_exe := if os() == "windows" { trim(`scripts/vs-install-path.cmd`) + "/MSBuild/Current/Bin/MSBuild.exe" } else { "" }

# ── Per-out-dir wasm mutex ────────────────────────────────────────────────────
# The 'build' pool above bounds LOAD, not OUTPUT DIRS: it is 2 slots wide, so two
# wasm-pack runs writing the SAME pkg/ both get a slot and corrupt it — the cryptic
# `Error: invalid type: sequence, expected a string at line 7 column 11` right after
# `Finished release profile` (the compile succeeded; the wasm-bindgen/wasm-opt
# post-step tripped over files the other build was rewriting). It is a real
# within-a-checkout case, not a theoretical one: `web` and `web-check` depend on
# `wasm` while `web-test` (what the e2e conftest builds) depends on wasm-core + 5
# more of the SAME chunks, so a session type-checking the SPA while pytest builds
# the test SPA runs two wasm-core builds into one pkg/. Until now the only guard
# was human discipline ("never run web-check during an e2e wasm build").
#
# Keyed on the out-dir's ABSOLUTE path — NOT the crate name. Parallel sessions each
# work in their own checkout of the repo, with its own libs/*/pkg/ and its own
# CARGO_TARGET_DIR, so two checkouts building one chunk write different files and
# are disjoint — they MUST stay parallel. A crate-name key would instead throttle
# every checkout on the machine (~22 of them) to one wasm build at a time, to fix a
# collision that cannot happen between them. Path-keying also keeps onboarding's
# pkg vs pkg-test uncoupled, which is exactly what made them safe.
#
# ORDERING IS LOAD-BEARING: the mutex is acquired OUTSIDE the build slot. The
# reverse deadlocks — A holds a build slot waiting on the mutex while B holds the
# mutex waiting for a build slot, and the build pool's 5400s bound means a
# 90-minute wedge before it fails. Asserted by tests/e2e-unified/tests/test_build_slot.py.
# Absent-script degradation, but NOT `slot_build`'s empty string: this variable is
# used as `{{wasm_mutex}}<out-dir> -- <cmd>`, so the `<out-dir> --` half is an
# ARGUMENT OF THE MUTEX CALL, not of the command. Expanding to nothing therefore
# leaves the shell running `libs/fauna-wasm/pkg -- <cmd>` — it tries to execute a
# directory, exit 126, and the wrapped build never runs. Measured 2026-08-24 in the
# local public-CI replay: the `web` job died there on the curated tree, where the
# private `scripts/build-slot.py` is deliberately absent. The shim below consumes
# exactly that two-argument pair and execs the rest, so every call site degrades to
# the bare command with no per-site special casing.
wasm_mutex := if path_exists(justfile_directory() / "scripts/build-slot.py") == "true" { py + " scripts/build-slot.py --pool wasm-outdir:" + justfile_directory() + "/" } else { "sh -c 'shift 2; exec \"$@\"' slot-noop " }

# Build all WASM chunks (no-op when sources unchanged).
# `wasm-content-index` is intentionally NOT here — tantivy can't run in a
# browser (spawns background threads; panics on wasm32-unknown-unknown). The
# crate is on hold (tracked internally). The recipe below stays so a future
# session can re-attempt the gate.
#
# ── Linux only ────────────────────────────────────────────────────────────────
# Web is built only on Linux (owner-ruled 2026-10-05, build-system.md § The Deno
# build sandbox): the `[macos]`/`[windows]` variant refuses. `linux` runs the 9
# chunks CONCURRENTLY under a SINGLE build slot (the `[linux]` recipe below)
# rather than as a sequential prerequisite list (measured 2026-07-23,
# dev-resources plan):
#   * a wasm build's cost is ~15% cargo compile + ~85% the wasm-bindgen/
#     wasm-opt tail. The compiles can't overlap (all 9 chunks share one
#     CARGO_TARGET_DIR, so cargo's build-directory lock serialises them — proven),
#     but the tails run OUTSIDE that lock and `wasm-opt` used only ~5-10 of the
#     then-16-core wasm mask solo, so 2-3 tails overlapped for
#     free. A controlled A/B (two `wasm-opt -O` runs seq vs concurrent,
#     interleaved + order-reversed) measured ~2x on the tail with no mask
#     contention.
[macos]
[windows]
wasm:
    @echo "web is built only on Linux — build-system.md § The Deno build sandbox" >&2 && exit 1

# Linux: fan the 9 chunks out concurrently, but inside ONE build slot and with
# the out-dir mutexes hoisted ABOVE that slot, so this is a good machine citizen:
#   * ONE build slot, not two — parallel builds keep a free slot (plain
#     `[parallel]` on the chunks would instead grab BOTH slots for the whole run).
#   * Queues for that slot ONCE, not once per chunk (the dominant cost on a busy
#     box — 23 slot-waits in a measured sequential baseline).
#   * Deadlock-safe: the GLOBAL lock order is "all out-dir mutexes, THEN the build
#     slot" — the same mutex-outside-slot order every per-chunk `_wasm-*-impl`
#     line already follows (asserted by test_build_slot.py). A process blocked on a
#     mutex therefore holds no slot, so no hold-and-wait cycle can form. Acquiring
#     all 9 mutexes up front is safe because parallel sessions work in separate
#     checkouts with DISJOINT (path-keyed) mutexes, and within one checkout the
#     9 are always requested in this one fixed order.
# Once the wrapper holds the 8 mutexes + 1 slot, each `_wasm-*-impl` the fan-out
# invokes re-requests its own mutex + the build slot and gets a reentrant no-op
# (FAUNA_SLOT_HELD_<POOL>), so it runs directly. build-if-stale still gates each
# chunk (the fan-out calls the PUBLIC recipes), so unchanged chunks stay no-ops.
# The 9 cargo compiles still serialise on the shared build-directory lock, so the
# concurrency this buys is specifically the wasm-opt tails overlapping — which is
# where ~85% of the cost is.
#
# A fully warm tree must still take NEITHER the mutexes NOR the slot (same-class
# residue as the mail-bridge/prev-build `--stamp` fixes — build-system.md §
# Build/e2e slot locks): so BEFORE any of that acquisition, probe all 9 chunks'
# own freshness gates via `build-if-stale --check` (no cmd, no mutex, no slot).
# Only on a real miss does the mutex+slot chain below run, unchanged.
[linux]
wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    # Each --check line mirrors its sibling PUBLIC recipe's own --target list
    # EXACTLY (wasm-core / wasm-onboarding / wasm-launch /
    # wasm-folders / wasm-backups / wasm-media / wasm-labeler-catalog /
    # wasm-atproto-settings
    # above) — if a target is added/renamed there, mirror it here too, or a
    # genuinely-stale chunk could read as fresh and the whole fan-out silently
    # skips.
    common=({{py}} scripts/build-if-stale.py --check -q --source libs --source Cargo.lock --exclude "*/pkg/*" --exclude "*/pkg-test/*")
    fresh=1
    # mirrors: wasm-core (openmls-dependent — see wasm-core's comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-core \
        --target libs/fauna-wasm/pkg/fauna_wasm_bg.wasm \
        --target libs/fauna-wasm/pkg/fauna_wasm.js \
        --target libs/fauna-wasm/pkg/fauna_wasm.d.ts \
        --target libs/fauna-wasm/pkg/fauna_wasm_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-onboarding
    "${common[@]}" --label wasm-onboarding \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.js \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.d.ts \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-launch (two-producer chunk — same caveat as
    # wasm-atproto-settings below)
    "${common[@]}" --label wasm-launch \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch.js \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch.d.ts \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-folders (openmls-dependent — see wasm-folders' comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-folders \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_folders.js \
        --target apps/fauna-web/static/fauna_wasm_folders.d.ts \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-backups (openmls-dependent — see wasm-backups' comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-backups \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_backups.js \
        --target apps/fauna-web/static/fauna_wasm_backups.d.ts \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-media (openmls-dependent — see wasm-media's comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-media \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_media.js \
        --target apps/fauna-web/static/fauna_wasm_media.d.ts \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-labeler-catalog
    "${common[@]}" --label wasm-labeler-catalog \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.js \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.d.ts \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-share
    "${common[@]}" --label wasm-share \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_share.js \
        --target apps/fauna-web/static/fauna_wasm_share.d.ts \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-connected-apps
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-connected-apps \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.js \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.d.ts \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-atproto-settings (two-producer chunk — see that recipe's
    # comment; this --check only proves `pkg/` itself is fresh, same caveat
    # web-test's own two-producer mirrors below call out)
    "${common[@]}" --label wasm-atproto-settings \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.js \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.d.ts \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm.d.ts || fresh=0
    if [ "$fresh" = "1" ]; then
        # `fauna_wasm`/`fauna_wasm_onboarding`/`fauna_wasm_launch`/
        # `fauna_wasm_atproto_settings` are two-producer chunks (prod vs test
        # flavor share one `static/` slot — see `wasm-core-test`'s comment).
        # The --check above only proves `pkg/` itself is fresh; it says nothing
        # about which flavor `static/` currently holds (e.g. a prior
        # `just web-test` left the TEST flavor there). Re-run all four
        # two-producer mirrors unconditionally — cheap (byte-compare, no
        # wasm-pack) — same reasoning, and now the same full set, as
        # `[linux] web-test:`'s own fast path below.
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm \
            --from libs/fauna-wasm/pkg --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_onboarding \
            --from libs/fauna-wasm-onboarding/pkg --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_launch \
            --from libs/fauna-wasm-launch/pkg --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_atproto_settings \
            --from libs/fauna-wasm-atproto-settings/pkg --to apps/fauna-web/static
        echo "[wasm] all 10 chunks up-to-date — no build slot taken"
        exit 0
    fi
    {{wasm_mutex}}libs/fauna-wasm/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-onboarding/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-launch/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-folders/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-backups/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-media/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-labeler-catalog/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-share/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-connected-apps/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-atproto-settings/pkg -- \
    {{slot_build_wide}} just _wasm-fanout

# Private: the concurrent fan-out. ONLY invoked by the `[linux]` `wasm:` wrapper
# above, which has already acquired the 10 out-dir mutexes + 1 build slot — so the
# per-chunk `_impl` mutex/slot acquisitions inside these recipes are reentrant
# no-ops. NEVER call this directly: it has no slot of its own, so a bare
# `just _wasm-fanout` would fan 9 wasm-pack builds out with no slot — and no
# CPU pin either, since the pin comes FROM the slot (build-slot.py).
# `[linux]`-only for the same reason the wrapper is (see the OS split above).
[linux]
[parallel]
_wasm-fanout: wasm-core wasm-onboarding wasm-launch wasm-folders wasm-backups wasm-media wasm-labeler-catalog wasm-share wasm-connected-apps wasm-atproto-settings

# CHECK-tier gate (merge-gate check leg, added 2026-07-29 with the rust-first
# default-workflow ratification): per-crate wasm32 TYPE-CHECK of every shipped
# wasm chunk. Catches the "web silently no longer compiles" class (wire-struct
# construction breaks in cfg(wasm32) arms, wasm-incompatible dep drift — the
# recurring tokio-net→mio class) at ~5% of a full `wasm` build: no codegen, no
# wasm-bindgen, no wasm-opt. TWO load-bearing shapes (build-system.md
# § Merge-gate check):
#   * ONE cargo invocation PER crate, never a combined `-p` list — cargo unions
#     features across listed packages, which activates a tokio-net→mio edge no
#     real per-crate wasm-pack build ever sees (measured 2026-07-29: combined
#     form false-reds on mio in 2s while every per-crate check is green).
#   * Keep the crate list in sync with the `wasm` aggregate above.
#     fauna-wasm-content-index stays EXCLUDED, same as there (on hold; its
#     standalone check is red today — ungated rot, not a gate false-red).
# This does NOT replace `web-check`: SPA TypeScript drift against regenerated
# .d.ts needs a real wasm-pack run (web trickle-down sessions' entry gate).
#   * The four FLAVORED chunks are checked BOTH ways. A flavor only the e2e lane
#     builds rots invisibly otherwise: nothing else compiles it, so a shared-crate
#     change can break `test-helpers` and the break surfaces as a mystery e2e
#     failure hours later instead of here. (This is also the positive half of
#     convention 15 — evidence the opt-in feature still builds at all.)
#   * Two NON-chunks: `fauna-account-plane`, the account plane's wasm-capable
#     half (account-client-lifecycle.md § The client-side lifecycle → The
#     trigger fired, rulings (1) and (2) — with `account-driver`, the driver
#     web will host), and `fauna-account-seams`, the conversations seams over
#     it that web registers (ruling (4)). No chunk links either yet — until
#     web hosts the account runtime these lines are the only thing that
#     proves they still build for wasm32 (the wasm-wallclock gate cannot see
#     them before a chunk does either).
wasm-chunk-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-onboarding
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-onboarding --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-launch
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-launch --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-account-store
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-account-store --features test-helpers --tests
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-folders
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-backups
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-media
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-labeler-catalog
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-share
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-connected-apps
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-atproto-settings
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-wasm-atproto-settings --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-account-plane --features account-driver
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-account-seams
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked --target wasm32-unknown-unknown -p fauna-account-port

# Convention-15 witness for the web/wasm leg: assert each covered production
# chunk exports NONE of the e2e injection seams, and that its test flavor still
# does. Builds every flavor pair (each build-if-stale gated, so this is cheap to
# repeat) and diffs each pair's generated `.d.ts` to derive the seam list rather
# than hard-coding one — see the script's docstring for why that ordering
# matters, and why the JS glue rather than the `.wasm` is the artifact to assert
# on (`wasm-opt` strips the binary's name section, so a `strings` check on the
# `.wasm` alone false-greens).
#
# Four of the ten seam-carrying chunks are covered: `fauna-wasm` (the
# original), `fauna-wasm-atproto-settings` (added 2026-08-10 — its
# `enableFakePlcDirectoryForTest` seam is an SSRF-adjacent PLC-directory-URL
# override), `fauna-wasm-onboarding` (
# prioritized ahead of the other seven: its seams inject a nest identity pin +
# a captured DNS credential, `setNestIdentityPinForTest`/
# `setCapturedDnsCredentialForTest`/etc., a materially richer surface than the
# remaining chunks' bare `panicForTestOnly`), and `fauna-wasm-launch` (added
# 2026-09-23, when it became a two-producer chunk for the launch clock's e2e
# seam — `launchClock*ForTest` plus the constructor's localStorage offset
# seed). Six chunks remain
# unwitnessed — build-system.md § Chunks with two producers tracks the gap and
# states the count explicitly; batching them is its own follow-up (the review
# finding that added onboarding scoped that split on purpose — ten flavor
# pairs is real build cost, sequenced by risk rather than paid all at once).
#
# NOT on the synchronous cheap merge tier: it needs real wasm builds,
# which is compile-scale (build-system.md § CI enforcement — a compile-scale check
# goes in the merge-gate check, never back onto the merge path).
wasm-seam-check: wasm-core wasm-core-test wasm-atproto-settings wasm-atproto-settings-test wasm-onboarding wasm-onboarding-test wasm-launch wasm-launch-test
    {{py}} scripts/check-wasm-seam-exclusion.py
    {{py}} scripts/check-wasm-seam-exclusion.py \
        --prod-pkg libs/fauna-wasm-atproto-settings/pkg \
        --test-pkg libs/fauna-wasm-atproto-settings/pkg-test \
        --stem fauna_wasm_atproto_settings
    {{py}} scripts/check-wasm-seam-exclusion.py \
        --prod-pkg libs/fauna-wasm-onboarding/pkg \
        --test-pkg libs/fauna-wasm-onboarding/pkg-test \
        --stem fauna_wasm_onboarding
    {{py}} scripts/check-wasm-seam-exclusion.py \
        --prod-pkg libs/fauna-wasm-launch/pkg \
        --test-pkg libs/fauna-wasm-launch/pkg-test \
        --stem fauna_wasm_launch

# Core WASM chunk (libs/fauna-wasm) — PRODUCTION flavor.
#
# Staleness watches the WHOLE `libs/` tree (minus wasm-pack `pkg/` outputs), not
# just this crate's own src: a wasm bundle's behavior depends on every workspace
# crate it transitively pulls in (fauna-core, fauna-protocol, …), and watching
# only `libs/fauna-wasm/src` silently shipped stale wasm when a shared crate
# changed. Over-rebuilding on an unrelated `libs/` edit is the cheap, safe
# failure mode; silently-stale wasm is not. Cargo.lock catches dep-version bumps.
#
# TWO PRODUCERS since 2026-07-30 — this recipe and `wasm-core-test` below, which
# builds the SAME crate with `--features test-helpers` (the e2e injection seams;
# testing.md § convention 15). The shape is copied wholesale from
# `wasm-onboarding`/`wasm-onboarding-test`, whose comment explains why each half
# is load-bearing; in short:
#
#   * The gate watches THIS recipe's own `pkg/`, never `static/`. When a gate
#     watched `static/`, the sibling flavor wrote those exact targets, so the next
#     prod build compared `libs/` against a just-written TEST artifact, concluded
#     "up-to-date", and skipped — leaving the test-helpers bundle in `static/`.
#     That is the flavor-staleness false-green convention 15 names as its one
#     structural caveat, and it was live in the fleet for the onboarding chunk.
#   * `static/` is a shared slot both producers write, so the mirror below runs
#     UNCONDITIONALLY (outside the gate) to re-assert this variant's artifacts. It
#     compares bytes, so an unchanged chunk leaves mtimes — and `web`'s own
#     `--source apps/fauna-web/static` gate — untouched.
#
# Before this split, `wasm-core` was the ONLY producer and cp'd straight into
# `static/`, which is exactly why the seams could not be gated at all: there was
# no test flavor to put them in.
wasm-core:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-core \
        --target libs/fauna-wasm/pkg/fauna_wasm_bg.wasm \
        --target libs/fauna-wasm/pkg/fauna_wasm.js \
        --target libs/fauna-wasm/pkg/fauna_wasm.d.ts \
        --target libs/fauna-wasm/pkg/fauna_wasm_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' \
        -- just _wasm-core-impl
    {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm \
        --from libs/fauna-wasm/pkg --to apps/fauna-web/static

_wasm-core-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm --target web

# Test-flavoured core wasm: adds `libs/fauna-wasm`'s `test-helpers` feature, which
# is what compiles the e2e injection seams (`installMockBackendsForTest`,
# `injectInboundForTest`, `injectSendFailure`, `createMlsGroupForTest`,
# `clearForTest`, `injectPostsForTest`, `enableDnsFakeProviderForTest`, …) into the
# bundle and exports them to JS. Used by `just web-test` and the conftest
# `static_dir` fixture; `just wasm` / `just web` build the production flavor above,
# which exports NONE of them (testing.md § convention 15 — the automation surface
# is compiled out of release artifacts, verifiable by grepping the built glue).
#
# `debug_assertions` is deliberately NOT the lever: wasm-pack builds release, so a
# profile-keyed gate would be on in neither flavor. Per convention 15 rule (b) a
# wasm export of a seam is keyed on the FEATURE ALONE, so the generated JS/.d.ts
# face stays a pure function of the feature set.
#
# Builds into `pkg-test`, NOT the `pkg` that `wasm-core` gates on — see that
# recipe's two-producer comment for why sharing the out-dir is a deterministic
# false-green rather than a race. The mirror re-asserts THIS variant into the
# shared `static/` slot, which is what makes `just web` and `just web-test`
# correct in either order.
wasm-core-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-core-test \
        --target libs/fauna-wasm/pkg-test/fauna_wasm_bg.wasm \
        --target libs/fauna-wasm/pkg-test/fauna_wasm.js \
        --target libs/fauna-wasm/pkg-test/fauna_wasm.d.ts \
        --target libs/fauna-wasm/pkg-test/fauna_wasm_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' \
        -- just _wasm-core-test-impl
    {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm \
        --from libs/fauna-wasm/pkg-test --to apps/fauna-web/static

_wasm-core-test-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm/pkg-test -- {{slot_build}} wasm-pack build libs/fauna-wasm --target web --out-dir pkg-test -- --features test-helpers

# Onboarding WASM chunk (libs/fauna-wasm-onboarding).
#
# This chunk has TWO producers — this recipe and `wasm-onboarding-test` below,
# which builds the SAME crate with `--features test-helpers`. They therefore
# cannot share either output path:
#
#   * Separate wasm-pack out-dirs (`pkg` vs `pkg-test`) keep the gate honest.
#     The staleness gate watches THIS recipe's own `pkg/`, never `static/`: when
#     both watched `static/`, the ungated test build wrote those exact targets,
#     so the next prod build compared `libs/` against a just-written test
#     artifact, concluded "up-to-date", and skipped — leaving the test-helpers
#     bundle in `static/` until someone happened to touch `libs/`. That is a
#     deterministic false-green, not a race, and it was live in the fleet.
#   * `static/` is a shared slot both producers write, so the mirror below runs
#     UNCONDITIONALLY (outside the gate) to re-assert this variant's artifacts.
#     It compares bytes, so an unchanged chunk leaves mtimes — and `web`'s own
#     `--source apps/fauna-web/static` gate — untouched.
#
# `--exclude '*/pkg-test/*'` is load-bearing: `--source libs` would otherwise
# see the test out-dir as a SOURCE for every wasm chunk, so one test build would
# mark all of them permanently stale and force endless full rebuilds.
wasm-onboarding:
    @{{py}} scripts/build-if-stale.py --label wasm-onboarding \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.js \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding.d.ts \
        --target libs/fauna-wasm-onboarding/pkg/fauna_wasm_onboarding_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' \
        -- just _wasm-onboarding-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_onboarding \
        --from libs/fauna-wasm-onboarding/pkg --to apps/fauna-web/static

_wasm-onboarding-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-onboarding/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-onboarding --target web

# Launch WASM chunk (libs/fauna-wasm-launch) — the shared `LaunchMachine`'s JS
# bindings, which drive the § App-launch routing rows off the SPA's
# `LaunchPersistence` impl (`lib/onboarding/launch-persistence.ts`).
#
# The crate shipped its bindings but never got a recipe, so nothing
# ever built it for wasm32 and no `static/` artifact existed for the SPA to
# import — a dark crate. Its own doc comment ("Loaded by the web client at
# app-launch time") described an adoption that could not happen. This recipe is
# what makes it real; keep it in `wasm` AND (as `wasm-launch-test`) in
# `web-test`'s chunk list below.
#
# TWO producers since 2026-09-23, same shape as wasm-onboarding/
# wasm-onboarding-test — this recipe and `wasm-launch-test` below, which builds
# the SAME crate with `--features test-helpers` (the launch clock's e2e seam:
# the `LaunchMachine` constructor's localStorage offset seed and the
# `launchClock*ForTest` getters the wrong-clock launch witness reads — a browser
# has no process env, so it cannot take native's `FAUNA_E2E_CLOCK_OFFSET_SECS`).
# Separate out-dirs (`pkg` vs `pkg-test`), each gate keyed on its own out-dir,
# and the `static/` mirror below runs unconditionally so `just web` and
# `just web-test` are correct in either order (see wasm-onboarding's comment
# for the false-green story this shape avoids).
wasm-launch:
    @{{py}} scripts/build-if-stale.py --label wasm-launch \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch.js \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch.d.ts \
        --target libs/fauna-wasm-launch/pkg/fauna_wasm_launch_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-launch-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_launch \
        --from libs/fauna-wasm-launch/pkg --to apps/fauna-web/static

_wasm-launch-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-launch/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-launch --target web

wasm-launch-test:
    @{{py}} scripts/build-if-stale.py --label wasm-launch-test \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch_bg.wasm \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch.js \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch.d.ts \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-launch-test-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_launch \
        --from libs/fauna-wasm-launch/pkg-test --to apps/fauna-web/static

_wasm-launch-test-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-launch/pkg-test -- {{slot_build}} wasm-pack build libs/fauna-wasm-launch --target web --out-dir pkg-test -- --features test-helpers

# Test-flavoured onboarding wasm: includes the test-helpers feature so
# the e2e bridge can call setHandleCheckSnapshotForTest /
# setInviteRequestSnapshotForTest / setStepForTest. Used by `just
# web-test` and the conftest static_dir fixture.
#
# Builds into `pkg-test`, NOT the `pkg` that `wasm-onboarding` gates on — see
# that recipe's comment. Sharing the out-dir let this ungated build satisfy the
# prod chunk's staleness gate, so `just web` silently bundled the test-helpers
# wasm (these very snapshot setters) and never rebuilt. The mirror then re-asserts
# THIS variant into the shared `static/` slot, which is what makes `just web` and
# `just web-test` correct in either order.
#
# GATED since 2026-07-24, mirroring `wasm-onboarding` above. It previously
# rebuilt unconditionally, justified as "the feature flag flips between dev and
# prod variants without changing source files" — but that rationale predates the
# `pkg`/`pkg-test` split described above. Once the out-dirs were separated,
# `pkg-test` became this recipe's EXCLUSIVE output, always built with
# `--features test-helpers`, so the flag can no longer vary for it and mtime
# freshness is a sound signal.
#
# The cost of leaving it ungated was not small: `wasm-pack` re-ran `wasm-bindgen`
# + `wasm-opt` on EVERY `--client web` pytest invocation (cargo itself was warm —
# "Finished release profile in 0.37s" — but wasm-opt is the expensive half).
# Measured 2026-07-24 on the primary dev VM: 1m26s–2m10s per invocation, against
# per-file totals of 2m05s–2m30s, i.e. ~80-90% of each run was this rebuild,
# ~78 min of a 113-min 39-file sweep. That is what made per-file isolation look prohibitively
# expensive; isolation is cheap, the missing gate was not.
#
# The `sync-wasm-static.py` mirror deliberately stays OUTSIDE the gate: `static/`
# is a slot both producers write, so this variant must re-assert itself even when
# its own build was skipped. The script compares bytes, so an unchanged chunk
# leaves mtimes (and `web`'s own `--source apps/fauna-web/static` gate) untouched.
wasm-onboarding-test:
    @{{py}} scripts/build-if-stale.py --label wasm-onboarding-test \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding_bg.wasm \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding.js \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding.d.ts \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' \
        -- just _wasm-onboarding-test-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_onboarding \
        --from libs/fauna-wasm-onboarding/pkg-test --to apps/fauna-web/static

_wasm-onboarding-test-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-onboarding/pkg-test -- {{slot_build}} wasm-pack build libs/fauna-wasm-onboarding --target web --out-dir pkg-test -- --features test-helpers

# Folder wizard + page-level Devices WASM chunk (libs/fauna-wasm-folders:
# `FolderWizardMachine` + `DevicesMachine`). Wired into the `wasm:` aggregate
# (and `web-test`) so `just web` builds it; the web Devices page renders off the
# shared `DevicesMachine` via `$lib/wasm-folders` (web-renderer track).
wasm-folders:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-folders \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_folders.js \
        --target apps/fauna-web/static/fauna_wasm_folders.d.ts \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-folders-impl

_wasm-folders-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-folders/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-folders --target web
    cp libs/fauna-wasm-folders/pkg/fauna_wasm_folders_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-folders/pkg/fauna_wasm_folders.js apps/fauna-web/static/
    cp libs/fauna-wasm-folders/pkg/fauna_wasm_folders.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-folders/pkg/fauna_wasm_folders_bg.wasm.d.ts apps/fauna-web/static/

# Page-level Backups WASM chunk (libs/fauna-wasm-backups: the snapshot half's
# `BackupsMachine`). Its own chunk (not folded into fauna-wasm-folders) because
# Backups is a distinct page from Devices — the same reason wasm-media and
# wasm-labeler-catalog stand alone. Wired into the `wasm:` aggregate (and
# `web-test`) so `just web` builds it; the web Backups page renders the snapshot
# half off the shared `BackupsMachine` via `$lib/wasm-backups`.
wasm-backups:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-backups \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_backups.js \
        --target apps/fauna-web/static/fauna_wasm_backups.d.ts \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-backups-impl

_wasm-backups-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-backups/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-backups --target web
    cp libs/fauna-wasm-backups/pkg/fauna_wasm_backups_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-backups/pkg/fauna_wasm_backups.js apps/fauna-web/static/
    cp libs/fauna-wasm-backups/pkg/fauna_wasm_backups.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-backups/pkg/fauna_wasm_backups_bg.wasm.d.ts apps/fauna-web/static/

# Page-level Media WASM chunk (libs/fauna-wasm-media: the cross-set all-media
# `MediaMachine`). Its own chunk (not folded into fauna-wasm-folders) because
# Media is a distinct page from Devices. Wired into the `wasm:` aggregate so
# `just web` builds it; the web Media page renders off the shared `MediaMachine`
# via `$lib/wasm-media`.
wasm-media:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-media \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_media.js \
        --target apps/fauna-web/static/fauna_wasm_media.d.ts \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-media-impl

_wasm-media-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-media/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-media --target web
    cp libs/fauna-wasm-media/pkg/fauna_wasm_media_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-media/pkg/fauna_wasm_media.js apps/fauna-web/static/
    cp libs/fauna-wasm-media/pkg/fauna_wasm_media.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-media/pkg/fauna_wasm_media_bg.wasm.d.ts apps/fauna-web/static/

# Page-level community-labeler-catalog WASM chunk (libs/fauna-wasm-labeler-catalog:
# `LabelerCatalogMachine` — browse / inspect-before-subscribe / (un)subscribe).
# Its own chunk (not folded into fauna-wasm-folders/-media) because the
# labeler-catalog page is distinct from Devices/Media. Wired into the `wasm:`
# aggregate so `just web` builds it; the web labeler-catalog page (and the
# personalization home's subscribed-labelers facet) render off the shared
# `LabelerCatalogMachine` via `$lib/wasm-labeler-catalog`.
wasm-labeler-catalog:
    @{{py}} scripts/build-if-stale.py --label wasm-labeler-catalog \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.js \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.d.ts \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-labeler-catalog-impl

_wasm-labeler-catalog-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-labeler-catalog/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-labeler-catalog --target web
    cp libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog.js apps/fauna-web/static/
    cp libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-labeler-catalog/pkg/fauna_wasm_labeler_catalog_bg.wasm.d.ts apps/fauna-web/static/

# The private share link viewer page's WASM chunk (libs/fauna-wasm-share: the
# shared open + decrypt + verify of a fragment-keyed link, over
# `fauna_client_share::viewer` — share-links.md § The private-file extension).
# Its own chunk, never a module of `fauna-wasm` (build-system.md § WASM chunking
# convention): the viewer page (`share-viewer.html`, the SPA build's second
# entry) loads this and nothing else, so opening a link downloads the decrypt,
# not the app runtime. Wired into the `wasm:` aggregate so `just web` builds it.
wasm-share:
    @{{py}} scripts/build-if-stale.py --label wasm-share \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_share.js \
        --target apps/fauna-web/static/fauna_wasm_share.d.ts \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-share-impl

_wasm-share-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-share/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-share --target web
    cp libs/fauna-wasm-share/pkg/fauna_wasm_share_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-share/pkg/fauna_wasm_share.js apps/fauna-web/static/
    cp libs/fauna-wasm-share/pkg/fauna_wasm_share.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-share/pkg/fauna_wasm_share_bg.wasm.d.ts apps/fauna-web/static/

# Page-level community-connected-apps WASM chunk (libs/fauna-wasm-connected-apps:
# `LabelerCatalogMachine` — browse / inspect-before-subscribe / (un)subscribe).
# Its own chunk (not folded into fauna-wasm-folders/-media) because the
# connected-apps page is distinct from Devices/Media. Wired into the `wasm:`
# aggregate so `just web` builds it; the web connected-apps page (and the
# personalization home's subscribed-labelers facet) render off the shared
# `LabelerCatalogMachine` via `$lib/wasm-connected-apps`.
wasm-connected-apps:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-connected-apps \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.js \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.d.ts \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm.d.ts \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-connected-apps-impl

_wasm-connected-apps-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-connected-apps/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-connected-apps --target web
    cp libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps.js apps/fauna-web/static/
    cp libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-connected-apps/pkg/fauna_wasm_connected_apps_bg.wasm.d.ts apps/fauna-web/static/

# Page-level Bluesky/ATProto login-plane settings WASM chunk
# (libs/fauna-wasm-atproto-settings: `AtprotoSettingsMachine` — app
# credentials + connected-app sessions + the external-apps kill-switch).
# Its own chunk (mirrors wasm-labeler-catalog) because the atproto-settings
# page is distinct from every other settings sub-page. Wired into the
# `wasm:` aggregate so `just web` builds it; the web atproto-settings page
# renders off the shared `AtprotoSettingsMachine` via
# `$lib/wasm-atproto-settings`.
#
# TWO producers, same shape as wasm-onboarding/wasm-onboarding-test — this
# recipe and `wasm-atproto-settings-test` below, which builds the SAME crate
# with `--features test-helpers` (gates `enableFakePlcDirectoryForTest`, the
# e2e-only seam `test_atproto_custody_alarm.py`'s web leg needs — a browser
# has no process env, so it cannot use native's `FAUNA_ATPROTO_PLC_DIRECTORY_
# URL`). Separate out-dirs (`pkg` vs `pkg-test`) keep the staleness gate
# honest; the `static/` mirror below runs unconditionally so `just web` and
# `just web-test` are correct in either order (see wasm-onboarding's comment
# for the full false-green story this shape avoids).
wasm-atproto-settings:
    @{{py}} scripts/build-if-stale.py --label wasm-atproto-settings \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.js \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings.d.ts \
        --target libs/fauna-wasm-atproto-settings/pkg/fauna_wasm_atproto_settings_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-atproto-settings-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_atproto_settings \
        --from libs/fauna-wasm-atproto-settings/pkg --to apps/fauna-web/static

_wasm-atproto-settings-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-atproto-settings/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-atproto-settings --target web

wasm-atproto-settings-test:
    @{{py}} scripts/build-if-stale.py --label wasm-atproto-settings-test \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings_bg.wasm \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings.js \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings.d.ts \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-atproto-settings-test-impl
    @{{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_atproto_settings \
        --from libs/fauna-wasm-atproto-settings/pkg-test --to apps/fauna-web/static

_wasm-atproto-settings-test-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-atproto-settings/pkg-test -- {{slot_build}} wasm-pack build libs/fauna-wasm-atproto-settings --target web --out-dir pkg-test -- --features test-helpers

# Content-index WASM chunk (libs/fauna-wasm-content-index). ON HOLD — not in
# the `wasm` aggregate. Compiles to wasm32 but tantivy panics at runtime
# ("Failed to spawn segment updater thread"). Kept for a future re-attempt
# (wasm-threads / upstream tantivy fix).
wasm-content-index:
    @{{py}} scripts/build-if-stale.py --label wasm-content-index \
        --target apps/fauna-web/static/fauna_wasm_content_index_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_content_index.js \
        --target apps/fauna-web/static/fauna_wasm_content_index.d.ts \
        --target apps/fauna-web/static/fauna_wasm_content_index_bg.wasm.d.ts \
        --source libs --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' \
        -- just _wasm-content-index-impl

_wasm-content-index-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-content-index/pkg -- {{slot_build}} wasm-pack build libs/fauna-wasm-content-index --target web
    cp libs/fauna-wasm-content-index/pkg/fauna_wasm_content_index_bg.wasm apps/fauna-web/static/
    cp libs/fauna-wasm-content-index/pkg/fauna_wasm_content_index.js apps/fauna-web/static/
    cp libs/fauna-wasm-content-index/pkg/fauna_wasm_content_index.d.ts apps/fauna-web/static/
    cp libs/fauna-wasm-content-index/pkg/fauna_wasm_content_index_bg.wasm.d.ts apps/fauna-web/static/

# Panic-hook headless-witness build — the `test-helpers` flavor of
# every secondary wasm chunk that has NO existing test-flavor infra, each
# output under a UNIQUE `_panic_witness` stem the real SPA never imports.
# Unlike wasm-core/wasm-onboarding's pkg/pkg-test split, this needs no
# two-producer flavor-collision guard: each stem is written ONLY by this
# recipe (via `--out-name`), so nothing else ever asserts a different flavor
# into the same static/ path. `fauna-wasm`/`fauna-wasm-onboarding` are
# excluded — their own -test flavor already lands in static/ under their real
# stem. `fauna-wasm-content-index` is excluded too: ON HOLD, not part of the
# `wasm`/`web-test` pipeline, its wasm32 check already known-red (see
# wasm-chunk-check's own exclusion).
#
# Not part of `wasm`/`web-test` — it exists solely for
# tests/e2e-unified/tests/test_wasm_panic_hook.py, which dynamically
# `import()`s each `*_panic_witness.js`, calls its `panicForTestOnly()`
# export, and asserts the chunk names itself in `driver.console_log()`.
wasm-panic-witness:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label wasm-panic-witness \
        --target apps/fauna-web/static/fauna_wasm_launch_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_media_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_folders_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_share_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_atproto_settings_panic_witness.js \
        --target apps/fauna-web/static/fauna_wasm_backups_panic_witness.js \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude '*/pkg-panic-witness/*' \
        -- just _wasm-panic-witness-impl

_wasm-panic-witness-impl:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-launch/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-launch --target web --out-dir pkg-panic-witness --out-name fauna_wasm_launch_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-launch/pkg-panic-witness/fauna_wasm_launch_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-launch/pkg-panic-witness/fauna_wasm_launch_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-media/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-media --target web --out-dir pkg-panic-witness --out-name fauna_wasm_media_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-media/pkg-panic-witness/fauna_wasm_media_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-media/pkg-panic-witness/fauna_wasm_media_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-folders/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-folders --target web --out-dir pkg-panic-witness --out-name fauna_wasm_folders_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-folders/pkg-panic-witness/fauna_wasm_folders_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-folders/pkg-panic-witness/fauna_wasm_folders_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-labeler-catalog/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-labeler-catalog --target web --out-dir pkg-panic-witness --out-name fauna_wasm_labeler_catalog_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-labeler-catalog/pkg-panic-witness/fauna_wasm_labeler_catalog_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-labeler-catalog/pkg-panic-witness/fauna_wasm_labeler_catalog_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-share/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-share --target web --out-dir pkg-panic-witness --out-name fauna_wasm_share_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-share/pkg-panic-witness/fauna_wasm_share_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-share/pkg-panic-witness/fauna_wasm_share_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-connected-apps/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-connected-apps --target web --out-dir pkg-panic-witness --out-name fauna_wasm_connected_apps_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-connected-apps/pkg-panic-witness/fauna_wasm_connected_apps_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-connected-apps/pkg-panic-witness/fauna_wasm_connected_apps_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-atproto-settings/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-atproto-settings --target web --out-dir pkg-panic-witness --out-name fauna_wasm_atproto_settings_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-atproto-settings/pkg-panic-witness/fauna_wasm_atproto_settings_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-atproto-settings/pkg-panic-witness/fauna_wasm_atproto_settings_panic_witness_bg.wasm apps/fauna-web/static/
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{wasm_mutex}}libs/fauna-wasm-backups/pkg-panic-witness -- {{slot_build}} wasm-pack build libs/fauna-wasm-backups --target web --out-dir pkg-panic-witness --out-name fauna_wasm_backups_panic_witness -- --features test-helpers
    cp libs/fauna-wasm-backups/pkg-panic-witness/fauna_wasm_backups_panic_witness.js apps/fauna-web/static/
    cp libs/fauna-wasm-backups/pkg-panic-witness/fauna_wasm_backups_panic_witness_bg.wasm apps/fauna-web/static/

# Build Svelte app (depends on WASM + generated files; no-op when sources unchanged).
# --stamp, not `build/`'s own mtime, carries freshness: `deno task build` reads its
# whole source tree early but keeps writing `build/` for several more seconds
# (prerender + adapter-static's own file-by-file write). A source edit landing in
# that write-phase window gets a target mtime NEWER than the edit but content
# OLDER than it — a false green that silently serves a stale bundle on the next
# `just web`/`just web-test` (confirmed via a controlled repro — build the SPA in
# the background, edit mid-write, observe `build-if-stale` report up-to-date while
# the edit is absent from the bundle). The stamp's PRE-run-instant
# commit closes the window: an edit made anytime during the build lands after the
# stamp, so the next freshness check correctly sees it as stale.
web: wasm i18n-generate providers-generate
    @{{py}} scripts/build-if-stale.py --label web \
        --stamp apps/fauna-web/.web-build.stamp \
        --target apps/fauna-web/build \
        --source apps/fauna-web/src \
        --source apps/fauna-web/static \
        --source apps/fauna-web/svelte.config.js \
        --source apps/fauna-web/vite.config.ts \
        --source apps/fauna-web/vite.share-viewer.config.ts \
        --source apps/fauna-web/share-viewer \
        --source apps/fauna-web/tsconfig.json \
        --source apps/fauna-web/deno.json \
        --source apps/fauna-web/deno.lock \
        --source apps/fauna-web/package.json \
        -- just _web-impl

_web-impl:
    deno install --config apps/fauna-web/deno.json
    {{slot_build}} bash -c 'cd apps/fauna-web && deno task build'

# The served tree's hash manifest (release-integrity.md § Release signing →
# Web-app verifiability, piece 2): builds the production SPA, then writes
# `apps/fauna-web/build/asset-manifest.json` — every file's SHA-256, the commit,
# the toolchain pins — INSIDE the tree, so the deploy that uploads the tree
# publishes and serves the manifest with it. The hosted app's deploy workflow
# runs exactly `just web-manifest`, because a verifier's `just web-verify`
# rebuilds with `just web`: the served tree and its re-check must come from the
# one recipe. The SPA step always runs (`_web-impl`, ~45 s), never behind the
# `web` freshness gate: the gate sees new and edited files but not DELETED ones,
# so a file removed from `static/` would survive in an incrementally reused
# `build/` and into the manifest. `--static` refuses git-ignored `static/` files
# `just wasm` does not produce (a test recipe's, a retired chunk's).
web-manifest: wasm i18n-generate providers-generate
    just _web-impl
    {{py}} scripts/web-asset-manifest.py write apps/fauna-web/build --static apps/fauna-web/static

# Rebuild the commit a published `asset-manifest.json` names — from a CLEAN
# clone of this repository, never this working tree — and compare every file
# (piece 2's verifier half). Exits non-zero naming each changed, missing or
# extra file. The commit must be in this clone (`git fetch` first if the
# manifest is newer than it). The wasm path remap (the
# CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS export above) is what lets a
# rebuild in a temporary directory match a build made anywhere else; a toolchain
# difference (wasm-pack's binaryen, deno) prints as a note beside any mismatch.
# The rebuild is cold by design — its own cargo target inside the throwaway
# directory (several GB under $TMPDIR), removed on exit. BASH_ENV is cleared
# for it because a shell profile that re-derives CARGO_TARGET_DIR per checkout
# (the primary dev VM's does, from BASH_ENV) would otherwise send the build to
# a directory named for the clone and leave it behind.
web-verify manifest:
    #!/usr/bin/env bash
    set -euo pipefail
    manifest="$(realpath '{{manifest}}')"
    commit="$({{py}} scripts/web-asset-manifest.py commit "$manifest")"
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
    export CARGO_TARGET_DIR="$work/target"
    unset BASH_ENV
    git clone --quiet --no-checkout "{{justfile_directory()}}" "$work/src"
    if ! git -C "$work/src" checkout --quiet --detach "$commit"; then
        echo "web-verify: commit $commit is not in this clone — git fetch, then re-run" >&2
        exit 2
    fi
    just --justfile "$work/src/justfile" --working-directory "$work/src" web
    {{py}} scripts/web-asset-manifest.py check "$manifest" "$work/src/apps/fauna-web/build" --repo "$work/src"

# Does one dependency compile to the same bytes twice? Builds `-p <crate>` alone
# (wasm32 + host, release) `runs` times into its own cargo target and compares
# the rlib, its ar members and the macro-expanded crate BY COUNTS ONLY — no
# dependency source ever reaches the terminal. Exit 0 = reproducible. The
# twenty-second reproducer behind the core web chunk's non-reproducibility
# (release-integrity.md § Release signing → *Web-app verifiability*, piece 1):
#   just crate-determinism mail-parser        # 3 runs; exit 1 while unfixed
#   just crate-determinism base64 2           # a control that passes
crate-determinism crate runs="3":
    {{slot_build}} scripts/crate-determinism-probe.sh {{crate}} {{runs}}

# Test-flavoured SPA build: FAUNA_WEB_E2E_AUTOMATION=1 flips the
# `__FAUNA_E2E_AUTOMATION__` vite define ON, compiling the `window.__fauna_*`
# automation surface into the bundle (testing.md § Test-agent build exclusion).
# The prod flavor (`_web-impl`) omits it, so production bundles strip the whole
# surface. Flavor switches always re-fire the SPA's build-if-stale gate because
# the onboarding wasm chunk swap dirties `static/` (see `wasm-onboarding`'s
# two-producer comment), so a stale cross-flavor `build/` cannot survive.
_web-test-impl:
    deno install --config apps/fauna-web/deno.json
    {{slot_build}} bash -c 'cd apps/fauna-web && FAUNA_WEB_E2E_AUTOMATION=1 deno task build'

# Type-check the SPA (svelte-check over the shipping tsconfig). This is the ONLY
# gate that reads TypeScript types: `web` / `web-test` run `vite build`, whose
# esbuild transform STRIPS types without checking them, and neither CI's web job
# nor the merge gate ever type-checked the SPA. A type error therefore shipped
# silently — and did: BOTH launch-resume rows in the onboarding page called their
# async store reader without `await`, so a `Promise` (always truthy) was read as
# the resolved record and every field came back `undefined`. That broke the CR-1
# factory-reset re-claim AND the deferred-DNS resume — client-recoverability
# paths (nest/common.md § Client-state recoverability) — and was misdiagnosed as
# a WS-RPC transport fault across sessions before this recipe existed.
# Deliberately NOT build-if-stale gated: a correctness gate must never
# false-green on an mtime. Needs the wasm chunks' .d.ts (gitignored — a clean
# checkout has to build them), hence the same deps as `web`.
web-check: wasm i18n-generate providers-generate
    deno install --config apps/fauna-web/deno.json
    @cmp -s apps/fauna-web/static/c2pa.wasm apps/fauna-web/node_modules/@contentauth/c2pa-web/dist/resources/c2pa_bg.wasm || { echo "web-check: apps/fauna-web/static/c2pa.wasm differs from the installed @contentauth/c2pa-web's dist/resources/c2pa_bg.wasm — the SDK's integrity check would block every fetch of it; copy the package's file over it" >&2; exit 1; }
    {{slot_build}} bash -c 'cd apps/fauna-web && deno task check'

# The SPA's own in-process unit tests (testing.md § four-tier taxonomy, tier_1):
# 8 `apps/fauna-web/src/lib/**/*.test.ts` files, run via `deno test`. Before this
# recipe existed they were tracked, reviewed, and run by NOTHING — no `deno test`
# anywhere in justfile/scripts, no task in `apps/fauna-web/deno.json` — including
# one covering the nest-URL override security guard
# (`safe-url.test.ts`) that had never actually run in this repo's configuration.
# `--no-check`: full type-checking from these entry points pulls in nearly the
# whole SPA import graph (even through `import type`, which still needs its
# source module resolved) and hits unrelated pre-existing gaps that only make
# sense under the SvelteKit/Vite build (the `$app/*` ambient aliases, DOM lib,
# Vite `define`s) — `web-check` above is the dedicated, correctly-configured
# place for that; this recipe only proves the pure-logic assertions pass.
# Deliberately NOT build-if-stale gated, matching `web-check`'s own reasoning:
# a correctness gate must never false-green on an mtime, and `deno test` here is
# near-instant (~0.3s total) so there is no caching upside to chase anyway.
web-unit-test:
    {{slot_build}} deno test --allow-read --no-check apps/fauna-web/src/lib/

# Test-flavoured Svelte build: same SPA but with the test-helpers wasm-core AND
# wasm-onboarding bundles. Used by the e2e conftest static_dir fixture, so
# `driver.call_machine_method` can reach the wizard's snapshot setters and
# `window.__fauna_callCommand` can reach the conversations/feed injection seams
# (both compiled out of the production flavor — testing.md § convention 15).
# Always re-runs the wasm step; the SPA build itself is gated by
# build-if-stale (its sources haven't changed).
#
# The bundle list MUST stay a superset of every chunk the SPA imports — it
# mirrors `wasm:` above, swapping `wasm-core` → `wasm-core-test` and
# `wasm-onboarding` → `wasm-onboarding-test`.
# `src/lib/wasm-media.ts` + `src/lib/wasm-labeler-catalog.ts` dynamic-import
# their chunks, so rollup hard-fails ("Could not resolve
# ../../static/fauna_wasm_media.js") when they are absent — which is every
# freshly-cloned checkout, since `static/fauna_wasm*` is gitignored. Both were
# added to `wasm:` (media, labeler-catalog) but not
# here, so `just web-test` — and therefore the whole web e2e lane via the
# conftest `static_dir` fixture — could not build from a clean checkout.
# (`wasm-content-index` stays out of both lists on purpose: it is ON HOLD, and
# its only importer is a skipped test, so it never enters the SPA's rollup graph.)
#
# Linux only, like `wasm:` above (see its comment): `[linux]` fans the 9 chunks
# out concurrently under one build slot (`_wasm-fanout-test` below); the
# `[macos]`/`[windows]` variant refuses. Paid more often
# than `web-check`'s own `wasm:` dependency: every `--client web` e2e run
# builds this, not just an explicit type-check invocation.
#
# Same false-green fix as `web:` above (see its comment): `--stamp`, not
# `build/`'s own mtime, carries freshness across the write-phase race window.
[macos]
[windows]
web-test:
    @echo "web is built only on Linux — build-system.md § The Deno build sandbox" >&2 && exit 1

# Linux: same fan-out shape as `[linux] wasm:` above, swapping BOTH flavored
# chunks for their test variants — `wasm-core` → `wasm-core-test` and
# `wasm-onboarding` → `wasm-onboarding-test` (web-test's own chunk list) — and
# locking each one's TEST out-dir (`pkg-test`, not `pkg` — see those recipes'
# comments on why the two producers use separate out-dirs). The SPA build itself
# (build-if-stale + `_web-test-impl`) runs AFTER the fan-out, unconditionally
# outside any mutex/slot — it only reads the wasm chunks' finished `static/`
# output, never races their out-dirs.
#
# Same warm-tree precheck as `[linux] wasm:` above (build-system.md § Build/e2e
# slot locks): probe all 9 chunks via `build-if-stale --check` BEFORE the
# mutex+slot chain. Unlike `wasm:`, this can't early-`exit` on all-fresh — the
# SPA build-if-stale step below must still run unconditionally either way.
[linux]
web-test: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    # Each --check line mirrors its sibling PUBLIC recipe's own --target list
    # EXACTLY (wasm-core / wasm-onboarding-test / wasm-launch-test /
    # wasm-folders / wasm-backups / wasm-media / wasm-labeler-catalog /
    # wasm-atproto-settings-test
    # above) — if a target is added/renamed there, mirror it here too, or a
    # genuinely-stale chunk could read as fresh and the whole fan-out silently
    # skips.
    common=({{py}} scripts/build-if-stale.py --check -q --source libs --source Cargo.lock --exclude "*/pkg/*" --exclude "*/pkg-test/*")
    fresh=1
    # mirrors: wasm-core-test (openmls-dependent — see wasm-core's comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-core-test \
        --target libs/fauna-wasm/pkg-test/fauna_wasm_bg.wasm \
        --target libs/fauna-wasm/pkg-test/fauna_wasm.js \
        --target libs/fauna-wasm/pkg-test/fauna_wasm.d.ts \
        --target libs/fauna-wasm/pkg-test/fauna_wasm_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-onboarding-test
    "${common[@]}" --label wasm-onboarding-test \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding_bg.wasm \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding.js \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding.d.ts \
        --target libs/fauna-wasm-onboarding/pkg-test/fauna_wasm_onboarding_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-launch-test
    "${common[@]}" --label wasm-launch-test \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch_bg.wasm \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch.js \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch.d.ts \
        --target libs/fauna-wasm-launch/pkg-test/fauna_wasm_launch_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-folders (openmls-dependent — see wasm-folders' comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-folders \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_folders.js \
        --target apps/fauna-web/static/fauna_wasm_folders.d.ts \
        --target apps/fauna-web/static/fauna_wasm_folders_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-backups (openmls-dependent — see wasm-backups' comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-backups \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_backups.js \
        --target apps/fauna-web/static/fauna_wasm_backups.d.ts \
        --target apps/fauna-web/static/fauna_wasm_backups_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-media (openmls-dependent — see wasm-media's comment)
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-media \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_media.js \
        --target apps/fauna-web/static/fauna_wasm_media.d.ts \
        --target apps/fauna-web/static/fauna_wasm_media_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-labeler-catalog
    "${common[@]}" --label wasm-labeler-catalog \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.js \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog.d.ts \
        --target apps/fauna-web/static/fauna_wasm_labeler_catalog_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-share
    "${common[@]}" --label wasm-share \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_share.js \
        --target apps/fauna-web/static/fauna_wasm_share.d.ts \
        --target apps/fauna-web/static/fauna_wasm_share_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-connected-apps
    "${common[@]}" ${VENDOR_SOURCE:-} --label wasm-connected-apps \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.js \
        --target apps/fauna-web/static/fauna_wasm_connected_apps.d.ts \
        --target apps/fauna-web/static/fauna_wasm_connected_apps_bg.wasm.d.ts || fresh=0
    # mirrors: wasm-atproto-settings-test
    "${common[@]}" --label wasm-atproto-settings-test \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings_bg.wasm \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings.js \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings.d.ts \
        --target libs/fauna-wasm-atproto-settings/pkg-test/fauna_wasm_atproto_settings_bg.wasm.d.ts || fresh=0
    if [ "$fresh" = "1" ]; then
        # `fauna_wasm`/`fauna_wasm_onboarding`/`fauna_wasm_launch`/
        # `fauna_wasm_atproto_settings` are two-producer chunks (prod vs test
        # flavor share one `static/` slot — see `wasm-core-test`'s comment). The
        # --check above only proves `pkg-test/` itself is fresh; it says nothing
        # about which flavor `static/` currently holds (e.g. a `just web`/
        # `wasm-seam-check` run in between last asserted the PROD flavor
        # there). Re-run all four two-producer mirrors unconditionally — cheap (byte-compare, no
        # wasm-pack) — so this fast path can't leave `static/` on the wrong
        # flavor the way the "all fresh, skip everything" shortcut used to.
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm \
            --from libs/fauna-wasm/pkg-test --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_onboarding \
            --from libs/fauna-wasm-onboarding/pkg-test --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_launch \
            --from libs/fauna-wasm-launch/pkg-test --to apps/fauna-web/static
        {{py}} scripts/sync-wasm-static.py -q --stem fauna_wasm_atproto_settings \
            --from libs/fauna-wasm-atproto-settings/pkg-test --to apps/fauna-web/static
        echo "[web-test] all 10 wasm chunks up-to-date — no build slot taken"
    else
        {{wasm_mutex}}libs/fauna-wasm/pkg-test -- \
        {{wasm_mutex}}libs/fauna-wasm-onboarding/pkg-test -- \
        {{wasm_mutex}}libs/fauna-wasm-launch/pkg-test -- \
        {{wasm_mutex}}libs/fauna-wasm-folders/pkg -- \
    {{wasm_mutex}}libs/fauna-wasm-backups/pkg -- \
        {{wasm_mutex}}libs/fauna-wasm-media/pkg -- \
        {{wasm_mutex}}libs/fauna-wasm-labeler-catalog/pkg -- \
        {{wasm_mutex}}libs/fauna-wasm-share/pkg -- \
        {{wasm_mutex}}libs/fauna-wasm-connected-apps/pkg -- \
        {{wasm_mutex}}libs/fauna-wasm-atproto-settings/pkg-test -- \
        {{slot_build_wide}} just _wasm-fanout-test
    fi
    # Same false-green fix as `web:` above (see its comment): `--stamp`, not
    # `build/`'s own mtime, carries freshness across the write-phase race window.
    {{py}} scripts/build-if-stale.py --label web \
        --stamp apps/fauna-web/.web-build.stamp \
        --target apps/fauna-web/build \
        --source apps/fauna-web/src \
        --source apps/fauna-web/static \
        --source apps/fauna-web/svelte.config.js \
        --source apps/fauna-web/vite.config.ts \
        --source apps/fauna-web/vite.share-viewer.config.ts \
        --source apps/fauna-web/share-viewer \
        --source apps/fauna-web/tsconfig.json \
        --source apps/fauna-web/deno.json \
        --source apps/fauna-web/deno.lock \
        --source apps/fauna-web/package.json \
        -- just _web-test-impl

# Private: the concurrent fan-out for `web-test`'s chunk list. ONLY invoked by
# the `[linux] web-test:` wrapper above, which has already acquired the 9
# out-dir mutexes + 1 build slot — see `_wasm-fanout`'s own comment (this is
# its `web-test` twin, mechanically identical except for the four `-test`
# swaps). NEVER call this directly.
[linux]
[parallel]
_wasm-fanout-test: wasm-core-test wasm-onboarding-test wasm-launch-test wasm-folders wasm-backups wasm-media wasm-labeler-catalog wasm-share wasm-connected-apps wasm-atproto-settings-test

# Notes WYSIWYG editor browser harness (web). Bundles the SHIPPING
# `notesEditorExtensions()` + the real wasm into a standalone esbuild page, mounts
# it in headless Chromium (Playwright's bundled build — sibling-safe, no singleton),
# and asserts the 20 feasibility-probe rendering rows + live structural gestures +
# IME composition on the production applier — the one layer the deno unit tests +
# the real-engine integration proof don't exercise (actual CodeMirror DOM render +
# atomic caret-skip + widget click in a browser). The interim browser proof until
# the gated (b) e2e-unified `--client web` run on `document_detail` lands
# (tracked internally). Needs deno + the e2e
# Playwright venv (resolved from the `pytest` shim's interpreter, same venv the
# e2e suite runs under); pass extra args (e.g. --headed) after the recipe name.
notes-browser-harness *ARGS: wasm-core
    # -A, not the Deno build sandbox: --allow-run cannot name the esbuild binary, which sits in
    # Deno's per-machine global npm cache (build-system.md § The Deno build sandbox).
    deno run -A tests/e2e-unified/web-notes-harness/build_harness.ts
    "$(sed -n '1s/^#!//p' "$(command -v pytest)")" tests/e2e-unified/web-notes-harness/run_notes_harness.py {{ARGS}}

# Dev: build web + start node and static file server.
# The SPA (:8080) and nest (:3000) are separate origins here, so the in-browser
# onboarding probe for a `test@localhost` handle (→ http://localhost:3000) is
# cross-origin; --cors-origin lets the nest answer it. (Production serves the
# SPA from the nest itself — same origin, no CORS needed.)
web-dev: web
    ln -sfn . apps/fauna-web/build/app
    @echo "Starting fauna-nest and static file server..."
    @echo "Node API: http://127.0.0.1:3000"
    @echo "Web UI:   http://127.0.0.1:8080"
    {{slot_build}} cargo build -p fauna-nest
    "$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')/debug/fauna-nest{{exe_suffix}}" --bind 0.0.0.0:3000 --cors-origin http://127.0.0.1:8080 --cors-origin http://localhost:8080 &
    {{py}} -m http.server 8080 --directory apps/fauna-web/build

# ── The public website, https://fauna.social (sites/fauna-social) ────────────
# Astro 7 under Deno, static output, zero JS shipped. Separate from the `web`
# SPA in every way: no wasm, no i18n/providers generation, no nest.
#
# The association's governing documents are a real source of this build: the
# bylaws pages import that markdown at build time — bylaws § 17 requires the
# bylaws be published at fauna.social and kept up to date — so editing a
# governing document correctly re-fires this gate. Those documents are kept in
# the development repo, outside the published tree, alongside the site itself.
# See build-system.md § Public website build.
#
# Build the public fauna.social site (no-op when sources unchanged)
site:
    @{{py}} scripts/build-if-stale.py --label site \
        --target sites/fauna-social/dist \
        --source sites/fauna-social/src \
        --source sites/fauna-social/public \
        --source sites/fauna-social/astro.config.mjs \
        --source sites/fauna-social/deno.json \
        --source sites/fauna-social/deno.lock \
        --source sites/fauna-social/package.json \
        --source docs/organization \
        --source docs/sites/fauna-social \
        --source docs/features \
        --source installer/official-apps.json \
        -- just _site-impl

_site-impl:
    deno install --config sites/fauna-social/deno.json
    {{slot_build}} bash -c 'cd sites/fauna-social && deno task build'

# Use this to review the site, NOT `deno task dev` — dev serves an unbundled
# build with an injected HMR client, i.e. not what visitors get.
#
# Serve the built fauna.social site at http://127.0.0.1:4321 (the exact uploaded bytes)
site-preview: site
    @echo "Serving sites/fauna-social/dist at http://127.0.0.1:4321 (Ctrl-C to stop)"
    cd sites/fauna-social && deno task preview

# Build fauna-nest release binary
nest: i18n-generate providers-generate
    {{slot_build}} cargo build -p fauna-nest --release

# Build nest Docker image
docker-build: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    commit=$(git rev-parse HEAD 2>/dev/null || echo dev)
    {{slot_build}} docker build --build-arg FAUNA_BUILD_COMMIT="$commit" -t fauna-nest:local .

# Run nest Docker container locally (http://localhost:3000, web app at /app/)
docker-run: docker-build
    -docker rm -f fauna-nest-local 2>/dev/null
    docker run -d --name fauna-nest-local -p 3000:3000 -p 8080:8080 -v fauna-nest-local-data:/data -e FAUNA_PORT=3000 fauna-nest:local
    @echo ""
    @echo "Nest running at http://localhost:3000"
    @echo "Web app at    http://localhost:3000/app/"
    @echo ""
    @docker logs fauna-nest-local 2>&1 | grep "CLAIM CODE" -A4 || true

# Stop and remove local nest Docker container
docker-stop:
    docker rm -f fauna-nest-local

# Cross-build linux/amd64 image and push to GHCR, stamped with the current
# commit so the running nest reports it at `/api/v1/health` (build.rs reads the
# FAUNA_BUILD_COMMIT build-arg). Pushes two tags: the moving `:dev` that
# deployments track, plus an immutable `:<commit>` for per-version registry
# history. A dirty working tree is stamped `<commit>-dirty`, so a deploy from
# uncommitted code is self-evident in the health endpoint.
# Requires: `docker login ghcr.io` once. Uses the DEFAULT (docker-driver)
# builder deliberately — `--builder default`:
#   * Its BuildKit cache lives in the persistent docker daemon, so the
#     Dockerfile's `--mount=type=cache` mounts (cargo registry/target, Go
#     module/build) survive across builds AND across a killed-and-retried
#     build — a retry recompiles only changed crates instead of all of Rust.
#   * On the virtualization host ARM VMs the persistent *container* buildx builder SIGSEGVs
#     rustc under Rosetta; the default daemon
#     driver builds linux/amd64 via the host's Rosetta binfmt without that
#     crash. `--builder default` also overrides any `--use`d container builder
#     a machine may have left selected.
# The docker driver can't `--push` a cross-platform image directly, so we
# `--load` the amd64 image into the daemon and `docker push` each tag.
# Pushes two tags: the moving `:dev` deployments track, plus an immutable
# `:<commit>` for registry history. A dirty working tree is stamped
# `<commit>-dirty`, self-evident in the health endpoint.
# Compile parallelism is capped (default 16 cores) so the build doesn't saturate
# the shared host and starve the other VMs; override via FAUNA_DOCKER_BUILD_JOBS=N.
docker-push-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    commit=$(git rev-parse --short=8 HEAD 2>/dev/null || echo dev)
    git diff --quiet HEAD 2>/dev/null || commit="${commit}-dirty"
    # Per-build artifact identity (Dockerfile's FAUNA_BUILD_ID; served by
    # /api/v1/health). `dev-<epoch>` can never collide with the pipeline's
    # `<run_id>-<run_attempt>`, which is what stops a dev push of the SAME
    # commit from satisfying a release's promotion gate — the exact sequence
    # this recipe's own docs describe.
    build_id="dev-$(date +%s)"
    # Bound the build so a stalled RUN step (e.g. a `go mod download` whose
    # proxy connection hangs with no read timeout) fails loudly instead of
    # hanging for hours — buildkit buffers per-step output, so a network stall
    # otherwise shows zero log progress and looks like a slow build. Override
    # via FAUNA_DOCKER_BUILD_TIMEOUT (seconds); 0 disables the guard. After a
    # timeout-kill the daemon cache mounts persist, so the re-run resumes.
    # `--progress=plain` streams each step's output line-by-line so a stall is
    # visible in non-TTY / piped logs.
    #
    # `timeout -k 30`: send SIGTERM first, then SIGKILL only after a 30 s grace.
    # The grace lets the buildx client propagate a cancel to the daemon-side
    # buildkit RUN instead of being hard-killed and orphaning it. KNOWN ISSUE:
    # with the docker driver this cancel is best-effort — a timeout landing
    # mid-Rust-compile can still leave the buildkitd RUN compiling in the
    # background. That orphan is benign for the network-stall case this guard
    # exists for (the hung step isn't compiling Rust, so the cargo target mount
    # stays consistent). For a mid-compile timeout: let the orphan settle (~the
    # grace + a few seconds) before re-running, because a concurrent retry races
    # it on the shared cargo target cache mount and scrambles cargo's incremental
    # fingerprints. Worst case is a one-time broad recompile on the next build —
    # cargo self-heals, nothing is corrupted.
    build_timeout="${FAUNA_DOCKER_BUILD_TIMEOUT:-1800}"
    timeout_cmd=(timeout -k 30 "$build_timeout")
    [ "$build_timeout" = "0" ] && timeout_cmd=()
    # Cap the build's compile parallelism so it doesn't saturate every core of the
    # shared host (the build host running the Linux/Windows/macOS VMs) and starve the other
    # VMs. Caps cargo's parallel-rustc count + Go's GOMAXPROCS (the Dockerfile
    # threads BUILD_JOBS into CARGO_BUILD_JOBS/GOMAXPROCS). The docker-driver build
    # runs daemon-side, so a `--cpus` cap is unavailable — this is the lever.
    # Default 16 (leaves ~12 free on a 28-core build VM); override per build.
    build_jobs="${FAUNA_DOCKER_BUILD_JOBS:-16}"
    # Capture the build's real exit code. `rc=$?` inside `if ! cmd; then` would
    # read the NEGATED status (always 0), masking the failure as success AND —
    # because the failure branch `exit "$rc"`s before the push — silently
    # skipping the push while the recipe reports exit 0. `rc=0; cmd || rc=$?`
    # keeps the genuine code (124 on timeout, buildkit's code on a build/solve
    # failure such as a concurrent-build `context canceled`).
    rc=0
    {{slot_build}} "${timeout_cmd[@]}" docker buildx build --builder default \
        --platform linux/amd64 --progress=plain \
        --build-arg FAUNA_BUILD_COMMIT="$commit" \
        --build-arg FAUNA_BUILD_ID="$build_id" \
        --build-arg BUILD_JOBS="$build_jobs" \
        --tag ghcr.io/faunasocial/nest:dev \
        --tag "ghcr.io/faunasocial/nest:$commit" \
        --load . || rc=$?
    if [ "$rc" != "0" ]; then
        if [ "$rc" = "124" ]; then
            echo "ERROR: docker build exceeded ${build_timeout}s and was killed." >&2
            echo "  A RUN step likely stalled on the network (go mod / cargo fetch)." >&2
            echo "  Re-run; the cache mounts resume. Raise/disable with FAUNA_DOCKER_BUILD_TIMEOUT." >&2
            echo "  If the timeout hit mid-Rust-compile, wait ~30s for the daemon-side" >&2
            echo "  build to settle before re-running (a concurrent retry races the cargo" >&2
            echo "  target cache mount; worst case is a one-time broad recompile)." >&2
        fi
        exit "$rc"
    fi
    docker push ghcr.io/faunasocial/nest:dev
    docker push "ghcr.io/faunasocial/nest:$commit"
    echo
    echo "Pushed ghcr.io/faunasocial/nest:dev and :$commit (commit $commit, build $build_id)."
    echo "Watchtower recreates fauna-nest on its next poll (interval per the VPS .env)."
    echo "Verify it's live:  curl -sk https://<domain>/api/v1/health   # commit should read $commit"
    echo "Tail on the VPS:   docker compose logs -f watchtower fauna-nest"

# Build, push :dev, and trigger the VPS to pull immediately via Watchtower's
# HTTP API (bound to 127.0.0.1 on the VPS, reached over SSH).
# Usage: just deploy-dev user@nest.example.com
deploy-dev host:
    just docker-push-dev
    ssh {{host}} 'set -a && . /srv/fauna/.env && set +a && \
        curl -fsS -H "Authorization: Bearer $WATCHTOWER_HTTP_TOKEN" \
             -X POST http://127.0.0.1:8080/v1/update'
    @echo "Deploy triggered on {{host}}"

# Turnkey FIRST-TIME VPS bring-up: build+push the :dev image, copy the compose
# bundle to the host, and `docker compose up -d` (nest + mail bridge + clamd +
# rspamd + watchtower). Use this once to stand a fresh VPS up; thereafter
# `deploy-dev` (Watchtower instant-update over SSH) is the fast inner loop.
# Prereqs on the host: Docker (cloud images ship it) and `docker login
# ghcr.io` (the image is private). On first run it seeds /srv/fauna/.env from
# the template and stops so you can fill in your domain + Watchtower token
# (DNS is published by your Fauna app, not via the .env).
# Usage: just dev-deploy user@nest.example.com
dev-deploy host: docker-push-dev
    #!/usr/bin/env bash
    set -euo pipefail
    ssh {{host}} 'mkdir -p /srv/fauna'
    scp docker-compose.yml {{host}}:/srv/fauna/docker-compose.yml
    scp docker/dev-deploy.env.example {{host}}:/srv/fauna/.env.example
    if ! ssh {{host}} 'test -f /srv/fauna/.env'; then
        ssh {{host}} 'cp /srv/fauna/.env.example /srv/fauna/.env'
        echo "Seeded /srv/fauna/.env on {{host}} from the template."
        echo "Edit it (WATCHTOWER_HTTP_TOKEN), then re-run: just dev-deploy {{host}}"
        exit 0
    fi
    ssh {{host}} 'cd /srv/fauna && set -a && . ./.env && set +a && docker compose up -d --remove-orphans'
    echo "Brought up fauna-nest + mail bridge + clamd/rspamd on {{host}}."
    echo "Claim code: ssh {{host}} \"docker logs fauna-nest 2>&1 | grep -i claim\""

# Run E2E tests (Python + Playwright)
e2e: web i18n-generate providers-generate
    {{slot_build}} cargo build -p fauna-nest
    pytest tests/e2e-unified/tests/web/ -v

# Tier-filtered E2E runs (by mocking depth; see tests/e2e-unified/README.md).
#   tier_1 = in-process Python, no driver, no nest
#   tier_2 = real driver + at least one stub (fakes/, set_*_snapshot,
#            wiremock, fake DNS, fake_bridge_daemon, etc.)
#   tier_3 = full stack — every binary real (locally-built)
#   tier_4 = full Docker deployment image + sidecars (slowest — exercises
#            image packaging + s6 supervision that tier_3 binaries bypass)
# Usage: just e2e-tier-1-test
#        just e2e-tier-2-test
#        just e2e-tier-3-test
#        just e2e-tier-4-test
#        just e2e-tier-1-test --client web         (forwards extra args)
#        just e2e-tier-2-test "--client linux,web"
#
# On win the recipe first provisions the test-flavor FFI dll (build-if-stale,
# ~50 ms when fresh): `fauna_ffi.py` builds a missing cdylib at import time on
# linux/mac but RAISES on Windows (its import must stay build-free — a fresh public
# clone's collection check imports it — `test_fauna_ffi_import_is_build_free.py`),
# so a cold win checkout — the merge-gate check's pinned tree is one on most
# passes — otherwise fails `test_fauna_ffi.py`'s builder tests, not skips them.
#
# The pytest line self-slots in the `tier_1` lane (slot_tier_1 above): the three
# merge-gate check scripts call this recipe bare, exactly like every recipe
# that owns its own slot, and a hand reproduction gets the same policy. The
# win-only FFI prerequisite stays OUTSIDE the lane — it is a build and takes
# its own `build` slot. The recipe still NAMES NO FILES (directory + marker);
# test_e2e_tier_1_test_recipe_names_no_files pins that, and
# test_e2e_tier1_gate_is_a_bare_just_call_whose_recipe_self_slots pins the lane.
e2e-tier-1-test *ARGS:
    {{ if os() == "windows" { "just windows-ffi-test dev" } else { "true" } }}
    {{slot_tier_1}} pytest tests/e2e-unified/tests/ --tier 1 -v {{ARGS}}

e2e-tier-2-test *ARGS:
    pytest tests/e2e-unified/tests/ --tier 2 -v {{ARGS}}

e2e-tier-3-test *ARGS:
    pytest tests/e2e-unified/tests/ --tier 3 -v {{ARGS}}

# CHECK-tier gate — the nightly cross-app parity run, one APP per invocation: the whole
# tier_2/3 selection on `tui` every night, `linux`/`web` on alternating nights.
# Names no files, like e2e-tier-1-test, so a test landing today runs tonight.
# Deliberately NO slot prefix: conftest takes `e2e_long` (the selection is far
# past LONG_LANE_MIN_HEAVY_TESTS) and then the app's own lane itself; a `build`
# wrap would invert the lock order, and `--pool e2e` is retired.
# `--max-run-secs 28800` is the gate's own 8 h bound (`GATE_MAX_SECS`) carried
# into the run itself, so a measuring run launched on its own is bounded too,
# and a run the gate abandons at its bound ends itself instead of holding the
# lane on as an orphan.
e2e-tier-23-check APP:
    pytest tests/e2e-unified/tests/ --tier 2,3 --app {{APP}} -v --durations=50 --max-run-secs 28800

e2e-tier-4-test *ARGS:
    pytest tests/e2e-unified/tests/ --tier 4 -v {{ARGS}}

# The nest-mode axis: run the ORDINARY journey suite against the real deployment
# artifact instead of a locally-built binary (testing.md § Default app and nest
# mode). Every tier_3 test that collects here is elevated to tier_4 for the run,
# because a journey whose nest is the image under s6 is exercising packaging and
# supervision whatever its file says.
#
# The mode is run-level and single-valued, so multi-mode coverage composes as
# SEPARATE invocations — never as an inner-loop multiplier. This is a scheduled
# sweep, not the red-green loop; `--nest standalone` (the default) stays the
# inner loop and pays nothing for this recipe existing.
#
# The image is NOT built here and never should be: building the nest image on a
# dev VM is forbidden — the dev VMs share one physical host and the build
# starves them (build-system.md § Image tags & channels). Pull a published one first
# (`docker pull ghcr.io/faunasocial/nest:latest`), or dispatch a real build via
# `gh workflow run build-nest-image.yml --ref main` and pull its tag. An absent
# image refuses the run loudly rather than falling back to standalone.
# Usage: just e2e-docker-mode-test
#        just e2e-docker-mode-test ghcr.io/faunasocial/nest:sha-abc123
#        just e2e-docker-mode-test ghcr.io/faunasocial/nest:latest --app tui
e2e-docker-mode-test IMAGE="ghcr.io/faunasocial/nest:latest" *ARGS:
    pytest tests/e2e-unified/tests/ --nest docker:{{IMAGE}} -v {{ARGS}}

# The nest-mode axis's LIVE sweep: run the suite against an already-deployed real
# box (testing.md § Default app and nest mode). The only mode with real DNS, a
# real CA cert, a real network path and — the part that catches bugs —
# ACCUMULATED STATE, so it is the only one that fails a
# works-on-a-fresh-box-only bug. tier_3 journeys elevate to tier_4 here for the
# same reason they do in docker mode.
#
# Nothing is provisioned, reset or restarted: the box belongs to whoever is
# using it. What the run creates is its own accounts, and those are reaped at
# teardown (suspended immediately, deletion scheduled — see
# tests/e2e-unified/helpers/live_accounts.py for why it is two steps). The
# machine-wide live-box flock is engaged by the MODE, so sibling sessions'
# live runs serialize without anyone remembering a marker.
#
# ⚠ The default target is the SHARED PRODUCTION BOX. Only non-destructive tests
# belong here (§ The shared-box rule): class (3) global-admin-mutating tests are
# excluded automatically, and the run prints what that inference does NOT yet
# cover. Admin comes from this machine's ambient identity store (~/.fauna-id or
# FAUNA_LIVE_SECRET_HEX); a machine without it is refused rather than run
# half-authenticated.
# Usage: just e2e-live-mode-test
#        just e2e-live-mode-test https://staging.example -k filesync
#        just e2e-live-mode-test https://example.com --app tui
e2e-live-mode-test URL="https://example.com" *ARGS:
    pytest tests/e2e-unified/tests/ --nest live:{{URL}} -v {{ARGS}}

# The CD gate: the curated `cd_suite` journey subset against a deployed box, as
# the promote-after-verify pipeline's verify job runs it. Every part of this
# invocation is load-bearing, which is exactly why it is a recipe and not a
# line someone retypes into a workflow:
#   --nest live:URL   keeps the selection from triggering a collection-time
#                     LOCAL fauna-nest cargo build (the session-autouse
#                     mail-domain fixture drags nest_instance into
#                     _prebuild_binaries' fixture closure — ~23 min on the
#                     primary dev VM, and an outright error on a
#                     toolchain-light CI runner). It
#                     also satisfies the live_box opt-in on its own.
#   --live-box disposable
#                     switches the shared-box policy (exclusion class (3)) off
#                     for the run. Without it `--nest live` DESELECTS the
#                     suite's two destructive mail residents at collection —
#                     a deselect is not a skip, so --fail-on-skip never sees
#                     it and the gate passes green having run neither
#                     (measured 2026-10-04). Refused unless URL is a staging
#                     box the tree names (dev.example.com, test.example.com), so
#                     this recipe pointed at example.com still deselects them
#                     (testing.md § The shared-box rule → The disposable-box
#                     declaration).
#   --fail-on-skip    is what makes the gate a gate: these tests skip silently
#                     on a missing FAUNA_LIVE_* var, an unbuilt app binary or
#                     an unreachable port, and a bare run then exits 0 having
#                     proved nothing (e2e-conventions.md convention 7).
#
# ⚠ DESTRUCTIVE against the target box (the suite factory-resets it) — point it
# at the disposable CD box, never at example.com or any box with real users. The
# admin seed is resolved per box: FAUNA_LIVE_SECRET_HEX > ~/.config/fauna/staging-box/<host>.json > ~/.fauna-id; the destructive residents run only under the
# --live-box disposable declaration and drive the box it names. The mailbox is
# resolved per box too: FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD > the
# staging-box file's handle / mail_password, so a provisioned box needs nothing
# exported; see each test's docstring.
# Usage: just e2e-cd-gate https://dev.example.com
#        just e2e-cd-gate https://dev.example.com -k port25
e2e-cd-gate URL *ARGS:
    pytest tests/e2e-unified/tests -m cd_suite --nest live:{{URL}} --live-box disposable --fail-on-skip -v {{ARGS}}

# Real-fediverse interop harness (tests/platform/fediverse/): a pinned official
# third-party server federated against a locally-built fauna-nest across a real
# TLS boundary, driven black-box via its Mastodon-compatible REST API — proves a
# real server accepts our WebFinger/actor JSON-LD/addressing/signatures
# (activitypub.md gap 4). tier_3 but opt-in like tier_4 (boots a whole server):
# the FAUNA_E2E_FEDIVERSE gate keeps it out of a general `--tier 3` run. Takes an
# e2e slot via conftest. Needs docker + a one-time docker-bridge→host firewall
# allowance on the primary dev VM (see the dev-setup notes).
#
# ONE assertion set (F1-F9) runs against whichever peer the run selects; the
# three recipes below differ only in that selection. A second, re-pointed copy of
# the assertions would drift, and a drifted assertion that passes proves nothing.
#
# This one: PERMISSIVE Mastodon, which is what most of the fediverse runs.
# `fediverse_strict` is deselected — those tests need a peer that refuses
# unsigned fetches, and would pass vacuously here.
# Usage: just e2e-fediverse-test
#        just e2e-fediverse-test -k test_f3
e2e-fediverse-test *ARGS:
    FAUNA_E2E_FEDIVERSE=1 pytest tests/e2e-unified/tests/platform/fediverse/ -v -m "not fediverse_strict" {{ARGS}}

# The SAME suite against a secure-mode (AUTHORIZED_FETCH=true) Mastodon — the
# other half of the phase-2 matrix. Not a subset: F1-F9 re-run in full plus the
# strict-only tests, because the mode is a property of the run (the env var picks
# which stack `peer` boots), so it still costs exactly one Rails stack. Secure
# mode refuses unsigned fetches of its objects, which is what the nest's
# instance-actor-signed outbound GET exists for (activitypub.md § Architecture →
# The instance actor).
# Usage: just e2e-fediverse-strict-test
e2e-fediverse-strict-test *ARGS:
    FAUNA_E2E_FEDIVERSE=1 FAUNA_E2E_MASTODON_STRICT=1 pytest tests/e2e-unified/tests/platform/fediverse/ -v {{ARGS}}

# The SAME suite against GoToSocial — a second, stricter peer. A different
# implementation in a different language, so it is the one that can disagree with
# Mastodon; and it requires signed inbound fetches UNCONDITIONALLY (0.22.1
# exposes no AUTHORIZED_FETCH equivalent), so the strict tests run here by
# default rather than behind an opt-in flag. One Go binary on SQLite: it boots in
# seconds, where the Rails stack takes ~25s.
# Usage: just e2e-gotosocial-test
e2e-gotosocial-test *ARGS:
    FAUNA_E2E_FEDIVERSE=1 FAUNA_E2E_FEDIVERSE_PEER=gotosocial pytest tests/e2e-unified/tests/platform/fediverse/ -v {{ARGS}}

# Adversarial crash-recovery journeys (nest/common.md § Client-state
# recoverability): SIGKILL client/nest mid-operation, assert recovery. tier_3
# journey tests on dedicated nests — out of the inner loop, like tier-4.
# Cross-app by nature ("out of the inner loop" above), so it keeps the full
# sweep set the pre-2026-08-01 default gave it rather than inheriting `[tui]`.
# A trailing `--app <x>` in ARGS still wins (last occurrence).
e2e-crash-recovery-test *ARGS:
    pytest tests/e2e-unified/tests/ -m crash_recovery -v --app sweep {{ARGS}}

# Warm the previous-build cache: rebuild the PINNED previous release's nest + the
# client binary THIS machine can build (pinned-previous-build.toml;
# version-compatibility.md § Dimension 6) and cache it under the platform's large-temp
# root (/work/tmp on the Linux box, ~/.cache/fauna-prev on the Mac). The client
# defaults to the platform's own (linux / macos); override with `--client <name>`.
# Slow on a cold cache (a full workspace build, plus the apple toolchain on macOS);
# a no-op once cached. The version-skew suites (`-m version_skew`) call the same
# helper, so this is only for warming it out-of-band rather than inside a pytest run.
e2e-prev-build *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    # `prev_build.py` parses the pin file with `tomllib` (3.11+), so it must run
    # on the repo's declared interpreter. This recipe used to hand-roll that:
    # prefer ~/.venvs/fauna, else bare `python3` — because bare `python3` is
    # Apple's 3.9 on the macOS box, where the recipe died with `No module named
    # 'tomllib'` while the identical code worked fine under pytest. `{{py}}` is
    # now that interpreter on every machine (§ The interpreter, top of file), so
    # the hand-rolled fallback is retired rather than re-spelled.
    #
    # Probe warmth OUTSIDE the build slot first (build-system.md § Build/e2e slot
    # locks, same-class residue): `--path` is a cheap check against the already
    # content-addressed (commit-keyed) cache — file existence + dependency
    # completeness, no cargo — so a fully warm cache must not queue for a build
    # slot just to let `--build` conclude there is nothing to do.
    if {{py}} tests/e2e-unified/helpers/prev_build.py --path {{ARGS}} >/dev/null 2>&1; then
        echo "[e2e-prev-build] pinned build already cached — no build slot taken"
    else
        {{slot_build}} {{py}} tests/e2e-unified/helpers/prev_build.py --build {{ARGS}}
    fi

# Real-binary version skew (version-compatibility.md § Dim 6, the real-binary
# half): HEAD client <-> PREVIOUS nest and PREVIOUS client <-> HEAD nest, plus the
# client at-rest upgrade-in-place grid. tier_3. The skew grid is parametrized per
# client leg — pass `--client linux` / `--client macos` to pick the one this machine
# can build (§ Dim 6: each leg runs on its own machine). Needs the previous-build
# cache (`just e2e-prev-build`) — the suites warm it themselves if cold.
# `--app sweep` (this machine's full set) is the recipe default because the skew
# grid has no tui leg: under the 2026-08-01 rust-first default (`[tui]`) a bare
# run collects ZERO tests here and reports success. A trailing `--app <x>` in
# ARGS still wins (last occurrence).
e2e-version-skew-test *ARGS:
    pytest tests/e2e-unified/tests/ -m version_skew -v --app sweep {{ARGS}}

# The macOS CLIENT-ARTIFACT suite: drive the SHIPPED artifacts instead of the
# locally-built binaries — the real `Fauna.app` bundle, a DMG-installed copy of
# it, and the iOS device `.xcarchive` (testing.md § The four-tier taxonomy, the
# client-artifact shape of tier_4). Every other apple suite launches the bare
# swift-build Mach-O, so bundle identity, entitlements, embedded frameworks and
# their rpaths, quarantine, and the whole iOS device build path are exercised by
# NOTHING until this runs.
#
# Convention 17's LAYER (c) walk sweep — a systematic walk of every canonical
# page's focus ring over the real binary, through the generic walk commands
# (`focus_move`/`switch_pane`), asserting the layer-(b) invariant catalogue after
# every step. See `docs/goal/architecture/e2e-conventions.md` § convention 17 and
# `tests/e2e-unified/helpers/ui_walk.py`.
#
# OPT-IN, never the inner loop — and that is the convention's own wording, not a
# cost judgement. Breadth belongs at tier_1 (layer (a)), where a step costs
# microseconds in-process instead of an agent round trip; what this layer buys
# that tier_1 cannot is proof the vocabulary reaches the SHIPPED binary at all.
# `--walk-sweep` is the COLLECTION gate: without it `tests/walk/` is never even
# imported, so no default sweep or tier filter can pull it in.
#
# Runs against whatever `--app` names (default tui). An app that has not built
# the walk commands does not skip quietly — it REFUSES them, and the sweep fails
# naming the command, which is convention 11 doing its job rather than a flake.
# Usage: just e2e-walk-sweep
#        just e2e-walk-sweep --app linux
e2e-walk-sweep *ARGS:
    pytest tests/e2e-unified/tests/walk/ --walk-sweep -v {{ARGS}}

# OPT-IN, never the inner loop, for two independent reasons: `--macos-artifact`
# swaps what `--app macos` launches (a run-level mode, like `--nest docker`), and
# the same flag is the COLLECTION gate on tests/artifact/ — without it that
# directory is never even imported, so no default sweep or tier filter can reach
# it. Cost is minutes: a `mac-app debug` bundle assembly, an `hdiutil` create +
# attach over ~570 MB, and a Release `xcodebuild archive`.
#
# `--app macos,ios` is deliberate, and NOT an iOS simulator run: the iOS archive
# tests use no driver, so selecting `ios` admits them without ever building or
# booting the simulator app (no test here requests the ios `app` fixture).
#
# ⚠ THE TWO HALVES CANNOT SHARE ONE PYTEST INVOCATION, and the reason is a real
# build-system interaction rather than a preference. The iOS archive links the
# PRODUCTION multi-slice `FaunaFFI.xcframework` (`just apple-ffi`), while the macOS
# half's `mac-app debug` runs `apple-ffi-host-test`, which DELETES that framework
# and reassembles it host-only — dropping the `ios-arm64` slice. Collection-time
# prebuild does the macOS build first, so a single combined run would reliably
# fail the iOS module on a framework the run itself had just destroyed. Hence two
# sequential invocations, iOS first. (`test_ios_archive.py`'s fixture also names
# this by hand rather than letting xcodebuild emit an unattributed link error.)
#
# Run `just apple-ffi` before this recipe if the xcframework is host-only — it is
# NOT run here on purpose: it is a ~45-90 min multi-slice release build, far past any
# per-test timeout, and must never run concurrently with e2e.
# Usage: just e2e-macos-artifact-test
#        just e2e-macos-artifact-test tests/e2e-unified/tests/artifact/test_macos_dmg_install.py
[macos]
e2e-macos-artifact-test *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    SUITE=tests/e2e-unified/tests/artifact
    # An explicit target means the caller is aiming at one thing; run exactly it
    # and let them own the ordering above.
    if [ -n "{{ARGS}}" ]; then
        exec pytest --macos-artifact --app macos,ios -v {{ARGS}}
    fi
    pytest "$SUITE/test_ios_archive.py" --macos-artifact --app macos,ios -v
    pytest "$SUITE" --macos-artifact --app macos -v --ignore="$SUITE/test_ios_archive.py"

# The macOS column of photo backup, against the macOS VM's REAL System Photo
# Library — the real-session photo-library venue (e2e-conventions.md convention 12's
# macOS arm; drivers/macos.py § the photo-library venue). OPT-IN like every
# `--real-session` run: it writes tagged test photos into the VM's own library
# (user-approved 2026-09-26, and the guard refuses anywhere but a VM), and its
# macOS apps launch as the stably signed test bundle through Launch Services.
# Needs the one-time grant below first; without it the grant gate fails in seconds
# naming this recipe's sibling.
# Usage: just e2e-macos-photo-library-test
[macos]
e2e-macos-photo-library-test *ARGS:
    pytest tests/e2e-unified/tests/real_session/test_photo_backup_macos.py --real-session --app macos -v {{ARGS}}

# THE ONE HUMAN STEP of the macOS photo-library venue, run BY the person at the
# Mac's screen: preflight first, then one "input needed now" line, then within
# seconds the keychain's codesign dialog (only after a rebuild — "Always Allow")
# and Photos access for "Fauna E2E Photos" ("Allow"). No nest, no pytest, no slot
# queue, so nothing waits an unpredictable time before asking. Already granted, it
# answers in seconds with no dialog. Needs `just mac-debug` first.
# Usage: just mac-photos-e2e-grant
[macos]
mac-photos-e2e-grant:
    {{py}} tests/e2e-unified/helpers/photo_library_grant.py

# The linux half of the shipped-client-artifact category: `install.sh` installs the
# app into a private prefix under `build/`, and the driver then drives WHAT GOT INSTALLED
# (docs/goal/architecture/testing.md § The four-tier taxonomy point 4). Its own
# invocation, and that is not a nicety: `installed_product` is read session-wide
# (`_installed_linux_app_override`), so folding these into an ordinary linux run
# would point every test in it at the installed prefix.
#
# Far cheaper than the macOS half — no packaging build, just a file-copy install
# over the debug binaries `linux-debug` already produced — but opt-in for the same
# reason: its subject is what an install channel produced, which the inner loop
# deliberately does not use.
# Usage: just e2e-linux-artifact-test
#        just e2e-linux-artifact-test -k sync_agent
[linux]
e2e-linux-artifact-test *ARGS:
    pytest tests/e2e-unified/tests/artifact --client-artifact --app linux -v {{ARGS}}

# The terminal app's half of the shipped-client-artifact category: the real
# `assemble-archive.sh` stages the archive layout over the debug binaries
# `tui-debug` produced, the archive's own `install.sh` installs it into a private
# prefix under `build/`, and the driver drives WHAT GOT INSTALLED
# (installers/tui.md § The ratified channel; the linux half's shape and reasons).
# Its own invocation for the linux half's reason: `installed_product` is read
# session-wide (`_installed_tui_app_override`). Runs on linux and macOS — the
# two OSes the archive's `install.sh` serves.
# Usage: just e2e-tui-artifact-test
#        just e2e-tui-artifact-test -k sync_agent
[unix]
e2e-tui-artifact-test *ARGS:
    pytest tests/e2e-unified/tests/artifact --client-artifact --app tui -v {{ARGS}}

# The full-flavor RELEASE build of the terminal app — what the archive channel
# ships (installers/tui.md § What any channel must satisfy: a direct download
# carries the full default-feature build; `tui-store-safe` is the store flavor).
# Both binaries, because the archive is two binaries: tui's spawner expects
# fauna-sync-agent beside it. `release.yml` inlines the same two builds per
# target; this recipe is the local twin (`just tui-release` → `assemble-archive.sh`).
tui-release: i18n-generate providers-generate
    {{slot_build}} cargo build --locked -p fauna-tui -p fauna-sync-agent --release

# Live, OPT-IN Hetzner provisioning e2e: provisions a REAL, PAID VPS through the
# client onboarding path (real api.hetzner.cloud — server + DNS on ONE token),
# runs the example.com deploy-verify gates against it (health/TLS/claim/firewall/
# mail-ports/DKIM, no SSH), then ALWAYS tears the box + DNS records down.
# The ONLY required input is HETZNER_API_TOKEN (a Cloud Read&Write token) — from
# the env, or from ~/.hetzner-token, the fleet's per-machine home for it (the env
# wins when both are set; helpers/live_provision.py `hetzner_token`); this
# recipe sets the FAUNA_E2E_LIVE=1 opt-in for you. Optional env:
#   FAUNA_E2E_ZONE=<apex>     pick the DNS zone if the token manages >1
#   FAUNA_E2E_KEEP=1          leave the box up for inspection (no teardown)
#   FAUNA_E2E_VERIFY_DKIM=0   skip the DKIM gate during bring-up
# The APP is the first positional arg and defaults to `linux`; the drive is
# cross-app (helpers/live_provision.py `LIVE_DRIVE_APPS`) and an app outside that
# set skips as unbuilt debt rather than running. No `linux-debug` dep: the app
# under test is built by the conftest's collection-time prebuild, which follows
# `--app` for all 7 apps — the same path every other `e2e-*` recipe already uses.
# Usage: just e2e-live-provision                 (token from ~/.hetzner-token)
#        HETZNER_API_TOKEN=… just e2e-live-provision tui
e2e-live-provision APP="linux" *ARGS:
    FAUNA_E2E_LIVE=1 pytest tests/e2e-unified/tests/live/ -m live_provisioning --app {{APP}} -v -s {{ARGS}}

# Live, OPT-IN private-nest-behind-public-relay e2e: provisions a REAL Hetzner
# relay box (same harness/cost/teardown as e2e-live-provision), runs the
# PRIVATE nest as a local Docker container from the production
# ghcr.io/faunasocial/nest:latest (pulled — never built locally), onboards BOTH
# through the one linux client's real UI, sends real internet mail through
# example.com's submission port, and reads it back on the private nest via the
# client UI + a python IMAP client (plus the public no-readable-copy check).
# Required env on top of HETZNER_API_TOKEN — the live sender identity. Both the
# SENDER credential and the sender ADDRESS are produced in-test: the credential
# via the mail-settings UI, the address by deriving the handle from the secret
# (helpers/live_handle.py — testing.md § The shared-box rule, "derive what the
# box knows"). FAUNA_LIVE_MAIL_ADDRESS still overrides if you need to pin one.
#   FAUNA_LIVE_NEST_URL=https://example.com
#   the live box's mail-enabled admin identity seed — resolved per box: FAUNA_LIVE_SECRET_HEX > ~/.config/fauna/staging-box/<host>.json > ~/.fauna-id
# Runtime ~60-75 min (Hetzner DNS propagation dominates).
# APP is the first positional arg, default `linux` (see e2e-live-provision).
# Usage: HETZNER_API_TOKEN=… FAUNA_LIVE_NEST_URL=… just e2e-live-private-relay
e2e-live-private-relay APP="linux" *ARGS:
    FAUNA_E2E_LIVE=1 pytest tests/e2e-unified/tests/live/test_private_relay_hetzner.py --app {{APP}} -v -s {{ARGS}}

# Live, OPT-IN ActivityPub SERVING-surface e2e against an already-deployed box
# (example.com). NON-DESTRUCTIVE and the first resident of testing.md § The
# shared-box rule's carve-out: enable AP through the linux client UI -> assert
# the public routes (nodeinfo, WebFinger, actor doc, note, outbox) -> delete the
# post -> unlink. Never factory-resets (the AP instance-actor rotation hazard,
# activitypub.md § Architecture), and refuses to run if the box shows ANY
# pre-existing AP state rather than "normalizing" it.
#
# Only two inputs, and neither is a value the box could have told you:
#   FAUNA_LIVE_NEST_URL=https://example.com
#   the box's admin identity seed — resolved per box: FAUNA_LIVE_SECRET_HEX > ~/.config/fauna/staging-box/<host>.json > ~/.fauna-id
# The admin HANDLE is DERIVED from that secret (helpers/live_handle.py) — do not
# go looking for it; FAUNA_LIVE_HANDLE exists only as an override. Add
# FAUNA_LIVE_CLAIM_CODE (and then an explicit FAUNA_LIVE_HANDLE) only for an
# unclaimed box.
# `--nest live:` is NOT optional and is why this recipe exists: it is BOTH the
# `live_box` opt-in (test_live_box_opt_in_gate.py pin 3 — naming the live box on
# the command line IS the explicit request) AND what sets `builds_local_nest =
# False`. Without it the run spends ~25 min building a local `fauna-nest` at
# COLLECTION time and then skips every test for want of the opt-in — the exact
# trap session #21 diagnosed and fixed for `-m cd_suite`.
# APP is the first positional arg, default `linux` (see e2e-live-provision).
# Runtime ~5-10 min. Usage: FAUNA_LIVE_NEST_URL=… just e2e-live-ap-serving
e2e-live-ap-serving APP="linux" *ARGS:
    pytest tests/e2e-unified/tests/test_activitypub_live.py --app {{APP}} --nest live:${FAUNA_LIVE_NEST_URL} -v -s {{ARGS}}

# Live, OPT-IN, NON-DESTRUCTIVE bootstrap of a claim-fresh deployed nest
# (example.com) through the linux app UI: claim (or sign in) -> wait for the claimed
# domain's ACME cert under strict TLS -> enable mail + approve the co-located
# bridges -> wait for mail.<domain> :993/:465/:587 under strict TLS. Idempotent:
# on an already-bootstrapped box it signs in and mutates nothing. Never resets.
# Base env: FAUNA_LIVE_NEST_URL (the admin seed is resolved per box: FAUNA_LIVE_SECRET_HEX > ~/.config/fauna/staging-box/<host>.json > ~/.fauna-id). An UNCLAIMED box also
# needs FAUNA_LIVE_HANDLE (the handle to claim under — its @suffix becomes the
# domain) and FAUNA_LIVE_CLAIM_CODE (the nest's startup banner, container log);
# mail still off needs FAUNA_LIVE_MAIL_PASSWORD. `--nest live:` is the live_box
# opt-in, as for e2e-live-ap-serving. APP is the first positional arg.
# Usage: FAUNA_LIVE_NEST_URL=… just e2e-live-bootstrap
e2e-live-bootstrap APP="linux" *ARGS:
    pytest tests/e2e-unified/tests/test_live_box_bootstrap.py --app {{APP}} --nest live:${FAUNA_LIVE_NEST_URL} -v -s {{ARGS}}

# Live, OPT-IN ActivityPub federation e2e: the private -> public -> fediverse chain
# across TWO real Hetzner VPSes + one local Docker private nest, ending at a real
# public GoToSocial instance with its own Let's Encrypt cert. VPS #1 = the public
# relay nest (provisioned through the real client onboarding path, AP enabled);
# VPS #2 = the GoToSocial peer (helpers/live_gotosocial); the private nest is a
# local Docker container from ghcr.io/faunasocial/nest:latest (pulled, never
# built). Proves GoToSocial can follow the fauna actor AND a post authored on the
# private nest auto-forwards to the public nest and AP-pushes into the real
# GoToSocial follower's home timeline. Does NOT touch example.com (no live_box).
# Only env needed on top of the base gate: HETZNER_API_TOKEN (a Cloud R/W token).
# Real € (2 paid VPSes/run); always tears both down (box fixture, e2e-<runid>
# anchor). APP is the first positional arg, default `linux` (see e2e-live-provision).
# Usage: HETZNER_API_TOKEN=… just e2e-live-ap-federation
e2e-live-ap-federation APP="linux" *ARGS:
    FAUNA_E2E_LIVE=1 pytest tests/e2e-unified/tests/live/test_activitypub_federation_live.py --app {{APP}} -v -s {{ARGS}}

# Run the `test-hooks`-gated fauna-nest integration tests. The feature is OFF by
# default, and cargo SKIPS a target whose required features are not enabled —
# silently, with exit 0 — so `cargo test --workspace` never runs them. The six
# targets below are what `bins/fauna-nest/Cargo.toml` declares
# `required-features = ["test-hooks"]` on, i.e. the feature's only real coverage.
# Local mirror of CI's `rust` job step (same silent-rot guard as `nostr`/`bluesky`).
#
# ⚠ This recipe used to name `mail_index_ingest_route`, deleted
# with the unsealed `__index` ingest it guarded. Three call sites kept the
# reference — this recipe and both workflows — and since nothing runs a manual
# recipe on a schedule, the rot sat here until the public workflow was replayed
# against a curated tree on 2026-08-24. Lint + test together.
#
# The four `desktop_serve*` / `index_survives_nest_restart` targets joined when the
# plain-HTTP escape was compiled out of the two shipped desktop shells: they set
# `FAUNA_INSECURE_DISABLE_TLS=1` and dial `http://`, so they need the feature at
# RUNTIME. `--lib --bins` clippy does not reach a test target, so this `cargo test`
# line is the only thing that compiles them at all — dropping one here would leave
# it declared-but-never-run, which reads exactly like passing.
nest-test-hooks *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    cargo clippy -p fauna-nest --features test-hooks --lib --bins -- -D warnings
    cargo test -p fauna-nest --features test-hooks \
        --test conformance_at_rest_byte_scan \
        --test conformance_content_sealing_epochs_client \
        --test desktop_serve_loop \
        --test desktop_serve_loopback \
        --test desktop_serve_port_change_loopback \
        --test index_survives_nest_restart {{ARGS}}

# Clippy-check the off-by-default cargo features that a session's `cargo build` /
# `cargo clippy --workspace` never compile, so they rot silently on `main`: `nostr`
# (gates bins/fauna-nest/src/nostr/* + libs/fauna-bridge-nostr), `bluesky` (gates
# bins/fauna-nest/src/bluesky/* + libs/fauna-bridge-atproto), `activitypub` (gates
# bins/fauna-nest/src/activitypub/* + libs/fauna-bridge-activitypub) — all three
# SHIP in the image (Dockerfile ~:175; AP joined it 2026-07-16). A fourth feature,
# `wireguard`, was covered here from 2026-07-17 until the WireGuard stack was
# deleted outright 2026-08-23. AP is why
# this gate covers more than the two that shipped first: it had accumulated 5
# collapsible_if reds while off-by-default, precisely because no gate ever compiled
# it — the rot this recipe exists to stop.
# This is the recipe the merge gate runs path-scoped as its feature-clippy
# merge gate (merge-gates.md § Local-merge gates) — feature-gated code rotted 3×
# this way (an unswept wire field at 26 nostr sites, an axum
# 0.7→0.8 route-literal startup panic, a clippy nit). It mirrors CI's per-feature
# clippy steps (.github/workflows/ci.yml) — provenance only: there is NO automatic CI
# (build-system.md § CI enforcement), so this recipe is the enforcement, not a mirror
# of one.
# `--lib --bins --tests`: lints the feature's production code AND its test code (unit
# `#[cfg(test)]` modules inside the gated src/ subtrees, plus the crate-level- and
# inner-`#[cfg(feature=...)]`-gated integration suites in bins/fauna-nest/tests/) in
# ONE pass — 9 crate-level suites (7 nostr incl. the v25 child-safety
# `conformance_family_dm_gate`, 2 bluesky). Until 2026-07-18 this was two separate
# recipes (`nest-feature-clippy` at --lib --bins, clippy-only; `nest-feature-test-check`
# at --tests, check-only, deliberately NOT linted — folding --tests in here used to fail
# on arrival because the *ungated* `conformance_family.rs` carried 2 clippy lints
# (`to_vec`, too-many-arguments) nothing lints today). Both were fixed in place (that
# file, plus a digit-grouping lint in bluesky/mod.rs + activitypub/push.rs and an
# items-after-test-module ordering lint in activitypub/bridge_provider.rs — 6 lints
# total, all pre-existing debt, not new), and the two recipes merged into this one:
# `cargo clippy --tests` is a strict superset of `cargo check --tests` (it compiles the
# same targets and lints them too), so running both was pure waste. The bijection
# regression test below (test_merge_gate_feature_scope.py) matches both crate-level and
# inner `#[cfg(feature=...)]` test-file gates — a crate-level-only scan silently missed
# wireguard's inner-gated suites for as long as this recipe existed (found + closed
# 2026-07-17/18; that feature is gone since 2026-08-23, but the inner-gate matching it
# forced is what keeps any future inner-gated suite covered).
#
# fauna-nest is the ONLY crate with this test-code-coverage gap, and that is a
# structural fact, not a coincidence: every other crate's gated tests ride a feature
# that some workspace member turns ON, so `--workspace` feature unification reaches
# them (fauna-mail/fauna-calendar `nest-segments` via bins/fauna-nest; fauna-index
# `uniffi` via libs/fauna-ffi — verified with `cargo tree -e features -i <crate>
# --workspace`). fauna-nest's own features are enabled by no member,
# so no --workspace build can ever see them.
#
# Running the suites (rather than just compiling + linting them) was rejected: they are
# tier_3 (real handlers + tables) and need ~200 test binaries LINKED, against a merge
# path whose only running gate is installer-test at ~0.5s.
# `test-hooks` WAS intentionally excluded here — "a test-only seam that every e2e
# `build_node()` compiles, so its compile-rot surfaces fast". That reasoning was
# retired 2026-08-02, because "surfaces fast" names the wrong detector: the thing
# that surfaces the rot is a SESSION'S TIER_3 E2E RUN FAILING TO BUILD, and since
# `build_node()` compiles `test-hooks` on every machine, one rotted commit blocks
# every tier_3 e2e fleet-wide until someone chases a "nest-binary already failed to
# build" error back to a compile break they did not cause. It happened twice in one
# day, and then FIVE commits chased the same E0252 across five hours because each
# session could only see it by running an e2e: one commit (09:02) moved a
# `test-hooks`-gated `use bytes::Bytes` into link_preview_handlers' fetcher block,
# another commit (09:07) independently added the same gated import at module scope,
# and the commits at 08:53–09:43 were three more
# sessions converging on the identical fix, in parallel, unaware of each other. That
# is the cost of having no gate: not one break, but N sessions each paying the
# discovery. `nest-test-hooks` (above) is a manual recipe the merge gate and the check
# script both never call, and its comment's "local mirror of CI's rust job" has been
# fiction since CI died. A ~3 min leg in an ASYNCHRONOUS gate (measured 2026-08-02:
# `--lib --bins --tests` warm, 2m58s) is strictly cheaper than the fleet finding out.
# Clippy rot gate (production + test code) for the off-by-default
# nostr/bluesky/activitypub/test-hooks features (async check tier).
nest-feature-clippy:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo clippy --locked -p fauna-nest --features nostr --lib --bins --tests -- -D warnings
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo clippy --locked -p fauna-nest --features bluesky --lib --bins --tests -- -D warnings
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo clippy --locked -p fauna-nest --features activitypub --lib --bins --tests -- -D warnings
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo clippy --locked -p fauna-nest --features test-hooks --lib --bins --tests -- -D warnings

# Type-check every DEFAULT-feature workspace TEST target — the merge gate for
# the class where production code lands while its test-code callers are broken and
# nothing goes red. Nothing else on the merge path compiles *default-feature* test
# code: the fmt/freshness gates parse only, and a plain `cargo check -p <crate> --lib`
# compiles none of it (fauna-nest alone has 202 integration binaries under
# bins/fauna-nest/tests/). The nostr/bluesky/activitypub features' test code
# is the feature-clippy gate's job (above) — this gate runs default features only, so
# the two are complementary, not overlapping.
#
# Four independent reds sat on `main` this way, none caught by any gate: a NestSection
# field-add broke 7 nest binaries; cancel_dispatch asserted `cancelled` against what
# the central capability gate actually returns (permission_denied);
# onboarding_ws_nest_api_roundtrip still asserted a retired storage mode; and
# the commit that deleted the registration flags left bins/fauna-router's #[cfg(test)]
# module constructing three fields that no longer exist — red on main for 4 days,
# found only by adding this gate. That last one is why the scope is the WORKSPACE and
# not one crate: the blast radius of a struct change follows the dependency graph, not
# the directory, so "which crate did the diff touch" cannot bound it.
#
# `cargo check`, NOT `cargo test --no-run`: check type-checks every test target but
# emits only .rmeta and never LINKS the per-test binaries. That is what makes this
# affordable — ~13s warm (the realistic merge case: the session just built its own
# code), ~2m cold, where `cargo test --tests` over-builds every binary and can exhaust
# the cargo-target refquota. --keep-going so one broken target doesn't mask the others.
#
# The 8 `fauna-wasm*` crates are excluded: they are cfg(target_arch = "wasm32")-gated
# and NEVER check on a native host (~82 errors naming crates you never touched). If a
# NEW fauna-wasm* crate is added, this gate goes red with exactly that confusing
# signature — add it to the list below rather than debugging it. `just wasm` is what
# covers their construction sites.
# Three workspace members only compile where their Linux system APIs exist:
# fauna-linux (glib/gtk4 via pkg-config), ksni (libdbus), fauna-sandbox
# (landlock/seccomp/caps — Linux LSM syscalls). On mac/win the merge gate still
# type-checks every other workspace test target (the blast radius reachable
# from those machines); the three excluded crates' test code is covered by
# every merge from the primary Linux dev machine.
# Mirror-image case: fauna-shellext-fixture (a Windows-only dev fixture, root
# workspace member since the apps/fauna-windows unification) calls
# #[cfg(windows)]-gated SyncPipeClient::connect_pipe/connect_pipe_to
# unconditionally from its own un-gated main.rs, so it fails on non-Windows
# hosts the same way fauna-linux/ksni/fauna-sandbox fail on non-Linux ones
# (found 2026-07-22: red on the primary Linux dev VM, E0599 on `connect_pipe`).
test-compile-check:
    {{slot_build}} cargo check --locked --workspace --tests --keep-going \
        --exclude fauna-wasm \
        --exclude fauna-wasm-content-index \
        --exclude fauna-wasm-onboarding \
        --exclude fauna-wasm-folders \
        --exclude fauna-wasm-backups \
        --exclude fauna-wasm-media \
        --exclude fauna-wasm-labeler-catalog \
        --exclude fauna-wasm-connected-apps \
        --exclude fauna-wasm-atproto-settings \
        --exclude fauna-wasm-launch \
        {{ if os() == "linux" { "" } else { "--exclude fauna-linux --exclude ksni --exclude fauna-sandbox" } }} \
        {{ if os() == "windows" { "" } else { "--exclude fauna-shellext-fixture" } }}

# Type-check the workspace's NON-default-feature TEST code — the half
# `test-compile-check` above cannot see. That gate passes no `--features` at all,
# so every `#[cfg(all(test, …, feature = "x"))]` module, every `tests/` file
# behind a `#![cfg(feature = "x")]`, and every `#[cfg(test)]` nested inside a
# feature-gated `mod` is compiled by NO gate on ANY machine. This is not the
# per-platform gap the Linux/Windows/macOS check-script split covers — the
# configuration is never built anywhere, so it rots until someone enables the
# feature by hand.
#
# The bill was already paid once: bins/fauna-sync-agent/src/restore_byteplane_tier3.rs
# (`#[cfg(all(test, windows, feature = "tier3-nest"))]`) had accumulated TWO
# independent compile breaks from two unrelated signature changes — a missing
# `predecessor_backup_keys` argument to build_sync_engine and a missing 17th
# SyncMode argument to SyncEngine::new. Neither gate reported either; both were
# fixed only because a session speculatively compiled the feature.
#
# ⚠️ The crate list is DERIVED, not curated — `scripts/check_feature_gated_test_coverage.py`
# reconstructs it from the workspace and fails the cheap merge tier when this
# recipe drifts from it (`feature-test-coverage-check`). Do not hand-edit a
# line here: run `python3 scripts/check_feature_gated_test_coverage.py --emit`
# and paste. `--list` shows the exact site that put each crate in the set.
# fauna-nest is deliberately absent — `nest-feature-clippy` above already runs
# its five non-default features with `--lib --bins --tests`.
#
# Per-crate invocations, never one `-p a -p b` list: cargo unions features across
# the packages named in a single invocation, which activates combinations no real
# build ever sees (the `wasm-chunk-check` false-red lesson, one gate down).
# `cargo check`, not `cargo test --no-run`, for the same rmeta-only/refquota
# reason `test-compile-check` states above.
#
# fauna-linux is os()-gated exactly as in test-compile-check: it needs glib/gtk4
# via pkg-config and cannot compile off Linux.
feature-test-compile-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-account-plane --tests --features account-driver,preference-store
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-account-store --tests --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-atproto-settings-machine --tests --features account-port
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-calendar --tests --features nest-segments
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-backup --tests --features local-clock
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-capabilities --tests --features local-clock,p2p-share
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-connected-apps --tests --features rpc-glue
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-dns --tests --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-folders --tests --features mls,p2p-share
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-mail-settings --tests --features rpc-glue
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-client-recovery --tests --features aftermath
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-contacts --tests --features nest-segments
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-conversations --tests --features test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-core --tests --features format_text,local-clock,qr_render,sqlite-schema-meta
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-credential-store --tests --features live-keychain
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-devices-machine --tests --features rpc-glue,account-port
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-ffi --tests --features file-provider-host,test-helpers
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-folders-machine --tests --features rpc-glue
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-index --tests --features uniffi
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-iroh --tests --features quic
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-labeler-catalog-machine --tests --features rpc-glue
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-launch-machine --tests --features test-helpers,test-observer
    {{ if os() == "linux" { "CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-linux --tests --features live-secret-service" } else { "true" } }}
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-mail --tests --features caldav-schedule,domain-rename,imap-client,imap-client-native,mail-export,multidomain,nest-segments,outbound-net,role-overrides,segments-codec,segments-receive,staged-envelope,tls-test-fixtures
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-media-machine --tests --features rpc-glue
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-nest-http --tests --features launch-machine
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-onboarding-machine --tests --features test-observer
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-peer-channel --tests --features p2p-share
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-protocol --tests --features p2p-share,payments,tls-spki
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-provisioning --tests --features atproto-seal,oauth-issuer
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-sync-agent --tests --features fuse-live,tier3-nest
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected cargo check --locked -p fauna-sync-engine --tests --features account-runtime,engine-lifecycle,p2p-share

# RUN (not merely compile) fauna-linux's unit suite — the 280 tests that
# `test-compile-check` only type-checks and `workspace-clippy-check` excludes
# outright, so until this gate nothing on any machine ever executed them.
#
# Linux-only by construction (the crate does not build elsewhere), which is why
# it lives in the Linux merge-gate check. Scope-gated rather than cadence-gated:
# the suite finishes in ~1s warm, so there is no merge-burst risk to trade away
# with a 24h stamp the way `nest-lib-test-check` must.
#
# `xvfb-run -a` supplies the display `gtk_init` needs to bootstrap. It is NOT
# what isolates the tests — `crate::testid::start_private_display` gives the GTK
# test thread a display of its own no matter how the suite is invoked, including
# a bare `cargo test` — it is only what lets this gate run in the detached,
# `setsid`-spawned environment `merge-gate-check.sh` uses, which inherits no
# desktop session's `DISPLAY`.
fauna-linux-test-check:
    {{slot_build}} xvfb-run -a cargo test --locked -p fauna-linux --bins -- --test-threads=1

# The PQ-2 peer-channel hardening bounded smoke (p2p.md § Cross-user shared-set
# transfer → Open questions PQ-2, gate: green before any p2p data-plane slice
# ships): checked-in corpus replay + a fixed deterministic mutation/random sweep
# through the same check bodies the cargo-fuzz targets call
# (fauna_peer_channel::hardening). Runs in seconds; the corpus replay doubles as
# a peer-wire compat canary (a captured frame must keep decoding). This recipe
# never builds the workspace-excluded fuzz crate — coverage-guided long runs
# stay manual and install-gated (libs/fauna-peer-channel/fuzz/README.md).
# `--features p2p-share`: the share plane's coverage table + smoke targets are
# feature-gated (hardening.rs § KIND_PAYLOAD_COVERAGE_P2P_SHARE), and a
# default-features run would leave the gated pre-auth surface the untested
# flavor (the union-vs-parts trap). The feature only ADDS targets — the base
# surface runs identically under it.
peer-channel-hardening-check:
    {{slot_build}} cargo test --locked -p fauna-peer-channel --features p2p-share --test fuzz_smoke

# Tree-wide clippy — CHECK-tier gate (merge-gate-check.md § Merge-gate check).
# Mirrors ci.yml's dead `Clippy` step (never runs — no automatic CI, § CI
# enforcement), which nothing has enforced since that workflow's push trigger
# died 2026-03-30: every crate NOT covered by nest-feature-clippy/mail-bridge-
# ffi-check/apple-ffi-construction-check has had its default-feature lib/bin
# code linted by nothing for 3.5+ months.
#
# The exclude list is corrected, not copied, from ci.yml's — measured
# 2026-07-21 (don't re-derive): ci.yml's own list would fail on arrival even if
# billing were restored. `fauna-wasm` was ci.yml's only wasm exclusion; the
# other 7 fauna-wasm* crates are ALSO wasm32-target-only (fauna-wasm-folders/
# -media fail with E0433 "WsRpcClient not found" natively; the rest at least
# risk it — same uniform-exclusion policy test-compile-check already uses, for
# the same "a new wasm crate goes red with a confusing signature" reason).
# ci.yml's apps/fauna-windows-adjacent excludes (fauna-ipc, fauna-sync-service,
# fauna-bridge-service, fauna-nest-service, fauna-cfapi) were also missing 2 of
# the then-5 apps/fauna-windows crates: fauna-ctl and fauna-shell-ext
# (dead_code errors when linted outside their Windows callers — real debt, not
# a compile failure). fauna-sync-service left that list 2026-10-02 because the
# crate is gone: it was a 29-line windows wrapper whose bin fauna-sync-agent now
# builds itself, and fauna-sync-agent is linted here like any other member.
#
# THOSE TWO CAME BACK IN 2026-08-30, and the reason they left is worth keeping
# because it expired quietly. The dead_code debt was cleared at some point after
# 2026-07-21 and nobody re-tested the exclusion, so it outlived its cause —
# measured that day, `--all-targets -- -D warnings` over both crates is clean.
# What made it matter rather than merely tidy: the PUBLIC workflow's own
# WORKSPACE_EXCLUDES never listed them, so this gate's crate set was NOT a
# superset of the public one, and a lint firing only in those two crates was
# reachable by no internal gate on any schedule. It would have surfaced first as
# a red on the public repository's CI, where the fleet cannot pre-empt it.
# The rule this encodes: whenever a crate is excluded here, check it against
# `.github/workflows/ci.yml`'s exclude list — this
# gate may be stricter than the public one, never narrower.
#
# fauna-shellext-fixture joined this list 2026-07-22 when the
# nested apps workspace was unified into the root one and it became a root member
# (same reason: a Windows-only dev fixture, linted outside its Windows callers).
# fauna-linux was excluded 2026-07-21..2026-09-01 for carrying pre-existing
# clippy debt (a burn-down out of this gate's original scope; measured ~421
# errors at exclusion, ~247 by the time it was burned down to zero and
# re-added here — the debt had already shrunk
# between those two dates, incidentally, as other work touched the same
# files). ci.yml's own
# `--exclude fauna-linux` is unrelated and stays: that job's runner has no
# GTK4/libadwaita, so it cannot build fauna-linux at all (the real build+lint
# happens in the separate `linux-desktop` job, build-only, no GTK dev headers
# for clippy there either) — an infrastructure reason, not a debt one, and
# this gate is still no narrower than the public one for it. fauna-ffi stays
# excluded — it has its own dedicated feature-specific gates (mail-bridge-ffi-
# check, apple-ffi-construction-check) that would conflict on default-vs-
# explicit features. fauna-ipc/fauna-cfapi compile clean but stay excluded to
# match ci.yml's original scope rather than expand it speculatively.
#
# Measured 2026-07-21: ~10s warm for the whole tree, once the real debt this
# gate's first run surfaced was fixed: ksni's ~25 errors (libs/ksni/src/*.rs),
# 5 dead_code lints in fauna-sync-agent (platform-conditional — real callers are
# #[cfg(windows)]/#[cfg(any(target_os = "macos", windows))]/#[cfg(test)]-gated,
# so they're genuinely unreferenced on a Linux --lib-only build; scoped
# #[allow(dead_code)] with the reason, not removed), and 8 in fauna-tui (2
# large_enum_variant — one #[allow]'d as not worth a 30+-site refactor, one
# boxed since it had only 2 real sites; 1 too_many_arguments #[allow]'d — a
# single-caller function whose params are the wire command's own fields; 5
# doc_lazy_continuation — a doc comment's line-initial `+` was parsed as an
# unindented markdown list continuation, reworded to commas).
#
# --all-targets added 2026-08-15. Without it cargo selects
# its DEFAULT targets — lib + bins — so every #[cfg(test)] module and every
# tests/*.rs target in the workspace outside fauna-nest was COMPILED by
# test-compile-check and linted by NOTHING: the two compile gates are `cargo
# check` and stop at compilation, workspace-test-check runs tests without
# linting them, and nest-feature-clippy is the one recipe anywhere that lints
# test targets — only for fauna-nest, only over its five non-default features.
# Measured the first time the flag was passed: 32 warnings tree-wide, 0 errors —
# 25 of them #[cfg(test)]-module debt (reported against `kind: lib`, because
# --all-targets builds each lib a SECOND time as its own test harness) and 7 in
# tests/*.rs integration targets. ZERO came from benches or examples, so the
# flag's cost is honestly about test code; the tree has no benches at all and 4
# example dirs. All 32 were cleared in the commit that added the flag. Pinned by
# test_merge_gate_check.py::test_workspace_clippy_check_lints_test_targets — a
# dropped flag leaves the gate green while it has stopped looking.
workspace-clippy-check:
    {{slot_build}} cargo clippy --locked --workspace --all-targets \
        --exclude fauna-ffi \
        --exclude fauna-ipc \
        --exclude fauna-bridge-service \
        --exclude fauna-nest-service \
        --exclude fauna-cfapi \
        --exclude fauna-shellext-fixture \
        --exclude fauna-wasm \
        --exclude fauna-wasm-content-index \
        --exclude fauna-wasm-onboarding \
        --exclude fauna-wasm-folders \
        --exclude fauna-wasm-backups \
        --exclude fauna-wasm-media \
        --exclude fauna-wasm-labeler-catalog \
        --exclude fauna-wasm-connected-apps \
        --exclude fauna-wasm-atproto-settings \
        --exclude fauna-wasm-launch \
        -- -D warnings

# CHECK-tier, SCOPE-gated gate (merge-gate-check.md § Merge-gate check ->
# "Fourteenth gate") — RUNS the in-crate test suite of every natively-buildable
# workspace member that does not already have an executing gate of its own.
#
# The hole this closes: until 2026-08-13 exactly TWO crates in the tree had their
# tests executed by anything — fauna-nest (nest-lib-test-check, two arms, nightly)
# and fauna-linux (fauna-linux-test-check, scope-gated). Everything else was
# COMPILE-covered only: test-compile-check is `cargo check --tests`,
# feature-test-compile-check is `cargo check` too, workspace-clippy-check lints
# but never runs, and check_feature_gated_test_coverage.py is a registry parity
# check that asserts a gated module HAS tests, never that anything executes them.
# Measured 2026-08-13: 10,563 in-crate tests across 131 binaries ran under NO gate.
# That included libs/fauna-core's ratified account-record merge-semantics pins — the
# finding that opened this row, surfaced by a mutation (deleting present-wins from
# the custody lattice left `cargo test -p fauna-core --lib` reporting green).
#
# --lib AND --bins, not --lib alone. `--lib` reaches only crates with a src/lib.rs,
# and nine members with tests are bin-only — including apps/fauna-tui, the LEAD app
# (the app new UI features land on first), whose 1407 tests live in its bin target. A --lib-only arm
# would have silently missed 1,499 tests, the largest dark suite in the tree after
# fauna-nest's, while looking like full workspace coverage. This is the same reason
# fauna-linux-test-check is `--bins`.
#
# SCOPE-gated, not cadence-gated — the opposite call from nest-lib-test-check, and
# the measurement is why. That gate must carry a 24h stamp because fauna-nest's
# suite costs ~40min, which under a merge burst would grow a detection-latency
# backlog for every gate in the same pass. This arm is ~3.5min total warm
# (measured 2026-08-13 on the primary Linux dev VM under normal contention, a sibling holding the
# other build slot: 1m37s build + ~1m48s run, 10563 passed / 0 failed), i.e.
# fauna-linux-test-check's class, not the nest's. Re-measured through the recipe
# itself with the graph fully warm — the gate's actual steady state, since it
# runs right after workspace-clippy-check — the build drops to 46s. The expensive crate is precisely
# the one already excluded. Same scope regex as test-compile-check /
# workspace-clippy-check for the same reason test-compile-check's comment gives:
# the blast radius of a struct change follows the DEPENDENCY GRAPH, not the
# directory, so "which crate did the diff touch" cannot bound it.
#
# Disk: +1.0G marginal over the warm workspace-clippy-check graph (11G -> 12G in
# the 40G-refquota dataset). --lib --bins links ~131 small unit binaries, NOT the
# 487 fat per-suite integration binaries `--tests` would add (counted 2026-08-26,
# 277 of them fauna-nest's) — that over-build is
# the refquota trap test-compile-check's and nest-lib-test-check's comments both
# name, and it is why integration tests stay out of this arm (stated residual,
# merge-gate-check.md).
#
# The exclude list is workspace-clippy-check's, MINUS fauna-ffi and fauna-ipc
# (which that gate excludes for reasons that do not apply here: fauna-ffi for
# feature conflicts with its dedicated clippy gates, fauna-ipc to match ci.yml's
# original scope rather than expand it — both compile and TEST clean natively,
# verified 2026-08-13, and between them carry 407 tests that would otherwise stay
# dark), PLUS four:
#   fauna-nest         — nest-lib-test-check runs it (two arms, nightly cadence).
#                        Including it here would re-import the ~40min this gate's
#                        whole scope-gated design exists to avoid.
#   fauna-linux        — fauna-linux-test-check runs it, and it needs `xvfb-run`
#                        for gtk_init in the detached setsid environment.
# ⚠ A FOURTH entry, `--exclude uniffi-bindgen-cs`, stood here on the reason
# "vendored third-party fork; its upstream suite is not this fleet's to keep
# green" and NEVER EXCLUDED ANYTHING (found 2026-09-12 by the standing public-CI
# replay sweep, diffing this list against the public workflow's). `--exclude`
# takes a PACKAGE spec; `uniffi-bindgen-cs` is the `[[bin]]` TARGET name of the
# package `uniffi-bindgen-cs-fauna`. Measured on a throwaway workspace of that
# exact shape: cargo prints `warning: excluded package(s) `x` not found in
# workspace`, exits 0, and builds and tests the package anyway — so the line was
# dead text and the fork's suite has been running here, and passing, for as long
# as the line existed. The line is REMOVED rather than corrected, because
# removing dead text changes no behaviour while correcting the name would
# silently NARROW this gate — and the public workflow's own `WORKSPACE_EXCLUDES`
# does not exclude the fork either, so its suite is already something a public
# red would hold the fleet to. Disowning it is therefore a decision for BOTH
# lists at once and is deliberately left open here, not made in passing: it
# would need `--exclude uniffi-bindgen-cs-fauna` in this recipe AND in the
# shipped workflow, or the public CI reds on ground nobody owns. Every exclude
# name in both lists is now checked against the real package set by
# `test_workspace_test_scope.py`, so a rename cannot kill an exclusion silently
# again.
# fauna-sandbox is deliberately NOT excluded, though it was in this recipe's first
# draft on the theory that landlock/seccomp LSM syscalls need privileges the
# detached setsid environment cannot be assumed to grant. Measured, that is false:
# all 4 tests pass there, test_landlock_blocks_disallowed_path and
# test_seccomp_blocks_ptrace included. An exclusion is a permanent coverage hole,
# so each one here is a measured fact, never a plausible-sounding guess.
# The 10 fauna-wasm* crates and the remaining Windows-only crates
# (fauna-bridge-service, fauna-nest-service, fauna-cfapi,
# fauna-shellext-fixture) are excluded for the same target-arch/platform reasons
# workspace-clippy-check's comment records.
#
# fauna-ctl and fauna-shell-ext used to be in that Windows-only group and are
# NOT any more (2026-08-30). The host claim was false: they build and their
# tests PASS on Linux — 20 and 79 tests, 0 failed, 0.01s — and two other gates
# in this tree already said so, `test-compile-check` (`cargo check --workspace
# --tests`, which excludes neither and is green here every heavy pass) and a
# `cargo clippy --all-targets` of both measured green on Linux the day before.
# Meanwhile the PUBLIC repository's `cargo test --workspace` tested them, so
# those 99 tests ran on the public runner and on no internal gate on any
# schedule. Found by diffing the two gates' crate SELECTION rather than their
# verdicts; the asymmetry is now pinned by
# test_workspace_test_scope.py::test_a_host_capability_exclusion_is_honoured_by_the_public_ci_too.
# Their `#[cfg(windows)]`-gated test modules stay dark here, which is the same
# per-machine residual this comment already accepts two paragraphs down.
#
# Primary-Linux-dev-VM-only, and one honest residual: it executes each crate's LINUX-resolved cfg,
# so #[cfg(windows)]/#[cfg(target_os = "macos")] test bodies stay dark here. That is
# the same per-machine boundary test-compile-check's comment accepts for its own
# excluded crates.
# ⚠ This comment used to end "win/mac still type-check them via their own scripts'
# gate 1" — true of win, FALSE of mac.
# Win's gate 1 IS test-compile-check. Mac's gate 1 is the apple Swift/FFI compile,
# whose cargo half is `-p fauna-ffi` alone — it never type-checked these crates at
# all. The macOS half is now covered by RUNNING them: `mac-rust-test-check` below
# (gate 4 of merge-gate-check-mac.sh), this recipe's macOS twin.
#
# The ratchet that keeps this honest is tests/e2e-unified/tests/
# test_workspace_test_scope.py — every member with in-crate tests must be executed
# by this gate or carry a declared exclusion reason, so a NEW crate cannot land
# test-dark the way ~100 of them silently had.
#
# ⚠ READ THIS BEFORE FILING "crate X's feature-gated tests are dark". The
# --features line below is a SUPPLEMENT to cargo's workspace feature unification,
# NOT the set of features this gate enables. Reading it as the full set is now a
# TWICE-MADE mistake (which measured `cargo test -p fauna-sync-engine`
# — a SINGLE-package build, where nothing unifies — and reported 82 dark tests that
# in fact all run here). Neither Cargo.toml greps nor this comment settle it:
# forwarding chains (account-runtime -> preference-store) and target-gated edges
# both decide resolution. Ask cargo, two cheap ways, no build either way:
#   pytest tests/e2e-unified/tests/test_workspace_test_scope.py   (~1s, the ratchet)
#   cargo test <this recipe's exact flags> --unit-graph -Z unstable-options
# --features: the measured feature-dark pairs (added 2026-08-13; the
# fourth, `fauna-peer/tunnel`, went with the WireGuard stack 2026-08-23).
# `cargo test --workspace` UNIFIES features across the SELECTED packages, so a
# feature being non-default in its own Cargo.toml does NOT make it dark — 50 of
# 57 feature-gated-test pairs are already on here via some other member's dep
# edge. Only three mechanisms actually darken one, and all three below are
# measured instances, not a curated wishlist:
#   fauna-iroh/quic            — the ONLY enabler is apps/fauna-linux, which THIS
#                                recipe excludes — so the exclusion above did not
#                                merely skip fauna-linux, it silently switched
#                                this off in a crate the gate DOES run:
#                                fauna-iroh ran 0 of its 7 tests while reporting
#                                as covered.
#   fauna-client-core/auth-ceremony — its only enabler is a
#                                [target.'cfg(target_arch = "wasm32")'] dep edge
#                                (fauna-launch-machine), which never unifies on a
#                                native run. +2.
#   fauna-ffi/test-helpers     — genuinely opt-in; nothing enables it. +2.
#   fauna-mail/mail-export     — genuinely opt-in (client-only export twin of
#                                imap-client, docs/goal/behavior/mail-export.md
#                                § Export pipeline); nothing enables it.
# Measured 2026-08-13: 10708 -> 10734 tests, 0 failed, and NO crate loses tests
# (checked per-binary, not just in total — the nest gate keeps a separate default
# arm precisely because widening can subtract, and here it demonstrably does not:
# none of them has a #[cfg(not(feature = …))] test site). So this stays ONE
# arm and ONE build graph — the "second build graph" the residual feared is not
# what buying this coverage costs.
# Five pairs stay deliberately dark HERE, each for a reason this recipe already
# honours elsewhere: fauna-conversations/test-helpers is a tests/ target (out of
# --lib --bins scope by the same choice that keeps integration binaries out —
# but no longer dark globally: `mls-integration-test-check` below executes it);
# fauna-peer-channel/p2p-share's one test site is its fuzz_smoke tests/ target,
# which peer-channel-hardening-check runs with the feature on;
# fauna-credential-store/live-keychain is a #[cfg(target_os = "macos")] module
# needing an unlocked login keychain; fauna-sync-agent/tier3-nest pulls the
# whole fauna-nest crate — the graph the
# fauna-nest exclusion above exists to avoid; fauna-sync-agent/fuse-live
# mounts a real FUSE root, needing `/dev/fuse` and `fusermount3`. test_workspace_test_scope.py
# recomputes this whole split, so a NEW dark feature cannot land silently.
workspace-test-check:
    {{slot_build}} cargo test --locked --workspace --lib --bins \
        --features fauna-iroh/quic,fauna-client-core/auth-ceremony,fauna-ffi/test-helpers,fauna-mail/mail-export \
        --exclude fauna-nest \
        --exclude fauna-linux \
        --exclude fauna-bridge-service \
        --exclude fauna-nest-service \
        --exclude fauna-cfapi \
        --exclude fauna-shellext-fixture \
        --exclude fauna-wasm \
        --exclude fauna-wasm-content-index \
        --exclude fauna-wasm-onboarding \
        --exclude fauna-wasm-folders \
        --exclude fauna-wasm-backups \
        --exclude fauna-wasm-media \
        --exclude fauna-wasm-labeler-catalog \
        --exclude fauna-wasm-connected-apps \
        --exclude fauna-wasm-atproto-settings \
        --exclude fauna-wasm-launch

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — RUNS the MLS-plane
# integration suites: every `tests/` target of fauna-mls and fauna-conversations,
# including the `required-features = ["test-helpers"]` manager suite nothing else
# can even collect. Why these two crates get the one carve-out from the stated
# `--tests` residual (see workspace-test-check's comment): their integration
# targets pin SECURITY invariants — the succession ceremony's thief-lockout, the
# forged-Welcome ingest refusal, the seed-escrow AAD/actor binding, the
# forged-delete ignore, EXIF-strip-before-hash — and until 2026-08-19 they were
# compile-covered only, while a dev-dep comment claimed CI coverage that did not
# exist (the excused-test class: a coverage claim that licenses shipping the
# mechanism untested).
# Measured 2026-08-19 (cold, empty dataset, 8 jobs): full five-crate graph built
# in ~1m26s / 1.3G; this recipe runs 732 tests across 20 binaries in ~4s (cargo's
# `--tests` includes the pair's lib/bin unit targets, so ~450 of those re-run
# what workspace-test-check already covers — harmless seconds, not a new graph).
# Warm in the check tree the cost is incremental links + the ~4s run.
# Membership here is NAMED and justified per crate, never `--workspace --tests`
# — the fat-binaries refquota trap the residual names stands. A new dark
# suite earns its slot with a measured paragraph like this one.
# --test-threads=2: integration tests spawn their own runtimes; don't fan out
# one thread per core on a shared box.
mls-integration-test-check:
    {{slot_build}} cargo test --locked -p fauna-mls -p fauna-conversations --tests \
        --features fauna-conversations/test-helpers \
        -- --test-threads=2

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — the SECOND named
# carve-out from the `--tests` residual (added 2026-08-19), same shape as
# mls-integration-test-check above. workspace-test-check's own build graph
# already unifies EVERY fauna-mail feature (confirmed via `cargo tree -e
# features` under its exact --exclude list — nothing is transitively dark),
# so all of fauna-mail's feature-gated `src/` unit tests already run there.
# What stays dark is the structurally out-of-scope `tests/` directory itself:
# 20 integration files (imap_client_native.rs's `required-features =
# ["imap-client-native"]` target included — nothing else can even collect
# it), compile-covered only by test-compile-check/feature-test-compile-check,
# never RUN by anything. `--all-features` rather than an enumerated list: a
# single crate's own feature set, not a workspace-scale flag, so a future
# feature earns coverage automatically instead of silently joining the dark
# set the residual exists to name.
# Measured 2026-08-19 (cold, empty dataset): full graph built in 1m23s / 973M;
# this recipe runs 842 tests across 21 binaries (cargo's `--tests` includes
# the lib's own unit-test binary, which re-runs what workspace-test-check
# already covers — harmless seconds) in ~9s total across binaries. Warm in
# the check tree the cost is incremental links + the run.
# --test-threads=2: same reasoning as mls-integration-test-check above.
mail-integration-test-check:
    {{slot_build}} cargo test --locked -p fauna-mail --all-features --tests \
        -- --test-threads=2

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — the THIRD named
# carve-out from the `--tests` residual (added 2026-08-19), same shape as the
# two above. fauna-protocol's `tests/` directory is nine files of pure
# wire-contract pins — the golden-vector conformance suite (decode → re-encode
# → byte-for-byte equality against schemas/test_vectors/*.bin, the only thing
# anywhere that pins Rust types to the canonical vector BYTES), forward-compat,
# the Go↔Rust signed-message byte pins, namespace policy, and the codec suites
# — executed by nothing (workspace-test-check is `--lib --bins`; the compile
# gates never run). These pins are the enforcement half of the wire
# additive-evolution invariant that cddl-evolution-check (cheap tier) gates at
# the schema level; a canonical-encoding regression breaks an OLDER PEER, which
# no same-version test can see, so darkness here was the worst-placed residual
# of all. The second line runs the L3 transport-agnosticism assert
# (spec § 1.9: no WS/HTTP libs in fauna-protocol's dep tree) — equally dark
# before this gate (its only executor was the dispatch-only ci.yml), metadata-
# only, ~1s, and the same crate's same contract, so it rides here rather than
# minting a fourth gate.
# Measured 2026-08-19 (cold, empty dataset): full graph ~90s / 1.3G; warm the
# recipe runs 1299 tests across 10 binaries in 1.28s wall (the lib unit binary
# re-runs what workspace-test-check covers — harmless seconds).
# --test-threads=2: same reasoning as mls-integration-test-check above.
#
# `--features payments` added 2026-08-30: `payments_codec.rs`
# was `#![cfg(feature = "payments")]` against this crate's `default = []`, so it
# compiled to an empty binary and ran zero tests — same shape as the nest arm's
# dark planes above, one crate over. NOT `--all-features` (mail's model, above):
# fauna-protocol carries `js = ["fauna-core/js"]`, which pulls `js-sys` into a
# native host build and is unverified here; `payments_codec.rs` is the crate's
# only crate-root-gated target, so naming that one feature closes the whole gap
# with no risk to the untested `js` combination. Re-measured 2026-08-30: 1408
# tests across 11 binaries in ~32s wall (`payments_codec` now reports 12 passed,
# was 0); red-verified by breaking one of its roundtrip assertions and watching
# the gate report the failure, then reverting.
protocol-integration-test-check:
    {{slot_build}} cargo test --locked -p fauna-protocol --features payments --tests \
        -- --test-threads=2
    ./scripts/check-protocol-deps.sh

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — a FIFTH named
# carve-out from the `--tests` residual, same shape as the four above. `workspace-test-check` is `--lib --bins`,
# so `libs/fauna-onboarding-machine/tests/` — 27 integration files driving the
# onboarding wizard's entire decision surface against `FakeNestApi` — was
# reached only by `feature-test-compile-check`'s `cargo check -p
# fauna-onboarding-machine --tests --features test-observer` line, which
# COMPILES them and runs nothing.
#
# What the darkness hid, and why this is not bookkeeping: at the moment the gap
# was found, `tests/recovery_entry_ceremony.rs` could fail **8 of its 21 tests**
# on `origin/main` — a sibling test's process-global nest-dial override leaking
# into every other test's freshly-minted machine (fixed in the same commit as
# this gate). ⚠ It was a RACE, not a constant: measured 2026-08-30, the same
# unchanged binary failed 2 runs in 40 on an otherwise-busy box, and passed the
# other 38. So a single green run proved nothing, `--test-threads=1` passed
# every time, and nothing anywhere reported any of it — this crate is the wizard
# all 7 apps drive. The determinstic witness for the rule now lives in
# `tests/nest_dial_override_mirror.rs`; this gate's job is the other 26 files.
#
# `--tests`, not an enumerated target list: measured 2026-08-30 at 28 binaries
# / 354 tests / ~3s of run time across them, nowhere near the fat-binaries
# refquota trap `nest-lib-test-check`'s comment names (fauna-nest has 277
# binaries), and an enumerated list would let a new file join the dark set
# silently — the exact failure the residual exists to name. Default features:
# the crate's own `[dev-dependencies]` self-dep already turns on
# `test-helpers` + `test-observer` for the integration-test build, so there is
# no dark feature here, only the structurally out-of-scope directory.
#
# --test-threads=2: the shared-box load cap, same as the four gates above —
# NOT a race mask. Verified green both here and at full default parallelism
# (one thread per core, the configuration that actually exercises the leak
# class above), plus 60 consecutive default-parallelism runs of the formerly
# flaky binary. A recurrence must be answered by fixing the sharing, never by
# lowering this number — that is the configuration which HIDES the class
# (`dial.rs`'s `NEST_DIAL_OVERRIDE` note; `e2e-conventions.md` convention 10).
onboarding-machine-integration-test-check:
    {{slot_build}} cargo test --locked -p fauna-onboarding-machine --tests \
        -- --test-threads=2

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — a FOURTH named
# carve-out from the `--tests` residual, added 2026-08-19 alongside the three
# above (landed independently, same standing harvest). `fauna-wasm` is wholly
# EXCLUDED from `workspace-test-check` (wasm32-only — `_WASM_ONLY` in
# `test_workspace_test_scope.py`), so unlike the MLS pair it has no OTHER
# executing gate at all: `wasm-chunk-check` only `cargo check`s the `--lib`
# target, never `--tests`, so both integration files
# (`tests/subscription.rs`, `tests/upload_sidecar.rs`) were compile-covered
# by NOTHING until this gate. `subscription.rs` pins the key-blob-minting
# path (`mint_key_blob_inner`, the author-side WASM binding for
# `fauna.subscription.mint_key_blob`, mirroring
# `bins/fauna-nest/src/subscription_handlers.rs::verify_encrypted_upload`) —
# security-relevant, and its 4 tests had never executed in ANY environment:
# native `cargo test` can't even resolve the wasm32-gated
# `fauna_wasm::mint_key_blob_inner` import, and this project's toolchain has
# no Node.js (wasm-bindgen-test's other default runtime — the SPA is built
# with Deno), so they need the explicit `run_in_browser` config this pass
# added. Getting this green found and fixed three real bugs (2026-08-19): a
# duplicate `use ed25519_dalek::Verifier` (E0252, in the auth-signature
# test), a call to a nonexistent `cbor_from_reader`, and a `.clone()` on
# `ActorKeypair` (which deliberately has no `Clone` impl — it holds zeroized
# secret key material; fixed by borrowing instead).
# Needs a headless browser: `--headless --firefox` (both Firefox and
# `geckodriver` are provisioned on the primary dev VM) — wasm-bindgen-test
# has no browser-independent runtime available here (no Node.js). Scoped the
# same way as `mls-integration-test-check` (firefox+geckodriver aren't
# provisioned on the other two dev VMs for this).
# Measured 2026-08-19: warm compile+link ~7s, the tests themselves ~0.1s —
# the crate graph is already warm from `wasm-chunk-check`/
# `mls-integration-test-check`, no new dependencies. `--locked` passes
# through `wasm-pack test`'s trailing extra-cargo-args to the underlying
# `cargo build --tests`, verified 2026-08-19.
# `fauna-rpc-wasm` joined as the second line 2026-09-15. It is wasm32-only
# too (an empty rlib on native), so its `adapter.rs` close-code tests had
# never run under a gate, and the `WsRpcClient` pins added that day (a
# stopped reconnect loop's reason reaching every request at once, a closed
# client's fast-fail) needed one. Measured: 37 s for its first wasm32 test
# build on this target, about 3 s warm; its 6 tests run in 0.06 s.
# `fauna-account-store` joined as the third line 2026-09-27: its `tests/web.rs`
# grades web's IndexedDB store arm against the backend-generic conformance suite
# every native arm also runs (`src/conformance.rs`), so the web replica is held
# to the one statement of the `StoreBackend` contract. `--features test-helpers`
# is the harness (the suite + its memory test double); `tests/web.rs` requires it.
wasm-test-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected wasm-pack test --headless --firefox libs/fauna-wasm --locked
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected wasm-pack test --headless --firefox libs/fauna-rpc-wasm --locked
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected wasm-pack test --headless --firefox libs/fauna-account-port --locked
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected wasm-pack test --headless --firefox libs/fauna-account-store --locked --features test-helpers

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — RUNS fauna-nest's
# test-hooks-gated at-rest byte-scan proof. `nest-feature-clippy` clippies `-p
# fauna-nest --features test-hooks --lib --bins --tests` but never RUNS anything;
# `nest-lib-test-check` enables `test-hooks` but only for `--lib`, never `--tests` —
# fauna-nest's crate-level `[[test]]` block (`conformance_at_rest_byte_scan`, the S8
# D5 at-rest byte-scan proof for `CacheDb::scrub_plaintext_where_sealed`) had no gate
# running it at all. Named target, not `--tests`: fauna-nest carries 277 separate
# integration-test binaries (the same 40G-refquota trap `nest-lib-test-check`'s own
# comment names), and this is its only explicit `[[test]]` block — everything else is
# auto-discovered `tests/*.rs` outside this gate's job.
#
# `conformance_content_sealing_epochs_client` (the B5 § 4/§ 8 windowed-holder
# proof) joined on 2026-08-23 as the crate's second `[[test]]` block. It had been
# auto-discovered and RED since 2026-07-18: it needs `test-hooks` for a ROUTE
# rather than a symbol, so unlike its sibling it compiled fine without the
# feature and failed at runtime on the static fallback's non-JSON 200. Guarding it
# stops the misleading red; naming it here is the other half — a guarded target
# nothing runs is just a dark test.
#
# The four `desktop_serve*` / `index_survives_nest_restart` targets joined when the
# plain-HTTP escape (`FAUNA_INSECURE_DISABLE_TLS`) was compiled out of the two
# shipped desktop shells `fauna-nest-service` + `fauna-nest-daemon`. They are the
# ROUTE-shaped kind again, one step further out: each sets the var and dials the
# loop it spawns over `http://`, so with the feature off they compile and run while
# the loop serves HTTPS, and the assertion dies on a transport error naming nothing
# about features. That is the population this recipe's own note called unmeasured —
# `bins/fauna-nest/tests/` did hold more of the runtime-dependency kind, and these
# are them.
nest-testhooks-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    cargo test --locked -p fauna-nest --test conformance_at_rest_byte_scan --features test-hooks
    cargo test --locked -p fauna-nest --test conformance_content_sealing_epochs_client --features test-hooks
    cargo test --locked -p fauna-nest --test desktop_serve_loop --features test-hooks
    cargo test --locked -p fauna-nest --test desktop_serve_loopback --features test-hooks
    cargo test --locked -p fauna-nest --test desktop_serve_port_change_loopback --features test-hooks
    cargo test --locked -p fauna-nest --test index_survives_nest_restart --features test-hooks
    cargo test --locked -p fauna-nest --test owed_web_render_drained_at_boot --features test-hooks
    cargo test --locked -p fauna-nest --test echo_round_trip --features test-hooks

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — RUNS the whole of
# `bins/fauna-nest/tests/`: the 277 integration targets `nest-lib-test-check`
# deliberately leaves out. That gate's own comment (below) has named this a
# stated residual since 2026-07-31, and the residual's cost is measured: FOUR
# reds have lived on `origin/main` behind it, every one found by coincidence
# rather than by a gate — `manifest_has_builtin_classifiers` (16 days),
# `conformance_content_sealing_epochs_client` (five weeks), the three a
# 2026-08-23 end-to-end sweep found, and five
# `conformance_email_send_in_domain` tests (four days). Cadence-gated nightly by `run_nest_integration_gate` in
# scripts/merge-gate-check.sh, NOT scope-gated, for exactly the reason its
# `--lib` sibling is: 47m 34s measured (2026-08-30, `--all-features`; was 36m 14s
# at `--features test-hooks` before the fix below).
#
# Why this closer and not the other two the residual names. A narrow
# `--test <name>` allowlist is cheaper but re-darkens every NEW file by
# default, and every one of the four incidents above was in a file nobody had
# thought to list — the allowlist would have caught none of them on arrival.
# `cargo-nextest` is new third-party tooling on a machine that builds every
# released artifact, so it needs the user's explicit install + channel approval
# first, under this project's standing rule on unvetted third-party software;
# not ruled out, simply never asked. This
# arm covers a new file the moment it lands, which is the property the incidents
# argue for.
#
# BATCHED in 7 named-target groups of 40 with a reclaim between, never one
# `--tests` invocation. Measured peak in an isolated dataset was only 2.8G, but
# the number that governs is not this one: the gate runs against
# `$CARGO_TARGET_DIR/main`, which it SHARES with the other CHECK-tier gates and
# which sits ~29G into its 40G refquota — so ~11G is the real headroom, and 277
# unreclaimed integration binaries do not fit it. ⚠ Both figures are COMPRESSED
# bytes: every cargo-target dataset carries `compression=zstd` (3.7-4.3x
# measured), and refquota charges compressed, so a logical-bytes estimate read
# against this limit overstates by ~4x (build-machine-resources.md § Cargo
# target dir layout).
#
# `--all-features`, not `--features test-hooks` (changed 2026-08-30 —
# merge-gate-check.md's own paragraph for this gate has the full measurement).
# The single-feature form left 22 of 23 crate-root-gated targets
# (20 `nostr`, 2 `bluesky`) compiling to EMPTY binaries that exit 0 having run
# zero tests — indistinguishable from success by exit code, and three of them
# were executed for real only by the PUBLIC repository's CI. `--all-features` is
# a strict superset of every plane, so it still satisfies the constraint that
# made `test-hooks` non-optional in the first place: six targets in fauna-nest's
# crate-level [[test]] block carry `required-features = ["test-hooks"]`
# (conformance_at_rest_byte_scan, conformance_content_sealing_epochs_client,
# desktop_serve_loop, desktop_serve_loopback, desktop_serve_port_change_loopback,
# index_survives_nest_restart) — without SOME arm enabling that feature cargo
# SKIPS them silently and still exits 0, which is the failure shape this whole
# gate exists to end. Verified legal first: `cargo check -p fauna-nest
# --all-features` compiles clean (the `store-safe` "complement" is a
# `--no-default-features` naming convention, not a real feature conflict, so
# there is nothing for the other planes to collide with today).
#
# Single-arm, and the residual is stated rather than hidden: this still runs
# one arm, not `nest-lib-test-check`'s bare-default + all-features PAIR. A test
# needing one plane ON and another specifically OFF at once is still executed by
# nothing — the same shape as that gate's own excision corollary, accepted here
# for the same reason: a second arm doubles a 47m 34s nightly.
#
# --test-threads=2 for the same reason as every sibling: integration tests spawn
# their own runtimes, so don't fan out one thread per core on a shared box.
#
# --no-fail-fast AND every batch runs even after one fails: a red in batch 1
# must not hide batches 2-7, or a run names one failure, someone fixes it, and
# the next pass discovers the next one — 6 tests across 3 binaries were found
# red on arrival across two separate batches.
nest-integration-test-check:
    #!/usr/bin/env bash
    set -uo pipefail
    cd {{justfile_directory()}}
    mapfile -t targets < <(cd bins/fauna-nest/tests && ls -1 *.rs | sed 's/\.rs$//' | sort)
    echo "nest-integration-test-check: ${#targets[@]} targets under bins/fauna-nest/tests/"
    rc_all=0
    batch=40
    for ((i = 0; i < ${#targets[@]}; i += batch)); do
        args=()
        for t in "${targets[@]:i:batch}"; do args+=(--test "$t"); done
        echo "--- batch $((i / batch + 1)): ${#args[@]} targets ---"
        {{slot_build}} cargo test --locked -p fauna-nest --all-features --no-fail-fast \
            "${args[@]}" -- --test-threads=2 || rc_all=1
        # Reclaim between batches: drop fauna-nest's own artifacts (the test
        # binaries are the bulk); the dependency graph stays warm, so the cost
        # is one lib relink per batch, not a cold rebuild.
        cargo clean -p fauna-nest
    done
    exit $rc_all

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — RUNS bins/fauna-sync-agent's
# tier3-nest-gated integration tests (
# workspace-test-check's own
# comment already names fauna-sync-agent/tier3-nest dark for the same reason, pulls the
# excluded fauna-nest crate). `agent_process_tier3` is where the row's second confirmed
# real bug lived (an engine-lock-release ordering defect, fixed forward — `git log --grep
# "releases the engine lock BEFORE shutdown"`) — proof this crate's tier3-nest suite was
# genuinely never run, not just quiet.
#
# `cross_nest_agent_capstone` joined 2026-08-20. Its
# long red was never the relay and never load: (1) the owner-side matcher keyed on
# resting plaintext `path`, which the S9 flip (2026-08-01) scrubbed on sealed planes —
# a deterministic hang nobody saw because nothing ran the suite; (2) the owner uploaded
# AFTER the member's bind, making delivery a race against the engine's one eager pull
# (a cross-nest member has no nudge and a 300 s cadence — no other sub-tick trigger
# exists). Both fixed 2026-08-20: the matcher keys on path_hash + opens the sealed
# label, and the seed upload precedes the bind so the eager pull delivers causally.
# Green 6.95 s on a loaded machine. The test's module docs § Order is load-bearing
# carry the full story — read them before touching its ordering or windows.
sync-agent-tier3-nest-check:
    {{slot_build}} cargo test --locked -p fauna-sync-agent --features tier3-nest \
        --test agent_process_tier3 --test same_nest_push_nudge \
        --test cross_nest_agent_capstone --test agent_custody_rekey

# Two devices behind two NATs: does our build ever leave the relayed path? The
# measurement `behavior/p2p.md` § NAT hole punching cites — a routed control
# (must go direct), then port-keeping, symmetric and mixed gateways, each graded
# against EXPECT (per mode; the default is what the shipped build measured — the
# cone pair punches through, symmetric and mixed stay relayed), so it fails if
# the build STOPS making direct paths. DISCOVERY=on is the shipped relay (it
# serves QUIC address discovery); `just p2p-nat-probe relay 60 off` measures the
# relay protocol alone, the shape that shipped before.
# Docker on this box, no new image: the probe, `nft` and `ip` run from the
# host's own /usr (scripts/p2p-nat-probe.py).
p2p-nat-probe EXPECT="cone=direct,symmetric=relay,mixed=relay" SECS="60" DISCOVERY="on":
    {{slot_build}} cargo build --locked -p fauna-iroh-relay --features relay --example nat_probe
    {{py}} scripts/p2p-nat-probe.py --mode all --secs {{SECS}} --expect {{EXPECT}} --discovery {{DISCOVERY}}

# CHECK-tier gate (merge-gate-catalog.md § The heavy gate catalog) — RUNS fauna-sync-agent's
# live FUSE suite (`--features fuse-live`): the linux on-demand root's headless proof
# (on-demand-files.md § Linux FUSE binding) over REAL mounts — listing, hydrate-on-open,
# pass-through writes, dehydrate never-a-delete, pins, both mode flips, the crash guard and
# the boot sweep. `feature-test-compile-check` only compiles it. ~20 s warm, mounts under
# /tmp (the distro's AppArmor profile for fusermount3 admits no other temp root). It needs
# an openable /dev/fuse and the fuse3 package's fusermount3: a box
# without them fails HERE, by name — never a silent pass, never one mount error per test.
fuse-live-test-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! { : <> /dev/fuse; } 2>/dev/null; then
        echo "fuse-live-test-check: /dev/fuse is not openable here — the live FUSE suite cannot run (a sandbox, or no fuse kernel module)" >&2
        exit 1
    fi
    if ! command -v fusermount3 >/dev/null; then
        echo "fuse-live-test-check: no fusermount3 on PATH (install the fuse3 package)" >&2
        exit 1
    fi
    {{slot_build}} cargo test --locked -p fauna-sync-agent --features fuse-live --lib fuse_ -- --test-threads=2

# CHECK-tier, NIGHTLY-cadence gate (merge-gate-check.md § Merge-gate check) — the only
# gate anywhere that RUNS fauna-nest's lib test suite. Every other gate on any path
# (cheap merge, this check script's other six) at most COMPILES tests; none executes
# them, so a runtime red sits on `origin/main` until a session happens to run the
# suite by hand. Three independent hits in one week proved this a real gap (not
# hypothetical): two genuine lib-test reds found only incidentally (one,
# by a session running the suite for an unrelated reason); a THIRD found by writing
# this gate's own measurement run (`db::outbound`'s retention sweeper, a defunct
# wall-clock sleep-then-assert); and — worst — a stale red that had ALREADY been
# fixed got re-reported as live 33 minutes after its fix landed, because nothing runs
# the suite to prove a fix took, and the proposed remedy for the "still-red" report
# would have reverted a closed security finding.
#
# `--lib`, not `--tests`: `-p fauna-nest --tests` (or `--workspace --tests`) builds
# 277 separate integration-test binaries and can fill the 40G-refquota cargo-target
# dataset this script shares with the other CHECK-tier gates (same trap
# test-compile-check's own comment names). `--lib` is the focused, disk-safe form.
# Accepted residual: an integration-test-only regression under
# `bins/fauna-nest/tests/` (one has occurred — `manifest_has_builtin_classifiers`,
# red 16 days before anyone noticed) is NOT caught by this gate. CLOSED 2026-08-26
# by the separate lower-cadence arm: `nest-integration-test-check` runs all 277
# targets nightly in batches of 40. That gate is ARMED (2026-08-26; the call site
# `run_nest_integration_gate` is live in scripts/merge-gate-check.sh, added once
# the six reds its own measuring run found were dispositioned), so this residual
# no longer stands in practice — the sentence saying it did outlived the arming
# by four days and is corrected here.
# ⚠ The residual it DOES leave is wider than "one plane ON and another OFF": that
# arm passes `--features test-hooks` and nothing else, so an integration file
# gated at the crate root on a BRIDGE plane compiles to an empty binary and
# reports green having run zero tests. Counted by the cfg rather than by
# filename: 23 targets here carry a crate-root feature gate -- 20 `nostr`, 2
# `bluesky`, 1 `payments` -- and since `payments` is default-ON, 22 of them run
# zero tests under this arm. Three (conformance_nostr_storage_gate,
# conformance_bluesky, conformance_bluesky_link) ARE executed at their real
# features -- by the public repository's CI and by nothing internal. CLOSED 2026-08-30: that arm now
# passes `--all-features` instead of `--features test-hooks` (a strict
# superset, so the six `required-features = ["test-hooks"]` targets still
# run), which closes all 22 dark targets — see that recipe's own comment
# below for the measurement and the red-verify.
#
# NIGHTLY, not per-push: measured under load (2026-07-30, 20+ concurrent sessions)
# at 2360.95s (39.3 min) for the full 3127-test suite — the lib target itself
# compiles warm in ~34s, so the run itself, not the build, is the cost. This
# script's other gates are kicked and re-verified on EVERY push and are designed to
# stay under ~1-2 min warm; wiring a ~40-minute gate into that per-push cadence would
# not just be slow once, it would fall behind a merge burst (3-15 concurrent
# sessions is this fleet's normal load, not a spike) and grow an ever-widening
# detection-latency backlog — the exact failure mode the 2026-07-23 pinned-build-tree
# work fixed for hybrid builds, reintroduced for a different reason. So this gate is
# cadence-gated in the check script itself (a `last-green` timestamp file, checked
# against a 24h interval) rather than scope-gated like the other six — see
# merge-gate-check.sh's `run_nest_lib_test_gate`. This mirrors the project's existing
# precedent for an expensive-but-valuable scan: `govulncheck` moved to a weekly-cron
# + manual-dispatch cadence for the identical reason (§ CI enforcement).
#
# TWO ARMS, and the second one is why this recipe exists in its current shape.
# `bins/fauna-nest/Cargo.toml` declared `default = []` (⚠ since 2026-08-10 it is
# `["store-safe", "payments", "zaps"]` — the gated-feature registry members; arm 1 is
# now "the registry members, every optional plane off" rather than "no features at
# all", and a DEFAULT-ON feature is by construction invisible to BOTH arms — see
# merge-gates.md § Feature scope → the excision corollary, and the separate
# `nest-store-safe-check` recipe that builds the excised nest nothing else builds),
# so arm 1 compiles — let alone
# runs — none of the nostr/bluesky/activitypub/test-hooks planes. Until
# 2026-08-05 that was the whole recipe, so those planes were COMPILE-gated by
# nest-feature-clippy and TEST-gated by nothing: a plane could be lint-clean, fully
# compiled, and have every behavioural pin unexecuted for months. Measured on
# 2026-08-05 (`cargo test … --lib -- --list`, counts of runnable lib tests):
#   default 3313 · nostr 3457 (+144) · activitypub 3409 (+96) · bluesky 3373 (+60)
#   · test-hooks 3316 (+3) · wireguard 3313 (+0) · all five at once 3623 (+310).
# (`wireguard` contributed +0 even then, and the feature was deleted outright
# 2026-08-23 — the union arm is four planes now, same 3623.)
# So arm 2 executes 310 tests that no gate anywhere had ever run.
#
# ONE union arm, NOT one arm per feature — three measured reasons, in order:
#   1. The union is STRICTLY MORE COVERAGE, not just cheaper. Five separate runs
#      total 3616 distinct tests; the union has 3623. The 8 in the union that no
#      single-feature build contains are `activitypub::outbound::tests::
#      resolve_override::*` (7) + `::test_hook_is_closed_without_the_env_var`, which
#      need `activitypub` AND `test-hooks` together — an SSRF/resolve-override
#      surface, exactly the kind a per-feature loop is blind to by construction.
#   2. Disk. Each `--features` permutation is its own build graph in the 40G-refquota
#      dataset this gate shares with the six compile gates. Five permutations was the
#      real risk the finding named; TWO (default + union) measured at ~2G marginal
#      over arm 1 alone.
#   3. Runtime. The run, not the build, is this gate's cost (~27-40 min for arm 1),
#      so five arms would be ~2.5-3.5h nightly. Two is ~2x, on a 24h cadence.
#      Measured 2026-08-05 under load 34 (both build slots held by siblings — normal
#      contention here): arm 2 ran `3623 passed; 0 failed` in 2378.24s (39.6 min),
#      the same order as arm 1's 2026-07-30 figure, and all 310 newly-covered tests
#      passed on arrival. The slot's 5400s bound is on ACQUIRING a slot, not holding
#      one, so a ~80 min recipe cannot fake-fail on it.
# Arm 1 is NOT redundant with arm 2 and must not be dropped to save the time: six
# `#[cfg(not(feature = ...))]` sites in bins/fauna-nest/src select genuinely different
# production code under `default = []` (the nostr-off empty `held_dm_conversations`,
# the test-hooks-off Live STARTTLS prober, and discovery_core's "RELAY must NOT be
# advertised" assertion), and `default = []`
# is a SHIPPED configuration: the Windows installer's FaunaNest service exe
# (apps/fauna-windows/fauna-nest-service path-depends on bins/fauna-nest with
# `default-features = false`; release.yml builds it `-p fauna-nest-service`). Under
# arm 2 alone that shipping arm would be the arm never tested.
# What arm 1 is NOT: the nest Docker image's shape. That artifact builds
# `--features bluesky,nostr,activitypub`, which no
# arm here runs — deliberately, because a third ~40 min arm and a third build graph
# in the 40G-refquota dataset is not worth it. What the two arms owe the Docker shape
# instead is that NOTHING IS DARK: no fauna-nest code or test may require a plane ON
# and another feature OFF at once, since arm 1 compiles no plane and arm 2 turns
# everything on. Pinned by test_merge_gate_feature_scope.py::
# test_no_nest_test_is_dark_to_the_two_arm_lib_gate. This replaced the count-form
# residual ("exactly ONE test") on 2026-08-06: that one test was
# `activitypub::outbound::tests::rejects_loopback_inbox`, and rather than buy an arm
# for it, the SSRF guard's cfg-free body was named `ap_outbound_client_strict` and the
# pin retargeted onto it, so it now runs in arm 2. A count could not have noticed a
# second such test arriving; the pin reads the cfg shapes.
# The feature set here is pinned against Cargo.toml's [features] table by
# tests/e2e-unified/tests/test_merge_gate_feature_scope.py::
# test_every_nest_feature_is_executed_by_the_lib_test_gate — a new flag cannot land
# test-dark.
nest-lib-test-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    cargo test --locked -p fauna-nest --lib
    cargo test --locked -p fauna-nest --lib --features nostr,bluesky,activitypub,test-hooks,test-helpers

# CHECK-tier gate — the tier_3 API contract suites (merge-gate-check.md
# § Merge-gate check -> Accepted gaps, item (6)). ARMED 2026-08-21 as the 24th
# gate, `run_api_contract_gate` in scripts/merge-gate-check.sh (cadence-stamped
# nightly, NOT scope-gated, modelled on `run_nest_lib_test_gate`: 24m05s
# measured, which sits inside nest-lib-test-check's own 27-39 min band and must
# not ride the per-push cadence). Was deliberately left unarmed while red on
# origin/main with 10 deterministic failures (measured 2026-08-20) —
# arming a gate red on arrival would have published a fleet-wide red section for ten defects nobody had triaged. A follow-up dispositioned all ten as stale tests (no production defect); the suite
# now runs 277 passed / 0 failed.
#
# Deliberately not `--tier 3`: the directory is the unit the gate protects (71
# tier_3 + 1 tier_2, no tier_4), and a tier filter would silently change scope
# as markers move.
#
# COST NOTE: this holds an *e2e* slot for its whole duration while ALSO
# queueing for a *build* slot for the nest prebuild, so it draws from both
# pools -- measured live. That is why the caller runs it bare (conftest
# takes its e2e lane) and never under `build` (build-slot.py's own § Cross-pool
# order: e2e-holds-while-build-queues is the only non-deadlocking direction),
# and why it rides the nightly stamp rather than the per-push cadence.
api-contract-check:
    pytest tests/e2e-unified/tests/api/ -v --durations=0

# CHECK-tier gate — the share/peer-transfer plane's only end-to-end witness
# (merge-gates.md § two-tier posture). BUILT
# 2026-08-24 as the 25th gate, `run_share_pump_gate` in
# scripts/merge-gate-check.sh (cadence-stamped nightly, NOT scope-gated,
# modelled on `run_api_contract_gate`: same cadence-not-scope shape, same
# reason — `test_share_pump_two_actor.py` carries `@pytest.mark.timeout(1800)`
# and a fresh measurement put `--app linux` at ~12 min, `--app tui` at up to
# ~36 min under load, so running BOTH every push would fall behind a merge
# burst). Landed because the test sat red on `origin/main` under both apps
# for days with nothing catching it — the same
# un-gated-compile-scale-check shape merge-gates.md already names for other
# tier_3 suites. User ruling (2026-08-24): BOTH apps, not just the cheaper
# `--app linux` leg alone — full two-app coverage over the cheaper single-app
# recommendation. NOT YET ARMED: the call site in merge-gate-check.sh's main
# sequence is commented out — a pre-arming dry run found the linux leg red
# (an escrow-deposit RPC disconnect stranding the folder dial row); see
# merge-gate-check.md § 25th gate for the disposition pointer.
#
# COST NOTE: same e2e/build cross-pool shape as api-contract-check — the
# caller runs this bare, never under `build` (build-slot.py § Cross-pool
# order), and it rides the nightly stamp rather than the per-push cadence.
share-pump-check:
    pytest tests/e2e-unified/tests/test_share_pump_two_actor.py --app linux,tui -v --durations=0

# CHECK-tier gate — the tier_3 suite under tests/platform/sync/
# (merge-gate-check.md § Merge-gate check, 27th gate; merge-gates.md's two-tier
# posture). Until 2026-09-02 this directory was in
# NO gate at all, and the cost of that darkness was measured twice in it
# (merge-gate-check.md § 27th gate has the record). It was the daemon-pair
# family — a real fauna-nest + the legacy sync daemon per test — until the legacy
# daemon was removed (2026-10-02, sync-agent.md § Headless deployment); what
# remains is `test_sync_register.py`, the user-class `fauna.sync.register`
# WS-RPC ceremony against a real nest, which no other gate runs, so the gate
# stays over it rather than re-darkening the directory: a new file here joins
# by existing. `run_platform_sync_gate` in scripts/merge-gate-check.sh runs
# this recipe — cadence-stamped nightly, NOT scope-gated, modelled on
# `run_share_pump_gate`.
#
# COST NOTE: same e2e/build cross-pool shape as api-contract-check — the
# caller runs this bare, never under `build` (build-slot.py § Cross-pool
# order), and it rides the nightly stamp rather than the per-push cadence.
# `--durations=0` so the check log carries every test's own wall-clock — the
# per-test figure is what decides the cadence, and the log is where it lives.
platform-sync-check:
    pytest tests/e2e-unified/tests/platform/sync/ -v --durations=0

# Run the curated re-claim-sensitive suites against a nest that has been put
# through a real factory-reset -> re-claim (same identity) cycle — the
# "everything, post-reclaim" gate for the works-on-first-claim / breaks-after-
# re-claim bug class. Built on `--reclaim-cycle` (resets the SHARED nest once at
# session start, before any test) + the `reclaim_cycle` marker (selects the
# representative suite per area: events / mail / devices / sync, plus the
# mechanism self-check). The broad sibling of the bespoke tier_3
# test_factory_reset_calendar_reclaim.py.
# Usage:
#   just e2e-reclaim-cycle-test --client linux   (focused: the bug's home; no web SPA build)
#   just e2e-reclaim-cycle-test                  (all clients available on this machine)
# To run ANY suite post-reclaim (not just the curated set), drop `-m reclaim_cycle`:
#   pytest tests/e2e-unified/tests/test_<x>.py --reclaim-cycle --tier 3 -v
# `--app sweep` keeps this recipe's documented "all clients available on this
# machine" contract after the 2026-08-01 rust-first default flip to `[tui]`.
# A trailing `--app <x>` in ARGS still wins (last occurrence).
e2e-reclaim-cycle-test *ARGS:
    pytest tests/e2e-unified/tests/ -m reclaim_cycle --reclaim-cycle --tier 3 -v --app sweep {{ARGS}}

# Real-wire acceptance for the client-driven DNS-01 cert order core (Phase 3, S4b). Drives `obtain_certificate_dns01` end to end against a local
# pebble ACME server + an in-process authoritative DNS responder — the only test
# that exercises the account -> order -> authorizations -> finalize half against a
# real CA implementation (the CA-free unit tests stop at the publish/teardown
# choreography). Needs Docker; the test (`#[ignore]`) pulls + runs pebble on the
# host network, so run it serially. Override the pebble version with
# `FAUNA_PEBBLE_IMAGE=ghcr.io/letsencrypt/pebble:<tag>` (default 2.9.0 — pinned
# when it was the newest that spoke instant-acme 0.7.2's wire shape; the pin was
# NOT re-evaluated at the 0.8.5 bump (2026-09-02), so a newer pebble may now work
# and nobody has checked. See the test's module docs).
e2e-pebble-dns01 *ARGS:
    {{slot_build}} cargo test -p fauna-client-dns --test pebble_dns01 -- --ignored {{ARGS}}

# The HTTP-01 twin: the shared nest/front-door flow (`fauna-acme-http01`)
# driven end-to-end against pebble, with the production challenge listener on
# pebble's baked httpPort (5002). Same Docker + serial-run caveats as above;
# don't run concurrently with e2e-pebble-dns01 (both bind host port 14000).
e2e-pebble-http01 *ARGS:
    {{slot_build}} cargo test -p fauna-acme-http01 --test pebble_http01 -- --ignored {{ARGS}}

# Build Android debug APK (+ androidTest APK for instrumented tests)
#
# The Gradle step is freshness-gated OUTSIDE the build slot (build-system.md §
# Build/e2e slot locks) — the same shape as windows-debug's MSBuild gate.
# UNGATED until 2026-07-31 (unlike windows-debug, this one never had ANY gate,
# not even a bindgen-tail-style one), this recipe took a machine-wide `build`
# slot and re-ran Gradle on every invocation, including one that had just
# succeeded.
#
# --stamp, not the two APKs, carries freshness: Gradle is incremental, so a
# no-op assemble leaves both APKs' mtimes untouched, and an APK-keyed gate
# would go permanently stale after any mtime churn that doesn't relink (every
# rebase does this). The APKs stay existence checks. The whole app tree is
# watched (not an enumerated source list) so `android-ffi-test`'s generated
# Kotlin bindings (src/debug/java/uniffi) and copied `.so`s (src/debug/jniLibs)
# — already inside this tree — re-trigger Gradle by construction, matching
# windows-debug's identical reasoning; build/ and .gradle/ are excluded as
# this recipe's own output/cache, never a build input.
#
# Depends on `android-ffi-test`, NOT `android-ffi`: the debug variant is what
# the e2e harness drives, and its `src/debug` TestAgent calls the `*ForTest`
# UniFFI seams, which exist only in the test-flavored bindings (testing.md
# § convention 15 — see `_android-ffi-flavor`).
android-debug: android-ffi-test i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label android-debug \
        --stamp apps/fauna-android/.gradle-debug.stamp \
        --target apps/fauna-android/app/build/outputs/apk/debug/app-debug.apk \
        --target apps/fauna-android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk \
        --source apps/fauna-android \
        --exclude '*/build/*' --exclude '*/.gradle/*' \
        -- {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleDebug assembleDebugAndroidTest

# Print the SSH tunnel command for the android e2e run venue (testing.md
# § Default app and nest mode → Android's run venue): the one command the
# emulator's machine runs to dial this one, with every forward generated from
# the venue's port constants (tests/e2e-unified/helpers/android_venue.py) so the
# tunnel and the harness cannot disagree about a port. Prints only — it runs
# nothing and opens nothing. `login` and `address` are this machine's, as the
# emulator's machine dials them; both default to what this machine reports.
android-tunnel-spec login="" address="":
    @{{py}} tests/e2e-unified/helpers/android_venue.py "{{login}}" "{{address}}"

# Build Android release APK
android-release: android-ffi i18n-generate providers-generate
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleRelease

# Build the Play upload artifact — a release **AAB**, not an APK.
#
# Play takes an Android App Bundle and splits it per-device itself; the APK that
# `android-release` builds is a dev/sideload artifact and cannot be uploaded to
# the Console. So this is the recipe the release chain ends on
# (installers/android.md § Signing model).
#
# Signing is opportunistic, and deliberately so: the release `signingConfig` in
# `app/build.gradle.kts` materialises only when
# `~/.config/fauna/android/signing.properties` exists, so this recipe produces a
# SIGNED bundle on the machine holding the upload key and an unsigned one
# everywhere else. Both traverse the identical R8/minify path, which is what
# keeps the release build verifiable on machines that must never hold the key.
#
# The strip check afterwards is convention 15's release-artifact half (testing.md
# § Cross-app e2e conventions point 15) applied to android's shipping artifact:
# the real `TestAgent` lives in `src/debug` and the shipping flavors compile the
# inert `src/noAgent` twin instead, so the bridge poll loop's own strings must be
# absent from the bundle's dex. It greps the DEX, not the `.aab`: dex string
# constants are deflate-compressed inside the archive, and a `strings` over the
# packed bytes reports zero for everything — which reads exactly like a perfect
# strip (the same trap `android-store-safe-check` documents at length).
android-bundle: (android-ffi "dist") i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-21-openjdk-arm64}"
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android bundleRelease

    AAB="apps/fauna-android/app/build/outputs/bundle/release/app-release.aab"
    [ -f "$AAB" ] || { echo "ERROR: no bundle at $AAB" >&2; exit 1; }

    WORK="$(mktemp -d "{{tmp_root}}/android-bundle-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT
    unzip -q -o "$AAB" 'base/dex/classes*.dex' -d "$WORK"
    strings -a "$WORK"/base/dex/classes*.dex > "$WORK/dex.strings"

    # Markers unique to the REAL agent — the inert twin carries none of them.
    # The two bridge endpoints are the poll loop itself; the Hilt entry point and
    # the log line are its wiring. Any one of them in a shipping dex means the
    # debug source set reached a release variant.
    rc=0
    for marker in '/app/commands' '/app/state' 'TestAgentEntryPoint' 'Starting with bridge'; do
        if grep -qF -- "$marker" "$WORK/dex.strings"; then
            echo "FAIL: automation-surface marker '$marker' present in the release bundle dex" >&2
            rc=1
        fi
    done
    if [ "$rc" -ne 0 ]; then
        echo "  the src/debug TestAgent reached a shipping flavor; see testing.md convention 15" >&2
        exit 1
    fi
    echo "android-bundle: automation surface absent from the release dex (4 markers checked)"

    # Signed-or-not is a property of THIS machine key custody, not of the build,
    # so report it rather than requiring it.
    # `grep -E >/dev/null`, never `grep -q`: under pipefail, -q exits at the first match
    # and SIGPIPEs unzip once the bundle's listing outgrows the pipe buffer, which reads
    # as UNSIGNED for a signed bundle.
    if unzip -l "$AAB" | grep -E 'META-INF/.*[.](RSA|DSA|EC)$' >/dev/null; then
        echo "android-bundle: SIGNED  $AAB"
    else
        echo "android-bundle: UNSIGNED (no signing.properties on this machine)  $AAB"
    fi

# Build the store-safe (payments-excised) Android APK — the android leg of the
# App-Store escape hatch (dynamic-features.md § The App-Store escape hatch).
#
# `assembleStoreSafe` is `assembleRelease`'s `initWith` twin: same minification,
# same proguard files, `BuildConfig.PAYMENTS = false`. R8 folds that constant
# and strips the payments renders, which is how the element ids leave the dex —
# the FFI's absence alone only removes the glue.
android-store-safe: android-ffi-store-safe i18n-generate providers-generate
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleStoreSafe

# Build the Fauna Kids APK (family-safety.md § The account age band, the
# kids-app bullet; installers/android.md § Goal): the `kids` build type —
# `storeSafe` under the `social.fauna.faunakids` identity, over the
# `kids-safe,kids-floor` FFI flavor staged under `src/kids/`.
#
# `assembleKids` is `assembleStoreSafe`'s `initWith` twin with
# `BuildConfig.KIDS = true`: R8 folds the constant and strips every
# `if (!BuildConfig.KIDS)` render, which is how the excised surfaces' element
# ids leave the dex — the FFI's absence alone only removes the glue.
android-kids: android-ffi-kids i18n-generate providers-generate
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleKids

# Build the F-Droid / direct-download APK (installers/android.md § Release
# channels): the `foss` build type — `release` minus every proprietary Google
# Play client library (the store-age arm's Play Integrity + Play Age Signals
# shims live only on the Play-distributed build types). Same production
# `fauna-ffi` flavor as `android-release`, staged under `src/foss/`.
android-foss: android-ffi-foss i18n-generate providers-generate
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleFoss

# The `foss` witness: the assembled APK's dex must name NO
# `com.google.android.play` class — that absence is what makes the artifact
# F-Droid-includable and honest on a Play-less device. Greps the dex members,
# not the packed APK (the `android-store-safe-check` trap: deflate-compressed
# dex strings read as a perfect strip).
android-foss-check: android-foss
    #!/usr/bin/env bash
    set -euo pipefail
    APK="apps/fauna-android/app/build/outputs/apk/foss/app-foss-unsigned.apk"
    [ -f "$APK" ] || APK="apps/fauna-android/app/build/outputs/apk/foss/app-foss.apk"
    [ -f "$APK" ] || { echo "ERROR: no foss APK under apps/fauna-android/app/build/outputs/apk/foss/" >&2; exit 1; }
    WORK="$(mktemp -d "{{tmp_root}}/android-foss-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT
    unzip -q -o "$APK" 'classes*.dex' -d "$WORK"
    HITS="$(cat "$WORK"/classes*.dex | strings -a | grep -c 'com/google/android/play/' || true)"
    if [ "$HITS" != "0" ]; then
        echo "ERROR: the foss APK's dex names com.google.android.play $HITS time(s) — a Play library reached the F-Droid artifact" >&2
        exit 1
    fi
    echo "android-foss-check: OK — no com.google.android.play class in $APK"

android-ffi-foss profile="release": i18n-generate providers-generate
    @just _android-ffi-flavor foss "" all "{{profile}}"

# Build fauna-ffi for Android targets and generate Kotlin bindings — PRODUCTION
# flavor. Carries no extra features beyond fauna-ffi's own defaults; the
# `test-helpers` seams are NOT compiled, so the `.so` that ships in the release
# APK exports none of them.
# Requires: Android NDK with clang on PATH
android-ffi profile="release": i18n-generate providers-generate
    @just _android-ffi-flavor release "" all "{{profile}}"

# Test-flavoured Android FFI: adds `test-helpers`, which compiles the E2E
# injection seams (`OnboardingMachine::set_*_for_test` / `call_machine_method`,
# `ConversationsManager::install_mock_backends_for_test` / `inject_inbound_for_test`,
# `LaunchMachine::set_phase_for_test`, …) and exports them over UniFFI. Consumed
# by `android-debug`, whose `src/debug` TestAgent calls them.
android-ffi-test: i18n-generate providers-generate
    @just _android-ffi-flavor debug test-helpers

# Store-safe Android FFI — the App-Store escape hatch's flavor
# (dynamic-features.md § The App-Store escape hatch). `payments` and `zaps` are
# compiled out, so the generated Kotlin face carries no `FfiPaymentsClient`, no
# `FfiProviderItem`/`FfiClaimItem`, no `paymentsKnownKinds`/`paymentsWebhookUrl`
# and no `FfiFeedManager.resolvePostTips`. That absence is what makes a
# half-excised shell a COMPILE ERROR instead of a silent leak — the app's
# payments glue lives in `app/src/noPayments/` for this variant.
#
# Stages into `src/storeSafe/`, the `storeSafe` build type's own source set, so
# all three flavors coexist on disk and no recipe's correctness depends on which
# ran last.
#
# `p2p-share` (whose ceremony half is fauna-ffi's `offline-share`) is excised
# the same way: the plain complement exports no `FfiCeremonySeat` /
# `offlineShare*` face, and the app's ceremony glue lives in
# `app/src/noP2pShare/` for this variant.
android-ffi-store-safe profile="release": i18n-generate providers-generate
    @just _android-ffi-flavor storeSafe store-safe all "{{profile}}"

# Kids Android FFI — the Fauna Kids flavor (family-safety.md § The account age
# band, the kids-app bullet; dynamic-features.md § Compile-time excision).
# `kids-safe` is the complement the kids app keeps, so the generated Kotlin face
# carries no feed, search, bridge, web-publishing, monetization or catalog
# export at all; `kids-floor` compiles the four guardian categories at `block`
# into every render face. Stages into `src/kids/`, the `kids` build type's own
# source set, beside the hand-written twin under `src/kids/java/com/fauna/app/`
# (which the bindgen's wipe never touches — it clears only `uniffi/` and
# `com/fauna/ffi/`).
android-ffi-kids profile="release": i18n-generate providers-generate
    @just _android-ffi-flavor kids "kids-safe,kids-floor" all "{{profile}}"

# Shared implementation of the two flavors above. `buildtype` is a Gradle
# buildType name (`release` | `debug`) AND the source-set the artifacts stage
# into — that is what makes the gate structural rather than ordering-dependent:
#
#   * `assembleRelease` compiles `src/main + src/release`, so it can only ever
#     see the production bindings staged here under `src/release/`;
#     `assembleDebug` / `testDebugUnitTest` / `assembleDebugAndroidTest` compile
#     `src/main + src/debug` and see only the test-flavored ones.
#   * Both flavors therefore coexist on disk. The alternative — one shared
#     `src/main/java` staging slot plus a flavor marker (the shape apple-ffi /
#     windows-ffi must use, since their app projects consume ONE fixed path) —
#     would make correctness depend on which recipe ran last, and a plain
#     `gradlew assembleRelease` would silently package whichever flavor was
#     staged. Android has buildType source sets; use them.
#
# Convention: testing.md § Cross-app e2e conventions point 15 — the automation
# surface is compiled out of release artifacts. Rule (b) there keeps any UniFFI
# export of a seam keyed on the FEATURE ALONE, never the profile, so "production
# flavor" is exactly "don't pass test-helpers" and the generated Kotlin face is
# a pure function of `features` below.
# `abis` restricts which Android ABIs are built: `all` (the four the APK ships)
# or `arm64` (aarch64 only). `arm64` exists for `android-store-safe-check`,
# whose two columns would otherwise pay for EIGHT release `fauna-ffi` builds to
# answer a question one ABI settles — the same reasoning that gives apple a
# 1-slice host twin (`apple-ffi-host-store-safe`) for its witness. It is folded
# into the flavor marker below, so an `arm64` run can never leave a later `all`
# run trusting three stale `.so`s.
# `profile` is a cargo PROFILE NAME (`release` | `dist`), mirroring
# `_windows-ffi-flavor`'s axis (installers/windows.md § Size & build profile):
# dev/test/e2e builds stay on `release` for a fast inner loop; only the
# shipping recipe (`android-bundle`) passes `dist`, the size-optimised profile
# (strip + thin-LTO + opt-level "s" + 1 CGU) `[profile.dist]` in the root
# `Cargo.toml` defines. `--profile release` and `--release` select the same
# built-in profile, so no shorthand branch is needed.
_android-ffi-flavor buildtype features abis="all" profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    # NDK clang-19 needs Rosetta's JIT-only mode (`CAMBRIA_DISABLE_AOT`, exported
    # justfile-wide — see the top of this file).
    # Explicit-target artifact build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    BUILDTYPE="{{buildtype}}"
    FEATURES="{{features}}"
    ABIS="{{abis}}"
    # {{profile}} is a cargo PROFILE NAME, not an output directory: cargo writes
    # `dev` into `debug/` and every other profile (`release`, `dist`) into a dir
    # of its own name — mirrors `_windows-ffi-flavor`'s PROFILE/PROFILE_DIR split.
    # Android never passes `dev` today (only `release`/`dist`), but the mapping
    # stays uniform with windows rather than assuming the unreached case away.
    PROFILE="{{profile}}"
    case "$PROFILE" in
        dev) PROFILE_DIR=debug ;;
        *)   PROFILE_DIR="$PROFILE" ;;
    esac
    STAGE_JAVA="apps/fauna-android/app/src/$BUILDTYPE/java"
    STAGE_JNI="apps/fauna-android/app/src/$BUILDTYPE/jniLibs"
    # `store-safe` is a THIRD axis, not a feature to add (dynamic-features.md
    # § The cargo feature spine): it is the COMPLEMENT feature naming every
    # surface that is NOT a gated-feature-registry member, so the excised flavor
    # is `--no-default-features --features store-safe` rather than `default`
    # minus a list. Spelling the complement on the command line instead would
    # rot the first time a surface is added to one place and not the other —
    # invisibly, since both flavors still build.
    # `file-provider-host` rides every flavor: the SAF `DocumentsProvider`
    # serves through `FfiFileProviderHost::app_dead_owned_tree`
    # (on-demand-files.md § Android SAF DocumentsProvider binding) — the same
    # always-on composition the apple recipes use.
    if [ "$FEATURES" = "store-safe" ]; then
        FEATURE_FLAGS="--no-default-features --features store-safe,file-provider-host"
    # `store-safe,<extra>` is the complement PLUS a named interim carry (see
    # `_STORE_SAFE_INTERIM_CARRY` in `test_payments_excision_spine.py`, which
    # pins every shell to none today), never a second spelling of the complement.
    elif [ "${FEATURES#store-safe,}" != "$FEATURES" ]; then
        FEATURE_FLAGS="--no-default-features --features $FEATURES,file-provider-host"
    # The kids flavor (family-safety.md § The account age band, the kids-app
    # bullet): `kids-safe` is the complement NESTED INSIDE `store-safe`, so it
    # is a complement root exactly as `store-safe` is, and `kids-floor` is the
    # one addition the flavor asks for. Matched whole, never by prefix — a
    # `kids-safe` build without the floor is not a flavor anything ships.
    elif [ "$FEATURES" = "kids-safe,kids-floor" ]; then
        FEATURE_FLAGS="--no-default-features --features kids-safe,kids-floor,file-provider-host"
    elif [ -z "$FEATURES" ]; then
        FEATURE_FLAGS="--features file-provider-host"
    else
        FEATURE_FLAGS="--features $FEATURES,file-provider-host"
    fi
    case "$ABIS" in
        all)   TRIPLES=(aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android) ;;
        arm64) TRIPLES=(aarch64-linux-android) ;;
        *)     echo "ERROR: unknown abis '$ABIS' (want 'all' or 'arm64')" >&2; exit 1 ;;
    esac
    # Validate NDK toolchain is available — only for the ABIs this run builds,
    # so an `arm64` witness run is not blocked by a toolchain gap in a triple it
    # never touches.
    declare -A NDK_CLANG=(
        [aarch64-linux-android]=aarch64-linux-android26-clang
        [armv7-linux-androideabi]=armv7a-linux-androideabi26-clang
        [x86_64-linux-android]=x86_64-linux-android26-clang
        [i686-linux-android]=i686-linux-android26-clang
    )
    for TRIPLE in "${TRIPLES[@]}"; do
        tool="${NDK_CLANG[$TRIPLE]}"
        if ! command -v "$tool" &>/dev/null; then
            echo "ERROR: $tool not found on PATH. Set up Android NDK toolchain first."
            exit 1
        fi
    done
    # Resolve cargo target directory (may be shared across checkouts)
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps 2>/dev/null | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null || echo target)}"
    # Build for all Android ABIs. `$FEATURES` adds `test-helpers` in the test
    # flavor only (see the recipe comment above). Until 2026-08-01 there was
    # only ONE recipe and it passed `test-helpers` unconditionally, rationalised
    # as "inert in production unless called" — precisely the reasoning
    # convention 15 was ratified to reject, and it rode all 23 seams into the
    # shipped release APK's `libfauna_ffi.so`.
    #
    # The 4 cargo builds write to `$TARGET_DIR/<triple>/release/`, ONE path per
    # triple that both flavors share, so the `.so` on disk is whichever flavor
    # built last. That is fine — the staged copies under `src/<buildtype>/` are
    # the durable artifacts — but it means the cargo stamp cannot be trusted
    # across a flavor switch: with a per-flavor stamp, build prod → test → prod
    # would find the prod stamp still "fresh", skip cargo, and stage the TEST
    # `.so` into `src/release/`. A deterministic wrong-flavor false-green, the
    # same class wasm-core's two-producer comment describes. So the last-built
    # flavor is recorded next to the stamp and a mismatch drops the stamp,
    # forcing a real rebuild (cargo would recompile for the changed feature set
    # anyway — this only stops build-if-stale from skipping ahead of it).
    #
    # The ABI SET is part of the marker for the same reason the feature set is:
    # an `arm64` run leaves the other three triples' `.so`s at whatever flavor
    # built them last, so a later `all` run that skipped cargo on a "fresh"
    # stamp would stage three stale-flavor libraries beside one current one.
    # PROFILE joins the marker for the same reason FEATURES/ABIS do: `release`
    # and `dist` builds write to DIFFERENT per-triple dirs (below), so they
    # never clobber each other's `.so`s directly — but the marker also gates
    # the shared `android-ffi-cargo.stamp`, and without PROFILE in it a `dist`
    # run followed by a `release` run (same FEATURES/ABIS) would read the stamp
    # as fresh and skip cargo, even though `$TARGET_DIR/<triple>/release/`
    # hasn't been touched by this invocation at all. The marker records the
    # resolved FEATURE_FLAGS, not the recipe's `features` argument, so a change
    # to what a flavor composes (e.g. `file-provider-host` joining every
    # flavor) invalidates the stamp too.
    FLAVOR_MARKER="$TARGET_DIR/android-ffi.flavor"
    [ "$(cat "$FLAVOR_MARKER" 2>/dev/null)" = "$FEATURE_FLAGS:$ABIS:$PROFILE" ] || \
        rm -f "$TARGET_DIR/android-ffi-cargo.stamp"
    # The 4 cargo builds are freshness-gated OUTSIDE the build slot (build-system.md
    # § Build/e2e slot locks) — the same shape as mail-bridge-ffi-lib / linux-debug /
    # tui-debug. UNGATED until 2026-07-31, these took a slot and queued behind
    # sibling builds 4 times over on every invocation, including a fully warm tree
    # (the bindgen tail below was
    # already gated, but that only skips the bindgen, not the slot wait ahead of it).
    # One combined --stamp covers all 4 targets: a source/dep change invalidates
    # them together (same crate, same libs/ dependency graph), so per-target
    # granularity buys nothing. The 4 .so outputs stay existence checks (cargo is
    # incremental and leaves them untouched on a no-op, so they cannot carry
    # freshness themselves — see linux-debug's comment for the general rule).
    # `--config profile.$PROFILE.strip=false`: measured 2026-09-11 — `[profile.dist]`'s
    # `strip = true` makes uniffi-bindgen's LIBRARY-MODE generation (`generate
    # --library <.so>`, no UDL) silently produce ZERO binding files against an
    # NDK-clang `llvm-strip`'d cdylib (exit 0, empty out-dir — no error text at
    # all; confirmed with a controlled `strip=false` A/B rebuild, 26 Kotlin files
    # vs 0). So the cargo-built .so must stay UNSTRIPPED regardless of profile —
    # this override is a no-op for `release`/`dev` (neither sets `strip`) and is
    # what makes `dist` bindgen-safe. The SHIPPED artifact still gets the size
    # win: `_android-ffi-bindgen` runs `llvm-strip` on the STAGED jniLibs copy,
    # AFTER bindgen has already read the unstripped original — never on this
    # cargo-target .so itself, which stays the durable, bindgen-readable source
    # for every future incremental build. Root cause not investigated further
    # (would mean reading uniffi's own vendored/registry source, which this
    # project's dependency-source policy forbids without explicit approval).
    # Any library-mode bindgen over a stripped `dist` cdylib can hit the same
    # failure; the A/B above is the Kotlin leg's measurement.
    TARGET_ARGS=()
    BUILD_CMDS="set -euo pipefail"
    for TRIPLE in "${TRIPLES[@]}"; do
        TARGET_ARGS+=(--target "$TARGET_DIR/$TRIPLE/$PROFILE_DIR/libfauna_ffi.so")
        BUILD_CMDS="$BUILD_CMDS
            {{slot_build}} cargo build --locked -p fauna-ffi $FEATURE_FLAGS --profile $PROFILE --config profile.$PROFILE.strip=false --target $TRIPLE"
    done
    {{py}} scripts/build-if-stale.py --label android-ffi-cargo \
        --stamp "$TARGET_DIR/android-ffi-cargo.stamp" \
        "${TARGET_ARGS[@]}" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- bash -c "$BUILD_CMDS"
    # Written only after a successful build, so a crashed/interrupted cargo run
    # leaves the marker on the PREVIOUS flavor and the next invocation still
    # forces a rebuild rather than trusting a half-built tree.
    echo "$FEATURE_FLAGS:$ABIS:$PROFILE" > "$FLAVOR_MARKER"
    # cargo is incremental: the 4 builds above no-op when fresh and relink (bump
    # each .so's mtime) only on an actual source/dep change. So the .so outputs
    # are the precise staleness signal — regenerate Kotlin bindings + jniLibs iff
    # a .so changed, else skip the expensive uniffi-bindgen run. Mirrors apple-ffi
    # (this is
    # the first Android-capable machine to run-verify it).
    # --stamp, not the staged trees, carries freshness — and here that is a
    # CORRECTNESS fix, not the usual cargo-no-op one. `_android-ffi-bindgen`
    # generates the bindings FIRST and runs the seam witness LAST, so a witness
    # failure leaves targets already written and newer than the .so sources. In
    # non-stamp mode the target IS the freshness signal, so the very next run
    # reported `up-to-date`, skipped the bindgen and exited 0: a REAL red that a
    # plain re-run turned green, with the seam violation still in the tree
    # (the same class as `web:`
    # above). The stamp is written only on rc == 0, so a failed witness commits
    # nothing and the next run re-runs and fails again. Targets stay existence
    # checks, so a deleted staged tree still forces a rebuild, and the warm-tree
    # skip the gate exists for is unchanged.
    SOURCE_ARGS=()
    for TRIPLE in "${TRIPLES[@]}"; do
        SOURCE_ARGS+=(--source "$TARGET_DIR/$TRIPLE/$PROFILE_DIR/libfauna_ffi.so")
    done
    {{py}} scripts/build-if-stale.py --label "android-ffi-$BUILDTYPE-$PROFILE" \
        --stamp "$TARGET_DIR/android-ffi-bindgen-$BUILDTYPE-$PROFILE.stamp" \
        --target "$STAGE_JAVA/uniffi" \
        --target "$STAGE_JNI" \
        "${SOURCE_ARGS[@]}" \
        -- just _android-ffi-bindgen "$TARGET_DIR" "$STAGE_JAVA" "$STAGE_JNI" "$ABIS" "$PROFILE_DIR"

# Regenerate the Kotlin bindings + copy the 4 .so libs into jniLibs, into the
# calling flavor's buildType source set. Gated by the caller
# (`_android-ffi-flavor`) on .so freshness via build-if-stale — don't invoke
# directly (it assumes the 4 targets are built, current, and of the flavor whose
# source set it is being pointed at).
_android-ffi-bindgen target_dir stage_java stage_jni abis="all" profile_dir="release":
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="{{target_dir}}"
    STAGE_JAVA="{{stage_java}}"
    STAGE_JNI="{{stage_jni}}"
    ABIS="{{abis}}"
    PROFILE_DIR="{{profile_dir}}"
    # Wipe the previous generation first: uniffi-bindgen only ever WRITES files,
    # so a seam-carrying binding left from an earlier test-flavored run would
    # survive a production regen and keep compiling (and shipping). The two
    # generated trees are this recipe's exclusive output — hand-written app code
    # lives under com/fauna/app/, never com/fauna/ffi/ or uniffi/.
    rm -rf "$STAGE_JAVA/uniffi" "$STAGE_JAVA/com/fauna/ffi" "$STAGE_JNI"
    # Also wipe the OLD pre-093959cfb7 (2026-08-01) shared staging slot,
    # src/main/java/ — main compiles into every buildType, so a checkout that
    # ran the single-recipe shape before the flavor split still has bindings
    # sitting there, and they duplicate-declare against whichever flavor this
    # run just staged (found this session, 2026-08-02, as a module-wide
    # "Overload resolution ambiguity" wall with no relation to whatever the
    # run actually touched). No-op once a checkout has rebuilt past the split
    # once.
    if [ "$STAGE_JAVA" != "apps/fauna-android/app/src/main/java" ]; then
        rm -rf apps/fauna-android/app/src/main/java/uniffi apps/fauna-android/app/src/main/java/com/fauna/ffi
    fi
    # ...and the jniLibs half of that same pre-split slot, which the line above
    # missed for two weeks (found 2026-08-16 by the first store-safe APK scan).
    # It is strictly worse than the java half, because nothing downstream ever
    # complains: `src/main` contributes to EVERY build type, so a stale
    # `src/main/jniLibs/<abi>/libfauna_ffi.so` is packaged into debug, release
    # and storeSafe APKs alongside the correctly-staged flavored one, and the
    # duplicate is silent — no "duplicate declaration" wall like the java side,
    # just two libraries in the archive.
    #
    # Measured on this machine: four such `.so`s dated 2026-07-23, i.e. built by
    # the single pre-split recipe that passed `test-helpers` UNCONDITIONALLY.
    # The arm64 one exports 177 `_for_test` symbols — so every release APK built
    # on a checkout carrying this residue ships the automation surface
    # convention 15 exists to remove (e2e-conventions.md § point 15), and every
    # store-safe APK ships the payments plane it was built to excise. The files
    # are gitignored build residue, so this is per-checkout rather than
    # repo-wide, and a fresh clone never has it — which is exactly why it could
    # sit unnoticed. Unconditional `rm -rf`: it is never a legitimate input.
    rm -rf apps/fauna-android/app/src/main/jniLibs
    mkdir -p "$STAGE_JAVA"
    # Generate Kotlin bindings (any one target's .so works — all share the ABI).
    {{slot_build}} cargo run -p fauna-ffi --bin uniffi-bindgen generate \
        --library "$TARGET_DIR/aarch64-linux-android/$PROFILE_DIR/libfauna_ffi.so" \
        --language kotlin --out-dir "$STAGE_JAVA/"
    # Copy .so files to jniLibs — only the ABIs this run actually built. An
    # `arm64` run (the store-safe witness) must NOT copy the other three: they
    # are whatever flavor built them last, and staging a payments-carrying
    # `.so` beside an excised one would make the witness pass or fail on the
    # wrong artifact. `rm -rf "$STAGE_JNI"` above already cleared the slot, so
    # the missing ABIs are absent rather than stale.
    case "$ABIS" in
        all)   ABI_PAIRS=(aarch64-linux-android:arm64-v8a armv7-linux-androideabi:armeabi-v7a x86_64-linux-android:x86_64 i686-linux-android:x86) ;;
        arm64) ABI_PAIRS=(aarch64-linux-android:arm64-v8a) ;;
        *)     echo "ERROR: unknown abis '$ABIS' (want 'all' or 'arm64')" >&2; exit 1 ;;
    esac
    for PAIR in "${ABI_PAIRS[@]}"; do
        TRIPLE="${PAIR%%:*}"
        JNI_DIR="${PAIR##*:}"
        mkdir -p "$STAGE_JNI/$JNI_DIR"
        cp "$TARGET_DIR/$TRIPLE/$PROFILE_DIR/libfauna_ffi.so" "$STAGE_JNI/$JNI_DIR/"
        # Strip the STAGED copy only, never the cargo-target original (the
        # `--config profile.$PROFILE.strip=false` override above keeps that one
        # bindgen-readable for every future run — the bindgen call two lines up
        # already consumed it before this loop starts). `dist` is the only
        # profile this applies to: `release`/`foss`/`storeSafe`/`debug` ship
        # unstripped by design (dev-loop or not-yet-size-optimised artifacts).
        if [ "$PROFILE_DIR" = "dist" ]; then
            llvm-strip "$STAGE_JNI/$JNI_DIR/libfauna_ffi.so"
        fi
    done
    # Seam witness (testing.md § convention 15). Derived from the tree that was
    # just generated rather than a hard-coded seam list — a list rots silently
    # the moment a seam is added, which is the lesson `just wasm-seam-check`
    # (scripts/check-wasm-seam-exclusion.py) was built on. `*ForTest` is the
    # naming convention every `test-helpers` UniFFI export follows, so counting
    # it in the generated Kotlin is a faithful, self-maintaining proxy for "the
    # seams are in this flavor's face".
    # `|| true` is load-bearing: a zero-match grep exits 1, and under the
    # `set -o pipefail` above that would abort the PRODUCTION build at exactly
    # the moment it is proving itself clean.
    SEAMS="$( { grep -rl 'ForTest' "$STAGE_JAVA/uniffi" "$STAGE_JAVA/com/fauna/ffi" 2>/dev/null || true; } | wc -l )"
    case "$STAGE_JAVA" in
      */src/release/*|*/src/storeSafe/*|*/src/foss/*|*/src/kids/*)
        # `storeSafe` is a shipping flavor too (dynamic-features.md § The
        # App-Store escape hatch), so it holds convention 15's production bar,
        # not the test flavor's. Without this arm it would fall through to the
        # `*)` branch below and fail for having ZERO seams — the correct state
        # for the one artifact an App Store review actually receives.
        if [ "$SEAMS" -ne 0 ]; then
            echo "ERROR: shipping-flavored Android bindings export e2e seams (testing.md convention 15)." >&2
            echo "       $SEAMS generated file(s) under $STAGE_JAVA name a *ForTest method:" >&2
            grep -rl 'ForTest' "$STAGE_JAVA/uniffi" "$STAGE_JAVA/com/fauna/ffi" 2>/dev/null >&2
            echo "       'just android-ffi' / 'just android-ffi-store-safe' / 'just android-ffi-foss' / 'just android-ffi-kids' must build WITHOUT --features test-helpers." >&2
            exit 1
        fi
        echo "Android FFI (shipping flavor): 0 e2e seams in the generated bindings ✓" ;;
      *)
        if [ "$SEAMS" -eq 0 ]; then
            echo "ERROR: test-flavored Android bindings export NO e2e seams — the e2e" >&2
            echo "       TestAgent (src/debug) will not compile. Did 'test-helpers'" >&2
            echo "       drop off the android-ffi-test feature list?" >&2
            exit 1
        fi
        echo "Android FFI (test flavor): $SEAMS generated file(s) carry e2e seams ✓" ;;
    esac
    # Flavor-diff seam witness (e2e-conventions.md point 15, ratified
    # 2026-08-13) — the grep above sees only *ForTest-NAMED seams; this also
    # pins the non-test-named floor (setProviderBaseUrls, resolvedNestDialUrl,
    # callMachineMethod*, setDnsCreds, FfiChildAgentSpawner, …): each floor
    # seam must be declared in the test flavor's face (a renamed seam fails
    # there instead of leaving the witness watching a dead name) and absent
    # from production's (a dead cfg gate ships the seam into BOTH faces — that
    # is the regression the suffix grep cannot see). When the sibling flavor's
    # staged tree is also on disk — android's per-buildType staging keeps both
    # at once — the full difference set is additionally checked: every
    # test-only declaration must be test-named, floor-listed, or a member of a
    # floor type, so a NEW non-test-named seam fails until it states its
    # witness. Only the generated subtrees are passed — the debug java root
    # also holds the hand-written TestAgent, which is app code, not the
    # binding face this witness is about.
    GEN_ARGS=()
    for d in "$STAGE_JAVA/uniffi" "$STAGE_JAVA/com/fauna/ffi"; do
        [ -d "$d" ] && GEN_ARGS+=("$d")
    done
    # `storeSafe` is a SHIPPING flavor, so it is a production tree here exactly
    # as `release` is — it carries no `test-helpers` and must declare no seam.
    # Without its own arm it falls through to `*)` and is asserted to be the
    # TEST tree, which fails on every floor seam at once and reads as "the seams
    # vanished" rather than "this flavor never had them" (measured 2026-08-16,
    # the first store-safe run).
    #
    # ⚠ It also gets NO SIBLING, and that is the load-bearing half. The
    # difference-coverage check is only meaningful between two trees that differ
    # by `test-helpers` ALONE — it reads every test-only declaration as a seam
    # owing a witness. `storeSafe` vs `debug` differ by the gated-feature
    # REGISTRY too, so the diff is the whole `payments`/`zaps` plane
    # (`FfiPaymentsClient`, `providersSet`, `resolvePostTips`, the zap signer
    # face, …) reported as ~31 undeclared seams — a category error, and one that
    # would push a future session to "fix" it by listing product surfaces in
    # KNOWN_SEAMS. Single-tree production mode keeps the two checks that DO
    # apply (no floor seam, no `*ForTest` name) and drops the one whose premise
    # is false. The payments plane's own absence is witnessed by
    # `just android-store-safe-check`, which is the right tool for it.
    case "$STAGE_JAVA" in
      */src/release/*)   FLAVOR_FLAG=--production-tree; SIBLING="${STAGE_JAVA%/release/java}/debug/java"; SIBLING_REFRESH="just android-ffi-test" ;;
      */src/storeSafe/*) FLAVOR_FLAG=--production-tree; SIBLING="";                                       SIBLING_REFRESH="" ;;
      # `kids` is `storeSafe`'s case one flavor further: it differs from `debug`
      # by the kids-excised planes as well, so it takes no sibling either. Their
      # absence is the kids dex witness's to measure (the `android-kids-check`
      # recipe, which lands with the shell's source split).
      */src/kids/*)      FLAVOR_FLAG=--production-tree; SIBLING="";                                       SIBLING_REFRESH="" ;;
      # `foss` (installers/android.md § Goal) is `release` minus the Google Play
      # client libraries — a Gradle-side difference only; its `fauna-ffi` flavor
      # is exactly release's, so it differs from `debug` by `test-helpers`
      # ALONE and takes the same sibling as release.
      */src/foss/*)      FLAVOR_FLAG=--production-tree; SIBLING="${STAGE_JAVA%/foss/java}/debug/java";    SIBLING_REFRESH="just android-ffi-test" ;;
      *)                 FLAVOR_FLAG=--test-tree;       SIBLING="${STAGE_JAVA%/debug/java}/release/java"; SIBLING_REFRESH="just android-ffi" ;;
    esac
    ARGS=(--lang kotlin)
    for d in "${GEN_ARGS[@]}"; do ARGS+=("$FLAVOR_FLAG" "$d"); done
    SIBLING_ARGS=()
    SIBLING_TARGETS=()
    if [ -n "$SIBLING" ]; then
        for d in "$SIBLING/uniffi" "$SIBLING/com/fauna/ffi"; do
            [ -d "$d" ] && { SIBLING_ARGS+=("$d"); SIBLING_TARGETS+=(--target "$d"); }
        done
    fi
    # ⚠ The sibling tree is STAGED, not generated by this run, and nothing
    # otherwise relates the two trees in TIME. The difference-coverage assert
    # reads every declaration present in the test tree but not the production
    # one as a seam owing a witness — a conclusion that only holds if both trees
    # were generated from the SAME sources. Across a vintage gap it reports
    # history instead: symbols the newer tree has since gained, or (the measured
    # case) symbols the older tree still carries that the source has since
    # deleted.
    #
    # Measured 2026-08-16:
    # a `src/debug` tree staged 2026-08-13, diffed against a freshly built
    # `release` tree, reported `FfiCueVisibilityGates`,
    # `FfiConverterTypeFfiCueVisibilityGates` and `setIncludeEncryptedMetadata`
    # as "declarations present ONLY in the test-flavored tree … new seam(s) that
    # have not stated their witness". All three are PRODUCT symbols, retired
    # 2026-08-14 — one day after that tree was staged (engagement-cues.md,
    # mail-export.md). Refreshing the sibling turned it straight to "difference
    # fully accounted ✓".
    #
    # That misreport is worse than a false alarm, because the failure text names
    # its own remedy — "Add each to KNOWN_SEAMS … with a one-line reason" — so a
    # session that believes it permanently floor-lists PRODUCT surfaces, which is
    # exactly how a witness quietly stops witnessing. It is bidirectional, too: a
    # stale RELEASE sibling puts newly added product symbols on the test-only
    # side of the same subtraction.
    #
    # So: probe the sibling against the same sources that produce the bindings,
    # with the same gate the rest of this recipe uses, and SKIP the pair check
    # loudly when it predates them. Not "rebuild the sibling instead" — both
    # flavors share one cargo output path per triple (see the flavor-marker
    # comment in `_android-ffi-flavor`), so a guaranteed-fresh sibling means
    # rebuilding the other flavor's cargo targets on every single build, the
    # precise cost build-if-stale exists to avoid. The property the check exists
    # for is untouched: when the vintages agree it runs exactly as before, and a
    # genuinely unwitnessed new seam still fails. The per-tree checks (no floor
    # seam, no `*ForTest` name) never depended on the sibling and always run.
    if [ "${#SIBLING_ARGS[@]}" -gt 0 ] && ! {{py}} scripts/build-if-stale.py --check -q \
            --label android-ffi-sibling "${SIBLING_TARGETS[@]}" \
            --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
            --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*'; then
        echo "[seam-diff] SKIPPING the two-tree difference check: the sibling staged tree"
        echo "[seam-diff]   $SIBLING"
        echo "[seam-diff] is older than the current FFI sources, so a symbol-set difference"
        echo "[seam-diff] between the two trees would reflect their VINTAGE GAP, not the"
        echo "[seam-diff] test-helpers flavor split. Do NOT add anything it would have named"
        echo "[seam-diff] to KNOWN_SEAMS. Refresh the sibling and re-run to restore the check:"
        echo "[seam-diff]   $SIBLING_REFRESH"
        echo "[seam-diff] (The per-tree checks — no floor seam, no *ForTest name — still run.)"
        SIBLING_ARGS=()
    fi
    if [ "${#SIBLING_ARGS[@]}" -gt 0 ]; then
        case "$FLAVOR_FLAG" in
          --production-tree) SIBLING_FLAG=--test-tree ;;
          *)                 SIBLING_FLAG=--production-tree ;;
        esac
        for d in "${SIBLING_ARGS[@]}"; do ARGS+=("$SIBLING_FLAG" "$d"); done
    fi
    {{py}} scripts/check-ffi-seam-diff.py "${ARGS[@]}"
    echo "Android FFI built: $STAGE_JNI + Kotlin bindings in $STAGE_JAVA"

# Run the Android Robolectric unit-test suite for REAL (not just compile) on
# the primary dev VM. Routes around the Rosetta-AOT NDK block by regenerating
# bindings from a HOST-target libfauna_ffi.so — the app/build.gradle.kts
# testOptions wiring points JNA at $CARGO_TARGET_DIR/debug so FFI-touching
# tests initialize instead of dying at class-init. Not CI-gated; run by hand
# after android changes.
#
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). The body builds
# libfauna_ffi.so into the cargo-target dataset and then CONSUMES it from
# Gradle; between two separate acquisitions the checkout holds nothing, and
# pool-pressure-reclaim.sh evicted the dataset in exactly that gap on
# 2026-07-30 and twice more on 2026-08-20 (960+ s queued for the second slot),
# each time producing 30+ phantom Robolectric failures. The body's own
# {{slot_build}} lines stay: they are reentrant no-ops under this one
# (build-slot.py § FIFO ticket queue), so the inner recipes remain correct when
# run directly.
android-host-test *ARGS: i18n-generate providers-generate
    {{slot_build}} just _android-host-test-impl {{ARGS}}

# Shared staging step for both `android-host-test` and
# `android-unit-test-compile-check`: builds a HOST-target `fauna-ffi` (no NDK,
# so it routes around the Rosetta-AOT cross-compile block) and regenerates the
# gitignored Kotlin bindings into the debug source set. Factored out so the
# two recipes cannot drift the way `rebuild_conv_segment_records_from_disk`
# drifted from its mail twin — one staging step, two
# different gradle tasks consuming it.
_android-host-ffi-stage:
    #!/usr/bin/env bash
    set -euo pipefail
    # ONE build grant for the whole body (justfile `slot_build_body`): its
    # builds share a product that must not be evicted between them.
    {{slot_build_body}}
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps 2>/dev/null | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null || echo target)}"
    cargo build -p fauna-ffi --features test-helpers,file-provider-host
    # Debug-variant source set (src/main + src/debug) — the same slot
    # `android-ffi-test` fills. Test-flavored by construction: these bindings
    # export the `*ForTest` seams the debug fixtures/tests drive.
    #
    # A checkout from before the (2026-08-01) flavor split may
    # still carry bindings staged at the OLD shared slot, src/main/java/ —
    # main is compiled into every variant, so a leftover copy there duplicate-
    # declares against the debug/release copy and fails the whole module with
    # "Overload resolution ambiguity" across every file that calls an FFI
    # function, none of which were touched (found this session, 2026-08-02).
    # Both are gitignored, pure-generated, and never legitimately populated by
    # any current recipe (`_android-ffi-flavor` only ever writes src/release
    # or src/debug) — safe to wipe unconditionally.
    rm -rf apps/fauna-android/app/src/main/java/uniffi \
           apps/fauna-android/app/src/main/java/com/fauna/ffi \
           apps/fauna-android/app/src/debug/java/uniffi \
           apps/fauna-android/app/src/debug/java/com/fauna/ffi
    cargo run -p fauna-ffi --bin uniffi-bindgen generate \
        --library "$TARGET_DIR/debug/libfauna_ffi.so" \
        --language kotlin --out-dir apps/fauna-android/app/src/debug/java/

_android-host-test-impl *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-arm64
    export ANDROID_HOME="$HOME/android-sdk"
    just _android-host-ffi-stage
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android :app:testDebugUnitTest {{ARGS}}

# Compile-only gate for android's `src/test/` source set — the android sibling of `windows-cs-test-compile`
# (merge-gate-check.md § Merge-gate check (win)). `:app:compileDebugUnitTestKotlin`
# is a fraction of `testDebugUnitTest`'s cost (no Robolectric execution) and
# catches exactly the class this gate exists for: a UniFFI record field-add
# breaking the Kotlin construction sites in `src/test/` silently, since
# nothing else on any merge path compiles that source set
# (`android-store-safe-check` builds only `src/main + src/release`).
# Deliberately compile-only, not `testDebugUnitTest`: when this gate was
# armed android's test suite carried pre-existing unrelated failures (the
# `peer-wg-key-copy-btn` fixtures, a WireGuard element already removed from
# ui.yaml), so a test-RUN gate would have been red on arrival — the exact trap
# `share-pump-check`'s deferred arming avoids. Those fixtures (the suite's
# only deterministic reds, 1747/1750 green) were dropped 2026-09-29; a run
# on a loaded box still flakes on Robolectric `AppNotIdleException`, so
# promoting this gate to a test run is open.
android-unit-test-compile-check:
    {{slot_build}} just _android-unit-test-compile-check-impl

_android-unit-test-compile-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-arm64
    export ANDROID_HOME="$HOME/android-sdk"
    just _android-host-ffi-stage
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android :app:compileDebugUnitTestKotlin

# Build fauna-ffi for the Apple targets something can LINK, and generate Swift
# bindings. Three slices by default — `aarch64-apple-{ios, ios-sim, darwin}`.
#
# THE WATCH SLICES ARE OPT-IN (2026-08-22). Until then this recipe also built
# `aarch64-apple-watchos{,-sim}` into every checkout's xcframework, and NOTHING
# could link them: the watchOS app has no build path at all — no `just` recipe,
# no Xcode target, no scheme. Two release compiles per
# checkout, on every cache-key bust, for an artifact with zero consumers. `just
# apple-ffi-watch` is the explicit 5-slice invocation; the slice shape is part of
# the cache key and the `.ffi-flavor` marker below, so a 3-slice artifact can
# never be served to a 5-slice request (the anti-pattern is `apple-ffi-host`
# silently dropping the iOS slices — same one staging slot, same class of trap).
# Compile coverage for the watch triples does NOT ride on this recipe any more:
# it is a `cargo check` line in `apple-swift-build-check`, i.e. in the mac
# merge-gate check on every tip, which is strictly tighter than the "someone
# eventually runs a full apple-ffi" net it replaces (that net took two days to
# notice the 2026-08-12 `secret-service`→`zbus` break).
# Design: docs/goal/architecture/build-machine-resources.md § "apple-ffi slice
# set — the watch slices are opt-in".
#
# Cross-checkout prebuilt-FFI cache: the consumed artifact (FaunaFFI.xcframework
# + the generated Swift bindings) is a pure function of the FFI inputs — `libs/`
# (every fauna-ffi path-dep lives there, incl. the tracked generated `strings.rs`
# / `providers_generated.rs`), `Cargo.lock` (external dep versions),
# `rust-toolchain.toml`, and this recipe's build flags/targets/deployment-mins
# (hashed via `just --show`, so a flag change auto-busts the key without keying on
# the whole 80+-edits/quarter justfile). Apple checkouts rebased onto the same
# `main` share identical inputs, so the FIRST to build a given input-set populates
# a machine-local cache and every other checkout copies the prebuilt artifact in
# and skips cargo entirely. This is the cross-checkout extension of the
# within-checkout build-if-stale gate below: macOS has no shared CARGO_TARGET_DIR
# (each checkout's `target/` is cold), so
# without this every macOS session rebuilds the full fauna-ffi dep tree once per
# slice, in release. Cache dir: $FAUNA_APPLE_FFI_CACHE (default ~/.cache/fauna-apple-ffi);
# set FAUNA_APPLE_FFI_CACHE=off to disable. Skipped automatically when libs/,
# Cargo.lock, or rust-toolchain.toml have uncommitted changes (a dirty tree can't
# be content-addressed by HEAD). Design: docs/goal/architecture/build-system.md
# § "Prebuilt-FFI cache (mac)".
# `profile` defaults to "release" (dev-loop/CI/device speed, unchanged); pass
# "dist" for the size-optimised shipping variant (installers/windows.md §
# Size & build profile is the prior-art pattern; installers/macos.md row 319).
apple-ffi profile="release": i18n-generate providers-generate
    @just _apple-ffi-flavor "" "" "{{profile}}"

# The 5-slice opt-in: everything `apple-ffi` builds PLUS the two watchOS slices
# (`aarch64-apple-watchos{,-sim}`, built `--no-default-features` — see
# `_apple-ffi-flavor`). Nothing on any `just` path consumes them today; this
# recipe exists so the watchOS-app owner has one, and so a watch xcframework can
# be produced deliberately rather than as a side effect every checkout pays for.
#
# Revisit-when: the watchOS app gets a build recipe — that session folds these
# slices into whatever builds it, and the default/opt-in split may collapse back.
apple-ffi-watch profile="release": i18n-generate providers-generate
    @just _apple-ffi-flavor "" watch "{{profile}}"

# Test-flavoured full FFI: adds `test-helpers`, which compiles the E2E injection
# seams (`OnboardingMachine::set_*_for_test` / `call_machine_method`,
# `ConversationsManager::install_mock_backends_for_test` / `inject_inbound_for_test`,
# `LaunchMachine::set_phase_for_test`, `FfiChildAgentSpawner`, …) and exports them
# over UniFFI. This is what the iOS e2e path builds (conftest routes ios here) —
# the `#if DEBUG` FaunaKit TestAgent that calls the seams is compiled into the
# simulator build it drives. `just apple-ffi` (production) is what device/CI
# builds and anything shippable takes.
apple-ffi-test profile="release": i18n-generate providers-generate
    @just _apple-ffi-flavor "test-helpers" "" "{{profile}}"

# Store-safe full FFI — the App-Store escape hatch's apple xcframework flavor
# (`dynamic-features.md` § The App-Store escape hatch; the row this recipe
# discharges is that section's "iOS/macOS xcframework flavor" line). Built
# `--no-default-features --features store-safe`, so the artifact provably cannot
# reach a real-money flow: no `FfiPaymentsClient`, no `fauna.payments.*` /
# `fauna.tips.*` kind strings, no zap face, and no runtime path back.
#
# The complement feature is the WHOLE spelling — never a hand-listed set — so a
# new client surface is added to `fauna-ffi`'s `store-safe` once and is in
# `default` by construction (§ The cargo feature spine). `file-provider-host`
# rides this flavor exactly as it rides the other two: it is an apple product
# capability, not a registry member.
#
# This is the DEVICE artifact (what an iOS archive links; 3 slices — the watch
# pair is `apple-ffi-watch`, see `apple-ffi`). The macOS
# app's own store-safe build takes the 1-slice host twin —
# `apple-ffi-host-store-safe`, via `just apple-store-safe-check`.
apple-ffi-store-safe profile="release": i18n-generate providers-generate
    @just _apple-ffi-flavor "store-safe" "" "{{profile}}"

# Shared implementation of the two full-slice flavors above.
#
# Convention: testing.md § Cross-app e2e conventions point 15 — the automation
# surface is compiled out of release artifacts. Rule (b) there keys any UniFFI
# export of a seam on the FEATURE ALONE, never the profile, so "production
# flavor" is exactly "don't pass test-helpers" and the generated Swift face is a
# pure function of `features` below. `file-provider-host` is orthogonal and
# rides BOTH flavors (it is a product capability, not a seam).
#
# Like windows — and unlike android, which has Gradle buildType source sets —
# both flavors write ONE staging slot (Package.swift's binaryTarget path is
# fixed), so the `.ffi-flavor` marker is the whole gate rather than a
# convenience. See `_windows-ffi-flavor` for the same shape.
_apple-ffi-flavor features watch="" profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    # Fail fast on a dead display rather than build for twenty minutes and then
    # hang in `xcodebuild` (`_apple-ffi-bindgen` has the why).
    {{gpu_check}}
    # "" (production) | "test-helpers" (e2e) | "store-safe" (the App-Store escape
    # hatch). Composed with the always-on `file-provider-host` in
    # `_apple-ffi-build-impl`; keeping the PARAMETER empty for production is what
    # makes the production cargo lines literally free of the string a reader (and
    # the tier_1 recipe-shape test) greps for.
    FEATURES="{{features}}"
    # {{profile}} is a cargo PROFILE NAME (`release` | `dist`), not an output
    # directory — mirrors `_android-ffi-flavor`/`_windows-ffi-flavor`. Default
    # stays `release` (dev-loop/CI/device speed, unchanged); shipping recipes
    # pass `dist` (installers/windows.md § Size & build profile).
    PROFILE="{{profile}}"
    # --- Slice SHAPE: the second axis over the same one staging slot ------------
    # Default is the three triples something can actually LINK. The two watchOS
    # slices are opt-in (`just apple-ffi-watch`) because no watchOS app build path
    # exists — no `just` recipe, no Xcode target, no scheme — so they were two release compiles per checkout for zero consumers.
    # SLICE_TRIPLES itself is (re)computed in `_apple-ffi-build-impl` from WATCH —
    # this half only needs SHAPE, for the cache key and the flavor marker.
    WATCH="{{watch}}"
    if [ -n "$WATCH" ]; then
        SHAPE=5slice
    else
        SHAPE=3slice
    fi
    # The `.ffi-flavor` marker records FLAVOR:FEATURES (see the guard in
    # `_apple-ffi-build-impl`), and the slice shape belongs in the FLAVOR half: it
    # is a property of the staged artifact, not of the feature set. Mirrors
    # `_apple-ffi-host-flavor`'s host|host-release.
    FLAVOR="full-$SHAPE"
    # PROFILE joins the FLAVOR half too (never FEATURES — a cargo profile is not
    # a feature set), same reasoning as the shape axis just above: `release` and
    # `dist` write DIFFERENT per-triple dirs (`_apple-ffi-build-impl`), so they
    # never clobber each other's `.a`s directly, but the ONE shared xcframework
    # staging slot and `.ffi-flavor` marker would otherwise read a `dist` build as
    # "fresh" after a `release` one (or vice versa) and serve the wrong artifact.
    # Suffixed rather than folded into the `FLAVOR="full-$SHAPE"` assignment
    # above so that literal keeps satisfying
    # `test_the_staged_artifacts_flavor_marker_records_the_slice_shape`'s pin
    # verbatim; `release` is elided (byte-identical to every FLAVOR value before
    # this axis existed) so only a non-default profile changes the marker.
    if [ "$PROFILE" != "release" ]; then
        FLAVOR="$FLAVOR-$PROFILE"
    fi

    # --- Cross-checkout prebuilt-FFI cache: lookup ------------------------------
    # Deliberately UNSLOTTED: a warm tree must never
    # queue for a build slot just to discover it has nothing to build
    # (build-machine-resources.md § Build/e2e slot locks). The ONE lease below
    # covers only the phases that actually produce and consume a build product —
    # see `_apple-ffi-build-impl`.
    #
    # CACHE_EPOCH guards the cached *artifact layout* only (what we stage/restore
    # below); build-flag changes are caught by the `just --show` hash, not this.
    CACHE_EPOCH=1
    CACHE_ROOT="${FAUNA_APPLE_FFI_CACHE:-$HOME/.cache/fauna-apple-ffi}"
    CACHE_KEY=""
    if [ "$CACHE_ROOT" != "off" ] && \
       [ -z "$(git status --porcelain -- libs Cargo.lock rust-toolchain.toml .cargo/config.toml)" ] && \
       mkdir -p "$CACHE_ROOT" 2>/dev/null; then
        # LRU-ish GC: drop entries (and crashed stagings) unused >2d — the mtime
        # is bumped on every hit/populate, so live input-sets survive. Shared
        # with the mac SessionStart hook (scripts/apple-ffi-cache-session-startup.sh);
        # scripts/apple-ffi-cache-gc.sh is the one place this TTL policy lives.
        scripts/apple-ffi-cache-gc.sh 2>/dev/null || true
        # `features=` is part of the key, and load-bearing: the two flavors build
        # the SAME target dir and stage the SAME xcframework path, so a
        # feature-blind key would serve a seam-carrying artifact to a production
        # build (and vice versa). `shape=` is the same argument for the slice set
        # (2026-08-22): a 3-slice and a 5-slice xcframework are the same path with
        # the same features, so without it an `apple-ffi-watch` would take the
        # 3-slice cache entry as a HIT and silently hand back an artifact with no
        # watch slices — the `apple-ffi-host` clobber trap, one axis over. It is
        # NOT covered by the `just --show` hash below: the shape is a recipe
        # PARAMETER, so the recipe text is identical for both shapes.
        # Hashing `_apple-ffi-build-impl` (2026-08-25), not
        # `_apple-ffi-flavor`: the slot-lease split moved every build-flag-bearing
        # line into the impl, so hashing THIS recipe's now build-irrelevant
        # cache-lookup text would stop catching a build-flag change entirely.
        CACHE_KEY="$(
            printf '%s\n' \
                "epoch=$CACHE_EPOCH" \
                "features=$FEATURES" \
                "shape=$SHAPE" \
                "profile=$PROFILE" \
                "$(git rev-parse HEAD:libs HEAD:Cargo.lock HEAD:rust-toolchain.toml HEAD:.cargo/config.toml)" \
                "$(just --show _apple-ffi-build-impl)" \
                "$(just --show _apple-ffi-bindgen)" \
            | shasum -a 256 | cut -d' ' -f1
        )"
        HIT="$CACHE_ROOT/$CACHE_KEY"
        STAMP="apps/fauna-apple/.ffi-cache.stamp"
        # Fast path: this checkout already holds artifacts for this exact key, so
        # do nothing — no copy at all. The cross-checkout restore below is an
        # ~830 MB cp, and swift-test/mac-debug invoke apple-ffi every time, so
        # repeat invocations must not pay it. (Replaces the old build-if-stale
        # ~2.6 s no-op with a cat+test.) Survives `cargo clean` (artifacts live
        # under apps/, the stamp alongside them).
        if [ -d apps/fauna-apple/FaunaFFI.xcframework ] && [ -f "$STAMP" ] && \
           [ "$(cat "$STAMP" 2>/dev/null)" = "$CACHE_KEY" ]; then
            # Stamp present+matching ⟹ a prior full build/restore of THIS flavor
            # (the key includes `features=`); `apple-ffi-host` clears the stamp,
            # so this never fast-paths against a host artifact.
            echo "$FLAVOR:$FEATURES" > apps/fauna-apple/.ffi-flavor
            echo "apple-ffi: artifacts already current ($CACHE_KEY) — nothing to do"
            exit 0
        fi
        # Restore in a subshell; on any failure fall through to a real build
        # rather than exit 0 with half-restored artifacts. FFICompat.swift is
        # hand-written/tracked — the *.swift copy never names it, so it stands.
        if [ -d "$HIT/FaunaFFI.xcframework" ] && (
                set -e
                rm -rf apps/fauna-apple/FaunaFFI.xcframework
                cp -R "$HIT/FaunaFFI.xcframework" apps/fauna-apple/FaunaFFI.xcframework
                rm -rf apps/fauna-apple/generated
                cp -R "$HIT/generated" apps/fauna-apple/generated
                cp apps/fauna-apple/generated/*.swift apps/fauna-apple/FaunaFFISwift/Sources/
            ); then
            echo "$CACHE_KEY" > "$STAMP"
            echo "$FLAVOR:$FEATURES" > apps/fauna-apple/.ffi-flavor
            touch "$HIT" 2>/dev/null || true
            echo "apple-ffi: cache HIT ($CACHE_KEY) — restored prebuilt FFI, skipped cargo"
            exit 0
        fi
        # Clean miss, or a corrupt entry that failed to restore: drop any partial
        # entry so the rebuild below repopulates this key cleanly (else the
        # populate race-guard would keep discarding fresh artifacts forever).
        rm -rf "$HIT" 2>/dev/null || true
        echo "apple-ffi: cache MISS ($CACHE_KEY) — building (will populate cache)"
    else
        echo "apple-ffi: cache disabled or working tree dirty (libs/Cargo.lock/toolchain) — building without cache"
    fi

    # ONE slot for the whole build (build-machine-resources.md § Build/e2e slot
    # locks → the slot is the recipe's dataset lease: the
    # xcframework assembly consumes the dataset staticlibs the slice builds
    # produce, so a separate acquisition per phase left the freshly built .a
    # files evictable in the gap between them. `_apple-ffi-build-impl`'s own
    # {{slot_build}} lines stay: under this one they are reentrant no-ops
    # (build-slot.py § FIFO ticket queue). Passed via env, not `just` params — a
    # parametrized delegate call defeats the tier_1 tests' generic one-line-
    # delegation detection (`_recipe_body`), which requires the call to be bare.
    export FEATURES WATCH CACHE_KEY CACHE_ROOT FLAVOR PROFILE
    {{slot_build}} just _apple-ffi-build-impl

# The slotted build phase split out of `_apple-ffi-flavor` (2026-08-25):
# the cache lookup above stays unslotted (a warm tree must never queue) while
# every phase that actually produces and consumes a build product — the slice
# builds, the xcframework assembly, the cache populate — shares ONE lease.
# Reads FEATURES/WATCH/CACHE_KEY/CACHE_ROOT/FLAVOR/PROFILE from the environment
# (see the export above); never invoked directly.
_apple-ffi-build-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Explicit-target artifact build: resolve features per-invocation, never
    # workspace-unified (.cargo/config.toml § feature unification — a unified
    # store-safe build re-admits payments/zaps on shared deps, measured 2026-08-22).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    # FEATURES uses the plain (no `:`) unset-only form, unlike CACHE_ROOT/FLAVOR
    # below: "" is FEATURES' legitimate production value, and bash's `${:?}`
    # treats empty the same as unset — `set -u` alone still catches a genuinely
    # missing export.
    FEATURES="${FEATURES?}"
    WATCH="${WATCH:-}"
    CACHE_KEY="${CACHE_KEY:-}"
    CACHE_ROOT="${CACHE_ROOT:?}"
    FLAVOR="${FLAVOR:?}"
    PROFILE="${PROFILE:?}"
    # `store-safe` is a THIRD axis, not a feature to add: it is the complement
    # feature, and the flavor is `--no-default-features --features store-safe`
    # (`dynamic-features.md` § The cargo feature spine — one list, never a
    # hand-spelled excision set). It rides the same one staging slot as the other
    # two, so the `.ffi-flavor` marker below (FLAVOR:FEATURES) is what keeps a
    # payments-carrying xcframework from being served to a store-safe app build.
    if [ "$FEATURES" = "store-safe" ]; then
        FEATURE_ARGS=(--no-default-features --features "store-safe,file-provider-host")
    elif [ -n "$FEATURES" ]; then
        FEATURE_ARGS=(--features "$FEATURES,file-provider-host")
    else
        FEATURE_ARGS=(--features "file-provider-host")
    fi
    # SHAPE/SLICE_TRIPLES: recomputed from WATCH rather than passed through
    # (bash arrays do not survive `export`) — `_apple-ffi-flavor` above derives
    # the same SHAPE from the same WATCH for the cache key, so this is one cheap
    # deterministic if/else duplicated, not a second hand-kept slice list.
    #
    # ⚠ SLICE_TRIPLES is the SINGLE home of the slice set: the cargo lines, the
    # build-if-stale `--source` list and `_apple-ffi-bindgen`'s slice arguments
    # are all derived from it below. A second hand-listed copy is exactly how the
    # windows bindgen gate went non-stamp — one list, always.
    if [ -n "$WATCH" ]; then
        SHAPE=5slice
        SLICE_TRIPLES=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-watchos aarch64-apple-watchos-sim aarch64-apple-darwin)
    else
        SHAPE=3slice
        SLICE_TRIPLES=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)
    fi
    # Deployment targets must match apps/fauna-apple/Package.swift's
    # `platforms:` mins, otherwise zstd-sys / ring / sqlite cc-rs builds use
    # the SDK default (e.g. iOS 26.4 on Xcode 26.4) while cargo's link step
    # uses Rust's default (iOS 10.0), and the linker fails on
    # `___chkstk_darwin` (a clang_rt builtin the older runtime doesn't have).
    export IPHONEOS_DEPLOYMENT_TARGET=17.0
    export MACOSX_DEPLOYMENT_TARGET=15.0
    export WATCHOS_DEPLOYMENT_TARGET=10.0

    # Flavor guard: `just apple-ffi-host` may have left a host-debug (1-slice)
    # xcframework at this shared path, or the OTHER feature flavor may have left a
    # full one. Drop it so build-if-stale rebuilds this flavor's artifact rather
    # than treating the staged one as current — a freshly built xcframework can be
    # newer than this flavor's .a sources, so build-if-stale would otherwise skip
    # and serve the wrong artifact.
    #
    # The marker records FLAVOR:FEATURES, not just the flavor (2026-08-02,
    # convention-15 recipe split): the feature set is a second axis over the SAME
    # staging slot, and it is the security-relevant one — a flavor-only marker
    # reads "fresh" across a production↔test switch and leaves the seam-carrying
    # xcframework staged for a `mac-release`. Same class as android's cargo-stamp
    # trap and windows' `.ffi-flavor` widening, one level up.
    #
    # Since 2026-08-22 the FLAVOR half carries the SLICE SHAPE too (full-3slice |
    # full-5slice), which is what makes a 3-slice artifact unable to serve an
    # `apple-ffi-watch`. Announce the mismatch rather than wiping in silence: the
    # row that asked for this split named "an explicit watch request finding a
    # 3-slice cache must rebuild, LOUDLY" as its contract, and a silent `rm -rf`
    # of a just-built xcframework is the confusing half of the `apple-ffi-host`
    # clobber gotcha it is modelled on.
    STAGED_FLAVOR="$(cat apps/fauna-apple/.ffi-flavor 2>/dev/null || true)"
    if [ "$STAGED_FLAVOR" != "$FLAVOR:$FEATURES" ]; then
        if [ -n "$STAGED_FLAVOR" ] && [ -d apps/fauna-apple/FaunaFFI.xcframework ]; then
            echo "apple-ffi: staged artifact is '$STAGED_FLAVOR', this build wants '$FLAVOR:$FEATURES' — discarding and rebuilding"
        fi
        rm -rf apps/fauna-apple/FaunaFFI.xcframework
    fi

    # iOS targets. `test-helpers` (TEST FLAVOR ONLY) exposes the OnboardingMachine
    # snapshot setters via UniFFI for the E2E `call_machine_method` bridge. Until
    # 2026-08-02 there was only ONE recipe and it passed the feature unconditionally,
    # so all 23 seams were exported by the xcframework `mac-release` / `mac-app` /
    # `mac-dmg` and every iOS device build linked. `file-provider-host` is default-OFF
    # (Cargo.toml's feature comment has the full rationale) and rides BOTH flavors —
    # FaunaKit's FileProviderHost.swift already calls FfiFileProviderHost, so every
    # apple slice needs it explicitly (it's default-OFF specifically so
    # windows doesn't compile it).
    {{slot_build}} cargo build --locked -p fauna-ffi --profile "$PROFILE" "${FEATURE_ARGS[@]}" --target aarch64-apple-ios
    {{slot_build}} cargo build --locked -p fauna-ffi --profile "$PROFILE" "${FEATURE_ARGS[@]}" --target aarch64-apple-ios-sim
    # watchOS targets — OPT-IN ONLY (`just apple-ffi-watch`; see the SHAPE block
    # above).
    # Build STATICLIB-ONLY via `cargo rustc --crate-type`: the xcframework below
    # consumes only the `.a` (every `--source …/libfauna_ffi.a`), never the cdylib.
    # watchos-DEVICE already yields just the `.a` (cargo auto-drops the unsupported
    # cdylib), but watchos-SIM supports cdylib, so a plain `cargo build` links
    # `libfauna_ffi.dylib` and fails `ld: framework 'SystemConfiguration' not found`:
    # hickory-resolver's default `system-config` feature pulls the macOS-only
    # `system-configuration` crate under `cfg(target_vendor = "apple")`, which wrongly
    # includes watchOS (SystemConfiguration.framework doesn't exist there). Emitting
    # only the consumed `.a` skips the cdylib link on both slices (kept symmetric) and
    # the dead uniffi-bindgen bin build. See build-system.md § apple-ffi watchOS slice.
    if [ -n "$WATCH" ]; then
        {{slot_build}} cargo rustc --locked -p fauna-ffi --lib --crate-type staticlib --no-default-features --profile "$PROFILE" --target aarch64-apple-watchos
        {{slot_build}} cargo rustc --locked -p fauna-ffi --lib --crate-type staticlib --no-default-features --profile "$PROFILE" --target aarch64-apple-watchos-sim
    fi
    # macOS target
    {{slot_build}} cargo build --locked -p fauna-ffi --profile "$PROFILE" "${FEATURE_ARGS[@]}" --target aarch64-apple-darwin
    # cargo is incremental: the slice builds above no-op when fresh and relink
    # (bump each .a's mtime) only on an actual source/dep change. So the .a slices
    # are the precise staleness signal — regenerate the bindings + xcframework iff
    # a slice changed, else skip the expensive uniffi-bindgen + xcodebuild. This is
    # what closes the stale-binding ABI trap:
    # ANY libs/fauna-ffi or transitive-dep change reaches cargo → a fresh .a →
    # a regenerated binding+xcframework, so swift-test/mac-debug never compile
    # against bindings that drifted from the linked .a. Keying on the .a outputs
    # (not a libs/fauna-ffi/src glob) is what catches transitive-dep changes.
    #
    # Both lists below are DERIVED from SLICE_TRIPLES, never re-listed: a
    # hand-kept second copy silently drifts from the shape actually built, and a
    # `--source` naming a slice this shape never builds would make build-if-stale
    # rebuild forever (missing source) or — worse, the other way — a `--source`
    # list missing a slice would let a stale binding through. The `.ffi-flavor`
    # guard above is what stops the OTHER staleness hazard here: a 3-slice
    # xcframework is newer than the 5-slice run's sources, so without the wipe
    # build-if-stale would read "fresh" and skip the bindgen entirely.
    SLICE_SOURCE_ARGS=()
    SLICE_LIBS=()
    for triple in "${SLICE_TRIPLES[@]}"; do
        SLICE_SOURCE_ARGS+=(--source "target/$triple/$PROFILE/libfauna_ffi.a")
        SLICE_LIBS+=("target/$triple/$PROFILE/libfauna_ffi.a")
    done
    # --stamp, not the staged xcframework, carries freshness — and here that is
    # a CORRECTNESS fix, not the usual cargo-no-op one. `_apple-ffi-bindgen`
    # generates the bindings FIRST and runs the seam witness LAST, so a witness
    # failure leaves both targets already written and newer than the .a
    # sources. In non-stamp mode the target IS the freshness signal, so the
    # very next run would report `up-to-date`, skip the bindgen and exit 0: a
    # REAL red a plain re-run turns green, with the seam violation still in
    # the tree (the same class android hit and fixed,
    # `git log --grep "a failed bindgen goes green on a re-run"`). The stamp
    # is written only on rc == 0, so a failed witness commits nothing and the
    # next run re-runs and fails again; `--target` entries stay existence
    # checks, so a deleted staged tree still forces a rebuild.
    #
    # ⚠ NOT android's shape: these two apple gates (this one and
    # `apple-ffi-host` below) share ONE target pair across every flavor and
    # distinguish runs only by `--label`, which build-if-stale never reads for
    # freshness — a stamp keyed on less than the label's own discriminators
    # would trade this false-green class for a flavor-switch one instead of
    # fixing it (production↔test, or 3slice↔5slice, silently serving the
    # other flavor's stamp). So the stamp path carries every discriminator the
    # label does: $SHAPE and $FEATURES both.
    {{py}} scripts/build-if-stale.py --label "apple-ffi-$SHAPE-$PROFILE-${FEATURES:-production}" \
        --stamp "target/apple-ffi-bindgen-$SHAPE-$PROFILE-${FEATURES:-production}.stamp" \
        --target apps/fauna-apple/FaunaFFI.xcframework \
        --target apps/fauna-apple/FaunaFFISwift/Sources/FaunaFFI.swift \
        "${SLICE_SOURCE_ARGS[@]}" \
        -- just _apple-ffi-bindgen "$FEATURES" \
            target/aarch64-apple-ios/$PROFILE/libfauna_ffi.a \
            "${SLICE_LIBS[@]}"

    # --- Cross-checkout prebuilt-FFI cache: populate ----------------------------
    # Non-fatal: a cache I/O error must never fail the build. Stage under a
    # pid-unique dir then publish with an atomic rename; if another checkout won
    # the race for this key, discard ours.
    if [ -n "$CACHE_KEY" ]; then
        (
            set -e
            STAGE="$CACHE_ROOT/.staging-$CACHE_KEY-$$"
            rm -rf "$STAGE"
            mkdir -p "$STAGE"
            cp -R apps/fauna-apple/FaunaFFI.xcframework "$STAGE/FaunaFFI.xcframework"
            cp -R apps/fauna-apple/generated "$STAGE/generated"
            if [ -d "$CACHE_ROOT/$CACHE_KEY" ]; then
                rm -rf "$STAGE"
            elif ! mv "$STAGE" "$CACHE_ROOT/$CACHE_KEY" 2>/dev/null; then
                rm -rf "$STAGE"
            fi
            echo "apple-ffi: populated cache $CACHE_ROOT/$CACHE_KEY"
        ) || echo "apple-ffi: cache populate failed (non-fatal) — artifacts are still built locally"
        # Stamp the checkout so the next invocation takes the fast path and skips
        # the re-copy — written whether or not the shared populate won its race,
        # since the local artifacts match this key regardless.
        echo "$CACHE_KEY" > apps/fauna-apple/.ffi-cache.stamp
    fi
    # Record the on-disk artifact flavor for the host↔full and production↔test
    # guards above / in apple-ffi-host. Written only after a successful
    # build+stage, so a crashed run leaves the marker on the PREVIOUS flavor and
    # the next invocation still forces a rebuild rather than trusting a
    # half-built tree (android's rule, same reason).
    echo "$FLAVOR:$FEATURES" > apps/fauna-apple/.ffi-flavor

# Host-only FFI — builds ONLY the aarch64-apple-darwin slice, in the requested
# config (`apple-ffi-host config="debug"`; pass "release" for the shippable app),
# and assembles a 1-slice xcframework. Every macOS-only consumer links only the
# darwin slice, so the iOS slices `apple-ffi` builds are dead weight:
# `mac-debug`/`swift-test` take this in DEBUG; `mac-release`/`mac-app`/`mac-dmg` +
# the installer build.sh take it in RELEASE. ~3× faster cold than `apple-ffi`
# (1 slice vs 3), and it wins the case the cross-checkout cache structurally cannot:
# a dirty libs/ tree (the cache is consulted only on a clean libs/ — see `apple-ffi`),
# exactly when you're iterating on shared Rust. The full multi-slice RELEASE xcframework
# stays on `apple-ffi` for device+simulator builds / iOS e2e (conftest routes ios →
# `apple-ffi`) / CI. No cross-checkout cache here. Shares the one FaunaFFI.xcframework
# path with `apple-ffi` (Package.swift's binaryTarget is fixed), so a gitignored
# `.ffi-flavor` marker (host | host-release | full-3slice | full-5slice) guards the
# switch — alternating
# only re-runs the cheap bindgen + xcframework assembly (the debug/, release/, and
# multi-slice .a sets coexist, never recompiled). The shippable macOS binary is
# byte-identical whether built here (release) or via full `apple-ffi` — both link
# only the release darwin slice. Design: docs/goal/architecture/build-system.md
# § "Host-only debug FFI (mac dev loop)".
apple-ffi-host config="debug" profile="": i18n-generate providers-generate
    @just _apple-ffi-host-flavor "{{config}}" "" "{{profile}}"

# Test-flavoured host-only FFI — the `apple-ffi-host` twin carrying
# `test-helpers`. `swift-test` / `mac-debug` take this (they build Swift in
# DEBUG, the configuration whose `#if DEBUG` FaunaKit TestAgent, in-process
# automation server and `ConversationsTestInject` are the seams' only callers);
# `mac-release` / `mac-app` / `mac-dmg` + the installer build.sh take the
# PRODUCTION `apple-ffi-host release`. Both axes stay independent — a
# release-profile e2e build (should apple ever need windows' `FAUNA_E2E_AGENT`
# escape hatch) is `apple-ffi-host-test release`.
apple-ffi-host-test config="debug" profile="": i18n-generate providers-generate
    @just _apple-ffi-host-flavor "{{config}}" "test-helpers" "{{profile}}"

# Store-safe host-only FFI — the 1-slice twin of `apple-ffi-store-safe`, and what
# `just apple-store-safe-check` links. FaunaMacOS links
# only the darwin slice, so the full-slice recipe would compile the dead ones; the
# device/archive path is the multi-slice one.
apple-ffi-host-store-safe config="debug" profile="": i18n-generate providers-generate
    @just _apple-ffi-host-flavor "{{config}}" "store-safe" "{{profile}}"

# Shared implementation of the two host-only flavors above. Same feature
# semantics as `_apple-ffi-flavor` (testing.md § convention 15).
#
# `profile` decouples the CARGO profile from `config` (Xcode's build
# CONFIGURATION — `debug`/`release`, unrelated to cargo and never "dist": Xcode
# has no such configuration/scheme). Default "" means "derive from config"
# (debug -> dev, release -> release), so every pre-existing bare call
# (`apple-ffi-host release`, `apple-ffi-host-test debug`, …) is unchanged.
# `dist` (installers/windows.md § Size & build profile; installers/android.md
# § Implementation status today) is a shipping-only override of the `release`
# config — passing it under `config=debug` is refused, mirroring android's
# `_android-ffi-flavor`.
_apple-ffi-host-flavor config features profile="":
    #!/usr/bin/env bash
    set -euo pipefail
    # Fail fast on a dead display rather than build and then hang in `xcodebuild`
    # (`_apple-ffi-bindgen` has the why).
    {{gpu_check}}
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification). Still
    # required here even though this is no longer an explicit-`--target` build —
    # the guard is about artifact SHAPE (a unified store-safe slice re-admits
    # payments/zaps on shared deps), not about the triple. See the ARTIFACTS
    # block below for why the `--target` went away.
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    CONFIG="{{config}}"
    # "" (production) | "test-helpers" (e2e) | "store-safe" (the escape hatch) —
    # same three-way axis as `_apple-ffi-flavor`, see its comment for why
    # store-safe is `--no-default-features --features store-safe` rather than a
    # feature to add.
    FEATURES="{{features}}"
    if [ "$FEATURES" = "store-safe" ]; then
        FEATURE_ARGS=(--no-default-features --features "store-safe,file-provider-host")
    elif [ -n "$FEATURES" ]; then
        FEATURE_ARGS=(--features "$FEATURES,file-provider-host")
    else
        FEATURE_ARGS=(--features "file-provider-host")
    fi
    # {{profile}} is a cargo PROFILE NAME (`dev` | `release` | `dist`), not an
    # output directory — mirrors `_android-ffi-flavor`/`_windows-ffi-flavor`.
    # CONFIG (Xcode's build configuration) picks the DEFAULT profile; an
    # explicit {{profile}} overrides it, but only ever WIDENS `release` to
    # `dist` — `dist` is a size-optimised shipping variant of a release build,
    # not a third Xcode configuration (justfile's own `_apple-ffi-host-flavor`
    # would otherwise hand `-configuration Dist` to xcodebuild, which has no
    # such scheme — installers/macos.md row 319).
    case "$CONFIG" in
        debug)   DEFAULT_PROFILE=dev ;;
        release) DEFAULT_PROFILE=release ;;
        *) echo "apple-ffi-host: config must be 'debug' or 'release', got '$CONFIG'" >&2; exit 1 ;;
    esac
    PROFILE="{{profile}}"
    PROFILE="${PROFILE:-$DEFAULT_PROFILE}"
    if [ "$PROFILE" != "$DEFAULT_PROFILE" ] && [ "$CONFIG" != "release" ]; then
        echo "apple-ffi-host: profile '$PROFILE' requires config=release (got config=$CONFIG) — dist is a shipping variant of a release build, never of debug" >&2
        exit 1
    fi
    case "$PROFILE" in
        dev) PROFILE_DIR=debug ;;
        *)   PROFILE_DIR="$PROFILE" ;;
    esac
    # FLAVOR keys the shared flavor marker/xcframework slot. It used to be
    # config-derived alone (host | host-release); it is now profile-derived, so
    # a release<->dist switch at the same config is seen as a flavor change
    # exactly like a config switch always was (the marker comparison below is
    # unchanged — `$FLAVOR:$FEATURES`, testing.md § convention 15).
    FLAVOR="host-$PROFILE"
    # Only the macOS slice is built, so only its deployment-min matters (must match
    # Package.swift's `platforms:` macOS min — see `apple-ffi` for the full why).
    export MACOSX_DEPLOYMENT_TARGET=15.0
    # IMPLICIT-host build (no `--target aarch64-apple-darwin`), and that is the
    # whole point. Measured 2026-08-22 with `cargo build -Z unstable-options
    # --unit-graph`: cargo hard-separates explicit-target units from implicit-host
    # ones even when the triple is identical, so the old `--target` form shared
    # ZERO of its 95 workspace-local units with the dev inner loop's tree — every
    # `libs/fauna-*` crate compiled a second time, in every mac checkout, on the
    # machine whose per-checkout DIVERGENCE is what caps how many sessions fit.
    # Dropping the flag joins the shared host tree: 55/95 workspace-local and
    # 818/1000 total units reused, with the `selected` guard above untouched.
    #
    # `--artifact-dir` then keeps this flavor's own `.a` OUT of the shared
    # `target/$PROFILE_DIR/` slot. Sharing the build is the win; sharing the
    # artifact PATH would be the "whichever built last owns the file" trap the
    # bluesky nest variant already paid for — `mail-bridge-ffi` writes a release
    # `libfauna_ffi.{a,dylib}` there from a different feature set
    # (`--no-default-features --features labeler`), and any workspace-wide test
    # build writes a default-featured one. A private per-flavor dir makes the
    # collision unrepresentable rather than merely unlikely.
    # Owner: build-machine-resources.md § Cargo target dir layout (mac).
    ARTIFACTS="target/$PROFILE_DIR/apple-ffi-host/${FEATURES:-production}"
    SLICE="$ARTIFACTS/libfauna_ffi.a"
    # Cargo freshness stamp, scoped by (config, features) via $ARTIFACTS' own
    # path. See the flavor guard below for why a naive
    # libs/Cargo.lock/rust-toolchain.toml-only stamp would be wrong here
    # (build-machine-resources.md § Build/e2e slot locks).
    CARGO_STAMP="$ARTIFACTS/apple-ffi-host-cargo.stamp"
    mkdir -p "$ARTIFACTS"
    # Flavor guard: drop a wrong-flavored artifact (full, the other host config,
    # or the other FEATURE set) so build-if-stale rebuilds this flavor's slice
    # cleanly instead of serving a stale xcframework whose mtime is newer than
    # this flavor's .a source. The marker carries FLAVOR:FEATURES for the reason
    # `_apple-ffi-flavor` spells out — the feature axis is the security-relevant
    # one, and every flavor writes this one xcframework slot. The cargo stamp is
    # dropped alongside it: the .a and the stamp are now per-flavor (they live
    # under $ARTIFACTS, above), so a switch-back can no longer serve the OTHER
    # flavor's .a from a "fresh"-reading stamp — but re-proving the slice on a
    # flavor switch costs one warm cargo no-op plus a hardlink, and the
    # xcframework it feeds was just deleted, so the belt stays on.
    [ "$(cat apps/fauna-apple/.ffi-flavor 2>/dev/null)" = "$FLAVOR:$FEATURES" ] || {
        rm -rf apps/fauna-apple/FaunaFFI.xcframework
        rm -f "$CARGO_STAMP"
    }
    # `file-provider-host` is default-OFF (Cargo.toml's feature comment has the
    # rationale) and rides both flavors — FaunaKit's FileProviderHost.swift
    # already calls FfiFileProviderHost, so the dev-loop host build needs it
    # explicitly. Freshness-gated OUTSIDE the build slot (a warm tree must not
    # queue — build-machine-resources.md § Build/e2e slot locks), same --stamp shape as
    # mail-bridge-ffi-lib: a cargo no-op leaves the .a's mtime untouched, so an
    # .a-keyed gate would go permanently stale on any mtime churn (rebases).
    {{py}} scripts/build-if-stale.py --label "apple-ffi-host-cargo-$PROFILE-${FEATURES:-production}" \
        --stamp "$CARGO_STAMP" \
        --target "$SLICE" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} cargo build --locked -p fauna-ffi --profile "$PROFILE" "${FEATURE_ARGS[@]}" \
           -Z unstable-options --artifact-dir "$ARTIFACTS"
    # Same .a-staleness gate as `apple-ffi`: the darwin .a is the precise signal —
    # regen bindings + xcframework iff it changed, else skip bindgen. Same
    # generate-then-validate shape as that gate too (`_apple-ffi-bindgen`'s seam
    # witness runs last), so this one needs the identical `--stamp` fix —
    # written only on rc == 0, so a witness failure commits nothing and a
    # plain re-run still fails. `$ARTIFACTS` already
    # encodes both discriminators the label does ($CONFIG via $PROFILE_DIR,
    # $FEATURES directly), so the stamp needs no separate keying — it lives
    # beside `$CARGO_STAMP` above, same directory, same reasoning.
    {{py}} scripts/build-if-stale.py --label "apple-ffi-host-$PROFILE-${FEATURES:-production}" \
        --stamp "$ARTIFACTS/apple-ffi-host-bindgen.stamp" \
        --target apps/fauna-apple/FaunaFFI.xcframework \
        --target apps/fauna-apple/FaunaFFISwift/Sources/FaunaFFI.swift \
        --source "$SLICE" \
        -- just _apple-ffi-bindgen "$FEATURES" "$SLICE" "$SLICE"
    echo "$FLAVOR:$FEATURES" > apps/fauna-apple/.ffi-flavor
    # This path never populates the cross-checkout release cache; clear the release
    # fast-path stamp so a later `apple-ffi` rebuilds/restores its full release
    # artifact rather than fast-pathing against a host artifact.
    rm -f apps/fauna-apple/.ffi-cache.stamp

# Regenerate the Swift bindings + assemble the FaunaFFI.xcframework. Parameterized
# by flavor so the full release path (`apple-ffi`: 3 slices, or 5 with
# `apple-ffi-watch`) and the host-only debug dev path (`apple-ffi-host`: 1 slice)
# share the load-bearing bindgen +
# modulemap-concat + binding-sync tail below — a second copy would silently drift.
# {{bindgen_lib}}: the .a uniffi-bindgen reads UniFFI metadata from (any one slice
# works — all slices share the ABI). {{slices}}: the .a's assembled into the
# xcframework (1 for host, 3 or 5 for full — the caller derives the list from its
# own SLICE_TRIPLES, so this recipe never re-lists them). Gated by the caller on .a freshness via
# build-if-stale — don't invoke directly (it assumes the slices are built + current).
_apple-ffi-bindgen features bindgen_lib +slices:
    #!/usr/bin/env bash
    set -euo pipefail
    FEATURES="{{features}}"
    # Wipe the previous generation first: uniffi-bindgen only ever WRITES files,
    # so a seam-carrying .swift left over from a test-flavored run would survive
    # a production regeneration that no longer emits it — the whole point of the
    # split, defeated by a stale file. Windows' `_windows-ffi-bindgen` and
    # android's `_android-ffi-bindgen` do the same for the same reason
    # (testing.md convention 15).
    rm -rf apps/fauna-apple/generated/
    # Generate Swift bindings from one slice (all slices share the UniFFI ABI).
    {{slot_build}} cargo run -p fauna-ffi --bin uniffi-bindgen generate \
        --library {{bindgen_lib}} \
        --language swift --out-dir apps/fauna-apple/generated/
    # Remove old XCFramework if present
    rm -rf apps/fauna-apple/FaunaFFI.xcframework
    # Create the XCFramework from the requested slices (one -library/-headers pair
    # per slice — 3 or 5 for the full release path, 1 for the host-only debug path).
    xcf_args=()
    for lib in {{slices}}; do
        xcf_args+=(-library "$lib" -headers apps/fauna-apple/generated/)
    done
    #
    # Run under the display-deadlock watch (`gpu_watch` at the top): on the dev
    # fleet's macOS guest, WindowServer can deadlock in the paravirtual GPU driver,
    # after which `xcodebuild` prints "successfully written out" and then blocks
    # in the kernel for good, and this recipe used to hang with it until the guest
    # was restarted. The watch refuses to start it on a dead display and gives up on
    # it (exit 75, naming the cause) if the display dies while it runs. A
    # half-assembled xcframework (raw per-crate headers, no merged module map)
    # must not survive that: it would read as fresh to the next run.
    {{gpu_watch}} \
        xcodebuild -create-xcframework "${xcf_args[@]}" \
        -output apps/fauna-apple/FaunaFFI.xcframework \
        || { rc=$?; rm -rf apps/fauna-apple/FaunaFFI.xcframework; exit "$rc"; }
    # SPM requires modulemap to be named module.modulemap. With multi-namespace
    # library mode (fauna_ffi + fauna_provisioning + fauna_onboarding_machine),
    # bindgen emits one .modulemap per namespace. Concatenate them into a
    # single module.modulemap so the xcframework exposes all three C-bridge
    # modules (FaunaFFIFFI, fauna_provisioningFFI, fauna_onboarding_machineFFI).
    # Swift binding files import the per-namespace bridge module by name.
    for dir in apps/fauna-apple/FaunaFFI.xcframework/*/Headers; do
        cat "$dir"/*FFI.modulemap > "$dir/module.modulemap"
        rm "$dir"/*FFI.modulemap
    done
    # Sync ALL generated namespace bindings into the FaunaFFISwift SPM target so
    # clients see a single Swift module (`import FaunaFFISwift` / `import FaunaKit`).
    # Copy *every* generated namespace, not a hand-maintained subset: the single
    # libfauna_ffi.a links every uniffi-exposed dep crate, and the xcframework's
    # concatenated module.modulemap exposes every C-bridge module, so any
    # cross-namespace reference resolves (e.g. fauna-ffi's `mail_admin`
    # `build_*_machine` free fns return `fauna_client_{dns,mail_settings}` types,
    # `folders`/`launch`/`mail` likewise). A hand-maintained copy-list silently
    # broke when mail-admin landed on a non-Mac (apple-ffi never ran to refresh
    # it). The .swift here are gitignored build artifacts regenerated by this
    # recipe; only the hand-written FFICompat.swift is tracked.
    #
    # Drop the PREVIOUS copies first, for the same only-ever-writes reason the
    # generated/ wipe above exists: this is a second staging slot both flavors
    # share, so a namespace file the production flavor no longer emits would
    # otherwise survive here and keep feeding seams to the Swift compiler.
    # FFICompat.swift is hand-written + tracked — never touched.
    find apps/fauna-apple/FaunaFFISwift/Sources -maxdepth 1 -name '*.swift' \
        ! -name 'FFICompat.swift' -delete
    cp apps/fauna-apple/generated/*.swift apps/fauna-apple/FaunaFFISwift/Sources/
    # The self-maintaining artifact witness (testing.md convention 15; android's
    # `_android-ffi-bindgen` established the shape, windows copied it). Every
    # SHIPPING flavor greps the tree it just generated and fails if any
    # `*ForTest` export survived — so a NEWLY added ungated seam is caught here,
    # with no list for anyone to keep up to date, instead of shipping silently in
    # the next `mac-release`. Derived from the generated bindings rather than the
    # .a because these are what the Swift compiler sees: an export the bindings
    # don't name is unreachable from the app even if the slice still carried it.
    #
    # ⚠ The condition is "not the TEST flavor", not "the production flavor":
    # `store-safe` (the App-Store escape hatch, 2026-08-15) is a third flavor and
    # it is every bit as shippable as production — an `[ -z "$FEATURES" ]` test
    # would have exempted the one artifact an App Store review actually receives.
    if [ "$FEATURES" != "test-helpers" ]; then
        LEAKED=$(grep -rlE '\bfunc [A-Za-z0-9_]*ForTest\(' apps/fauna-apple/generated/ || true)
        if [ -n "$LEAKED" ]; then
            echo "ERROR: the PRODUCTION apple FFI flavor generated e2e seam exports." >&2
            echo "       testing.md convention 15 — these would ship in mac-release /" >&2
            echo "       mac-app / mac-dmg and every iOS device build." >&2
            echo "       Files: $LEAKED" >&2
            echo "       Fix: gate the seam's uniffi::export on fauna-ffi's test-helpers" >&2
            echo "       feature (never a dep line — rule (b)), then rerun." >&2
            exit 1
        fi
    fi
    # Flavor-diff seam witness (e2e-conventions.md point 15, ratified
    # 2026-08-13) — the grep above sees only *ForTest-NAMED seams; this also
    # pins the non-test-named floor (setProviderBaseUrls, resolvedNestDialUrl,
    # callMachineMethod*, setDnsCreds, createMlsGroup, FfiChildAgentSpawner,
    # the clock-offset pair, …): each floor seam must be declared in the test
    # flavor's face and absent from production's — a dead cfg gate ships the
    # seam into BOTH faces, which the suffix grep above cannot see. Per-tree
    # halves SUFFICE on apple (mirrors windows): every platform's bindings
    # generate from the same fauna-ffi library, so the seam SET is
    # fleet-identical and android's own pair check already polices it — this
    # leg's job is only that APPLE's OWN staged face is clean on whichever
    # flavor this run just staged. Do not force a pair check onto the single
    # staging slot (one tree on disk at a time, behind .ffi-flavor).
    #
    # ⚠ Same three-flavor caveat as the suffix grep above: route on
    # `= "test-helpers"`, not `-z`/`!= "test-helpers"` inverted — `store-safe`
    # is shippable and must take the PRODUCTION arm, same as empty FEATURES.
    if [ "$FEATURES" = "test-helpers" ]; then
        FLAVOR_FLAG=--test-tree
    else
        FLAVOR_FLAG=--production-tree
    fi
    {{py}} scripts/check-ffi-seam-diff.py --lang swift \
        "$FLAVOR_FLAG" apps/fauna-apple/generated/
    echo "XCFramework built at apps/fauna-apple/FaunaFFI.xcframework (flavor: ${FEATURES:-production})"

# Run the FaunaKit Swift tests, then typecheck the iOS app target for the host.
#   1. `swift test` builds + runs FaunaApplePackageTests.xctest — which is just
#      FaunaKitTests now that FaunaiOSTests (XCUITest, can't run via `swift test`)
#      is no longer an SPM target (see apps/fauna-apple/Package.swift).
#   2. `swift build --target FaunaiOS` compiles Fauna-iOS/ for the macOS host so
#      a portability regression there is caught here, not only by the (slower)
#      iOS-simulator build. Fauna-iOS/ uses iOS-only SwiftUI/UIKit API but the
#      shims in FaunaKit/Utilities/CrossPlatformUI.swift make it typecheck for
#      macOS; the iOS app never *runs* there. (Real iOS build: `xcodebuild
#      -scheme FaunaiOS`; macOS app: `just mac-debug`.)
#
# Takes the TEST FFI flavor: `swift test` and the FaunaiOS typecheck build Swift
# in DEBUG, so `#if DEBUG` compiles FaunaKit's TestAgent / in-process automation
# server / `ConversationsTestInject`, whose calls to the `*ForTest` UniFFI seams
# only resolve against `test-helpers` bindings (testing.md § convention 15).
#
# ONE slot for both phases (build-machine-resources.md § Build/e2e slot locks →
# the slot is the recipe's dataset lease: the FaunaiOS
# typecheck consumes the same shared FFI `swift test` built, so two separate
# acquisitions left it evictable between them. `_swift-test-impl`'s own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
swift-test: (apple-ffi-host-test "debug")
    {{slot_build}} just _swift-test-impl

_swift-test-impl:
    {{slot_build}} swift test --package-path apps/fauna-apple
    {{slot_build}} swift build --package-path apps/fauna-apple --target FaunaiOS

# Build Fauna macOS desktop app (debug). Test FFI flavor for the same reason
# swift-test takes it — this is also the binary the macOS e2e driver launches,
# and its in-process agent drives the seams. Freshness-gated OUTSIDE the build
# slot (a warm tree must not queue — build-machine-resources.md § Build/e2e slot locks),
# same shape as android-debug's gradlew gate: --show-bin-path resolves the
# product path without building (cheap, side-effect-free — confirmed: prints
# only the path, no "Compiling"/"Building" output), watching all of
# apps/fauna-apple as source EXCEPT swiftpm's own .build/ output dir (a source
# that watches its own build output would always look stale right after
# building it) AND .ffi-flavor, which the `apple-ffi-host-test` prerequisite
# above rewrites (touching its mtime) on EVERY invocation regardless of
# whether it did any real work — without this exclude the gate reads "stale"
# on every single warm re-run too (caught live: a truly warm re-run still said
# "stale → building", diagnosed to this file being the one apps/fauna-apple
# source with no byte-comparing/--keep write, unlike the generated i18n/
# providers files it sits beside).
mac-debug: (apple-ffi-host-test "debug")
    #!/usr/bin/env bash
    set -euo pipefail
    BIN_PATH="$(swift build --package-path apps/fauna-apple --product FaunaMacOS --show-bin-path)"
    {{py}} scripts/build-if-stale.py --label mac-debug \
        --stamp apps/fauna-apple/.mac-debug.stamp \
        --target "$BIN_PATH/FaunaMacOS" \
        --source apps/fauna-apple \
        --exclude '*/.build/*' --exclude '*/.ffi-flavor' \
        -- {{slot_build}} swift build --package-path apps/fauna-apple --product FaunaMacOS

# apple-swift-build-check — the mac merge-gate check's one gate (scripts/
# merge-gate-check-mac.sh; merge-gate-check.md § Merge-gate check (mac)): compile
# BOTH apple targets from freshly-regenerated bindings, run NO tests. mac-debug
# = apple-ffi-host (bindgen from current libs/) + the FaunaMacOS product build;
# the FaunaiOS line typechecks the iOS target for the macOS host — the same
# literal line swift-test runs after its tests (kept identical on purpose; a
# gate must be build-only, so it can't just call swift-test and inherit its
# test run). Catches the class no Linux/Windows-side gate can see: a shared-Rust change
# whose regenerated Swift bindings break an apple call site — e.g. a new FFI
# enum variant vs FaunaKit's exhaustive switches (one such variant left both apple
# targets un-buildable on main for ~4.5h; fixed). Self-slots via ONE
# outer {{slot_build}} wrapping `_apple-swift-build-check-impl` (2026-08-25, row
# 149 — was three separate acquisitions) — callers must NOT wrap it again.
#
# THE iOS RUST SLICE (added 2026-08-12). Everything above compiles for the macOS
# HOST — `mac-debug` builds the darwin fauna-ffi slice and the FaunaiOS line only
# TYPECHECKS Swift against it — so nothing here, and nothing on the Linux or
# Windows dev VMs, ever compiled Rust for an apple PHONE triple. That hole let a
# dependency-graph
# change land on main that made `just apple-ffi` (the full 5-slice build every
# `--app ios` e2e run and every iOS archive needs) fail outright: a crate gated
# `cfg(not(any(target_os = "macos", target_os = "windows")))` sent iOS/watchOS
# down a freedesktop D-Bus arm that cannot compile there. Green everywhere,
# broken on the one triple nobody built. A `cargo check` of the iOS slice closes
# that class at check-cost — the full `apple-ffi` is 5 RELEASE slices and stays
# deliberately out of the gates (header above).
#
# THE WATCH SLICE (added 2026-08-22, replacing this gate's "accepted residual:
# the watchOS slices stay ungated"). That residual was sound only while its own
# stated net existed: "a watchOS-only dep break is caught by the next full
# `apple-ffi`". Since the watch slices became opt-in (`just apple-ffi-watch` — see
# `apple-ffi`; nothing on any path builds them any more), that net is gone, and
# removing coverage while calling it a cost saving is the failure mode. So the
# same one-line `cargo check` the iOS slice got in 2026-08-12 covers the watch
# triple too — and it is strictly TIGHTER than what it replaces: every tip,
# instead of whenever a session happened to reach for a 5-slice build (the 2026-08-12
# break sat on main for two days precisely because nobody did).
#
# `--no-default-features` and NO feature list, because that is literally how
# `_apple-ffi-flavor` builds the watch slices — a check on a different feature set
# would gate a graph the build never resolves. One triple, not both: the break
# class is dependency resolution under `target_os = "watchos"`, and the device and
# sim triples agree on that cfg (they differ only in `target_abi`), so the sim
# slice would re-check the same graph for a second full check closure.
#
# Accepted residual, narrower than the old one: a watchOS break that is a LINK
# failure rather than a resolution/typeck failure still escapes (`cargo check`
# does not link) — that is the class § apple-ffi watchOS slice already documents
# as latent, and it surfaces on the first real watch app link, which by
# construction cannot happen before a watch app exists.
#
# ONE slot for all three checks (build-machine-resources.md § Build/e2e slot
# locks → the slot is the recipe's dataset lease: all
# three read the same shared FFI `mac-debug` (the dependency above) built, so
# three separate acquisitions left it evictable between them.
# `_apple-swift-build-check-impl`'s own {{slot_build}} lines stay: under this one
# they are reentrant no-ops (build-slot.py § FIFO ticket queue). Self-slots —
# callers must NOT wrap it again.
[macos]
apple-swift-build-check: mac-debug
    {{slot_build}} just _apple-swift-build-check-impl

_apple-swift-build-check-impl:
    {{slot_build}} swift build --package-path apps/fauna-apple --target FaunaiOS
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{slot_build}} cargo check --locked -p fauna-ffi --features file-provider-host --target aarch64-apple-ios
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{slot_build}} cargo check --locked -p fauna-ffi --no-default-features --target aarch64-apple-watchos

# Build Fauna macOS desktop app (release)
# Host-only release FFI (1 darwin slice) — FaunaMacOS links only that slice, so the
# multi-slice `apple-ffi` would be dead slices of compile. See build-system.md
# § "Host-only debug FFI (mac dev loop)".
# `profile` defaults to "" (-> release, unchanged) — pass "dist" for the
# size-optimised shipping variant (installers/macos.md § Build Pipeline).
mac-release profile="": (apple-ffi-host "release" profile)
    {{slot_build}} swift build --package-path apps/fauna-apple --product FaunaMacOS -c release

# Build the Fauna.app bundle from the thin `Fauna.xcodeproj`.
# config = release (default) | debug → produces build/<Config>/Fauna.app, the macOS
# app bundle that `mac-dmg` packages and the all-in-one installer .pkg installs.
#
# Why xcodebuild and not `swift build` (2026-08-28): SwiftPM cannot build an app
# EXTENSION and never emits a `.app` at all, so the SwiftPM route shipped an
# app-only bundle with no `Contents/PlugIns/` — and therefore no Fauna location in
# Finder, ever. The project's `Fauna` target is the one thing in the tree that
# builds the app WITH its `Fauna-FileProvider.appex` / `Fauna-FileProviderUI.appex`
# embedded; installers/macos.md § App extensions records the route and why the
# alternative (SwiftPM app + a separately-xcodebuilt appex) was rejected. It links
# the same `FaunaMacOSLib` + shared `@main` shell the `FaunaMacOS` executable did,
# so what is COMPILED did not change with the build system.
#
# The recipe still stages what the project does not: the rendered `AppIcon.icns`
# (a build artifact, not a source — `scripts/render-app-icons.py`; same brand mark
# as the Windows installer's `AppIcon.ico`) and the per-user `fauna-sync-agent`,
# then ad-hoc-signs inside-out. Developer ID signing + notarization stay in
# `mac-dmg` / the installer `build.sh --sign` (they re-sign over the ad-hoc sig
# through the same `sign-app-bundle.sh`).
#
# FFI: builds the host darwin slice only (`apple-ffi-host*`) — the app links
# only that slice, so the multi-slice `apple-ffi` would compile dead slices. The
# resulting binary is byte-identical to the old full-`apple-ffi` build (which also
# linked only darwin). See build-system.md § "Host-only debug FFI (mac dev loop)".
#
# The FFI flavor is chosen in the BODY rather than as a recipe dependency because
# it must follow `{{config}}`: a `debug` build compiles the `#if DEBUG`
# automation surface, whose calls to the `*ForTest` seams only resolve against
# test-flavored bindings, while `release` — the shipped Fauna.app — must not
# carry them at all (testing.md § convention 15). A single config-parameterised
# dependency cannot express that split.
#
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → the slot is the recipe's dataset lease: the
# packaging step below consumes both the FFI/xcodebuild output and the
# `fauna-sync-agent` cargo build's, so two separate acquisitions left the
# dataset evictable in the gap. Not a warm-path regression — this recipe never
# had a freshness-gated no-op path of its own (unlike `mac-debug`): it always
# rebuilds and repackages, so it always took at least two slots already.
# `_mac-app-impl`'s own {{slot_build}} lines stay: under this one they are
# reentrant no-ops (build-slot.py § FIFO ticket queue). CONFIG travels via env,
# not a `just` param, so the delegate call stays bare for the tier_1 tests'
# generic one-line-delegation detection (`_recipe_body`).
# `profile` defaults to "" (derive from config, i.e. today's unchanged
# release/dev behavior) so the CI `macos` job's `just mac-app release` stays on
# the fast `release` cargo profile; only a real shipping cut passes
# `profile=dist` (installers/macos.md § Build Pipeline → Size & build profile).
mac-app config="release" profile="":
    #!/usr/bin/env bash
    set -euo pipefail
    export CONFIG="{{config}}"
    export PROFILE="{{profile}}"
    {{slot_build}} just _mac-app-impl

_mac-app-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    CONFIG="${CONFIG:?}"
    PROFILE="${PROFILE:-}"
    if [ "$CONFIG" = "debug" ]; then
        just apple-ffi-host-test debug
    else
        just apple-ffi-host "$CONFIG" "$PROFILE"
    fi

    # release -> Release, debug -> Debug (matches mac-dmg's build/Release/Fauna.app,
    # and is also xcodebuild's own configuration name).
    OUT="$(tr '[:lower:]' '[:upper:]' <<< "${CONFIG:0:1}")${CONFIG:1}"

    # THE APP COMES FROM xcodebuild, not `swift build` (2026-08-28). SwiftPM
    # cannot build an app EXTENSION, and the shipped `.pkg`/`.dmg` must carry
    # `Contents/PlugIns/Fauna-FileProvider.appex` or no Fauna location can ever
    # appear in Finder (installers/macos.md § App extensions — the M2-M5 gap this
    # closed). The `Fauna` target's Embed App Extensions phase is the only thing
    # in the tree that produces that layout, and xcodebuild validates the
    # parent-bundle-id prefix while it does it.
    #
    # Why the whole app moved here rather than embedding an appex built alongside
    # a SwiftPM app: the two-build-system alternative compiles FaunaKit TWICE (once
    # under SwiftPM for the app, once under xcodebuild for the appex) and leaves
    # version/identifier/deployment-target agreement as a standing invariant to
    # police, while this route has one build system and no agreement to police.
    # It also converges macOS onto the shape iOS already ships through
    # (`apple-ios-store-safe-check` archives the same project).
    #
    # The app target links the SAME SwiftPM library product as the `FaunaMacOS`
    # executable did (`FaunaMacOSLib` + the shared `@main` shell), so this is a
    # change of BUILD SYSTEM, not of what is compiled — installers/macos.md
    # § App extensions has the "can never drift" argument.
    DERIVED="apps/fauna-apple/.xcode-build"
    # ENABLE_DEBUG_DYLIB=NO: Xcode 16 otherwise splits a Debug app into a
    # `Fauna.debug.dylib` beside the executable, which is not a shape any of our
    # packaging/signing/launch paths know.
    {{slot_build}} xcodebuild \
        -project apps/fauna-apple/Fauna.xcodeproj \
        -scheme Fauna \
        -configuration "$OUT" \
        -derivedDataPath "$DERIVED" \
        ENABLE_DEBUG_DYLIB=NO \
        CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
        build
    BUILT="$DERIVED/Build/Products/$OUT/Fauna.app"
    [ -d "$BUILT" ] || { echo "xcodebuild produced no bundle at $BUILT" >&2; exit 1; }

    APP="build/$OUT/Fauna.app"
    rm -rf "$APP"
    mkdir -p "build/$OUT"
    cp -R "$BUILT" "$APP"

    # UNREGISTER the derived-data copy. xcodebuild's own last step is
    # `lsregister -f -R -trusted` on its product, so building now leaves a SECOND
    # bundle claiming `social.fauna.fauna` in the Launch Services database —
    # exactly the same-bundle-id twin that fails every `NSFileProviderManager`
    # register with FP -2001/-2014, because fileproviderd resolves the registering
    # app BY BUNDLE ID (installers/macos.md § App extensions, the twin gotcha).
    # Registering nothing is what this recipe did before it moved to xcodebuild,
    # and what it should keep doing: the bundle users get is registered by the
    # installer, not by a build. `-u` is reversible and does not touch the bundle;
    # `-R` matches the recursive `-f -R` xcodebuild registered with, so the
    # nested `.appex` bundles come out too rather than lingering as build output in
    # the LaunchServices database. It prints a benign `failed to scan …: -10814
    # from spotlight` — that line is a Spotlight scan, not the unregister.
    #
    # ⚠ The exit code is IGNORED, and that is load-bearing (measured 2026-08-28,
    # the run after this recipe landed): the same benign Spotlight scan ALSO makes
    # lsregister exit **1**, not 0 as this comment first claimed. With `set -e` on
    # and stderr dropped, that killed `just pkg-sign-only` 857 s in, immediately
    # after `** BUILD SUCCEEDED **`, with NO diagnostic anywhere — the copy had
    # already succeeded, so the bundle looked complete and only the absent
    # `Contents/Resources` said how far it got. Dropping stderr and trusting the
    # exit code was exactly backwards: the noise surfaces in BOTH channels, so the
    # unregister must be best-effort in both. What the recipe actually owes — that
    # no derived-data twin claims the bundle id — is a LaunchServices state, and it
    # is checked where it matters (installers/macos.md § App extensions: `mdfind
    # "kMDItemCFBundleIdentifier == 'social.fauna.fauna'"` names exactly
    # /Applications/Fauna.app), not by this housekeeping call's status.
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
        -u -R "$BUILT" 2>/dev/null || true

    # The app icon is NOT in the project's (empty) Resources phase — the .icns is
    # a rendered artifact, not a source (scripts/render-app-icons.py), so the
    # recipe stages it exactly as it always has. xcodebuild creates no
    # `Contents/Resources` for a target with no resources, hence the mkdir.
    mkdir -p "$APP/Contents/Resources"
    cp apps/fauna-apple/Fauna-macOS/Resources/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

    # What xcodebuild owes us, asserted rather than assumed: a silent regression
    # in any of these ships an app that launches but has no extension, no
    # auto-start at sign-in (the `SMAppService.agent` plist —
    # apps/macos.md § App Lifecycle), or the wrong version.
    for want in Contents/MacOS/Fauna Contents/Info.plist Contents/PkgInfo \
                Contents/Library/LaunchAgents/social.fauna.FaunaMacOS.plist \
                Contents/PlugIns/Fauna-FileProvider.appex \
                Contents/PlugIns/Fauna-FileProviderUI.appex; do
        [ -e "$APP/$want" ] || { echo "assembled bundle is missing $want" >&2; exit 1; }
    done

    # Bundle the per-user sync agent beside the app executable — the .dmg
    # channel's agent source: the app's LaunchdSyncAgentSpawner resolves
    # Contents/MacOS/fauna-sync-agent and self-installs its LaunchAgent
    # (sync-agent.md § Packaging + lifecycle; a .pkg install also ships it at
    # /usr/local/bin, the spawner's fallback path).
    RELEASE_FLAG=""
    [ "$CONFIG" = "release" ] && RELEASE_FLAG="--release"
    # Implicit-host, for the same reason `_apple-ffi-host-flavor` dropped its
    # `--target aarch64-apple-darwin`: the explicit triple bought a separate
    # artifact tree and nothing else, so this compiled the whole sync-agent
    # closure a second time in every mac checkout. Measured 2026-08-22 against
    # the tui/e2e prebuild's graph (`cargo build -p fauna-tui -p fauna-sync-agent`):
    # 26.4% → 89.3% of units reused, 0/45 → 30/45 of them workspace-local.
    #
    # `--artifact-dir` for the same reason it is there: this recipe builds an
    # ARTIFACT, so it runs `selected` (the export at the top of the recipe) while
    # that prebuild runs workspace-unified — 15 workspace-local units genuinely
    # differ, so the two produce DIFFERENT binaries and must not race for one
    # `target/$CONFIG/fauna-sync-agent` path, which the e2e harness resolves
    # directly (`helpers/sync_seats.py`, `helpers/multiseat_config.py`).
    # build-machine-resources.md § Cargo target dir layout (mac).
    AGENT_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
    AGENT_ARTIFACTS="$AGENT_TARGET_DIR/$CONFIG/mac-app"
    mkdir -p "$AGENT_ARTIFACTS"
    {{slot_build}} cargo build $RELEASE_FLAG -p fauna-sync-agent \
        -Z unstable-options --artifact-dir "$AGENT_ARTIFACTS"
    cp "$AGENT_ARTIFACTS/fauna-sync-agent" \
       "$APP/Contents/MacOS/fauna-sync-agent"

    # Ad-hoc sign LAST, inside-out: the bundled agent with ITS entitlements, each
    # .appex with ITS OWN, then the app — never `--deep` on the app,
    # which re-stamps nested code with the app's claims
    # (installer/macos/sign-app-bundle.sh has the measurement). mac-dmg /
    # build.sh --sign re-sign with the Developer ID cert through the same script.
    # (xcodebuild was told not to sign: the app-group + sandbox capabilities are
    # restricted, and xcodebuild refuses them without a provisioning profile —
    # installers/macos.md § App extensions, M0 finding (b).)
    ./installer/macos/sign-app-bundle.sh "$APP" -
    echo "==> Assembled $APP ($(du -sh "$APP" | cut -f1))"

# Build signed, notarized .dmg for macOS desktop app (build + package in one go).
# `profile` defaults to "" (-> release); pass "dist" for a real release cut.
mac-dmg profile="": (mac-app "release" profile)
    just mac-dmg-package

# Sign, notarize and staple an ALREADY-BUILT build/Release/Fauna.app into
# build/Fauna-<version>.dmg (+ .sha256). Split from `mac-dmg` so the release workflow
# can build the bundle in a credential-free job and sign it in a separate,
# Environment-gated one (installers/macos.md § Build Pipeline → The `.dmg`
# release pipeline). Requires a Developer ID Application identity in a
# keychain the current session can use, plus notarization credentials in ONE
# of two shapes:
#
#   App Store Connect API key (preferred; what the release workflow uses):
#     FAUNA_NOTARY_KEY_FILE   path to the AuthKey_<id>.p8
#     FAUNA_NOTARY_KEY_ID     the key's id
#     FAUNA_NOTARY_ISSUER     the issuer id (App Store Connect → Users and Access → Keys)
#   Apple ID (the original, still accepted for a maintainer's local run):
#     FAUNA_APPLE_ID          Apple ID email
#     FAUNA_NOTARY_PASSWORD   app-specific password (NOT the account password)
#
# FAUNA_SIGN_IDENTITY and FAUNA_TEAM_ID are required in both shapes.
mac-dmg-package:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${FAUNA_SIGN_IDENTITY:?Set FAUNA_SIGN_IDENTITY}"
    : "${FAUNA_TEAM_ID:?Set FAUNA_TEAM_ID}"
    if [ -n "${FAUNA_NOTARY_KEY_FILE:-}" ]; then
        : "${FAUNA_NOTARY_KEY_ID:?Set FAUNA_NOTARY_KEY_ID (with FAUNA_NOTARY_KEY_FILE)}"
        : "${FAUNA_NOTARY_ISSUER:?Set FAUNA_NOTARY_ISSUER (with FAUNA_NOTARY_KEY_FILE)}"
        [ -r "$FAUNA_NOTARY_KEY_FILE" ] || { echo "FAUNA_NOTARY_KEY_FILE not readable: $FAUNA_NOTARY_KEY_FILE" >&2; exit 1; }
        NOTARY_AUTH=(--key "$FAUNA_NOTARY_KEY_FILE" --key-id "$FAUNA_NOTARY_KEY_ID" --issuer "$FAUNA_NOTARY_ISSUER")
    else
        : "${FAUNA_APPLE_ID:?Set FAUNA_APPLE_ID (or FAUNA_NOTARY_KEY_FILE + FAUNA_NOTARY_KEY_ID + FAUNA_NOTARY_ISSUER)}"
        : "${FAUNA_NOTARY_PASSWORD:?Set FAUNA_NOTARY_PASSWORD}"
        NOTARY_AUTH=(--apple-id "$FAUNA_APPLE_ID" --team-id "$FAUNA_TEAM_ID" --password "$FAUNA_NOTARY_PASSWORD")
    fi

    VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    APP_PATH="build/Release/Fauna.app"
    DMG_PATH="build/Fauna-${VERSION}.dmg"
    [ -d "$APP_PATH" ] || { echo "$APP_PATH not found — run \`just mac-app release\` first (or \`just mac-dmg\`)" >&2; exit 1; }

    echo "==> Signing Fauna.app (inside-out — installer/macos/sign-app-bundle.sh)..."
    ./installer/macos/sign-app-bundle.sh "$APP_PATH" "$FAUNA_SIGN_IDENTITY"
    codesign --verify --deep --strict "$APP_PATH"

    echo "==> Creating DMG..."
    rm -f "$DMG_PATH"
    hdiutil create -volname "Fauna" -srcfolder "$APP_PATH" \
        -ov -format UDZO "$DMG_PATH"

    echo "==> Signing DMG..."
    codesign --sign "$FAUNA_SIGN_IDENTITY" --timestamp "$DMG_PATH"

    echo "==> Notarizing..."
    xcrun notarytool submit "$DMG_PATH" "${NOTARY_AUTH[@]}" --wait

    echo "==> Stapling..."
    xcrun stapler staple "$DMG_PATH"
    # The app inside was notarized as part of the .dmg submission (one ticket per
    # cdhash), so its own ticket exists and staples onto the bundle — a copy
    # dragged out of the .dmg then passes Gatekeeper offline too.
    xcrun stapler staple "$APP_PATH"

    shasum -a 256 "$DMG_PATH" > "${DMG_PATH}.sha256"
    echo "==> Done! DMG: $DMG_PATH"

# Sign, notarize and tar ONE of the terminal app's macOS archives
# (installers/tui.md § The ratified channel, decision 2: Developer ID-signed and
# notarized before it is published — an ad-hoc-signed archive never ships).
# `dir` is the staged archive layout `assemble-archive.sh` produced (fauna-tui +
# fauna-sync-agent + install.sh); `suffix` names the target (aarch64-darwin,
# x86_64-darwin). Both binaries get the hardened runtime + a timestamp; the
# agent signs with ITS OWN entitlements file, exactly as the .pkg's
# installer/macos/build.sh signs it, so both exec shapes carry one entitlement
# set. Notarization submits a zip (notarytool takes zip/dmg/pkg, not tar) and
# the published artifact is the tar.gz of the SAME signed files — the ticket is
# per cdhash and Gatekeeper looks it up online, so a bare executable needs no
# staple (stapler has nothing to staple to). Same credential env as
# `mac-dmg-package`; release-macos.yml's `sign` job runs it once per target.
# Usage: just mac-tui-archive aarch64-darwin tui-unsigned/fauna-tui-aarch64-darwin
mac-tui-archive suffix dir:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${FAUNA_SIGN_IDENTITY:?Set FAUNA_SIGN_IDENTITY}"
    : "${FAUNA_TEAM_ID:?Set FAUNA_TEAM_ID}"
    if [ -n "${FAUNA_NOTARY_KEY_FILE:-}" ]; then
        : "${FAUNA_NOTARY_KEY_ID:?Set FAUNA_NOTARY_KEY_ID (with FAUNA_NOTARY_KEY_FILE)}"
        : "${FAUNA_NOTARY_ISSUER:?Set FAUNA_NOTARY_ISSUER (with FAUNA_NOTARY_KEY_FILE)}"
        NOTARY_AUTH=(--key "$FAUNA_NOTARY_KEY_FILE" --key-id "$FAUNA_NOTARY_KEY_ID" --issuer "$FAUNA_NOTARY_ISSUER")
    else
        : "${FAUNA_APPLE_ID:?Set FAUNA_APPLE_ID (or FAUNA_NOTARY_KEY_FILE + FAUNA_NOTARY_KEY_ID + FAUNA_NOTARY_ISSUER)}"
        : "${FAUNA_NOTARY_PASSWORD:?Set FAUNA_NOTARY_PASSWORD}"
        NOTARY_AUTH=(--apple-id "$FAUNA_APPLE_ID" --team-id "$FAUNA_TEAM_ID" --password "$FAUNA_NOTARY_PASSWORD")
    fi
    DIR="{{dir}}"
    NAME="fauna-tui-{{suffix}}"
    for f in fauna-tui fauna-sync-agent install.sh; do
        [ -f "$DIR/$f" ] || { echo "$DIR/$f not found — stage the archive with apps/fauna-tui/packaging/assemble-archive.sh first" >&2; exit 1; }
    done
    mkdir -p build
    STAGE="build/$NAME"
    rm -rf "$STAGE"
    cp -R "$DIR" "$STAGE"

    echo "==> Signing $NAME (Developer ID, hardened runtime)..."
    codesign --sign "$FAUNA_SIGN_IDENTITY" --options runtime --timestamp --force "$STAGE/fauna-tui"
    codesign --sign "$FAUNA_SIGN_IDENTITY" --options runtime --timestamp --force \
        --entitlements installer/macos/fauna-sync-agent.entitlements "$STAGE/fauna-sync-agent"
    codesign --verify --strict "$STAGE/fauna-tui"
    codesign --verify --strict "$STAGE/fauna-sync-agent"

    echo "==> Notarizing $NAME..."
    ZIP="build/$NAME.notarize.zip"
    rm -f "$ZIP"
    ditto -c -k --keepParent "$STAGE" "$ZIP"
    xcrun notarytool submit "$ZIP" "${NOTARY_AUTH[@]}" --wait
    rm -f "$ZIP"

    TAR="build/$NAME.tar.gz"
    rm -f "$TAR"
    tar -C build -czf "$TAR" "$NAME"
    shasum -a 256 "$TAR" > "${TAR}.sha256"
    echo "==> Done! Archive: $TAR"

# Build fauna-ffi for Windows ARM64 and generate C# bindings.
# Uses the in-tree uniffi-bindgen-cs (libs/uniffi-bindgen-cs/, vendored from
# NordSecurity PR #168 + patched for uniffi 0.31). Mirrors apple-ffi/android-ffi.
# Bare cargo fails on Git Bash (its link.exe shadows the MSVC linker) — use
# scripts/cargo-win.cmd.
#
# x86_64 (amd64): pass `target="x86_64-pc-windows-msvc"` for the x64 cross-compile
# leg (needs `rustup target add x86_64-pc-windows-msvc`, user-approved). Empty
# (default) keeps the original arm64 implicit-host build unchanged.
# `cargo-win.cmd` selects the matching cl/link/lib environment via CARGO_WIN_ARCH
# (the `Hostarm64\x64` cross tools + `lib\x64`/SDK `Lib\...\{um,ucrt}\x64` ship in
# the same BuildTools/SDK install already used natively for arm64).
windows-ffi profile="release" target="": i18n-generate providers-generate
    @just _windows-ffi-flavor "{{profile}}" "" "{{target}}"

# Test-flavoured Windows FFI: adds `test-helpers`, which compiles the E2E
# injection seams (`OnboardingMachine::set_*_for_test` / `call_machine_method`,
# `ConversationsManager::install_mock_backends_for_test` / `inject_inbound_for_test`,
# `LaunchMachine::set_phase_for_test`, …) and exports them over UniFFI. Consumed
# by `windows-debug` and `windows-cs-test`, which build `Configuration=Debug` —
# the configuration whose `#if DEBUG` TestAgent (and the 5 unit-test files
# calling `InstallMockBackendsForTest`) are the seams' only callers.
windows-ffi-test profile="dev" target="": i18n-generate providers-generate
    @just _windows-ffi-flavor "{{profile}}" "test-helpers" "{{target}}"

# Store-safe Windows FFI — the App-Store escape hatch's Rust half
# (`dynamic-features.md` § Platform-family surface excision). `--no-default-features
# --features store-safe` is the COMPLEMENT feature, never a hand-spelled excision
# set, so a future default-on client surface is added to `store-safe` once in
# fauna-ffi and is in this flavor everywhere.
#
# ⚠ This alone does NOT produce an excised app: an excised flavor is "the FFI built
# without the feature PLUS the shell built with the matching condition off" (same
# §). The shell half is `just windows-store-safe`, which takes this as a
# prerequisite; `just windows-store-safe-check` is what keeps both honest.
#
# `release`, not `dev`: this is the flavor a store submission would carry, and the
# witness greps the artifact a submission would carry.
#
# Plain `store-safe`, no extra: the `p2p-share` member's ceremony half
# (`offline-share`) is outside it, and the shell's ceremony glue sits behind
# its own `P2P_SHARE` define (FaunaApp.csproj), so the excised shell compiles
# against a face with no ceremony at all.
windows-ffi-store-safe profile="release" target="": i18n-generate providers-generate
    @just _windows-ffi-flavor "{{profile}}" "store-safe" "{{target}}"

# Shared implementation of the two flavors above.
#
# Unlike android — which has Gradle buildType source sets and stages each flavor
# into its own (`_android-ffi-flavor`) — the WinUI project consumes ONE fixed
# pair of paths (`runtimes/win-arm64/native/fauna_ffi.dll` and
# `FaunaApp.Core/Generated/uniffi/`), so both flavors share a staging slot. That
# is precisely the shape `_android-ffi-flavor`'s comment predicted apple/windows
# would need, and it is why the `.ffi-flavor` marker below is load-bearing
# rather than a convenience: it is the only thing standing between a warm tree
# and a wrong-flavor false-green.
#
# Convention: testing.md § Cross-app e2e conventions point 15 — the automation
# surface is compiled out of release artifacts. Rule (b) there keys any UniFFI
# export of a seam on the FEATURE ALONE, never the profile, so "production
# flavor" is exactly "don't pass test-helpers" and the generated C# face is a
# pure function of `features` below.
_windows-ffi-flavor profile features target:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    # {{profile}} is a cargo PROFILE NAME (`dev` | `release` | `dist`), not an
    # output directory: cargo writes the `dev` profile into `debug/` and every
    # other profile into a dir of its own name. Everything below that touches
    # the filesystem uses $PROFILE_DIR; only the cargo invocation uses $PROFILE.
    PROFILE="{{profile}}"
    FEATURES="{{features}}"
    case "$PROFILE" in
        dev) PROFILE_DIR=debug ;;
        *)   PROFILE_DIR="$PROFILE" ;;
    esac
    # {{target}} is empty (implicit-host arm64, the original behavior) or a cargo
    # target triple. Only x86_64-pc-windows-msvc is wired up today (the x64 cross
    # leg, 2026-08-24) — RID is the .NET runtime-identifier folder name the C#
    # csproj already branches on (FaunaApp.csproj's two `runtimes\win-*\native\`
    # ItemGroups), CARGO_WIN_ARCH selects cargo-win.cmd's matching cl/link/lib env,
    # TARGET_ARG is threaded into the cargo invocation, and CARGO_OUT_DIR mirrors
    # cargo's own target-dir shape (a bare `--target` adds a `<triple>/` path
    # segment that the implicit-host build below deliberately avoids).
    TARGET="{{target}}"
    case "$TARGET" in
        "")                         RID=win-arm64; CARGO_WIN_ARCH=arm64; TARGET_ARG="" ;;
        x86_64-pc-windows-msvc)     RID=win-x64;   CARGO_WIN_ARCH=x64;   TARGET_ARG="--target x86_64-pc-windows-msvc" ;;
        *) echo "_windows-ffi-flavor: unsupported target '$TARGET' (only x86_64-pc-windows-msvc is wired up besides the default arm64 host)" >&2; exit 1 ;;
    esac
    CARGO_OUT_DIR="target${TARGET:+/$TARGET}/$PROFILE_DIR"
    # Flavor guard — mirrors apple-ffi-host's `.ffi-flavor` (§ Host-only debug
    # FFI (mac dev loop)). The app consumes ONE fixed pair of paths regardless
    # of profile (runtimes/win-arm64/native/fauna_ffi.dll + Generated/uniffi),
    # and build-if-stale keys on mtime — so alternating profiles can stage the
    # WRONG flavor: build `dev`, then build `release` off a warm cache, and
    # cargo no-ops, leaving the release .dll with its older mtime while the
    # staged `dev` .dll is newer. The gate reads "fresh", skips the copy, and
    # the app silently links the other flavor's core. Dropping the staged .dll
    # on a flavor switch forces the gate to restage. Marker is gitignored.
    #
    # The marker records PROFILE:FEATURES, not just the profile (2026-08-01,
    # convention-15 recipe split): the feature set is now a second flavor axis
    # over the SAME staging slot, and it is the security-relevant one — a
    # profile-only marker reads "fresh" across a production↔test switch at the
    # same profile and leaves the seam-carrying .dll and bindings staged for a
    # `windows-release`. Same class as android's cargo-stamp trap, one level up.
    FLAVOR_MARKER=apps/fauna-windows/.ffi-flavor
    # Per-(profile,features) cargo freshness stamp — MUST be scoped by both axes,
    # not just PROFILE_DIR: the cargo output path below
    # (target/<triple>/$PROFILE_DIR/fauna_ffi.dll) carries no features, so the
    # production and test-helpers flavors share one file at the same profile.
    # Mirrors `_apple-ffi-host-flavor`'s CARGO_STAMP for the same reason.
    # The FLAVOR-PRIVATE slot this recipe owns, and the only fauna_ffi.dll
    # anything downstream is allowed to read (2026-08-23). The cargo
    # step below builds IMPLICIT-HOST — it no longer passes `--target
    # aarch64-pc-windows-msvc`, because win is arm64 and that IS the host
    # triple, so the flag bought a second artifact tree and nothing else
    # (measured: 0 of 96 workspace-local units shared with the dev inner loop,
    # 50 of 96 after the drop — build-machine-resources.md § Cargo target dir
    # layout (win) → *The host-side builds ask for their own triple*).
    #
    # Joining the shared host tree is the whole win, but it puts this flavor's
    # dll in `target/$PROFILE_DIR/fauna_ffi.dll` — one slot for every host build
    # of fauna-ffi in the workspace, at two levels of risk:
    #
    #   1. THIS recipe's own two flavors. `production` and `test-helpers` are the
    #      convention-15 security seam split, and they'd share one path. That
    #      alone justifies the private slot, and it is live on Windows today.
    #   2. Other recipes, from other feature sets, at the same dev profile:
    #      `ffi-store-safe-check` (`--no-default-features --features store-safe`)
    #      and `android-host-test` (`--features test-helpers`), plus any
    #      bare `cargo build -p fauna-ffi` / `--workspace`. Neither recipe is on
    #      win's merge-gate path (that runs test-compile-check, dotnet test,
    #      windows-debug, windows-release, feature-test-compile-check — and
    #      store-safe's artifact probe is unix-only), so here this is the
    #      hand-run/workspace-build case rather than an automated one.
    #
    # Either way the failure is silent and fail-dangerous: a foreign build leaves
    # the shared dll NEWER while our source-keyed --stamp still reads fresh, so
    # cargo is skipped and the bindgen gate below — keyed on the dll's mtime —
    # regenerates the C# bindings from the WRONG flavor and stages that dll into
    # runtimes/. So: share the BUILD, never the PATH.
    #
    # The copy is deliberately part of the gated build command itself, not a
    # step of its own. A separate `--source shared --target private` gate would
    # re-copy whenever ANOTHER recipe touched the shared slot — i.e. it would
    # import exactly the wrong dll it exists to keep out. Inside the command,
    # the private slot can only ever be written by a build this recipe just ran.
    # The copy is `scripts/win-stage-dll.sh`, not a bare `cp`: the slot's dll
    # may be MAPPED by a live process (a pytest that imported `fauna_ffi`, a
    # second run in this checkout), and windows refuses to overwrite a mapped
    # image — `cp` died `Device or resource busy` and errored a whole windows
    # e2e run after its cold build. The script
    # renames the old file aside (allowed while mapped) and copies into the
    # freed name.
    # RID-scoped (2026-08-24, the x64 leg): the private slot, stamp and staged
    # runtime path all key on RID too, alongside the existing FEATURES axis —
    # same collision class the FEATURES axis comment above already guards
    # against, one axis further. arm64's RID=win-arm64 keeps every existing path
    # byte-identical to before this change.
    FFI_DIR="target/$PROFILE_DIR/windows-ffi/$RID/${FEATURES:-production}"
    FFI_DLL="$FFI_DIR/fauna_ffi.dll"
    mkdir -p "$FFI_DIR"
    CARGO_STAMP="$FFI_DIR/windows-ffi-cargo-${FEATURES:-production}.stamp"
    # `_windows-ffi-bindgen`'s own stamp (see its gate below) lives beside
    # CARGO_STAMP in the same flavor-private FFI_DIR, so it inherits the same
    # RID:PROFILE_DIR:FEATURES keying with no separate discriminator needed —
    # same reasoning as `apple-ffi-host`'s bindgen stamp under $ARTIFACTS.
    BINDGEN_STAMP="$FFI_DIR/windows-ffi-bindgen-${FEATURES:-production}.stamp"
    # Flavor guard (see the long comment above). ALSO drops this flavor's own
    # cargo stamp: because the .dll path is shared across production/test-helpers
    # at the same profile, a stamp keyed only on source mtimes would read "fresh"
    # the moment we switch BACK to this flavor, even though the .dll on disk right
    # now holds the OTHER flavor's build. The marker mismatch is itself the proof
    # a rebuild is owed, independent of what any stamp says. Marker records
    # RID:PROFILE:FEATURES — RID joins the axis for the same reason FEATURES did.
    [ "$(cat "$FLAVOR_MARKER" 2>/dev/null)" = "$RID:$PROFILE:$FEATURES" ] || {
        rm -f "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/$RID/native/fauna_ffi.dll"
        rm -f "$CARGO_STAMP"
    }
    # `test-helpers` (TEST FLAVOR ONLY) exposes the OnboardingMachine snapshot
    # setters (`set_handle_check_snapshot_for_test`, `call_machine_method`), the
    # ConversationsManager injection seams and the LaunchMachine phase setter
    # used by the cross-client E2E bridge. Until 2026-08-01 there was only ONE
    # recipe and it passed `test-helpers` unconditionally, so all 23 seams were
    # exported by the fauna_ffi.dll that `windows-release` packages.
    #
    # Empty `$FEATURES` must not become a bare `--features ''` — cargo accepts
    # it, but keeping the flag off entirely is what makes the production line
    # literally free of the string a reader (and the tier_1 recipe-shape test)
    # greps for.
    #
    # `store-safe` is a THIRD axis, not a feature to ADD: it is the complement
    # feature, so the flavor is `--no-default-features --features store-safe`
    # (dynamic-features.md § The cargo feature spine — one list, never a
    # hand-spelled excision set), the same spelling `_apple-ffi-flavor` and
    # `_android-ffi-flavor` use. It rides the SAME one staging slot as the other
    # two flavors, which is why the `.ffi-flavor` marker above records
    # PROFILE:FEATURES rather than the profile alone: without it a warm tree
    # would serve a payments-carrying fauna_ffi.dll (and its payments-carrying
    # generated C# face) to a store-safe app build, and every absence assertion
    # downstream would read that as a clean excision.
    if [ "$FEATURES" = "store-safe" ]; then
        FEATURE_ARGS="--no-default-features --features store-safe"
    # `store-safe,<extra>` is the complement PLUS a named interim carry (see
    # `_STORE_SAFE_INTERIM_CARRY` in `test_payments_excision_spine.py`, which
    # pins every shell to none today), never a second spelling of the complement.
    elif [ "${FEATURES#store-safe,}" != "$FEATURES" ]; then
        FEATURE_ARGS="--no-default-features --features $FEATURES"
    elif [ -n "$FEATURES" ]; then
        FEATURE_ARGS="--features $FEATURES"
    else
        FEATURE_ARGS=""
    fi
    # Exported, not interpolated: the gated build command below is a SINGLE-quoted
    # `bash -c` script (so the justfile and this shell both leave it alone) and the
    # child expands these itself. Keeping $FEATURE_ARGS unquoted inside that child
    # is what preserves the "empty features must not become `--features ''`" rule
    # above — an empty variable expands to no argument at all. The store-safe
    # branch composes with it unchanged: it sets two words rather than one, and an
    # unquoted expansion passes both.
    export PROFILE PROFILE_DIR FEATURE_ARGS FFI_DLL TARGET_ARG CARGO_WIN_ARCH CARGO_OUT_DIR
    #
    # cargo's incremental build is the staleness gate for the .dll: it tracks
    # fauna-ffi's full transitive source graph, so this no-ops cheaply when
    # fresh and relinks (bumping the .dll mtime) when any input changed.
    # Wrapped in a machine-wide build slot (see the slot_build header) so N
    # parallel win sessions queue this heavy compile instead of thrashing the
    # shared host. The MSYS `//c`→`/c` rewrite still fires at the python3 exec
    # boundary, so cmd.exe receives the right switch.
    #
    # CARGO_INCREMENTAL=1 (`release` only, never `dev` or `dist`): release
    # profiles default incremental OFF, and the FFI-touching rebuild is
    # dominated by rustc re-compiling the fauna-ffi crate itself (~6.5 min
    # measured; the cdylib link is ~8 s). Turning it on for local crates cuts
    # the touch-rebuild ~4× (387 s → 93 s measured) for +3.2 G of caches. The
    # `dev` profile gets NO override: since 2026-08-23 the workspace profile
    # sets `incremental = false` fleet-wide (disk ruling — the cache measured
    # 32-63% of a warm target; build-machine-resources.md § Dev-profile
    # incremental OFF), so the dev leg builds non-incremental like everything
    # else, and the env's fingerprint-flip footgun stays confined to this one
    # release leg. `dist` (shipped artifacts) stays non-incremental by
    # construction.
    # Numbers + rationale: docs/goal/architecture/build-system.md § UniFFI
    # cdylib rebuild cost (win).
    if [ "$PROFILE" = "release" ]; then export CARGO_INCREMENTAL=1; fi
    # `--lib --crate-type cdylib` restricts the emitted artifact to the .dll this
    # recipe actually consumes (`_windows-ffi-bindgen` below reads only
    # fauna_ffi.dll) — a plain `cargo build` would also link the unused `lib`
    # (rlib) and `staticlib` crate-types declared in Cargo.toml (the staticlib
    # alone is ~474 MB) and build the dead `uniffi-bindgen` bin target. Inverts
    # the watchOS staticlib-only shape (justfile:1014, § apple-ffi watchOS
    # slice) — same technique, opposite crate-type. Nothing in the workspace
    # depends on fauna-ffi as a path/rlib dependency, so dropping `lib` is safe.
    # Freshness-gated OUTSIDE the build slot (a warm tree must not queue —
    # build-machine-resources.md § Build/e2e slot locks), the same shape as
    # mail-bridge-ffi-lib / _apple-ffi-host-flavor / windows-debug's own MSBuild
    # step. UNGATED until 2026-08-22: `windows-debug`'s MSBuild leg had been gated
    # since 2026-07-30, but `windows-debug` takes `(windows-ffi-test "dev")` as a
    # PREREQUISITE, so the queue wait simply happened one recipe earlier — every
    # windows e2e run's build phase still took a machine-wide build slot on a fully
    # warm tree. Reading only `windows-debug`'s body wrongly suggests otherwise.
    #
    # --stamp, not the .dll, carries freshness: cargo is incremental, so a no-op
    # leaves the .dll's mtime untouched and a .dll-keyed gate would go permanently
    # stale after any mtime churn that doesn't relink (a rebase does this to the
    # whole tree). The .dll stays a --target existence check, so deleting it — as
    # the flavor guard above does — still forces a rebuild.
    {{py}} scripts/build-if-stale.py --label "windows-ffi-cargo-$PROFILE_DIR-$RID-${FEATURES:-production}" \
        --stamp "$CARGO_STAMP" \
        --target "$FFI_DLL" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} bash -c 'cmd //c "scripts\cargo-win.cmd rustc --locked -p fauna-ffi --lib --crate-type cdylib --profile $PROFILE $FEATURE_ARGS $TARGET_ARG" && bash scripts/win-stage-dll.sh "$CARGO_OUT_DIR/fauna_ffi.dll" "$FFI_DLL"'
    # Gate the expensive bindgen + dll-copy on the freshly-built .dll rather
    # than an enumerated source list: keying on cargo's artifact catches a
    # change in any re-exported sibling crate (fauna-core, fauna-conversations,
    # …) without listing them, and can't silently miss one → no stale bindings.
    # Sources: cargo's freshly-built .dll (catches any fauna-ffi / re-exported
    # sibling-crate change) PLUS the bindgen's own templates + src — a generator
    # change (e.g. a new emit in CallbackInterfaceTemplate/ObjectTemplate) doesn't
    # touch the .dll, so without these the gate would wrongly stay "fresh" and ship
    # bindings generated by the old template.
    # Generated fake bases (2026-07-23) are also `--target`/`--source`'d here,
    # not just emitted inside `_windows-ffi-bindgen`: without the target, a
    # deleted FakeBases.g.cs would wrongly read "fresh"; without the source, an
    # edited generate-windows-fake-bases.py wouldn't re-trigger this unit.
    #
    # --stamp, not the generated targets, carries freshness — the class android hit and fixed first (`git log --grep "a
    # failed bindgen goes green on a re-run"`), apple's twin fix.
    # `_windows-ffi-bindgen` GENERATES its targets first and only THEN runs its
    # seam witness (the `*ForTest` grep + `check-ffi-seam-diff.py` at the tail of
    # that recipe) — so a witness failure leaves the targets already written and
    # newer than the sources. In non-stamp mode the target IS the freshness
    # signal, so the very next invocation reports `up-to-date`, skips the
    # bindgen and exits 0: a real red a plain re-run turns green, with the seam
    # violation still in the generated tree. The stamp is written only on
    # rc == 0, so a failed witness commits nothing and the next run re-runs and
    # fails again; `--target` entries stay existence checks (a deleted
    # `Generated/uniffi` dir still forces a rebuild).
    #
    # ⚠ NOT android's single-axis shape: like apple, this gate's targets are
    # ONE fixed slot the WinUI project consumes regardless of flavor (§
    # `_windows-ffi-flavor`'s own long comment on why the `.ffi-flavor` marker
    # is load-bearing), so a stamp keyed on anything less than every flavor axis
    # would trade this false-green class for a flavor-switch one instead of
    # fixing it. BINDGEN_STAMP lives under FFI_DIR, which already keys on
    # RID:PROFILE_DIR:FEATURES (the same axes the cargo gate's CARGO_STAMP
    # above uses) — no separate discriminator needed.
    {{py}} scripts/build-if-stale.py --label "windows-ffi-bindgen-$RID" \
        --stamp "$BINDGEN_STAMP" \
        --target apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi \
        --target "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/$RID/native/fauna_ffi.dll" \
        --target apps/fauna-windows/FaunaApp/FaunaApp.Tests/Generated/FakeBases.g.cs \
        --source "$FFI_DLL" \
        --source libs/uniffi-bindgen-cs/templates \
        --source libs/uniffi-bindgen-cs/src \
        --source scripts/generate-windows-fake-bases.py \
        -- just _windows-ffi-bindgen "$PROFILE_DIR" "$FEATURES" "$RID"
    # Written only after a successful build+stage, so a crashed or interrupted run
    # leaves the marker on the PREVIOUS flavor and the next invocation still forces
    # a restage rather than trusting a half-built tree (android's rule, same reason).
    echo "$RID:$PROFILE:$FEATURES" > "$FLAVOR_MARKER"

# Regenerate the C# UniFFI bindings + refresh runtimes/ from the built .dll.
# Gated by `_windows-ffi-flavor` on the .dll mtime — don't invoke directly.
# Takes the cargo OUTPUT DIR (`debug` | `release` | `dist`), not the profile
# name — `_windows-ffi-flavor` does the `dev`→`debug` mapping before calling
# this — plus the flavor's feature set, which selects the production assertion
# at the bottom, plus the RID (`win-arm64` default | `win-x64`) that selects
# the flavor's private slot and the app's runtime staging dir (2026-08-24).
_windows-ffi-bindgen profile_dir="release" features="" rid="win-arm64":
    #!/usr/bin/env bash
    set -euo pipefail
    FEATURES="{{features}}"
    RID="{{rid}}"
    # Wipe the previous generation first: uniffi-bindgen-cs only ever WRITES
    # files, so a seam-carrying .cs left over from a test-flavored run would
    # survive a production regeneration that no longer emits it — the whole point
    # of the split, defeated by a stale file. Android's `_android-ffi-bindgen`
    # does the same for the same reason (testing.md convention 15).
    rm -rf apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/
    # The FLAVOR-PRIVATE dll `_windows-ffi-flavor` just produced — NOT the shared
    # `target/<profile>/fauna_ffi.dll`, which `ffi-store-safe-check` and
    # `android-host-test` also write from different feature sets (the long comment
    # in that recipe). Recomputed here rather than passed, so this recipe cannot be
    # invoked against the wrong slot; the `${features:-production}` default is the
    # same one the flavor recipe uses.
    FFI_DLL="target/{{profile_dir}}/windows-ffi/$RID/${FEATURES:-production}/fauna_ffi.dll"
    # Generate C# bindings from the flavor's build (the metadata carried in any
    # slice is platform-independent — bindgen is target-agnostic). The bindgen
    # TOOL itself always builds+runs host-native (arm64) regardless of which
    # flavor's .dll it is reading — it is a build-time executable, never a
    # shipped cross-arch artifact, so CARGO_WIN_ARCH is forced back to arm64
    # here even if the caller's environment carries an x64 flavor's setting.
    # Under a build slot: the `cargo run` compiles
    # the bindgen tool first, a release compile of its own on a cold tree, and
    # the dll gate's slot above was released before this recipe ran. Through
    # `bash -c` like the cdylib build above, so the `//c` is rewritten by the
    # bash that launches cmd itself.
    {{slot_build}} bash -c "cmd //c \"set CARGO_WIN_ARCH=arm64&& scripts\\cargo-win.cmd run -p uniffi-bindgen-cs-fauna --bin uniffi-bindgen-cs --release -- --library $FFI_DLL --out-dir apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/\""
    # Copy the flavor's .dll into the app's runtimes/<RID>/native/ for
    # CopyToOutputDirectory (FaunaApp.csproj branches on RID/Platform per arch).
    mkdir -p "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/$RID/native/"
    cp "$FFI_DLL" \
       "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/$RID/native/"
    # Generated fake bases (2026-07-23; docs/goal/architecture/build-system.md
    # § Merge-gate check (win)): one abstract <X>FakeBase : I<X> per interface just emitted above, so a
    # hand-written test fake only needs to `override` what it implements — a
    # Rust interface growth then compiles clean instead of CS0535. Run as the
    # LAST step here (never a separate gate) so it can never go stale relative
    # to the bindings it reads.
    {{py}} scripts/generate-windows-fake-bases.py
    # The self-maintaining artifact witness (testing.md convention 15; android's
    # `_android-ffi-bindgen` established the shape). The PRODUCTION flavor greps
    # the tree it just generated and fails if any `*ForTest` export survived — so
    # a NEWLY added ungated seam is caught here, with no list for anyone to keep
    # up to date, instead of shipping silently in the next `windows-release`.
    # Derived from the generated bindings rather than the .dll because these are
    # what the C# compiler sees: an export the bindings don't name is unreachable
    # from the app even if the .dll still carried it.
    if [ -z "$FEATURES" ]; then
        LEAKED=$(grep -rlE '\bpublic [A-Za-z0-9_<>?,\[\] ]+ [A-Za-z0-9_]*ForTest\(' \
            apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/ || true)
        if [ -n "$LEAKED" ]; then
            echo "ERROR: the PRODUCTION windows FFI flavor generated e2e seam exports." >&2
            echo "       testing.md convention 15 — these would ship in windows-release." >&2
            echo "       Files: $LEAKED" >&2
            echo "       Fix: gate the seam's uniffi::export on fauna-ffi's test-helpers" >&2
            echo "       feature (never a dep line — rule (b)), then rerun." >&2
            exit 1
        fi
    fi
    # Flavor-diff seam witness (e2e-conventions.md point 15, ratified
    # 2026-08-13) — the grep above sees only *ForTest-NAMED seams; this also
    # pins the non-test-named floor (SetProviderBaseUrls, ResolvedNestDialUrl,
    # CallMachineMethod*, SetDnsCreds, CreateMlsGroup, FfiChildAgentSpawner,
    # the clock-offset pair, …): each floor seam must be declared in the test
    # flavor's face and absent from production's — a dead cfg gate ships the
    # seam into BOTH faces, which the suffix grep above cannot see. Per-tree
    # halves SUFFICE on windows (unlike android's simultaneous buildType
    # staging, which also runs the two-tree difference-coverage assert): every
    # platform's bindings generate from the same fauna-ffi library, so the
    # seam SET is fleet-identical and android's own pair check already polices
    # it — this leg's job is only that WINDOWS' OWN staged face is clean on
    # whichever flavor this run just staged. Do not force a pair check onto
    # the single staging slot (one tree on disk at a time, behind
    # .ffi-flavor) — windows-ffi and windows-ffi-test never run back-to-back
    # with both trees live.
    # The predicate is "does this flavor carry test-helpers", NOT "is $FEATURES
    # empty": `store-safe` is a non-empty PRODUCTION flavor (the App-Store escape
    # hatch — dynamic-features.md § Platform-family surface excision), and an
    # emptiness test would hand it --test-tree and stop asserting the very thing
    # a shipped artifact most needs asserted.
    case ",$FEATURES," in
        *,test-helpers,*) FLAVOR_FLAG=--test-tree ;;
        *)                FLAVOR_FLAG=--production-tree ;;
    esac
    {{py}} scripts/check-ffi-seam-diff.py --lang csharp \
        "$FLAVOR_FLAG" apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/
    echo "Windows FFI bindings regenerated (flavor: ${FEATURES:-production}): runtimes/ + Generated/uniffi/ + Tests/Generated/FakeBases.g.cs"

# Install the pinned uniffi-bindgen-go if this machine lacks it — THE ONE PLACE
# THE PIN IS SPELLED. Both `mail-bridge-ffi` and `mail-bridge-ffi-check` call
# this rather than open-coding the install (they carried two copies, in two
# different profiles, until 2026-08-29; the shipped ci.yml carried a third and
# a cache key holding a fourth, all of which this consolidation retires).
#
# Pin: NordSecurity/uniffi-bindgen-go @ tag v0.7.1+v0.31.0 (commit
# 0b7fb4ceef12). Installed via `cargo install` because Go's module proxy strips
# the `+v0.31.0` semver-build-metadata suffix; the binary lands at
# ~/.cargo/bin/uniffi-bindgen-go.
#
# `--debug`: the bindgen is a code generator that runs for seconds per FFI
# change, so its own optimisation buys nothing — while a release-profile build
# of it is what killed the public repository's first two CI runs (the runner
# was shut down for memory 12–16 min into `cargo install`, 2026-08-28). Debug
# profile: a fraction of the memory and the time. A machine where even that
# does not fit uses `FAUNA_GO_BINDINGS=tracked` and never reaches this recipe.
_uniffi-bindgen-go:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -x "$HOME/.cargo/bin/uniffi-bindgen-go" ]; then exit 0; fi
    {{slot_build}} cargo install uniffi-bindgen-go --locked --debug \
        --git https://github.com/NordSecurity/uniffi-bindgen-go \
        --tag v0.7.1+v0.31.0

# Build fauna-mail-bridge UniFFI Go bindings (regenerates libs/fauna-mail-go).
# `FAUNA_GO_BINDINGS=tracked` builds only the cdylib and consumes the committed
# bindings instead — see the `go_bindings` variable at the top of this file.
# `profile`: the cargo PROFILE fauna-ffi builds at (`release` default, `dist`
# the macOS installer's shipping build — installers/macos.md § Size & build
# profile). Mirrors `apple-ffi-host`/
# `windows-mail-bridge-build`'s own `profile` parameter.
mail-bridge-ffi profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    case "{{go_bindings}}" in
        regenerate|tracked) ;;
        *) echo "FAUNA_GO_BINDINGS must be 'regenerate' or 'tracked', not '{{go_bindings}}'" >&2
           exit 2 ;;
    esac
    case "{{profile}}" in
        release|dist) ;;
        *) echo "mail-bridge-ffi: profile must be 'release' or 'dist', not '{{profile}}'" >&2
           exit 2 ;;
    esac
    if [ "{{go_bindings}}" = regenerate ]; then just _uniffi-bindgen-go; fi
    # Build fauna-ffi with `--no-default-features`: the mail-bridge is a
    # server-side WS-RPC consumer and needs none of the `store-safe` app
    # surface, so the Go binding stays to the symbols it actually calls.
    # (Until 2026-08-23 this carve-out ALSO existed to dodge a uniffi-bindgen-go
    # name collision — `WgTunnelStatus` and `wg_tunnel_status` map to the same
    # Go identifier, where Swift/Kotlin separate them by camel- vs PascalCase.
    # `wireguard` rode `store-safe`, so this flag already dropped it; the
    # WireGuard stack is deleted now and the collision is gone with it, but the
    # hazard is generic to uniffi-bindgen-go — a future Type/fn pair differing
    # only in case will collide the same way.)
    #
    # cargo is incremental: this no-ops when fresh and relinks libfauna_ffi.so
    # (bumping its mtime) only on an actual source/transitive-dep change. So the
    # .so is the precise staleness signal — regenerate the Go bindings iff the
    # lib changed, else skip the bindgen. Keying the staleness gate on the .so
    # OUTPUT (not a libs/*/src glob over the committed sources) is what (a) catches
    # transitive-dep changes cargo sees but a hand-listed src glob misses, and
    # (b) survives `git rebase`: a rebase resets the committed fauna_mail.go's
    # mtime to "now" so a .go-as-target gate wrongly skips, but it does NOT touch
    # the cargo-target .so — and cargo, not mtimes, decides whether to relink it.
    # Mirrors the `apple-ffi` recipe (and `mail-bridge-ffi-check`'s own build).
    #
    # `--features labeler` re-enables the community-labeler WASM executor
    # (`run_wasm_labeler_score` + `mail_to_labeler_input_bare`), which
    # `--no-default-features` would otherwise drop: the content-processor holder
    # is a role of this same mail-bridge binary and IS the executor's server-side
    # consumer (labeler-registry design §6). `mail-bridge-ffi-check` MUST match
    # these flags.
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    # cdylib suffix is platform-specific: .so on Linux, .dylib on macOS. Picked
    # deterministically (not probed): on a fresh tree neither exists yet, and a
    # probe defaulting to .so would hand the gate a target that never appears
    # on macOS, forcing a slot + cargo run forever.
    case "$(uname -s)" in Darwin) LIBNAME=libfauna_ffi.dylib ;; *) LIBNAME=libfauna_ffi.so ;; esac
    # cargo's own output slot — SHARED with every other host build of fauna-ffi
    # AT THIS PROFILE — and this recipe's FLAVOR-PRIVATE copy of it, the only
    # one anything downstream reads. See the `mail-ffi-slot` comment at the top
    # of this file for the collision, the measurement, and why `cp` rather than
    # `--artifact-dir`. `dev` (never passed here — {{profile}} is `release` or
    # `dist`) would map to cargo's `debug/`; named profiles use their own name.
    CARGO_LIB="$TARGET_DIR/{{profile}}/$LIBNAME"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot {{profile}} >"$FFI_SLOT_FILE"
    FFI_DIR="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    LIB="$FFI_DIR/$LIBNAME"
    mkdir -p "$FFI_DIR"
    # The cargo step is freshness-gated OUTSIDE the build slot (a warm tree must
    # not queue — build-machine-resources.md § Build/e2e slot locks). --stamp, not the .so,
    # carries freshness: a cargo no-op leaves the .so untouched, so an
    # .so-keyed gate would go permanently stale on any mtime churn (rebases).
    # The .so stays as an existence check (`cargo clean -p` removes it but not
    # the stamp). libs/fauna-mail-go is EXCLUDED from the sources: it is this
    # pipeline's own output (synced back below), not a cargo input — watching
    # it would re-stale the gate once after every genuine regen.
    # The copy is deliberately part of the gated build command, not a step of its
    # own: a separate `--source $CARGO_LIB --target $LIB` gate would re-copy
    # whenever ANOTHER recipe touched the shared slot — i.e. it would import
    # exactly the wrong cdylib it exists to keep out. Inside the command, the
    # private slot can only ever be written by a build this recipe just ran.
    # `--target` names the PRIVATE copy, so deleting it forces the rebuild.
    export CARGO_LIB LIB
    # Stamped on {{profile}} (both the label and the stamp path) — a
    # single-axis stamp would read "fresh" across a release<->dist switch and
    # serve the wrong-profile artifact into the private slot (the class
    # `_windows-go-cgo-build`'s identical stamp note names).
    {{py}} scripts/build-if-stale.py --label "mail-bridge-ffi-lib-{{profile}}" \
        --stamp "$TARGET_DIR/mail-bridge-ffi-lib-{{profile}}.stamp" \
        --target "$LIB" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} bash -c 'cargo build --locked -p fauna-ffi --profile {{profile}} --no-default-features --features labeler && cp "$CARGO_LIB" "$LIB"'
    # The bindgen stages OUTSIDE libs/ (generator/mtime contract —
    # build-system.md § How the gate works): uniffi-bindgen-go rewrites its
    # out-dir unconditionally, and libs/ is `--source` for all nine wasm chunk
    # gates, so regenerating in place marked every one of them stale even on
    # byte-identical output (observed 2026-07-24: a ~10-minute spurious wasm
    # rebuild in the next web e2e run, presenting as "the harness is slow").
    # The gate keys on the STAGED copy — like the .so it lives in the cargo
    # target dir, which git never touches, so rebases can't skew it. The
    # bindgen binary is a source for the same reason the windows gate watches
    # uniffi-bindgen-cs templates/src: a tool upgrade changes the output
    # without touching the .so.
    STAGING="$TARGET_DIR/fauna-mail-go-bindgen"
    if [ "{{go_bindings}}" = tracked ]; then
        # No bindgen on this machine (see `go_bindings`). The committed tree IS
        # the binding — assert it is actually there rather than letting cgo fail
        # later with an undefined-reference wall that reads like a code bug.
        if [ ! -f libs/fauna-mail-go/fauna_mail/fauna_mail.go ]; then
            echo "FAUNA_GO_BINDINGS=tracked but libs/fauna-mail-go is missing or" >&2
            echo "incomplete — nothing to consume. Unset it to regenerate." >&2
            exit 1
        fi
        echo "mail-bridge-ffi: cdylib built; consuming the TRACKED libs/fauna-mail-go"
        echo "  (FAUNA_GO_BINDINGS=tracked — no bindgen built, no regeneration)."
        echo "  Freshness of the tracked tree is gated by mail-bridge-ffi-check."
        exit 0
    fi
    {{py}} scripts/build-if-stale.py --label mail-bridge-ffi-bindgen \
        --target "$STAGING/fauna_mail/fauna_mail.go" \
        --source "$LIB" \
        --source "$HOME/.cargo/bin/uniffi-bindgen-go" \
        -- just _mail-bridge-ffi-bindgen "$LIB"
    # Committed-tree refresh is UNCONDITIONAL + byte-comparing (mirrors the
    # wasm-onboarding static/ mirror): an unchanged binding keeps its mtime —
    # and the `--source libs` gates stay fresh — while a damaged or missing
    # committed file is re-asserted from staging on every run. `--keep` shields
    # the hand-maintained files `mail-bridge-ffi-check`'s diff also excludes.
    {{py}} scripts/sync-generated-tree.py "$STAGING" libs/fauna-mail-go \
        --keep go.mod --keep go.sum --keep README.md
    # Skeleton go.mod for the vendored bindings module (consumer module
    # uses `replace ... => ../../libs/fauna-mail-go`).
    if [ ! -f libs/fauna-mail-go/go.mod ]; then
        printf 'module github.com/faunasocial/fauna/libs/fauna-mail-go\n\ngo 1.26\n' \
            > libs/fauna-mail-go/go.mod
    fi

# Regenerate the Go UniFFI bindings from the already-built release libfauna_ffi.so
# into the cargo-target STAGING dir; `mail-bridge-ffi` then byte-compare-syncs
# them into libs/fauna-mail-go (generator/mtime contract). Gated by
# `mail-bridge-ffi` on .so freshness — don't invoke directly (it assumes the
# `cargo build -p fauna-ffi` slice is present + current).
# `lib`: the FLAVOR-PRIVATE cdylib to read, passed by the caller rather than
# re-derived here. It used to probe `$TARGET_DIR/release/libfauna_ffi.{so,dylib}`
# / `fauna_ffi.dll` in turn — the SHARED slot every host build of fauna-ffi
# writes, which is how a foreign flavor's cdylib got read and its 26-namespace
# surface synced into the committed tree (see `mail-ffi-slot` at the top of this
# file). One derivation, in the caller that owns the slot, is what keeps the two
# from drifting; the platform suffix is decided there too.
_mail-bridge-ffi-bindgen lib:
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    LIB="{{lib}}"
    STAGING="$TARGET_DIR/fauna-mail-go-bindgen"
    # Fresh staging every regen: the bindgen is not guaranteed to clean its
    # out-dir, and a lingering file here would be synced into the committed
    # tree forever.
    rm -rf "$STAGING"
    mkdir -p "$STAGING"
    "$HOME/.cargo/bin/uniffi-bindgen-go" \
        --out-dir "$STAGING" \
        "$LIB"
    # Adapt the generator's cross-namespace imports to our single-module tree:
    # uniffi-bindgen-go writes a BARE `import "fauna_core"` for a type from
    # another namespace, which is not a resolvable Go module path here. Rewriting
    # it closes the whole class (the alternative — gating each offending export
    # behind a feature the Go build drops — is one-by-one forever and costs the
    # Swift/Kotlin surface those exports serve). Runs on STAGING so the committed
    # tree is what the sync mirrors; `mail-bridge-ffi-check` runs the identical
    # step on its own bindgen output before diffing, or the drift check reds.
    {{py}} scripts/qualify-go-binding-imports.py "$STAGING"
    echo "mail-bridge FFI bindings regenerated into staging: $STAGING"

# Build the Go mail-bridge binary (depends on FFI bindings).
# The generated UniFFI Go bindings #include namespace-local headers and
# resolve symbols against libfauna_ffi.so at link + runtime; CGO_* and
# LD_LIBRARY_PATH are derived from cargo's target dir. `profile`: forwarded to
# `mail-bridge-ffi` (installers/macos.md § Size & build profile); under `dist` the Go binary also links `-trimpath -ldflags '-s -w'`
# (windows parity — `_windows-go-cgo-build`'s identical GOLDFLAGS switch).
mail-bridge-build profile="release": (mail-bridge-ffi profile)
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot {{profile}} >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_CFLAGS="$INCLUDES"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    GOLDFLAGS=""
    if [ "{{profile}}" = "dist" ]; then GOLDFLAGS="-s -w"; fi
    {{slot_build}} go -C bins/fauna-bridges build -trimpath -ldflags="$GOLDFLAGS" ./cmd/fauna-mail-bridge

# The e2e-unified test harness's cdylib slot (`tests/e2e-unified/fauna_ffi.py`)
# — HARNESS-PRIVATE, for the same reason `mail-ffi-slot` above is: the shared
# `target/$PROFILE/libfauna_ffi.*` is the ONE slot every host build of
# fauna-ffi in the workspace writes (`mail-bridge-ffi`, `apple-ffi-host`, a
# bare `cargo build -p fauna-ffi`/`--workspace`), so reading it directly means
# whichever build ran last owns the file the harness loads — measured stale
# 2026-09-25: a `just mail-bridge-ffi` labeler build
# left behind predated a `libs/fauna-ffi/src/cabi.rs` change, and four folder
# e2e tests silently ran the OLD fixture code. One `cabi` submodule IS gated:
# the networked seed exports (`src/cabi/harness.rs`, fauna-ffi's `e2e-harness`
# feature — test surface, e2e convention 15), which `e2e-ffi` below turns on
# and `mail-bridge-ffi` does not. So the private slot is also what keeps the
# flavor right: a `mail-bridge-ffi` build left in the shared slot lacks those
# symbols. One flavor, "harness".
e2e-ffi-slot profile="release":
    @echo "{{profile}}/e2e-ffi/harness"

# Build fauna-ffi for the e2e-unified test harness, build-if-stale-gated.
# `tests/e2e-unified/fauna_ffi.py`'s `_find_cdylib` calls this on EVERY call
# (linux/mac only — win deliberately raises instead of auto-building, `just
# windows-ffi` needing cargo-win.cmd + an explicit --target), not only when
# nothing exists on disk, following the "fixtures trust just to handle
# freshness" rule `nest_binary`/`_ensure_client_built` already follow
# (build-system.md § Test fixtures). Implicit-host, `--no-default-features
# --features labeler,e2e-harness` — `mail-bridge-ffi`'s minimal flavor plus the
# harness's gated networked seed exports (see `e2e-ffi-slot`'s comment) —
# copied into its own private slot so no sibling host build of fauna-ffi can
# clobber it between this gate and the harness loading it.
e2e-ffi profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    case "{{profile}}" in
        release|debug) ;;
        *) echo "e2e-ffi: profile must be 'release' or 'debug', not '{{profile}}'" >&2
           exit 2 ;;
    esac
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    case "$(uname -s)" in Darwin) LIBNAME=libfauna_ffi.dylib ;; *) LIBNAME=libfauna_ffi.so ;; esac
    CARGO_LIB="$TARGET_DIR/{{profile}}/$LIBNAME"
    FFI_SLOT_FILE="$(mktemp)"
    just e2e-ffi-slot {{profile}} >"$FFI_SLOT_FILE"
    FFI_DIR="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    LIB="$FFI_DIR/$LIBNAME"
    mkdir -p "$FFI_DIR"
    PROFILE_FLAG=""
    if [ "{{profile}}" = "release" ]; then PROFILE_FLAG="--release"; fi
    # Same shape as `mail-bridge-ffi`'s cargo step: the copy lives INSIDE the
    # gated command (never its own separately-gated step), so the private
    # slot can only ever be written by a build this recipe just ran — a
    # separate `--source $CARGO_LIB --target $LIB` gate would re-import
    # whichever foreign flavor last touched the shared slot, the exact bug
    # this recipe exists to close.
    export CARGO_LIB LIB
    {{py}} scripts/build-if-stale.py --label "e2e-ffi-{{profile}}" \
        --stamp "$TARGET_DIR/e2e-ffi-{{profile}}.stamp" \
        --target "$LIB" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} bash -c 'cargo build --locked -p fauna-ffi --no-default-features --features labeler,e2e-harness '"$PROFILE_FLAG"' && cp "$CARGO_LIB" "$LIB"'

# CHECK-tier gate (merge-gate-catalog.md): type-check the EXACT flavor `e2e-ffi`
# above builds — same package, same `--no-default-features` feature list, the
# release profile it defaults to — so the harness library cannot stop building
# unseen. The harness module itself (`src/cabi/harness.rs`) is also compiled by
# `feature-test-compile-check` (fauna-ffi's `test-helpers` forwards
# `e2e-harness`), but only on top of the DEFAULT features: an optional
# dependency or a module the minimal flavor does not pull stays invisible there
# and fails here. Without either, the first thing to notice a break is a test
# that needs the library — `RuntimeError: … needs the native fauna-ffi library`
# in whichever tree had to rebuild it, while every tree holding a warm, older
# library keeps passing. A `cargo check`: it builds no artifact, so the harness
# surface still ships from `e2e-ffi` alone (`test_ffi_flavor_split.py` pins that
# the two name one flavor).
e2e-ffi-flavor-check:
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{slot_build}} cargo check --release --locked -p fauna-ffi --no-default-features --features labeler,e2e-harness

# Build the TEST-ONLY seal-helper binary (depends on FFI bindings).
# This is NOT a production artifact — it performs the admin/user
# client-side wrapped-blob seal step so the Python e2e harness (which has
# no in-process Rust binding) can drive it as a subprocess. Production
# sealing happens in the Fauna app UI over the same shared-Rust FFI;
# see bins/fauna-bridges/cmd/seal-helper-testonly/seal.go. The output
# binary lands at bins/fauna-bridges/seal-helper-testonly. Always `release` —
# a test helper never needs `dist`'s shipping optimizations.
seal-helper-build: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_CFLAGS="$INCLUDES"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    {{slot_build}} go -C bins/fauna-bridges build ./cmd/seal-helper-testonly

# Test the Go mail-bridge binary (depends on FFI bindings). The legs live in
# `_mail-bridge-go-test-legs` below, which the Linux merge-gate check's
# `mail-bridge-ffi-check` runs too — a local green here and a gate green are the
# same fact.
mail-bridge-test: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    just _mail-bridge-go-test-legs

# Every `go test` leg of the Go bridge suite on Linux and macOS — the ONE home
# of the leg list, with two callers: `mail-bridge-test` (the dev loop, after
# `mail-bridge-ffi` has built — and by default regenerated — the binding) and
# `mail-bridge-ffi-check` (the Linux merge-gate check, over the TRACKED binding it
# has just verified fresh). Neither caller spells a leg of its own, so a leg
# added here reaches the gate the day it lands. Until 2026-09-19 the gate kept
# its own copy, and that copy had fallen two legs behind: the e2e-flavor leg ran
# on no gate at all (build-parity-gates.md § The UniFFI Go-binding check).
# Builds nothing and writes nothing. The cgo env (CGO_CFLAGS, CGO_LDFLAGS,
# LD_LIBRARY_PATH) comes from the caller, derived from the flavor-private cdylib
# slot it just filled, so the gate's never-writes-the-checkout rule holds
# through this recipe — the same contract as `_go-bridge-test-subset`. win
# carries these legs in `mail-bridge-test-portable` + `mail-bridge-test-win-cgo`.
_mail-bridge-go-test-legs:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    : "${CGO_LDFLAGS:?_mail-bridge-go-test-legs needs the cgo env its caller exports — run just mail-bridge-test}"
    # The two cross-language pins in internal/confinement read
    # bins/fauna-sandbox/src/main.rs — a file OUTSIDE this Go module, which Go's
    # test cache does not track. Measured 2026-08-23: rename a token there,
    # touch no Go file, and the package comes back `(cached)` green while wrong.
    # Run them uncached first (milliseconds, no cgo) so a Rust-side rename
    # cannot land green. Keep this line.
    go -C bins/fauna-bridges test -count=1 -run 'TheRustWrapper' ./internal/confinement/
    # Both cgo-linked legs below (`./...` and the e2e-flavor run) link
    # libfauna_ffi.so through LD_LIBRARY_PATH at run time, which Go's test
    # cache does not hash: a Rust-only change relinks the
    # .so but every cgo-linked package still reads `(cached)`, testing the
    # PREVIOUS .so's behavior as this tree's. Measured 2026-09-15 on Linux — same
    # class as mail-bridge-test-win-cgo's DLL-load finding on Windows, fixed the
    # same way: GOFLAGS rather than a flag, exported around just these two
    # invocations, so the confinement pin above and the go-imap leg below (no
    # cgo) keep their cache.
    export GOFLAGS="${GOFLAGS:+$GOFLAGS }-count=1"
    go -C bins/fauna-bridges test ./...
    # The atproto bridge's e2e-flavor seam files and their tests compile only
    # under `-tags fauna_e2e_fixtures`; `./...` above is the production flavor
    # (seams inert), this line is the e2e flavor (seams work). Convention 15's
    # two-way check, mirrored from `atproto-bridge-test`.
    go -C bins/fauna-bridges test -tags fauna_e2e_fixtures ./internal/atprotoid/ ./cmd/fauna-atproto-bridge/
    unset GOFLAGS
    # FAUNA-FORK: the vendored go-imap server is a separate module (replace
    # directive), so `./...` above stops at its boundary. Run its imapserver
    # tests — they cover the additive CONDSTORE/QRESYNC wire seams (no cgo).
    go -C bins/fauna-bridges/third_party/go-imap test ./imapserver/...

# Fail fast, NAMING what is missing, when the box lacks a prerequisite of the Go
# bridge test suite above: `go`, plus the external executables those tests
# hard-require — today `ffmpeg` and `ffprobe`, which the video projection's tests
# demand with a deliberate `t.Fatalf` rather than a skip, because the projection
# cannot publish video without them. The list is not hand-kept here: a tier_1 pin
# derives it from those `exec.LookPath` guards (helpers/go_bridge_inputs.py's
# `hard_required_tools`), so a test that starts requiring a fourth tool cannot
# leave this line behind.
#
# Its OWN recipe rather than a line inside `mail-bridge-test`, for the reason
# `_windows-go-bridge-test-preflight`'s header gives: a merge-gate check runs the
# preflight as a separate step and routes its failure to INFRA on the exit status
# alone — a box missing a tool has verified nothing, and must never publish a
# code red naming innocent commits. It is deliberately NOT a dependency of
# `mail-bridge-test` either: that recipe is shared with Linux, and on a box
# without ffmpeg a dev is better served by the 36 green packages plus five
# self-describing video failures than by no run at all.
#
# The mac merge-gate check is the intended caller; until that gate is armed this
# is also what a human runs to confirm an install satisfied the suite. Plain
# (non-shebang); host arch only.
_mac-go-bridge-test-preflight:
    @MISSING=""; for tool in go ffmpeg ffprobe; do command -v "$tool" >/dev/null 2>&1 || MISSING="$MISSING $tool"; done; if [ -n "$MISSING" ]; then echo "_mac-go-bridge-test-preflight: MISSING:$MISSING -- install per the machine setup guide (go: the Go mail-bridge section; ffmpeg and ffprobe: the video projection's test prerequisites)" >&2; exit 1; fi; echo "_mac-go-bridge-test-preflight: go, ffmpeg and ffprobe are all present"

# Test the cgo-free subset of the Go bridge packages — no `fauna-ffi` build,
# no CGO_LDFLAGS/rpath/LD_LIBRARY_PATH plumbing, so it runs the same way on
# all three dev machines. Its cgo-linked complement is `mail-bridge-test-win-cgo`
# (below) on Windows, and `mail-bridge-test` above on Linux and macOS, which
# links the host cdylib through rpath/LD_LIBRARY_PATH — mechanisms Windows lacks
# (build-parity-gates.md § The UniFFI Go-binding check names which half runs
# where). Deliberately a plain (non-shebang) recipe so it also works from a bare
# win PowerShell prompt, not just Git Bash — which is why the classifier it
# calls is plain too. Together the two win recipes carry every leg
# `mail-bridge-test` runs (the legs live in `_mail-bridge-go-test-legs`): the
# uncached confinement pins (see the "Keep this line" note there) and the
# vendored go-imap module are cgo-free, so they are
# mirrored here verbatim; `./...` and the `fauna_e2e_fixtures` e2e-flavor leg
# are split between the two halves by `_go-bridge-test-subset` (pinned by
# tests/e2e-unified/tests/test_go_bridge_win_test_reach.py).
mail-bridge-test-portable:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    go -C bins/fauna-bridges test -count=1 -run 'TheRustWrapper' ./internal/confinement/
    just _go-bridge-test-subset portable "" ./...
    just _go-bridge-test-subset portable fauna_e2e_fixtures ./internal/atprotoid/ ./cmd/fauna-atproto-bridge/
    go -C bins/fauna-bridges/third_party/go-imap test ./imapserver/...

# Test the cgo-linked Go bridge packages on Windows — the half
# `mail-bridge-test-portable` leaves out. win-only (Go + llvm-mingw + the gnullvm
# Rust target, like `_windows-go-cgo-build`) and Git-Bash-only: unlike portable
# this is a shebang recipe, and `just` resolves a shebang's interpreter through
# Git Bash's `cygpath`, which a bare PowerShell PATH does not carry.
#
# It links exactly as the shipped MDA does — `_windows-go-cgo-env` builds the
# gnullvm fauna_ffi.dll and emits the cgo env — and differs from the build
# recipes in one place, which is the whole trick. Windows has no rpath/
# LD_LIBRARY_PATH, but its DLL search does fall through to PATH after the exe's
# own dir, and under `go test` that dir is a temp `$WORK` dir, so staging the
# DLL beside the exe (what the build recipes do) is not available. Prepending
# the gnullvm profile dir to PATH is what lets every test exe load
# fauna_ffi.dll; libunwind.dll already resolves through llvm-mingw's bin dir,
# which is on PATH for `CC` to resolve at all. The entry goes through `cygpath
# -u`: under `just`, `$(pwd)` (hence `REL`) is the mixed `D:/src/…` form, whose
# drive colon splits a bash PATH entry in two — `D` and `/src/…`, which MSYS
# then maps under the Git install dir — so a bare `$REL:$PATH` links green and
# fails every test exe with 0xc0000135 (STATUS_DLL_NOT_FOUND), measured
# 2026-09-14. Host arch only (aarch64/arm64): a test exe has to run here, not
# ship elsewhere.
mail-bridge-test-win-cgo:
    #!/usr/bin/env bash
    set -euo pipefail
    ENV_FILE="$(mktemp)"
    just _windows-go-cgo-env aarch64-pc-windows-gnullvm arm64 release >"$ENV_FILE"
    . "$ENV_FILE"
    rm -f "$ENV_FILE"
    export PATH="$(cygpath -u "$REL"):$PATH"
    # Uncached, deliberately. Every test here runs against fauna_ffi.dll, which the
    # OS loader finds through PATH at run time — outside what Go's test cache
    # hashes — so a Rust-only change rebuilds the DLL yet leaves each test result
    # cached, and a cached run reports the PREVIOUS DLL's results as this tree's:
    # measured 2026-09-14, every cgo package read `(cached)` straight after a 7m51s
    # gnullvm `fauna-ffi` rebuild. The portable half loads no DLL, so its cache
    # stays sound. GOFLAGS rather than a flag on the helper: `go list` (the
    # classifier) ignores a test-only flag in GOFLAGS; `go test` applies it.
    export GOFLAGS="${GOFLAGS:+$GOFLAGS }-count=1"
    just _go-bridge-test-subset cgo "" ./...
    just _go-bridge-test-subset cgo fauna_e2e_fixtures ./internal/atprotoid/ ./cmd/fauna-atproto-bridge/

# The whole Go bridge suite on Windows, in one recipe: the toolchain preflight, then
# the cgo-free half, then the cgo-linked half — cheapest first, so a break the
# portable half can name (~1 min warm, no cargo) never waits on the gnullvm
# `fauna-ffi` build (11m36s cold) to be reported. Git-Bash-only, because the cgo
# half is. Gate 9 of the win merge-gate check runs exactly this, so a local green
# and a gate green are the same fact (merge-gate-check.md § Merge-gate check
# (win); the dependency order is pinned by test_merge_gate_check_win.py).
mail-bridge-test-win: _windows-go-bridge-test-preflight mail-bridge-test-portable mail-bridge-test-win-cgo

# Fail fast, NAMING what is missing, when win lacks a prerequisite of the two Go
# bridge test halves: `go`; `ffmpeg` + `ffprobe` (the portable half's video tests
# hard-fail without them, by design — see the machine setup guide); llvm-mingw's C
# compiler and the gnullvm Rust target (the cgo half, whose names are owned by
# `_windows-go-cgo-env` and `mail-bridge-test-win-cgo` — a tier_1 pin keeps this
# list in lockstep with them). The target is checked for the toolchain THIS
# checkout's rust-toolchain.toml selects: it is not in that file's `targets`, so a
# pin bump drops it. Its own recipe rather than a line inside the halves because
# the merge-gate check runs it as a separate step and routes its failure to
# INFRA: a box missing a tool has verified nothing, and must never publish a code
# red naming innocent commits. Plain (non-shebang); host arch only.
_windows-go-bridge-test-preflight:
    @MISSING=""; for tool in go ffmpeg ffprobe aarch64-w64-mingw32-clang; do command -v "$tool" >/dev/null 2>&1 || MISSING="$MISSING $tool"; done; rustup target list --installed 2>/dev/null | tr -d '\r' | grep -qx aarch64-pc-windows-gnullvm || MISSING="$MISSING rust-target:aarch64-pc-windows-gnullvm"; if [ -n "$MISSING" ]; then echo "_windows-go-bridge-test-preflight: MISSING:$MISSING -- install per the machine setup guide (Go + llvm-mingw: the Go mail-bridge cgo build section; ffmpeg/ffprobe: the E2E test dependencies; the target: rustup target add aarch64-pc-windows-gnullvm)" >&2; exit 1; fi; echo "_windows-go-bridge-test-preflight: go, ffmpeg, ffprobe, aarch64-w64-mingw32-clang and the aarch64-pc-windows-gnullvm target are all present"

# Run `go test` over ONE half of the `bins/fauna-bridges` packages `patterns`
# match, split on whether a package's TEST binary links `fauna-mail-go` (hence
# cgo + the `fauna_ffi` cdylib): `subset` is `cgo` or `portable`, `tags` a Go
# build-tag list or "". The ONE classifier behind both win halves above, derived
# from the import graph each run rather than hand-listed, so a newly-added
# package lands in the right half with no edit here. `-test` because a
# `_test.go` importing the cgo tree links it exactly as a production import
# does; `-e` because only the import edge matters, so a package cgo cannot build
# in the caller's env is still classified rather than aborting the run. The env
# (cgo vars, PATH) is the caller's. Plain, not shebang: portable's PowerShell
# reach depends on it. An empty half prints a line instead of letting a bare
# `go test` fall back to the module root.
_go-bridge-test-subset subset tags +patterns:
    @case "{{subset}}" in cgo|portable) ;; *) echo "_go-bridge-test-subset: subset must be 'cgo' or 'portable', not '{{subset}}'" >&2; exit 2 ;; esac
    @TAGS=""; [ -z "{{tags}}" ] || TAGS="-tags {{tags}}"; LIST_FILE="$(mktemp)"; DEPS_FILE="$(mktemp)"; trap 'rm -f "$LIST_FILE" "$DEPS_FILE"' EXIT; go -C bins/fauna-bridges list $TAGS {{patterns}} >"$LIST_FILE" || exit 1; SEL=""; while read -r pkg; do [ -z "$pkg" ] && continue; go -C bins/fauna-bridges list -e $TAGS -test -deps "$pkg" >"$DEPS_FILE" || exit 1; if grep -q fauna-mail-go "$DEPS_FILE"; then HALF=cgo; else HALF=portable; fi; if [ "$HALF" = "{{subset}}" ]; then SEL="$SEL $pkg"; fi; done <"$LIST_FILE"; if [ -z "$SEL" ]; then echo "_go-bridge-test-subset: no {{subset}} packages among {{patterns}}"; else echo "go test $TAGS ({{subset}}):$SEL"; {{slot_build}} go -C bins/fauna-bridges test $TAGS $SEL; fi

# ── the App-Store escape hatch: the store-safe (payments-excised) flavor ─────
#
# `docs/goal/architecture/dynamic-features.md` § The App-Store escape hatch —
# "from 'Apple blocks the update over feature X' to 'excised iOS build
# submitted' in HOURS, not days". That promise is only real if the flavor is
# built and checked continuously, so this recipe is the KEPT-GREEN half: it
# builds fauna-ffi both ways for the HOST target (no Apple toolchain, so every
# machine can run it) and witnesses the difference on the artifacts themselves.
#
# THREE columns, deliberately. A one-column "0 payments strings" assertion
# passes vacuously if the grep pattern is wrong, the build silently no-ops, or
# someone renames the kinds — the same trap `atproto-bridge-build-e2e`'s
# positive witness exists to close. The third column is the SUBSET relation: the
# registry has two members on this artifact and `zaps = ["payments"]`, so the
# interesting flavor is neither extreme but the middle one — payments kept, zaps
# gone — which is literally what the Damus precedent describes (Apple forced the
# zap button off posts while the rest of the app stayed). Without it, nothing
# distinguishes a real subset edge from `zaps` being an alias of `payments`.
#
# Measured 2026-08-10 on these three:
#   default    = 12 `fauna.payments.*` + 1 `fauna.tips.*` + 183 `FfiPaymentsClient`
#                + 7 `fauna.nostr.zap_signers.` + 121 `FfiNostrZapSignerClient`
#   damus      = payments present, 0 zap strings
#   store-safe = 0 across the board
#
# ⚠ CRITERION 1 IS SCANNED HERE TOO, and it was the missing axis. The old reading was that a library root
# has no element id to leak — that is FALSE for a UniFFI cdylib: UniFFI embeds
# Rust docstrings in the library metadata, so a doc comment naming a gated
# element id ships in the artifact and in the generated Swift/Kotlin face. It
# was live, not hypothetical: a store-safe `libfauna_ffi.so` carried 9
# `post-tip-` + 1 `subscription-claim-` lines, every one a docstring on a
# deliberately-UNGATED inert surface (`TipView`/`TipSenderView`,
# `value_format`'s tip + claim formatters). The android column had to narrow
# criterion 1 to `classes*.dex` to work around exactly this residue. Fixed at
# the source by gating the id-naming doc lines (`#[cfg_attr(feature =
# "payments", doc = …)]`), and pinned here so it cannot come back —
# `dynamic-features.md` § What "completely compiled away" means, criterion 1
# ("prose included"), with the id↔member mapping in ui.yaml's `gated_features:`.
#
# The DEFAULT column asserts these ids PRESENT, and the carrier being a
# docstring is the point rather than a weakness: it is what proves the absence
# in column 1 comes from the cfg_attr gate rather than from the ids having
# never been in this artifact at all. Reword a gated doc line and this column
# goes red — correctly, telling you the witness needs updating.
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). Every column builds an
# artifact into the cargo-target dataset and then scans it with `strings`, so
# each column is a produce→consume pair. Between two separate acquisitions the
# checkout holds no slot at all, and the pressure tier evicts exactly there —
# measured twice on 2026-08-20. The body's own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
ffi-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _ffi-store-safe-check-impl

_ffi-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    LIB="$TARGET_DIR/debug/libfauna_ffi.so"
    [ -f "$TARGET_DIR/debug/libfauna_ffi.dylib" ] && LIB="$TARGET_DIR/debug/libfauna_ffi.dylib"
    # A `strings` scan of a VANISHED artifact is an EMPTY scan, and every
    # absence assertion below reads an empty scan as a clean excision — a
    # FALSE GREEN on the excision gate rather than a loud failure. So each
    # column asserts its artifact the moment its build returns: if it is gone,
    # the cargo-target dataset was reclaimed under us (build-machine-resources.md
    # § Build/e2e slot locks → the slot is the recipe's dataset lease).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # `zaps` is a SUBSET member of `payments`, so it excises with the money
    # plane in the store-safe column and on its own in the Damus column.
    ZAP_PATS=('fauna\.nostr\.zap_signers\.' 'FfiNostrZapSignerClient')
    # Criterion 1, the axis this column lacked until 2026-08-18 (see the header).
    # PREFIXES from ui.yaml's `gated_features.payments.id_prefixes`, so a new
    # §4/§5/tip element is covered the day it is added.
    #
    # A SUBSET of the catalog, and measured rather than assumed (2026-08-18, on
    # the default flavor of this very artifact). One payments catalog prefix is
    # deliberately NOT here because it matches nothing in EITHER column, which is
    # exactly the vacuity the default column exists to prevent:
    #   * `subscription-provider-` — 0. No doc comment in fauna-ffi's graph names
    #     a §4 provider id; `provider_status_label`'s docs describe the badge
    #     without spelling it.
    # It joins the day a doc comment here names it.
    # `compose-sell-asking-price` is a FULL id, and the first of the price-and-route
    # class's doc-comment carriers in this graph to be watched here
    # (`dynamic-features.md` § Platform-family surface excision → *The
    # price-and-route class*): `fauna-feed`'s `SellComposeState::asking_price`
    # field doc spells it, a `payments`-gated doc line since 2026-10-07 — and it
    # had been riding every store-safe FFI artifact unmeasured since the id was
    # catalogued on 2026-08-22. Its five sibling carriers (`compose-sell-price`,
    # `compose-sell-subscribers-free`, `gated-post-price`, `gated-post-payment-link`,
    # `gated-post-buy-button` — the same record docs plus `FfiFeedManager`'s method
    # docs, gated the same day) join this array the day the catalog carries them:
    # the spine pin holds this array to the payments catalog, and that entry lands
    # with the last family's gates (the ruling's *Catalog shape and sequencing*).
    PAY_ID_PATS=('subscription-claim-' 'post-tip-' 'compose-sell-asking-price')
    # The `p2p-share` member, its own axis (2026-09-28): the ceremony half
    # (`offline-share`) left `store-safe` for `p2p-share`, the plane half
    # (`share_plane`) was never in it. All FOUR catalog prefixes are live here —
    # measured on the default debug cdylib: `offline-share-` 7, `offline-receive-`
    # 2, `share-transfer-` 5, `share-serve-` 1, all carried by the member's own
    # p2p-share-gated modules (`fauna-client-capabilities`' `group_ceremony_view`
    # docstrings among them). The store-safe column is what proves that gating
    # the whole feature removes them, with no `cfg_attr` line owed.
    # The faces are the UniFFI exports and records a shell calls, spelled as the
    # export SYMBOL: a bare `offline_share_` also matches the shared i18n
    # catalog's `folders.offline_share_*` keys, which ship in every flavor (3 in
    # store-safe, measured 2026-09-28) and are neither an element id nor a face.
    # `group_ceremony` is NOT a pattern for the same reason: that module is
    # ungated shared logic the store-safe graph legitimately links.
    P2P_ID_PATS=('offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-')
    P2P_FACE_PATS=('uniffi_fauna_ffi_fn_func_offline_share_' 'FfiCeremonySeat' 'share_plane_view' 'FfiSharePlaneView')

    absent() {  # absent <flavor-label> <pattern>...
        local flavor="$1"; shift
        for pat in "$@"; do
            n="$(strings -a "$LIB" | grep -c "$pat" || true)"
            if [ "$n" != "0" ]; then
                echo "ERROR: the $flavor fauna-ffi flavor still carries $n '$pat' occurrence(s)." >&2
                echo "  That plane must be COMPLETELY compiled away (dynamic-features.md" >&2
                echo "  § What \"completely compiled away\" means: criterion 1 no element ids," >&2
                echo "  criterion 2 no wire senders, criterion 5 no re-enable path — and BOTH" >&2
                echo "  of the first two are \"absent means absent, PROSE INCLUDED\")." >&2
                echo "  TWO causes, and the second is the one that looks impossible:" >&2
                echo "  (1) a crate in fauna-ffi's graph re-enabled the feature through cargo's" >&2
                echo "      additive unification — see fauna-protocol's manifest comment." >&2
                echo "      Check with: cargo tree -p fauna-ffi -e features -i fauna-protocol" >&2
                echo "  (2) a DOC COMMENT on an UNGATED #[uniffi::export]/uniffi::Record item" >&2
                echo "      names the kind or the element id: UniFFI embeds docstrings in the" >&2
                echo "      library metadata, so prose ships in the artifact and in the generated" >&2
                echo "      Swift/Kotlin face. This broke the KIND axis on 2026-08-11 (TipView)" >&2
                echo "      and was the whole of the ELEMENT-ID residue fixed 2026-08-18." >&2
                echo "      Fix: gate the id-naming doc line, do NOT reword it —" >&2
                echo "        #[cfg_attr(feature = \"payments\", doc = \" …post-tip-total…\")]" >&2
                echo "      which keeps the id greppable in source and out of the artifact." >&2
                echo "      Confirm which you have — the string itself says so:" >&2
                echo "      strings -a \$LIB | grep '$pat'" >&2
                exit 1
            fi
        done
    }

    present() {  # present <flavor-label> <pattern>...
        local flavor="$1"; shift
        for pat in "$@"; do
            n="$(strings -a "$LIB" | grep -c "$pat" || true)"
            if [ "$n" = "0" ]; then
                echo "ERROR: the $flavor fauna-ffi flavor carries no '$pat' — the absence" >&2
                echo "  assertions in the other columns are therefore vacuous. Either the plane" >&2
                echo "  was removed from that build (a product regression) or the strings were" >&2
                echo "  renamed and this witness needs its patterns updated." >&2
                exit 1
            fi
        done
    }

    # --- column 1: store-safe must reach neither money nor zaps --------------
    {{slot_build}} cargo build --locked -p fauna-ffi --no-default-features --features store-safe
    built "$LIB"
    absent "store-safe" 'fauna\.payments\.' 'fauna\.tips\.' 'FfiPaymentsClient' "${ZAP_PATS[@]}" "${PAY_ID_PATS[@]}" "${P2P_ID_PATS[@]}" "${P2P_FACE_PATS[@]}"

    # --- column 2: the Damus flavor keeps payments, drops zaps ---------------
    # The subset edge, witnessed on the artifact rather than asserted from the
    # manifest: `--features store-safe,payments` is a build that can still take
    # a card and cannot touch Lightning.
    {{slot_build}} cargo build --locked -p fauna-ffi --no-default-features --features store-safe,payments
    built "$LIB"
    absent "Damus (payments-without-zaps)" "${ZAP_PATS[@]}" "${P2P_ID_PATS[@]}" "${P2P_FACE_PATS[@]}"
    present "Damus (payments-without-zaps)" 'fauna\.payments\.' 'FfiPaymentsClient' "${PAY_ID_PATS[@]}"

    # --- column 3: the DEFAULT flavor must still ship both -------------------
    # Without this the absences above are indistinguishable from greps that
    # match nothing, and the escape hatch would read "green" while excising
    # planes that were never there.
    {{slot_build}} cargo build --locked -p fauna-ffi
    built "$LIB"
    present "DEFAULT" 'fauna\.payments\.' 'FfiPaymentsClient' "${ZAP_PATS[@]}" "${PAY_ID_PATS[@]}" "${P2P_ID_PATS[@]}" "${P2P_FACE_PATS[@]}"

    echo "ffi-store-safe-check: OK — payments+zaps+p2p-share in default, payments-only in damus, none in store-safe (kind strings, FFI faces AND element ids)"

# The NEST column of the same witness (`dynamic-features.md` § The feature-matrix
# test story). `ffi-store-safe-check` above keeps the CLIENT flavor honest; this
# keeps the server one honest, and it is needed for a reason the client column
# does not have: `payments` is default-ON at the nest, so NEITHER arm of
# `nest-lib-test-check` (bare-default, then the all-features union) ever compiles
# the excised nest. Without this recipe the whole nest-side excision is executed
# by no gate anywhere — precisely the "compile-gating without
# test-gating" failure that section exists to prevent.
#
# Two-column for the same reason as the ffi one: the positive column is what
# makes the negative one mean something. Note the excised flavor is spelled with
# the COMPLEMENT feature (`--no-default-features --features store-safe`), never a
# hand-listed set — so a future default-on nest surface is added to `store-safe`
# once and stays in both flavors by construction. Measured 2026-08-10 on this
# exact pair: default = 4 `fauna.payments.*` + 4 `fauna.tips.*` +
# 2 `fauna.nostr.zap_signers.*` + 1 `nostr.zaps.total`; store-safe = 0 across all
# four.
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). Every column builds an
# artifact into the cargo-target dataset and then scans it with `strings`, so
# each column is a produce→consume pair. Between two separate acquisitions the
# checkout holds no slot at all, and the pressure tier evicts exactly there —
# measured twice on 2026-08-20. The body's own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
nest-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _nest-store-safe-check-impl

_nest-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    BIN="$TARGET_DIR/debug/fauna-nest"
    # A `strings` scan of a VANISHED artifact is an EMPTY scan, and every
    # absence assertion below reads an empty scan as a clean excision — a
    # FALSE GREEN on the excision gate rather than a loud failure. So each
    # column asserts its artifact the moment its build returns: if it is gone,
    # the cargo-target dataset was reclaimed under us (build-machine-resources.md
    # § Build/e2e slot locks → the slot is the recipe's dataset lease).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # --- column 1: the store-safe nest must not be able to reach money --------
    {{slot_build}} cargo build --locked -p fauna-nest --bin fauna-nest --no-default-features --features store-safe
    built "$BIN"
    # The zap patterns are in the loop because the nest's `zaps` feature forwards
    # `fauna-protocol/zaps` (W1.5 (account-data-plane.md § Workstreams)'s shared-crate leg). Measured why that forward
    # matters: this recipe's FIRST run — nest handlers gated, forward not yet
    # possible — reported payments/tips already at 0 but 1 surviving
    # `fauna.nostr.zap_signers.` occurrence, coming from `fauna-protocol`'s wire
    # types rather than from any nest code. Gating handlers is not excising a plane.
    for pat in 'fauna\.payments\.' 'fauna\.tips\.' 'fauna\.nostr\.zap_signers\.' 'nostr\.zaps\.total'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe fauna-nest flavor still carries $n '$pat' occurrence(s)." >&2
            echo "  The payments plane must be COMPLETELY compiled away (dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means: no wire senders, no re-enable path)." >&2
            echo "  Two likely causes: (1) a nest dep re-enabled fauna-protocol's \`payments\`" >&2
            echo "  through cargo's additive unification — check with" >&2
            echo "  \`cargo tree -p fauna-nest -e features -i fauna-protocol\`, which must show" >&2
            echo "  fauna-nest itself as the ONLY enabler; (2) a kind string was added outside" >&2
            echo "  the gated modules (the bridge_method_allowlist arms are the known site)." >&2
            exit 1
        fi
    done

    # --- column 2: the DEFAULT nest must still ship it ------------------------
    # Without this the check above is indistinguishable from a grep that matches
    # nothing, and the escape hatch would read "green" while excising a plane
    # that was never there.
    {{slot_build}} cargo build --locked -p fauna-nest --bin fauna-nest
    built "$BIN"
    for pat in 'fauna\.payments\.' 'fauna\.tips\.'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT fauna-nest flavor carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the payments plane was" >&2
            echo "  removed from the default build (a product regression) or the kind" >&2
            echo "  strings were renamed and this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    echo "nest-store-safe-check: OK — payments present in default, absent in store-safe"

# Artifact witness for the nest's unauthenticated `/api/v1/test/*` automation
# surface (docs/goal/architecture/e2e-automation-surface-gating.md § The
# convention 15's "reads the built artifact rather than the source" half —
# the nest was the one artifact cited throughout that doc as apps' precedent
# while never itself carrying the witness). Same
# two-column strings-scan shape as `nest-store-safe-check` just above, same
# `built()` false-green guard, same `CARGO_RESOLVER_FEATURE_UNIFICATION=selected`
# flavor discipline — but the columns run the OPPOSITE direction: the
# automation surface must be ABSENT from the default flavor and PRESENT only
# under `--features test-hooks` (every `*_test_hook.rs` module is
# `#![cfg(feature = "test-hooks")]`, not in `default`). Not `nest-testhooks-check`
# (no hyphen between "test" and "hooks") — that recipe RUNS specific test-hooks
# integration targets; this one SCANS the built binary for the route prefix.
nest-automation-surface-check: i18n-generate providers-generate
    {{slot_build}} just _nest-automation-surface-check-impl

_nest-automation-surface-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    BIN="$TARGET_DIR/debug/fauna-nest"
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # --- column A: the DEFAULT nest must ship with NO automation surface ------
    {{slot_build}} cargo build --locked -p fauna-nest --bin fauna-nest
    built "$BIN"
    n="$(strings -a "$BIN" | grep -c '/api/v1/test/' || true)"
    if [ "$n" != "0" ]; then
        echo "ERROR: the DEFAULT fauna-nest flavor carries $n '/api/v1/test/' occurrence(s)." >&2
        echo "  Every *_test_hook.rs module is #![cfg(feature = \"test-hooks\")], which is" >&2
        echo "  NOT in fauna-nest's default feature set — this string must be completely" >&2
        echo "  compiled away from a released nest binary" >&2
        echo "  (e2e-automation-surface-gating.md § The convention). Check for a dep that" >&2
        echo "  re-enabled test-hooks additively: cargo tree -p fauna-nest -e features -i fauna-mls." >&2
        exit 1
    fi

    # --- column B: the test-hooks nest must still carry it --------------------
    # Without this the column-A absence assertion is indistinguishable from a
    # grep that matches nothing, and the gate would read "green" for a prefix
    # that was renamed or a feature that no longer wires any routes.
    {{slot_build}} cargo build --locked -p fauna-nest --bin fauna-nest --features test-hooks
    built "$BIN"
    n="$(strings -a "$BIN" | grep -c '/api/v1/test/' || true)"
    if [ "$n" = "0" ]; then
        echo "ERROR: the --features test-hooks fauna-nest flavor carries no" >&2
        echo "  '/api/v1/test/' — the default-flavor absence assertion above is" >&2
        echo "  therefore vacuous. Either the automation surface's route prefix was" >&2
        echo "  renamed (update this gate's pattern) or the test-hooks feature no" >&2
        echo "  longer wires any routes." >&2
        exit 1
    fi
    echo "nest-automation-surface-check: OK — /api/v1/test/ absent from default, present under --features test-hooks"

# Build the excised tui — the App-Store escape hatch's Rust-app shell flavor
# (`dynamic-features.md` § Platform-family surface excision). The artifact a
# store submission would carry; `tui-store-safe-check` is what keeps it honest.
tui-store-safe: i18n-generate
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{slot_build}} cargo build --locked -p fauna-tui --bin fauna-tui --release --no-default-features --features store-safe

# The APP-SHELL column of the store-safe family (`dynamic-features.md` § The
# feature-matrix test story), and the first one that asserts criterion 1.
#
# WHY IT IS NOT REDUNDANT WITH THE OTHER THREE. `ffi-store-safe-check`,
# `nest-store-safe-check` and `wasm-store-safe-check` all witness a *library or
# server* root: they prove the plane's KIND STRINGS (criterion 2) are gone from
# something an app calls. None of them compiles an app SHELL, and an app shell
# is where criterion 1 — "**No UI surface** — the feature's element IDs absent"
# — actually lives. The two criteria fail independently, and the interesting
# direction is the one this recipe alone can see: a shell whose payments RENDER
# survives while every wire sender it would have called is already gone. That
# is not hypothetical, it is the default outcome of gating only the dep —
# `PostSummary.tips` is a deliberately UNGATED inert record that stays `None`
# forever in this flavor (`fauna-feed/src/snapshot.rs`), so the tip section is
# already dead code in a store-safe build and still ships `post-tip-total`,
# `post-tip-count` and `post-tip-list-button` unless the render carries its own
# `#[cfg]`. Dead is not absent. `TipView` sprang exactly this trap on
# `ffi-store-safe-check` (2026-08-11): red with no sender anywhere in the graph.
#
# Element-id patterns are PREFIXES, not whole ids, so a new §4/§5 or tip element
# is covered on the day it is added rather than the day someone remembers to
# extend this list.
# `share-transfer-` / `share-serve-` (p2p-share) and `nostr-zap-signer-` (zaps)
# joined 2026-08-25: ui.yaml's `gated_features:` catalog grew these
# three prefixes on 2026-08-18/2026-08-22 without any witness ever watching
# them — this shell is where the real render lives for all three
# (`src/settings/folders.rs`, `src/share_glue.rs`, `src/nostr.rs`, each
# `#[cfg(feature = "p2p-share"|"zaps")]`-gated), so it is the natural column to
# close the gap rather than adding a seventh witness.
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). Every column builds an
# artifact into the cargo-target dataset and then scans it with `strings`, so
# each column is a produce→consume pair. Between two separate acquisitions the
# checkout holds no slot at all, and the pressure tier evicts exactly there —
# measured twice on 2026-08-20. The body's own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
tui-store-safe-check: i18n-generate
    {{slot_build}} just _tui-store-safe-check-impl

_tui-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    BIN="$TARGET_DIR/debug/fauna-tui"
    # A `strings` scan of a VANISHED artifact is an EMPTY scan, and every
    # absence assertion below reads an empty scan as a clean excision — a
    # FALSE GREEN on the excision gate rather than a loud failure. So each
    # column asserts its artifact the moment its build returns: if it is gone,
    # the cargo-target dataset was reclaimed under us (build-machine-resources.md
    # § Build/e2e slot locks → the slot is the recipe's dataset lease).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # --- column 1: the store-safe tui carries neither the UI nor the wire -----
    {{slot_build}} cargo build --locked -p fauna-tui --bin fauna-tui --no-default-features --features store-safe
    built "$BIN"
    # The twelve FULL ids after the prefixes are the price-and-route class
    # (dynamic-features.md § Platform-family surface excision → *The
    # price-and-route class*): listed whole because their natural prefixes
    # (`subscription-offer-`, `subscription-tier-`, `gated-post-`,
    # `compose-sell-`) also cover ids outside the plane. Hand-listed here until
    # the catalog entry lands with the last family's gates (that section says why
    # the entry comes last).
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'fauna\.payments\.' 'fauna\.tips\.' 'offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-' 'fauna\.peer\.share\.' 'nostr-zap-signer-' 'subscription-tier-form-price-hint' 'subscription-tier-form-asking-price' 'subscription-tier-form-payment-url' 'compose-sell-price' 'compose-sell-asking-price' 'compose-sell-subscribers-free' 'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' 'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe fauna-tui flavor still carries $n '$pat' occurrence(s)." >&2
            echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            echo "  The likely cause is a RENDER that compiles fine because the data it" >&2
            echo "  reads is an ungated inert record (PostSummary.tips is the known one)," >&2
            echo "  so it paints nothing and still ships every id it would have painted." >&2
            echo "  Gate the render itself with #[cfg(feature = \"payments\")], not just" >&2
            echo "  the resolver that feeds it." >&2
            exit 1
        fi
    done

    # …and the store-safe flavor must STILL carry the same-account peer-sync
    # leg: the excision cuts exactly the `p2p-share` plane, never the sync leg
    # sharing its transport. This is rule 5's third column stated in full —
    # "same-account `fauna.peer.sync.` PRESENT + `fauna.peer.share.` ABSENT"
    # (p2p.md § Wormability walk rule 5). A presence check needs no
    # anti-vacuity twin: a renamed kind family turns it red, never silently
    # green.
    n="$(strings -a "$BIN" | grep -c 'fauna\.peer\.sync\.' || true)"
    if [ "$n" = "0" ]; then
        echo "ERROR: the store-safe fauna-tui flavor carries no 'fauna.peer.sync.'." >&2
        echo "  The p2p-share excision has over-cut into the same-account peer-sync" >&2
        echo "  leg (or the kind family was renamed and this witness needs its" >&2
        echo "  pattern updated). Rule 5's third column is PRESENT + ABSENT, not" >&2
        echo "  absent-only — p2p.md § Wormability walk rule 5." >&2
        exit 1
    fi

    # --- column 2: the DEFAULT tui must still ship all of it ------------------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id would read as a successful excision.
    {{slot_build}} cargo build --locked -p fauna-tui --bin fauna-tui
    built "$BIN"
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'fauna\.payments\.' 'offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-' 'fauna\.peer\.share\.' 'nostr-zap-signer-' 'subscription-tier-form-price-hint' 'subscription-tier-form-asking-price' 'subscription-tier-form-payment-url' 'compose-sell-price' 'compose-sell-asking-price' 'compose-sell-subscribers-free' 'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' 'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT fauna-tui flavor carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    echo "tui-store-safe-check: OK — payments + p2p-share UI/kinds present in default, absent in store-safe; same-account peer-sync kinds present in store-safe"

# Build the excised linux desktop — tui's twin for the GTK shell
# (`dynamic-features.md` § Platform-family surface excision).
#
# ⚠ The binary is `fauna-desktop`, NOT `fauna-linux`: the `fauna-linux` crate
# declares `[[bin]] name = "fauna-desktop"` (apps/fauna-linux/Cargo.toml). A
# verbatim copy of the tui recipe greps a path that never exists and passes
# vacuously — the same trap `linux-debug`'s comment records above.
#
# `store-safe = []` on this crate, the empty complement tui uses.
linux-store-safe: i18n-generate providers-generate
    CARGO_RESOLVER_FEATURE_UNIFICATION=selected {{slot_build}} cargo build --locked -p fauna-linux --bin fauna-desktop --release --no-default-features --features store-safe

# The linux APP-SHELL column of the store-safe family — tui's twin. See
# `tui-store-safe-check` above for why an app-shell column is not redundant
# with the library/server ones: criterion 1 ("no UI surface — the feature's
# element IDs absent") only lives where a shell renders.
#
# The render trap is REAL on this shell and it is the tip surface:
# `views/feed/post_list.rs` paints `post-tip-total` / `-count` /
# `-list-button` / `-list` / `-item` and mentions `payments` nowhere, so
# dep-gating alone would leave five element ids shipping in a store-safe
# build. Dead is not absent.
#
# Element-id patterns are PREFIXES, not whole ids, so a new §4/§5 or tip
# element is covered on the day it is added.
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). Every column builds an
# artifact into the cargo-target dataset and then scans it with `strings`, so
# each column is a produce→consume pair. Between two separate acquisitions the
# checkout holds no slot at all, and the pressure tier evicts exactly there —
# measured twice on 2026-08-20. The body's own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
linux-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _linux-store-safe-check-impl

_linux-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    BIN="$TARGET_DIR/debug/fauna-desktop"
    # A `strings` scan of a VANISHED artifact is an EMPTY scan, and every
    # absence assertion below reads an empty scan as a clean excision — a
    # FALSE GREEN on the excision gate rather than a loud failure. So each
    # column asserts its artifact the moment its build returns: if it is gone,
    # the cargo-target dataset was reclaimed under us (build-machine-resources.md
    # § Build/e2e slot locks → the slot is the recipe's dataset lease).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # --- column 1: the store-safe shell carries neither the UI nor the wire ---
    # The `p2p-share` member's four prefixes joined both columns 2026-09-28
    # (the ceremony + plane surfaces behind linux's own `p2p-share` feature), as
    # tui's recipe already had them.
    {{slot_build}} cargo build --locked -p fauna-linux --bin fauna-desktop --no-default-features --features store-safe
    built "$BIN"
    # The twelve FULL ids after the prefixes are the price-and-route class
    # (dynamic-features.md § Platform-family surface excision → *The
    # price-and-route class*): listed whole because their natural prefixes
    # (`subscription-offer-`, `subscription-tier-`, `gated-post-`,
    # `compose-sell-`) also cover ids outside the plane. Hand-listed here until
    # the catalog entry lands with the last family's gates (that section says why
    # the entry comes last).
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'fauna\.payments\.' 'fauna\.tips\.' 'offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-' 'subscription-tier-form-price-hint' 'subscription-tier-form-asking-price' 'subscription-tier-form-payment-url' 'compose-sell-price' 'compose-sell-asking-price' 'compose-sell-subscribers-free' 'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' 'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe fauna-desktop flavor still carries $n '$pat' occurrence(s)." >&2
            echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            echo "  The likely cause is a RENDER that compiles fine because the data it" >&2
            echo "  reads is an ungated inert record (PostSummary.tips is the known one)," >&2
            echo "  so it paints nothing and still ships every id it would have painted." >&2
            echo "  Gate the render itself with #[cfg(feature = \"payments\")], not just" >&2
            echo "  the resolver that feeds it." >&2
            exit 1
        fi
    done

    # --- column 2: the DEFAULT shell must still ship all of it ----------------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id would read as a successful excision.
    {{slot_build}} cargo build --locked -p fauna-linux --bin fauna-desktop
    built "$BIN"
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'fauna\.payments\.' 'offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-' 'subscription-tier-form-price-hint' 'subscription-tier-form-asking-price' 'subscription-tier-form-payment-url' 'compose-sell-price' 'compose-sell-asking-price' 'compose-sell-subscribers-free' 'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' 'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT fauna-desktop flavor carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    echo "linux-store-safe-check: OK — payments + p2p-share UI and payments kinds present in default, absent in store-safe"

# Build the excised WinUI app — the App-Store escape hatch's windows shell
# flavor (`dynamic-features.md` § Platform-family surface excision), and the SIXTH
# and last app shell to get one. Two halves, both required and both here:
# `windows-ffi-store-safe` (the prerequisite) stages a fauna_ffi.dll with no
# payments plane and a generated C# face to match, and `FaunaStoreSafe=true`
# removes the shell's own `PAYMENTS` define plus every markup item under
# Views\Payments\ (FaunaApp.csproj states both).
#
# ⚠ DELIBERATELY NOT build-if-stale GATED, unlike `windows-release` right above.
# A flavor flip changes no source file, so a gated store-safe build would find the
# tree fresh and hand back whichever flavor was built last — the payments plane
# shipping in the artifact a store submission carries, with nothing failing.
# That is the same failure mode `dynamic-features.md` § The fifth app-shell
# column describes, and it costs a rebuild each run; `windows-store-safe-check`
# below wipes bin/ + obj/ for the same reason, one level up.
#
# RELEASE, not Debug: a Debug build compiles the
# `#if DEBUG` automation surface, whose *ForTest calls resolve only against the
# `test-helpers` FFI flavor that a shipping artifact must never carry
# (e2e-automation-surface-gating.md point 15). Debug + store-safe is an
# un-buildable pairing by construction, and grepping the artifact a submission
# would actually carry is the same choice made honestly.
windows-store-safe: windows-ffi-store-safe i18n-generate providers-generate
    {{slot_build}} "{{msbuild_exe}}" \
        apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj \
        "-restore" "//p:Platform=ARM64" "//p:Configuration=Release" \
        "//p:FaunaStoreSafe=true" "//verbosity:minimal"

# Build the Microsoft Store package — the full MSIX a Partner Center upload
# carries (installers/windows.md § Store distribution): the shipped-flavour
# payload, built and staged here, then packed by scripts/build-store-package.py.
# An upload is built from a clean public clone at the recorded commit
# (release-integrity.md § Release signing → *A store upload is built from a
# recorded public commit*), with the reserved identity:
#
#   just windows-store-package arm64 full FaunaSocial.FaunaSocial "CN=E8868D60-047A-46A3-B608-0D4CA88AB791"
#
# Empty identity/publisher → the packer's dev placeholders (a local check, never
# an upload). `flavor` is `full` or `store-safe` (the escape hatch:
# dynamic-features.md § The App-Store escape hatch).
#
# Why a recipe of its own and not the MSI helper's stage: the only other local
# payload builder, test_installer.py::_build_msi, compiles the e2e automation
# surface in ON PURPOSE (`-p:FaunaE2eAgent=true` over `windows-ffi-test`), because
# the installed-MSI journeys drive the app through it. That is right for those
# tests and fatal for an upload (convention 15), so this recipe never touches
# build/installer/stage/ and stages into build/installer/store-payload/<arch>/,
# the packer's default payload root. The packer then refuses any payload that
# still shows the surface. Pinned by test_store_package_build.py.
#
# ONE build slot for the whole recipe (`slot_build_body` re-runs this script under
# it unless the caller already holds one): the FFI, the two cargo builds, the
# publish and the pack are one produce->consume chain, and the nested
# `just windows-ffi` finds the slot held. Deliberately NOT build-if-stale gated,
# like `windows-store-safe`: a flavour flip touches no source file.
windows-store-package arch="arm64" flavor="full" identity_name="" publisher="": i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    # Artifact build: per-invocation feature resolution, never workspace-unified
    # (.cargo/config.toml § feature unification) — same as `_windows-ffi-flavor`.
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    ARCH="{{arch}}"
    FLAVOR="{{flavor}}"
    # Installer artifacts build at the size-optimised `dist` profile; `release` is
    # the dev/test inner loop (installers/windows.md § Size & build profile).
    case "$ARCH" in
        arm64) TARGET="";                     RID=win-arm64; CARGO_WIN_ARCH=arm64; CARGO_OUT=target/dist ;;
        x64)   TARGET=x86_64-pc-windows-msvc; RID=win-x64;   CARGO_WIN_ARCH=x64;   CARGO_OUT=target/x86_64-pc-windows-msvc/dist ;;
        *) echo "windows-store-package: arch must be arm64 or x64, got '$ARCH'" >&2; exit 2 ;;
    esac
    case "$FLAVOR" in
        full|store-safe) ;;
        *) echo "windows-store-package: flavor must be full or store-safe, got '$FLAVOR'" >&2; exit 2 ;;
    esac
    # cargo-win.cmd selects the matching cl/link/lib environment from this.
    export CARGO_WIN_ARCH
    TARGET_ARG="${TARGET:+--target $TARGET}"
    APP=apps/fauna-windows/FaunaApp/FaunaApp
    CORE=apps/fauna-windows/FaunaApp/FaunaApp.Core

    # 1. The PRODUCTION FFI: stages runtimes/<rid>/native/fauna_ffi.dll and the
    #    generated C# bindings the publish compiles against. Never the -test flavour.
    if [ "$FLAVOR" = store-safe ]; then
        just windows-ffi-store-safe dist "$TARGET"
        AGENT_FEATURES="--no-default-features"   # drops p2p-share, the agent's only default
        PUBLISH_FLAVOR="-p:FaunaStoreSafe=true"
    else
        just windows-ffi dist "$TARGET"
        AGENT_FEATURES=""
        PUBLISH_FLAVOR=""
    fi

    # 2. The two Rust binaries the package carries, each in its OWN invocation:
    #    one naming several packages unifies their features (release.yml's rule).
    cmd //c "scripts\\cargo-win.cmd build --profile dist --locked -p fauna-sync-agent $AGENT_FEATURES $TARGET_ARG"
    cmd //c "scripts\\cargo-win.cmd build --profile dist --locked -p fauna-shell-ext $TARGET_ARG"

    # 3. The app, self-contained Release. bin/ + obj/ are wiped first: a flavour
    #    flip touches no source, and _build_msi publishes its e2e-agent build into
    #    this very same bin/Release/<tfm>/<rid>/publish directory, so an
    #    incremental publish could hand that assembly back (the trap
    #    `windows-store-safe-check` measured).
    rm -rf "$APP/bin" "$APP/obj" "$CORE/bin" "$CORE/obj"
    "{{msbuild_exe}}" "$APP/FaunaApp.csproj" -restore -t:Publish \
        -p:Configuration=Release -p:RuntimeIdentifier=$RID \
        -p:SelfContained=true -p:WindowsAppSDKSelfContained=true \
        $PUBLISH_FLAVOR -verbosity:minimal
    PUBLISH_DIR="$APP/bin/Release/net10.0-windows10.0.26100/$RID/publish"
    [ -f "$PUBLISH_DIR/FaunaApp.exe" ] || {
        echo "windows-store-package: no FaunaApp.exe under $PUBLISH_DIR after the publish" >&2; exit 1; }

    # 4. Stage exactly the payload the manifest references.
    STAGE="build/installer/store-payload/$ARCH"
    rm -rf "$STAGE"
    mkdir -p "$STAGE/App"
    cp -r "$PUBLISH_DIR/." "$STAGE/App/"
    cp "$CARGO_OUT/fauna-sync-agent.exe" "$STAGE/"
    cp "$CARGO_OUT/fauna_shell.dll" "$STAGE/"

    # 5. Pack. The packer refuses a payload missing its native core or carrying
    #    the automation surface; this recipe never opts out of either.
    PACK=(--arch "$ARCH" --payload-root "$STAGE")
    [ -n "{{identity_name}}" ] && PACK+=(--identity-name "{{identity_name}}")
    [ -n "{{publisher}}" ] && PACK+=(--publisher "{{publisher}}")
    {{py}} scripts/build-store-package.py "${PACK[@]}"

# The windows APP-SHELL column of the store-safe family — the SIXTH, closing the
# app-shell half (`dynamic-features.md` § The feature-matrix test story). See
# `tui-store-safe-check` for why an app-shell column is not redundant with the
# library/server ones; three things are windows' own and none of them is optional.
#
# (1) THE SCAN HAS TO SEE UTF-16. Every other column runs `strings -a | grep`,
# which works on an ELF/Mach-O/dex because their string data is UTF-8. A .NET
# assembly stores `const string` values in the `Constant` metadata table and every
# `ldstr` literal — which is what the XAML compiler emits for an AutomationId — in
# the `#US` heap, BOTH as UTF-16LE. GNU strings could read them with `-e l`; the
# `strings` on this machine is `llvm-strings`, which has no encoding flag at all.
# So the columns below scan through `scripts/strings-utf16.py`. A verbatim copy of
# tui's recipe would report 0 in BOTH columns — column 2 turns that red rather than
# letting it read as a clean excision, which is exactly what column 2 is for.
#
# (2) THE TWO CRITERIA LAND ON DIFFERENT ARTIFACTS, as they do on android
# (dex vs archive). Criterion 1 (element ids) is a claim about the SHELL's own
# painted surface, which is the managed FaunaApp.dll — and on C# the generated id
# TABLE is a second carrier beside the renders, because a `const string` sits in
# assembly metadata whether or not anything reads it (hence
# `GATED_EMISSION["csharp"]`). Criterion 2 (kind strings) cannot be asserted there
# at all: windows C# spells no `fauna.payments.*` literal anywhere — the kinds live
# in the native cdylib, and the only C# occurrences are XML doc comments, which
# never reach IL. So the kind axis scans the PACKAGED fauna_ffi.dll, and the
# managed column instead carries a UniFFI FACE-NAME axis (`FfiPaymentsClient`,
# `PaymentsKnownKinds`, `PaymentsWebhookUrl`) — the C# twin of the wasm export
# names web greps, and a real column because `mod payments_client` is
# `#[cfg(feature = "payments")]` whole, so the generated face loses all three.
#
# (3) THE MARKUP IS NOT IN THE ASSEMBLY. The WinUI XAML compiler emits a `.xbf`
# per page BESIDE FaunaApp.dll ($OUT/App.xbf, $OUT/Views/*.xbf,
# $OUT/Controls/*.xbf), and that is where every AutomationId literal lives. So
# criterion 1 scans the assembly (for the generated const table) AND every .xbf
# (for the renders). Measured the hard way 2026-08-24: the first version of this
# recipe scanned the assembly alone, reported 15/0, 10/0, 5/0 — numbers that
# looked right and even matched web's and iOS's — and passed GREEN with an
# ungated `post-tip-count` sitting in a page outside Views\Payments\. 15+10+5 is
# exactly the 30 ids in the gated const table; the scan had never seen a render
# at all. The mutant in point (5) is what found it.
#
# (4) BOTH COLUMNS WIPE bin/ + obj/ FIRST. Both flavors write the same output
# directory, and a flavor flip touches no source file — so an incremental second
# column can hand back the first column's assembly. Wiping obj/ forces a full
# recompile and makes the flavor a fact about the run rather than a hope about
# MSBuild's up-to-date checks.
#
# (5) THE MUTANT THAT GRADES THIS COLUMN is an ungated `AutomationId` carrying a
# payments id, in a page outside Views\Payments\ — tui's `Element::label(
# "post-tip-count", " ")` translated to markup. It compiles clean in BOTH flavors,
# paints an empty TextBlock nobody would notice, and no compile gate anywhere can
# see it. Re-run it after any change to this recipe's artifact set: it is the only
# thing that distinguishes "the renders are excised" from "the scan cannot see the
# renders", and those two look identical from a green.
#
# Element-id patterns are PREFIXES from ui.yaml's `gated_features:` catalog
# (`dynamic-features.md` § Which element IDs belong to a gated feature), so a new
# §4/§5 element is covered the day it is added. `post-tip-` is in the list even
# though windows paints no tip surface: it is non-vacuous through the generated
# const table alone, which is the carrier this shell most needed gating.
# The `p2p-share` member rides its own three lists (P2P_ID_PATS, P2P_FACE_PATS,
# P2P_NATIVE_PATS) because it is its own define (`P2P_SHARE`) and its own render
# directory (Views\P2pShare\). Its ceremony prefixes are carried by the render
# and the table; `share-transfer-` / `share-serve-` (the plane, which windows
# does not paint) by the generated table alone. ⚠ Adding a registry member
# later is the same shape: one id list, one face list, one native list, one
# render directory.
#
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot locks
# → The slot is the recipe's dataset lease): every column builds an artifact and
# then scans it, so each is a produce->consume pair and must not lose the slot in
# between. The body's own {{slot_build}} lines are reentrant no-ops under it.
windows-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _windows-store-safe-check-impl

_windows-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    APP=apps/fauna-windows/FaunaApp/FaunaApp
    OUT="$APP/bin/ARM64/Release/net10.0-windows10.0.26100"
    MSBUILD="{{msbuild_exe}}"
    WORK="$(mktemp -d "{{tmp_root}}/windows-store-safe-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT

    # Criterion 1 — the shell's own painted surface + the generated id table.
    # `nostr-zap-signer-` is here because `zaps` is a SUBSET member of
    # `payments` and rides the SAME store-safe axis (dynamic-features.md §
    # Which element IDs belong to a gated feature) — mirrors the swift row's
    # `_APPLE_ID_PREFIXES`.
    PAY_ID_PATS=('subscription-provider-' 'subscription-claim-' 'post-tip-' 'nostr-zap-signer-' \
        'subscription-tier-form-asking-price' 'compose-sell-asking-price')
    # The price-and-route class (dynamic-features.md § Platform-family surface
    # excision → The price-and-route class): the ten ids the catalog does not list
    # yet, hand-listed as FULL ids, since each natural prefix also covers ids
    # outside the plane. They are not in PAY_ID_PATS because they cannot read 0
    # here yet: until the catalog row moves them into Generated/UiIds.cs's
    # `#if PAYMENTS` half, each one's `const string` sits in the store-safe
    # assembly's metadata whether or not anything reads it (the C# row of § Which
    # element IDs belong to a gated feature) — one occurrence, the table's. A
    # render adds a second: an x:Bind or C# reference compiles to an `ldstr` in
    # the IL, a literal AutomationId lands in a .xbf. So store-safe must read
    # EXACTLY 1 (the table and nothing else) and default AT LEAST 2 (the table
    # and a render). A 0 check scoped to the .xbf set would be vacuous: the
    # class's renders use x:Bind, which compiles into IL, not the .xbf (measured
    # 2026-10-08). When the catalog lands the store-safe reading drops to 0 and
    # table_only goes red on purpose: move these into PAY_ID_PATS then.
    PAY_CLASS_IDS=('subscription-tier-form-price-hint' 'subscription-tier-form-payment-url' \
        'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' \
        'compose-sell-price' 'compose-sell-subscribers-free' \
        'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button')
    # The UniFFI faces the shell would call — the C# twin of web's wasm exports.
    PAY_FACE_PATS=('FfiPaymentsClient' 'PaymentsKnownKinds' 'PaymentsWebhookUrl' 'FfiNostrZapSignerClient' 'FfiZapSignerEntry')
    # Criterion 2 — kind strings, which live in the native cdylib, never in the C#.
    PAY_KIND_PATS=('fauna\.payments\.' 'fauna\.tips\.' 'fauna\.nostr\.zap_signers\.')
    # The `p2p-share` member (its own `P2P_SHARE` define). Criterion 1: all four
    # catalog prefixes, as tui's recipe takes them.
    P2P_ID_PATS=('offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-')
    # The UniFFI faces the ceremony glue would call. `FfiCeremonySeat` is the
    # generated class; the `uniffi_fauna_ffi_fn_func_offline_share_` prefix is the
    # extern entry-point names of every ceremony door. A bare `offline_share_`
    # would also match the shared i18n keys (`folders.offline_share_*`), which
    # ship in every flavor and are neither a face nor an id.
    P2P_FACE_PATS=('FfiCeremonySeat' 'uniffi_fauna_ffi_fn_func_offline_share_')
    # The native half: the same export symbols and records `ffi-store-safe-check`
    # watches in its p2p-share column, plus the plane's, which the store-safe
    # fauna_ffi.dll never carried.
    P2P_NATIVE_PATS=('uniffi_fauna_ffi_fn_func_offline_share_' 'FfiCeremonySeat' 'share_plane_view' 'FfiSharePlaneView')

    # A scan of a VANISHED artifact is an EMPTY scan, and every absence assertion
    # below reads an empty scan as a clean excision — a FALSE GREEN on the excision
    # gate rather than a loud failure. So each column asserts its artifacts the
    # moment its build returns (tui's `built` guard, same reasoning).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # SCAN_FILES is set by the caller immediately before each assertion — an array
    # rather than a string so a path with a space cannot split.
    scan() { {{py}} scripts/strings-utf16.py "${SCAN_FILES[@]}"; }

    absent() {
        local label="$1"; shift
        local dump; dump="$(scan)"
        local pat n
        for pat in "$@"; do
            n="$(printf '%s\n' "$dump" | grep -c "$pat" || true)"
            # Print every count, pass or fail: the two columns' numbers are what
            # dynamic-features.md records per shell, and a witness that only speaks
            # when it fails makes the next session re-measure by hand.
            echo "  store-safe $label: $pat = $n"
            if [ "$n" != "0" ]; then
                echo "ERROR: the store-safe windows $label still carries $n '$pat' occurrence(s)." >&2
                echo "  An excised app artifact must contain NO UI surface (element ids), NO" >&2
                echo "  UniFFI faces and NO wire senders (kind strings) for the plane —" >&2
                echo "  dynamic-features.md § What \"completely compiled away\" means, items 1-2." >&2
                echo "  On this shell there are THREE likely causes, in order of likelihood:" >&2
                echo "    · a payments render outside Views\\Payments\\ (or a p2p-share one" >&2
                echo "      outside Views\\P2pShare\\) — XAML has no preprocessor, so only" >&2
                echo "      the csproj's item removal can drop markup; a render anywhere" >&2
                echo "      else compiles clean in BOTH flavors, paints nothing, and still" >&2
                echo "      ships every id it would have painted;" >&2
                echo "    · a generated element-id constant outside its '#if PAYMENTS' /" >&2
                echo "      '#if P2P_SHARE' half" >&2
                echo "      (a C# 'const string' is in assembly metadata even unreferenced —" >&2
                echo "      scripts/ui-ids-generate.py § GATED_EMISSION);" >&2
                echo "    · the store-safe fauna_ffi.dll was not the one staged — check" >&2
                echo "      apps/fauna-windows/.ffi-flavor reads 'release:store-safe'." >&2
                exit 1
            fi
        done
    }

    present() {
        local label="$1"; shift
        local dump; dump="$(scan)"
        local pat n
        for pat in "$@"; do
            n="$(printf '%s\n' "$dump" | grep -c "$pat" || true)"
            echo "  default    $label: $pat = $n"
            if [ "$n" = "0" ]; then
                echo "ERROR: the DEFAULT windows $label carries no '$pat' — the store-safe" >&2
                echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
                echo "  from the default build (a product regression) or it was renamed and" >&2
                echo "  this witness needs its patterns updated. A pattern that matches" >&2
                echo "  nothing in BOTH columns is not a stricter check, it is a tripwire." >&2
                exit 1
            fi
        done
    }

    # The price-and-route class's store-safe reading: the generated table's one
    # line per id and nothing else (PAY_CLASS_IDS says why it is not 0 yet).
    table_only() {
        local label="$1"; shift
        local dump; dump="$(scan)"
        local pat n
        for pat in "$@"; do
            n="$(printf '%s\n' "$dump" | grep -c "$pat" || true)"
            echo "  store-safe $label: $pat = $n (the generated table's line only)"
            [ "$n" = "1" ] && continue
            if [ "$n" = "0" ]; then
                echo "ERROR: the store-safe windows $label carries no '$pat' at all, not even" >&2
                echo "  the generated table's line. The catalog row has most likely landed and" >&2
                echo "  moved it behind '#if PAYMENTS': move it from PAY_CLASS_IDS into" >&2
                echo "  PAY_ID_PATS, which asserts 0 here." >&2
            else
                echo "ERROR: the store-safe windows $label carries $n '$pat' occurrences. One" >&2
                echo "  is the generated table's const; every other one is a RENDER (an x:Bind" >&2
                echo "  or C# reference compiles to an ldstr, a literal sits in a .xbf). Every" >&2
                echo "  render of the price-and-route class lives in Views\\Payments\\ and is" >&2
                echo "  hosted under '#if PAYMENTS' (dynamic-features.md § Platform-family" >&2
                echo "  surface excision → The price-and-route class)." >&2
            fi
            exit 1
        done
    }

    # …and its default reading: the table's line plus at least one render.
    rendered() {
        local label="$1"; shift
        local dump; dump="$(scan)"
        local pat n
        for pat in "$@"; do
            n="$(printf '%s\n' "$dump" | grep -c "$pat" || true)"
            echo "  default    $label: $pat = $n (the generated table's line + renders)"
            if [ "$n" -lt 2 ]; then
                echo "ERROR: the DEFAULT windows $label carries $n '$pat', so no render" >&2
                echo "  paints it and the store-safe reading of 1 above says nothing about the" >&2
                echo "  render gate. Either the render left the default build or the id was" >&2
                echo "  renamed and PAY_CLASS_IDS needs updating." >&2
                exit 1
            fi
        done
    }

    # Criterion 1's artifact set, and getting this wrong is a SILENT FALSE GREEN —
    # measured 2026-08-24, by the mutant that found it. The WinUI XAML compiler does
    # NOT put compiled markup inside FaunaApp.dll: it emits a `.xbf` per page beside
    # the assembly ($OUT/App.xbf, $OUT/Views/*.xbf, $OUT/Controls/*.xbf), and that is
    # where every `AutomationProperties.AutomationId` literal actually lives. A scan
    # of the assembly alone therefore measures only the GENERATED CONST TABLE and
    # says nothing whatever about the renders — which is exactly the half the csproj
    # item-removal is responsible for. The first version of this recipe scanned the
    # assembly alone, reported a tidy 15/0, 10/0, 5/0 (which is precisely 15+10+5 =
    # the 30 ids in the gated table, and no markup at all), and let an ungated
    # `post-tip-count` in a page outside Views\Payments\ pass GREEN.
    #
    # So: the assembly (the const table) AND every .xbf (the renders). `globstar` for
    # depth, `nullglob` so a vanished tree yields an empty array rather than a
    # literal pattern — which the guard below then catches.
    shopt -s globstar nullglob
    collect_id_artifacts() {
        ID_FILES=("$OUT/FaunaApp.dll" "$OUT"/**/*.xbf)
        # A .xbf-less output is a LAYOUT CHANGE, not an empty app: whatever replaced
        # the per-page .xbf now carries the ids, and until this recipe knows where,
        # every element-id assertion below is vacuous.
        if [ "${#ID_FILES[@]}" -lt 2 ]; then
            echo "ERROR: no .xbf found under $OUT — the compiled-XAML layout changed." >&2
            echo "  Every element-id assertion in this recipe would then be scanning the" >&2
            echo "  managed assembly alone, which carries only the generated const table" >&2
            echo "  and no render at all. Find where the AutomationId literals went" >&2
            echo "  before trusting another green." >&2
            exit 1
        fi
    }

    # Both flavors write $OUT and a flavor flip touches no source, so each column
    # starts from a wiped tree — see the header, point (3).
    #
    # ⚠ ALL THREE projects, not just the WinUI one. The payments define reaches
    # FaunaApp.Core too (MSBuild does not propagate DefineConstants across a
    # ProjectReference, so each csproj states it and a global /p: reaches all
    # three), and Core is where the UniFFI faces this recipe greps actually live.
    # Wiping only $APP would leave Core's obj/ holding the previous flavor's
    # compile state and make the second column's verdict depend on whether
    # MSBuild's up-to-date check happens to hash $(DefineConstants) — which is
    # exactly the hope point (3) says not to build a gate on.
    wipe() { rm -rf apps/fauna-windows/FaunaApp/*/bin apps/fauna-windows/FaunaApp/*/obj; }

    # --- column 1: the store-safe shell carries neither the UI nor the plane ---
    wipe
    just windows-store-safe
    built "$OUT/FaunaApp.dll"
    built "$OUT/FaunaApp.Core.dll"
    built "$OUT/fauna_ffi.dll"
    collect_id_artifacts
    SCAN_FILES=("${ID_FILES[@]}");            absent "assembly + compiled XAML" "${PAY_ID_PATS[@]}" "${P2P_ID_PATS[@]}"
    SCAN_FILES=("${ID_FILES[@]}");            table_only "assembly + compiled XAML" "${PAY_CLASS_IDS[@]}"
    SCAN_FILES=("$OUT/FaunaApp.Core.dll");    absent "core assembly" "${PAY_FACE_PATS[@]}" "${P2P_FACE_PATS[@]}"
    SCAN_FILES=("$OUT/fauna_ffi.dll");        absent "native cdylib" "${PAY_KIND_PATS[@]}" "${P2P_NATIVE_PATS[@]}"
    # The renders' own .xbf files must be GONE, not merely id-free: they are the
    # unit the csproj removes, so their presence would mean the item-removal silently
    # stopped matching while some other reason kept the ids out of the scan.
    for gone in "$OUT"/Views/Payments/*.xbf "$OUT"/Views/P2pShare/*.xbf; do
        echo "ERROR: the store-safe build still emitted $gone." >&2
        echo "  FaunaApp.csproj's <Page Remove> for Views\\Payments\\** or" >&2
        echo "  Views\\P2pShare\\** no longer matches, so that member's markup was" >&2
        echo "  compiled into the artifact." >&2
        exit 1
    done

    # --- column 2: the DEFAULT shell must still ship all of it -----------------
    # Without this every assertion above is indistinguishable from a grep that
    # matches nothing — and on THIS shell that is the likeliest failure of all,
    # because an encoding-blind scan reports 0 for every pattern (header, point 1).
    wipe
    just windows-release
    built "$OUT/FaunaApp.dll"
    built "$OUT/FaunaApp.Core.dll"
    built "$OUT/fauna_ffi.dll"
    collect_id_artifacts
    SCAN_FILES=("${ID_FILES[@]}");            present "assembly + compiled XAML" "${PAY_ID_PATS[@]}" "${P2P_ID_PATS[@]}"
    SCAN_FILES=("${ID_FILES[@]}");            rendered "assembly + compiled XAML" "${PAY_CLASS_IDS[@]}"
    SCAN_FILES=("$OUT/FaunaApp.Core.dll");    present "core assembly" "${PAY_FACE_PATS[@]}" "${P2P_FACE_PATS[@]}"
    SCAN_FILES=("$OUT/fauna_ffi.dll");        present "native cdylib" "${PAY_KIND_PATS[@]}" "${P2P_NATIVE_PATS[@]}"
    # …and the renders' .xbf files must EXIST here, or column 1's absence of them
    # proves nothing about the item-removal.
    for pat in PaymentsAuthorSections ClaimRedeemPanel PostTipDisplay PostTipListDialog NostrZapSignersSection \
        SubscriptionTierMoneyFields SubscriptionTierPriceText SubscriptionOfferPriceText \
        SubscriptionOfferPaymentLink SellComposeFields SoldPostTeaser; do
        if [ ! -s "$OUT/Views/Payments/$pat.xbf" ]; then
            echo "ERROR: the DEFAULT build emitted no $pat.xbf, so column 1's check that" >&2
            echo "  the store-safe build lacks it is vacuous. Either the render moved or" >&2
            echo "  the compiled-XAML layout changed." >&2
            exit 1
        fi
    done
    if [ ! -s "$OUT/Views/P2pShare/OfflineShareSection.xbf" ]; then
        echo "ERROR: the DEFAULT build emitted no OfflineShareSection.xbf, so column 1's" >&2
        echo "  check that the store-safe build lacks it is vacuous. Either the render" >&2
        echo "  moved or the compiled-XAML layout changed." >&2
        exit 1
    fi

    echo "windows-store-safe-check: OK — payments + p2p-share element ids, UniFFI faces and native strings present in default, absent in store-safe; the price-and-route class rendered in default, table-only in store-safe"

# The android APP-SHELL column of the store-safe family — the FOURTH app-shell
# witness (after tui, linux and apple) and the second over a non-Rust shell.
# See `tui-store-safe-check` for why an app-shell column is not redundant with
# the library/server ones: criterion 1 ("no UI surface — the feature's element
# IDs absent") only lives where a shell renders.
#
# Three things make this column's mechanics its own, and none of them are
# transferable from the shells that came before:
#
#  1. **The artifact is a ZIP, so `strings` on it is meaningless.** Dex string
#     constants are deflate-compressed inside the APK; a `strings -a app.apk`
#     scans compressed bytes and reports 0 for everything, which reads exactly
#     like a perfect excision. Both columns therefore UNPACK first and scan
#     `classes*.dex` (criterion 1's element ids) plus `lib/*/*.so` (criterion
#     2's kind strings) — the two halves live in different members of the same
#     archive.
#  2. **The absence is R8's doing, not the compiler's.** Kotlin has no inline
#     compile-time exclusion, so android's family condition is a `BuildConfig`
#     constant (`PAYMENTS=false` in the `storeSafe` build type) whose branch R8
#     folds away. `dynamic-features.md` § Platform-family surface excision says
#     it outright: a constant-gated `if` that R8 *should* fold is not the same
#     claim as an id that is absent. This recipe is what turns that should into
#     a measurement.
#  3. **One ABI, deliberately.** Both columns build `arm64` only — eight
#     release `fauna-ffi` builds to answer a question one ABI settles is the
#     cost apple's witness avoids with its 1-slice host twin. The flavor marker
#     records the ABI set, so this never leaves a later 4-ABI build trusting
#     three stale `.so`s.
#
# Element-id patterns are PREFIXES, so a new §4/§5 or tip element is covered on
# the day it is added, not the day someone remembers this list.
android-store-safe-check: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-21-openjdk-arm64}"
    WORK="$(mktemp -d "{{tmp_root}}/android-store-safe-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT

    # Unpack the two artifact members that can carry the surface and emit every
    # printable string in them. `-a` because a dex is not a recognised object
    # format; the `.so` is scanned in the same pass so one helper answers both
    # criteria.
    # Criterion 1 (element ids) and criterion 2 (kind strings) are scanned over
    # DIFFERENT members, and the split is a measured finding rather than a
    # convenience (2026-08-16, this witness's first run):
    #
    #   * Element ids are a claim about the SHELL — "no UI surface". They are
    #     painted by Compose and live in `classes*.dex`. Scanning the packaged
    #     `.so` for them instead reports the UniFFI docstrings that shared Rust
    #     embeds in the cdylib's metadata: `TipView` and `value_format`'s tip
    #     formatters are deliberately UNGATED inert surfaces (snapshot.rs's own
    #     posture), and their doc comments *name* `post-tip-*` while painting
    #     nothing and being reachable by nothing. Measured: 9 such occurrences,
    #     every one a doc comment, zero in the dex.
    #   * Kind strings ARE scanned everywhere, docstrings included — that is
    #     criterion 2's explicit "absent means absent, prose included" rule, and
    #     it passes: 0 `fauna.payments.` / `fauna.tips.` across the whole APK.
    #
    # ⚠ This does NOT weaken the gate against the defect it exists for. The
    # canonical mutant — an ungated `Modifier.testTag("post-tip-count")` — is
    # Compose code and lands in the dex, so it still dies here.
    apk_strings() {
        local apk="$1" out="$2" what="$3"
        rm -rf "$out" && mkdir -p "$out"
        case "$what" in
            dex) unzip -q -o "$apk" 'classes*.dex' -d "$out" ;;
            all) unzip -q -o "$apk" 'classes*.dex' 'lib/*/*.so' -d "$out" ;;
        esac
        # A silent zero-file unpack would make every assertion below vacuous.
        [ -n "$(find "$out" -type f -print -quit)" ] || {
            echo "ERROR: unpacking '$what' from $apk produced no files." >&2
            exit 1
        }
        find "$out" -type f -print0 | xargs -0 strings -a
    }

    # The `-d` guard is load-bearing under `set -euo pipefail`: a missing output
    # directory makes `find` exit non-zero, which would abort the recipe with
    # find's own error instead of the "no APK produced" message below — i.e. the
    # failure mode would be least legible in exactly the case it describes.
    find_apk() {
        # Two `local` statements, deliberately: `local a="$1" b="…$a"` expands
        # `$a` BEFORE `local` assigns it (all arguments to the `local` builtin
        # are expanded first), which under `set -u` aborts with
        # "variant: unbound variable".
        local variant="$1"
        local dir="apps/fauna-android/app/build/outputs/apk/$variant"
        [ -d "$dir" ] || return 0
        find "$dir" -name '*.apk' -print -quit
    }

    # --- column 1: the store-safe APK carries neither the UI nor the wire -----
    just _android-ffi-flavor storeSafe store-safe arm64
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleStoreSafe
    SS_APK="$(find_apk storeSafe)"
    [ -n "$SS_APK" ] || { echo "ERROR: no store-safe APK produced." >&2; exit 1; }
    apk_strings "$SS_APK" "$WORK/ss-dex" dex > "$WORK/ss-dex.txt"
    apk_strings "$SS_APK" "$WORK/ss-all" all > "$WORK/ss-all.txt"
    # The `p2p-share` member's four catalog prefixes ride the element-id
    # column: its ceremony render is gated on `BuildConfig.P2P_SHARE`, and its
    # plane half (`share-transfer-` / `share-serve-`) is painted nowhere on
    # android, so all four must be absent from the dex.
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'nostr-zap-signer-' 'offline-share-' 'offline-receive-' 'share-transfer-' 'share-serve-' 'fauna\.payments\.' 'fauna\.tips\.' 'fauna\.nostr\.zap_signers\.'; do
        case "$pat" in
            fauna*) SCAN="$WORK/ss-all.txt" ;;   # criterion 2 — prose included
            *)      SCAN="$WORK/ss-dex.txt" ;;   # criterion 1 — the shell's own surface
        esac
        n="$(grep -c "$pat" "$SCAN" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe Android APK still carries $n '$pat' occurrence(s)." >&2
            echo "  ($SS_APK)" >&2
            echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            echo "  Two causes, in order of likelihood:" >&2
            echo "   * a RENDER with no BuildConfig.PAYMENTS (or, for a p2p-share id," >&2
            echo "     BuildConfig.P2P_SHARE) condition. It compiles fine —" >&2
            echo "     the data it reads is an ungated inert record (PostSummary.tips is" >&2
            echo "     the known one) — so it paints nothing and still ships every id it" >&2
            echo "     would have painted. Gate the render, not just the resolver." >&2
            echo "   * R8 kept the folded branch (a keep rule, or a reference that made it" >&2
            echo "     reachable). Move that surface into app/src/payments/ (or p2pShare/) instead, where" >&2
            echo "     the absence is the compiler's doing rather than the optimizer's." >&2
            exit 1
        fi
    done

    # --- column 2: the DEFAULT (release) APK must still ship all of it --------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id, or an unpack that silently extracted zero
    # files, would both read as a successful excision.
    just _android-ffi-flavor release "" arm64
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleRelease
    REL_APK="$(find_apk release)"
    [ -n "$REL_APK" ] || { echo "ERROR: no release APK produced." >&2; exit 1; }
    apk_strings "$REL_APK" "$WORK/rel-dex" dex > "$WORK/rel-dex.txt"
    apk_strings "$REL_APK" "$WORK/rel-all" all > "$WORK/rel-all.txt"
    # Only `offline-share-` of the p2p-share prefixes, MEASURED (2026-09-30):
    # `share-transfer-` / `share-serve-` are painted nowhere on android, and
    # the default release dex carries just one ceremony id,
    # `offline-share-own-code`. R8 folds the section's two gates
    # (`showsEntryButtons`, `showsCodeWidgets`) to false in every minified
    # build and drops the rest of the render with them, a release-only defect
    # that predates the P2P_SHARE leg.
    # `offline-receive-` joins this column when that lands.
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' 'nostr-zap-signer-' 'offline-share-' 'fauna\.payments\.' 'fauna\.nostr\.zap_signers\.'; do
        case "$pat" in
            fauna*) SCAN="$WORK/rel-all.txt" ;;
            *)      SCAN="$WORK/rel-dex.txt" ;;
        esac
        n="$(grep -c "$pat" "$SCAN" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT Android APK carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    echo "android-store-safe-check: OK — payments/zaps/p2p-share UI + kinds present in default, absent in store-safe"

# The Fauna Kids witness (family-safety.md § The account age band, the kids-app
# bullet, item (5); installers/android.md § Goal): the `kids` APK carries none of
# the surfaces the kids flavor excises, and the DEFAULT `release` APK carries all
# of them — `android-store-safe-check`'s two-column shape, with its three traps
# (unpack before grepping, the absence is R8's doing for a `BuildConfig.KIDS`
# render, one ABI) carried over unchanged; read that recipe's header for them.
#
# What the kids artifact's absence is made of, and why each column scans what it
# scans:
#
#  * **Element ids (criterion 1, the dex).** Most excised surfaces are absent by
#    the COMPILER's doing: they live in `app/src/noKids/` (the feed, search,
#    every bridge, web publishing, the Personalization page, connected apps, the
#    labeler catalog), which the `kids` build type never compiles. The rest are
#    `BuildConfig.KIDS` renders in shared code that R8 folds: the profile Follow
#    button and Tiers tab, the conversation link-preview card, the folder
#    paywall select, the top-bar search toggle. Both kinds land in this column,
#    so a mutant on either side — a surface moved back into `src/main`, or a
#    render that lost its condition — dies here.
#  * **Exported faces (the `.so`).** The kids `fauna-ffi` flavor
#    (`kids-safe,kids-floor`) generates no binding module for the feed manager,
#    search manager, Bluesky settings, the labeler catalog, web publishing,
#    critical alerts or connected apps, and no `FfiFeedManager` /
#    `FfiSearchManager` / `FfiWebClient` / mail-settings / DNS-credential face:
#    the packaged library must export no UniFFI symbol of any of them.
#  * **Kind strings (criterion 2) are NOT asserted yet — a measured gap, not an
#    oversight (2026-10-05).** The kids `.so` still carries `fauna.feed.*`,
#    `fauna.search.query` and `fauna.web.*` strings: the thin `FfiFeedClient` /
#    `FfiSearchClient` RPC clients are ungated in `fauna-ffi`, and
#    `fauna-protocol`'s kind catalog lists every plane with no per-plane feature
#    (dynamic-features.md § Implementation status today, the kids paragraph).
#    The kind-string column joins this witness when that shared-Rust leg lands.
#  * **The store identity.** `social.fauna.faunakids` must be in the kids APK's
#    manifest and NOT in the release one (a kids listing is its own registry
#    unit, never a suffix — installers/android.md § Store identity).
#
# The content floor (`kids-floor`) has no string to grep: it is a compiled
# constant policy (`fauna_core::obligation::KIDS_CONTENT_FLOOR`). Its presence is
# pinned where it can be — `android-ffi-kids` passing `kids-floor`
# (test_kids_excision_spine.py) and the floor's own tests in `fauna-ffi`
# (`cargo test -p fauna-ffi --lib --no-default-features --features kids-safe,kids-floor`).
#
# Deliberately NOT in the merge-gate check, on `android-store-safe-check`'s
# measured cost grounds (two release `fauna-ffi` builds plus two R8 passes).
#
# ⚠ Column 2's `_android-ffi-flavor release` runs the FFI seam-diff against the
# staged `src/debug` tree whenever that tree is as fresh as the sources, and
# that two-tree check is red today on test-helpers seams the floor list has not
# declared — a red of the seam witness, not of this
# one. Until it is fixed, a run on a box with fresh debug bindings stops there.
android-kids-check: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-21-openjdk-arm64}"
    WORK="$(mktemp -d "{{tmp_root}}/android-kids-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT

    # One printable string per line, over the members that can carry a surface
    # (see `android-store-safe-check`: an APK is a ZIP, so grep the members).
    # `manifest` reads the binary AndroidManifest.xml in both encodings AAPT2
    # may pool its strings in.
    apk_strings() {
        local apk="$1" out="$2" what="$3"
        rm -rf "$out" && mkdir -p "$out"
        case "$what" in
            dex)      unzip -q -o "$apk" 'classes*.dex' -d "$out" ;;
            all)      unzip -q -o "$apk" 'classes*.dex' 'lib/*/*.so' -d "$out" ;;
            manifest) unzip -q -o "$apk" 'AndroidManifest.xml' -d "$out" ;;
        esac
        [ -n "$(find "$out" -type f -print -quit)" ] || {
            echo "ERROR: unpacking '$what' from $apk produced no files." >&2
            exit 1
        }
        find "$out" -type f -print0 | xargs -0 strings -a
        if [ "$what" = manifest ]; then
            find "$out" -type f -print0 | xargs -0 strings -a -el
        fi
    }
    find_apk() {
        local variant="$1"
        local dir="apps/fauna-android/app/build/outputs/apk/$variant"
        [ -d "$dir" ] || return 0
        find "$dir" -name '*.apk' -print -quit
    }
    count() { grep -c -- "$1" "$2" || true; }

    # Element-id patterns are anchored PREFIXES (`strings` prints one id per
    # line), so a new id on an excised page is covered the day it is added and
    # an unrelated id that merely contains the word is not.
    ID_PATS=('^feed-' '^search-' '^atproto-' '^personalization-' '^nostr-' '^labeler-catalog' '^connected-app' '^mail-settings-' '^web-settings-' '^admin-mail-' '^admin-dns-' '^bridge-' '^profile-follow-button' '^profile-tiers-tab' '^link-preview-card' '^folder-paywall-tier-select')
    # UniFFI export symbols, one per line in the `.so`'s dynamic string table.
    FACE_PATS=('^uniffi_fauna_feed_fn_' '^uniffi_fauna_client_search_fn_' '^uniffi_fauna_atproto_settings_machine_fn_' '^uniffi_fauna_labeler_catalog_machine_fn_' '^uniffi_fauna_client_alerts_fn_' '^uniffi_fauna_client_connected_apps_fn_' '^uniffi_fauna_ffi_fn_.*ffifeedmanager' '^uniffi_fauna_ffi_fn_.*ffisearchmanager' '^uniffi_fauna_ffi_fn_.*ffiwebclient' '^uniffi_fauna_ffi_fn_func_build_mail_settings_machine' '^uniffi_fauna_ffi_fn_func_build_dns_management_machine_with_credentials')
    KIDS_ID='social\.fauna\.faunakids'

    # --- column 1: the kids APK carries none of it ---------------------------
    just _android-ffi-flavor kids "kids-safe,kids-floor" arm64
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleKids
    KIDS_APK="$(find_apk kids)"
    [ -n "$KIDS_APK" ] || { echo "ERROR: no kids APK produced." >&2; exit 1; }
    apk_strings "$KIDS_APK" "$WORK/kids-dex" dex > "$WORK/kids-dex.txt"
    apk_strings "$KIDS_APK" "$WORK/kids-all" all > "$WORK/kids-all.txt"
    apk_strings "$KIDS_APK" "$WORK/kids-manifest" manifest > "$WORK/kids-manifest.txt"
    fail=0
    for pat in "${ID_PATS[@]}"; do
        n="$(count "$pat" "$WORK/kids-dex.txt")"
        if [ "$n" != "0" ]; then
            echo "ERROR: the kids APK's dex still carries $n '$pat' element id(s):" >&2
            grep -- "$pat" "$WORK/kids-dex.txt" | sort -u | head -5 | sed 's/^/    /' >&2
            fail=1
        fi
    done
    for pat in "${FACE_PATS[@]}"; do
        n="$(count "$pat" "$WORK/kids-all.txt")"
        if [ "$n" != "0" ]; then
            echo "ERROR: the kids APK's libfauna_ffi.so still exports $n '$pat' UniFFI symbol(s)." >&2
            fail=1
        fi
    done
    if [ "$fail" != "0" ]; then
        echo "  ($KIDS_APK)" >&2
        echo "  A kids artifact must contain NO UI surface and NO wire sender for an" >&2
        echo "  excised plane (family-safety.md § The account age band, the kids-app" >&2
        echo "  bullet, item (4)). Three causes, in order of likelihood:" >&2
        echo "   * a surface in app/src/main that belongs in app/src/noKids/ (with an" >&2
        echo "     inert twin in app/src/kids/ only if src/main still names it);" >&2
        echo "   * a shared render with no BuildConfig.KIDS condition, or one R8 kept;" >&2
        echo "   * an exported face: a fauna-ffi module the kids-safe set reaches again." >&2
        exit 1
    fi
    if [ "$(count "$KIDS_ID" "$WORK/kids-manifest.txt")" = "0" ]; then
        echo "ERROR: the kids APK's manifest does not carry the social.fauna.faunakids" >&2
        echo "  identity (installers/android.md § Store identity)." >&2
        exit 1
    fi

    # --- column 2: the DEFAULT (release) APK must still ship all of it -------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id, or an unpack that silently extracted zero
    # files, would both read as a successful excision.
    just _android-ffi-flavor release "" arm64
    {{slot_build}} apps/fauna-android/gradlew -p apps/fauna-android assembleRelease
    REL_APK="$(find_apk release)"
    [ -n "$REL_APK" ] || { echo "ERROR: no release APK produced." >&2; exit 1; }
    apk_strings "$REL_APK" "$WORK/rel-dex" dex > "$WORK/rel-dex.txt"
    apk_strings "$REL_APK" "$WORK/rel-all" all > "$WORK/rel-all.txt"
    apk_strings "$REL_APK" "$WORK/rel-manifest" manifest > "$WORK/rel-manifest.txt"
    for pat in "${ID_PATS[@]}"; do
        if [ "$(count "$pat" "$WORK/rel-dex.txt")" = "0" ]; then
            echo "ERROR: the DEFAULT Android APK carries no '$pat' element id — the kids" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    for pat in "${FACE_PATS[@]}"; do
        if [ "$(count "$pat" "$WORK/rel-all.txt")" = "0" ]; then
            echo "ERROR: the DEFAULT Android APK's libfauna_ffi.so exports no '$pat' UniFFI" >&2
            echo "  symbol — the kids assertion above is vacuous (or the symbol was renamed)." >&2
            exit 1
        fi
    done
    if [ "$(count "$KIDS_ID" "$WORK/rel-manifest.txt")" != "0" ]; then
        echo "ERROR: the DEFAULT Android APK's manifest carries the Fauna Kids identity." >&2
        exit 1
    fi
    echo "android-kids-check: OK — kids-excised element ids + exported UniFFI faces present in release, absent in kids; kids identity on the kids APK only"

# Build the excised macOS app — the apple family's escape-hatch artifact
# (`dynamic-features.md` § Platform-family surface excision). Twin of
# `mac-release`, with BOTH halves excised: the host FFI built
# `--no-default-features --features store-safe`, and the Swift shell built with
# the apple family's compile condition.
#
# ⚠ THE CONDITION IS NEGATIVE (`FAUNA_EXCISE_PAYMENTS`, absent by default),
# unlike the cargo spine's positive `payments` feature, and that is forced by the
# toolchain rather than chosen: `swift build` can only ADD a compilation
# condition (`-Xswiftc -D`), never remove one, so a positive `FAUNA_PAYMENTS`
# would have to be declared in Package.swift and could not be switched off from
# a recipe. The invariant that matters survives the inversion and is in fact
# stronger: a plain build ships the plane with NO configuration at all, so a new
# target, a stray `swift build`, or an Xcode scheme cannot silently excise it.
# The apple family's own § in dynamic-features.md records this.
mac-store-safe: (apple-ffi-host-store-safe "release")
    {{slot_build}} swift build --package-path apps/fauna-apple --product FaunaMacOS -c release -Xswiftc -DFAUNA_EXCISE_PAYMENTS -Xswiftc -DFAUNA_EXCISE_P2P_SHARE

# The APPLE APP-SHELL column of the store-safe family — tui's and linux's twin
# for the Swift shells. See `tui-store-safe-check` above for why an app-shell
# column is not redundant with the library/server ones: criterion 1 ("no UI
# surface — the feature's element IDs absent") only lives where a shell renders.
#
# ⚠ WHAT THIS COLUMN SEES THAT NO OTHER ONE CAN, on this family specifically.
# The apple shells' payments surface is §§4-5 of the profile Tiers tab and the
# buyer's claim redemption — all in FaunaKit, SHARED by the macOS and iOS
# targets. `ffi-store-safe-check` proves the kind strings leave `libfauna_ffi`;
# only a Swift artifact can prove `subscription-provider-*` /
# `subscription-claim-*` leave the app. Apple has no tip surface today
# (`post-tip-*` is unimplemented here, so it is deliberately NOT a pattern below
# — a pattern that matches nothing in BOTH columns is the vacuity this witness's
# second column exists to prevent). The day a tip render lands, its prefix is
# added here in the same commit.
#
# ⚠ THE iOS ARM IS A TYPECHECK, NOT AN ARTIFACT. There is no iOS archive recipe
# yet, so the iOS half is `swift build --target FaunaiOS` under the same
# condition: it proves the shared shell still compiles excised for the iOS
# target, which is what keeps `Fauna-iOS/` from drifting into an un-excisable
# state between now and the archive recipe. The device artifact's own witness is
# owed when that recipe lands (`dynamic-features.md` § The App-Store escape
# hatch).
#
# ⚠ BOTH COLUMNS ARE RELEASE BUILDS, and that is a correctness requirement, not
# a preference (measured 2026-08-15, the first store-safe Swift build): a DEBUG
# Swift build compiles FaunaKit's `#if DEBUG` automation surface, whose calls to
# the `*ForTest` UniFFI seams only resolve against the **test-helpers** FFI
# flavor — which a store-safe xcframework, being a shipping artifact, must never
# carry (convention 15). Debug + store-safe is therefore an un-buildable pairing
# by construction, and the right resolution is the one that also makes the
# witness honest: grep the artifact a submission would actually carry.
#
# Element-id patterns are PREFIXES, not whole ids, so a new §4/§5 element is
# covered on the day it is added.
#
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → the slot is the recipe's dataset lease: each
# column builds an artifact into the cargo-target dataset and then scans it with
# `strings`, so each column is a produce→consume pair, and three separate
# acquisitions left the dataset evictable between them.
# `_apple-store-safe-check-impl`'s own {{slot_build}} lines stay: under this one
# they are reentrant no-ops (build-slot.py § FIFO ticket queue).
[macos]
apple-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _apple-store-safe-check-impl

_apple-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    BIN="$(swift build --package-path apps/fauna-apple --product FaunaMacOS -c release --show-bin-path)/FaunaMacOS"

    # --- column 1: the store-safe app carries neither the UI nor the wire -----
    just apple-ffi-host-store-safe release
    {{slot_build}} swift build --package-path apps/fauna-apple --product FaunaMacOS -c release -Xswiftc -DFAUNA_EXCISE_PAYMENTS -Xswiftc -DFAUNA_EXCISE_P2P_SHARE
    {{slot_build}} swift build --package-path apps/fauna-apple --target FaunaiOS -c release -Xswiftc -DFAUNA_EXCISE_PAYMENTS -Xswiftc -DFAUNA_EXCISE_P2P_SHARE
    # The price-and-route class (dynamic-features.md § Platform-family surface
    # excision → *The price-and-route class*) is NOT in this list yet, on purpose:
    # on apple the generated `Ids` table is itself a carrier (the Swift row of
    # § Which element IDs belong to a gated feature) and gates an id only once the
    # catalog lists it — measured 2026-10-08 on this store-safe artifact with every
    # render already gated: each of the class's ten uncatalogued ids read exactly 1
    # (the table's line), its two catalogued twins 0. The twelve full ids join both
    # columns here and in the iOS archive witness below with the catalog row's
    # apple re-run.
    for pat in 'subscription-provider-' 'subscription-claim-' 'fauna\.payments\.' 'fauna\.tips\.' 'post-tip-' 'share-transfer-' 'share-serve-' 'offline-share-' 'offline-receive-'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe FaunaMacOS flavor still carries $n '$pat' occurrence(s)." >&2
            echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            echo "  The likely cause is a RENDER that compiles fine because the data it" >&2
            echo "  reads is an ungated inert record, so it paints nothing and still" >&2
            echo "  ships every id it would have painted. Gate the render itself with" >&2
            echo "  #if !FAUNA_EXCISE_PAYMENTS, not just the API call that feeds it." >&2
            exit 1
        fi
    done
    just _apple-assert-no-fp-test-cli "$BIN" "store-safe FaunaMacOS"
    just _apple-assert-no-e2e-env "$BIN" "store-safe FaunaMacOS"

    # --- column 2: the DEFAULT app must still ship all of it ------------------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id would read as a successful excision. Same
    # release config as column 1, and the same pairing `mac-release` ships.
    just apple-ffi-host release
    {{slot_build}} swift build --package-path apps/fauna-apple --product FaunaMacOS -c release
    for pat in 'subscription-provider-' 'subscription-claim-' 'fauna\.payments\.' 'fauna\.tips\.' 'post-tip-' 'share-transfer-' 'share-serve-' 'offline-share-' 'offline-receive-'; do
        n="$(strings -a "$BIN" | grep -c "$pat" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT FaunaMacOS flavor carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    just _apple-assert-no-fp-test-cli "$BIN" "default FaunaMacOS"
    just _apple-assert-no-e2e-env "$BIN" "default FaunaMacOS"
    echo "apple-store-safe-check: OK — payments + p2p-share UI and payments kinds present in default, absent in store-safe; File Provider test CLI and e2e env surface absent from both"

# Convention 15's ARTIFACT witness for apple's File Provider test CLI
# (`FaunaKit/FileProvider/FileProviderTestCLI.swift`, `#if DEBUG` only —
# `e2e-automation-surface-gating.md` § The convention, the Swift bullet): a Release
# artifact must carry none of the CLI's own strings, because the verbs it
# implements (`provision` writes a bearer + backup key from argv; `revoke`,
# `register`, `remove`, `signal` act on the user's File Provider domain) would
# otherwise be drivable by any process running as the user through the signed app.
# Called on every Release artifact the two `*-store-safe-check` recipes build.
#
# The markers are distinctive literals the CLI prints, and each is LONGER THAN 15
# UTF-8 bytes on purpose: Swift stores a shorter literal inline in the instruction
# stream, where `strings` never sees it (measured 2026-09-21 on a Release
# `FaunaMacOS`: `usage: Fauna signal <folder>` 1 hit, `provision OK` and
# `signal OK: ` 0). `test_apple_release_surface_gating.py` pins that each marker is
# a string literal in the CLI source and over that length — the vacuity guard this
# absence-only witness cannot carry itself, since no Release build has the CLI to
# find. The control it stands in for was measured once: a pre-fix Release
# `FaunaMacOS` (CLI ungated) carried all four markers.
fp_test_cli_markers := "usage: Fauna provision|usage: Fauna signal|provision FAILED: actor_id|register FAILED: "

[macos]
_apple-assert-no-fp-test-cli binary label:
    #!/usr/bin/env bash
    set -euo pipefail
    n="$(strings -a "{{binary}}" | grep -cE '{{fp_test_cli_markers}}' || true)"
    if [ "$n" != "0" ]; then
        echo "ERROR: the {{label}} release artifact carries $n File Provider test-CLI string(s)." >&2
        echo "  FileProviderTestCLI (provision / revoke / register / remove / signal) is" >&2
        echo "  automation surface and must be compiled out of every shipped artifact —" >&2
        echo "  convention 15, e2e-automation-surface-gating.md § The convention. It and" >&2
        echo "  its one call site (PlatformMainEntry) belong under #if DEBUG." >&2
        exit 1
    fi

# Convention 15's ARTIFACT witness for apple's e2e ENVIRONMENT surface
# (`e2e-automation-surface-gating.md` § The convention, the Swift bullet). Every
# `FAUNA_E2E_*` read in hand-written apple Swift goes through the one gated door
# `FaunaKit/Sources/FaunaKit/Testing/AutomationRegistry.swift`'s `E2eEnv`, whose `#else` twin names no
# variable — so a Release artifact must carry the prefix ZERO times.
#
# Why a prefix and not a marker list: unlike the File Provider CLI above, the
# thing being excluded IS the variable name, so the artifact can be asked the
# question directly. `FAUNA_E2E_` is 10 bytes, but every name it prefixes is 16+
# (`FAUNA_E2E_BRIDGE` is the shortest), comfortably past the 15-byte Swift
# small-string cutoff that would hide a literal from `strings` — pinned, with the
# rest of this witness's source half, in `test_apple_release_surface_gating.py`.
#
# The severity is the payload's: three of these variables are REDIRECTS, not mode
# toggles — `FAUNA_E2E_CREDENTIAL_DIR` relocates the credential store and stands
# in for the device-owner re-auth verdict, `FAUNA_E2E_DOWNLOAD_DIR` relocates the
# user's exported snapshot and account data. Called on every Release artifact the
# two `*-store-safe-check` recipes build.
apple_e2e_env_marker := "FAUNA_E2E_"

[macos]
_apple-assert-no-e2e-env binary label:
    #!/usr/bin/env bash
    set -euo pipefail
    n="$(strings -a "{{binary}}" | grep -c '{{apple_e2e_env_marker}}' || true)"
    if [ "$n" != "0" ]; then
        echo "ERROR: the {{label}} release artifact names $n e2e environment variable(s):" >&2
        strings -a "{{binary}}" | grep -o '{{apple_e2e_env_marker}}[A-Z_]*' | sort -u | sed 's/^/    /' >&2
        echo "  A shipped apple artifact must read no harness variable — whoever controls" >&2
        echo "  the launch environment would otherwise relocate the credential store, stand" >&2
        echo "  in for the device-owner re-auth verdict, or redirect the user's exported" >&2
        echo "  data (convention 15, e2e-automation-surface-gating.md § The convention)." >&2
        echo "  Read it through AutomationRegistry.swift's E2eEnv door instead, and" >&2
        echo "  put the whole e2e branch under #if DEBUG — a constant-nil twin the optimizer" >&2
        echo "  folds is not a gate this family accepts." >&2
        exit 1
    fi

# The iOS twin of `apple-store-safe-check`, over the artifact a SUBMISSION
# carries: an unsigned Release `.xcarchive` for a real device, not a `swift
# build` product (`dynamic-features.md` § The App-Store escape hatch).
#
# **Why this exists as its own recipe rather than another column above.** The
# macOS check's iOS column is `swift build --target FaunaiOS`, which compiles the
# iOS sources FOR THE MACOS HOST through `CrossPlatformUI.swift`'s shims
# (`Package.swift:94-100` says so). That column therefore cannot see inside any
# `#if os(iOS)` block — FaunaKit has 37 — and it produces no app binary to grep.
# Only the `.xcodeproj` archive path yields the real thing.
#
# **The propagation mechanism, settled 2026-08-22.**
# Xcode does not propagate `OTHER_SWIFT_FLAGS` from an app target into a local
# SwiftPM package's targets, and every payments surface lives in the `FaunaKit`
# package — so the `-Xswiftc -D` spelling the SPM recipes use has no project
# equivalent to inherit. A **command-line** build setting is a different
# mechanism from target-level inheritance: it applies at the highest precedence
# to every target in the build, package targets included. Measured: the Swift-side
# element ids `subscription-provider-` (15 → 0) and `subscription-claim-` (10 → 0)
# vanish from the archived app binary, and those come from
# `FaunaKit/Sources/FaunaKit/Generated/UiIds.swift`'s `#if !FAUNA_EXCISE_PAYMENTS`
# extension — Swift source, reachable only by the flag.
#
# Wall clock (macOS VM, 2026-08-22): the archive step itself is ~165 s in either
# column; the pole is the 5-slice device xcframework it needs first (~19 min
# cold). Budget ~22 min cold from nothing to a submittable archive.
[macos]
apple-ios-store-safe-check: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    SCRATCH="{{tmp_root}}/fauna-ios-store-safe-check"
    rm -rf "$SCRATCH"; mkdir -p "$SCRATCH"

    archive() {  # $1 = archive name, $2… = extra xcodebuild settings
        local name="$1"; shift
        rm -rf "$SCRATCH/$name.xcarchive"
        {{slot_build}} xcodebuild archive \
            -project apps/fauna-apple/Fauna.xcodeproj \
            -scheme Fauna-iOS \
            -destination generic/platform=iOS \
            -archivePath "$SCRATCH/$name.xcarchive" \
            -skipPackagePluginValidation -skipMacroValidation \
            CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO "$@" >"$SCRATCH/$name.log" 2>&1 || {
                echo "ERROR: the $name iOS archive failed to build; tail of $SCRATCH/$name.log:" >&2
                tail -40 "$SCRATCH/$name.log" >&2
                exit 1
            }
        strings -a "$SCRATCH/$name.xcarchive/Products/Applications/Fauna-iOS.app/Fauna-iOS" \
            > "$SCRATCH/$name.strings"
        # Convention 15: no shipped apple artifact carries the File Provider test
        # CLI, and none names an e2e environment variable.
        just _apple-assert-no-fp-test-cli \
            "$SCRATCH/$name.xcarchive/Products/Applications/Fauna-iOS.app/Fauna-iOS" "$name iOS archive"
        just _apple-assert-no-e2e-env \
            "$SCRATCH/$name.xcarchive/Products/Applications/Fauna-iOS.app/Fauna-iOS" "$name iOS archive"
    }

    # --- column 1: the store-safe archive carries neither the UI nor the wire --
    # ⚠ The 5-slice build must be the LAST FFI build before the archive: a
    # `mac-debug` / `swift-test` in between reassembles the xcframework HOST-ONLY
    # and the archive then dies on "no library for this platform".
    just apple-ffi-store-safe
    archive store-safe OTHER_SWIFT_FLAGS='$(inherited) -D FAUNA_EXCISE_PAYMENTS -D FAUNA_EXCISE_P2P_SHARE'
    # The price-and-route class (dynamic-features.md § Platform-family surface
    # excision → *The price-and-route class*) — the class this witness's first
    # run surfaced — is NOT in this list yet: the generated `Ids` table carries
    # every uncatalogued id into the apple artifacts (the macOS witness above says
    # how that was measured), so the twelve full ids join both columns with the
    # catalog row's apple re-run —
    # the run that proves the archive clean of the class.
    for pat in 'subscription-provider-' 'subscription-claim-' 'fauna\.payments\.' 'fauna\.tips\.' 'post-tip-' 'share-transfer-' 'share-serve-' 'offline-share-' 'offline-receive-'; do
        n="$(grep -c "$pat" "$SCRATCH/store-safe.strings" || true)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe iOS archive still carries $n '$pat' occurrence(s)." >&2
            echo "  A submitted artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            exit 1
        fi
    done

    # --- column 2: the DEFAULT archive must still ship all of it ---------------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id would read as a successful excision. This is
    # not hypothetical here: the id table was restructured 2026-08-17 (the
    # generated payments ids moved inside an `extension Ids`), which is exactly
    # the change that would silently hollow out column 1.
    just apple-ffi
    archive default
    for pat in 'subscription-provider-' 'subscription-claim-' 'fauna\.payments\.' 'fauna\.tips\.' 'post-tip-' 'share-transfer-' 'share-serve-' 'offline-share-' 'offline-receive-'; do
        n="$(grep -c "$pat" "$SCRATCH/default.strings" || true)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT iOS archive carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
    done
    echo "apple-ios-store-safe-check: OK — payments + p2p-share UI and payments kinds present in the default archive, absent in the store-safe one"

# The WASM/web column of the same witness (`dynamic-features.md` § The
# feature-matrix test story). Third and last of the store-safe family:
# `ffi-store-safe-check` keeps the native-client flavor honest,
# `nest-store-safe-check` the server one, this one the browser one.
#
# WHY IT IS NOT REDUNDANT WITH THE FFI COLUMN — and note the obvious version of
# this argument is WRONG, so don't restate it: the two dependency graphs are not
# meaningfully disjoint (of the 42 fauna-* crates in fauna-wasm's wasm32-only
# block, 40 are also fauna-ffi deps — measured 2026-08-10). What differs is the
# CFG SET. A host-target build never compiles `#[cfg(target_arch = "wasm32")]`
# code at all, and fauna-wasm's whole WS-RPC face (src/rpc.rs, gated at the mod)
# is exactly that — which is literally where the W1 defect lived: rpc.rs named
# `fauna_protocol::payments::` unconditionally and NO host build in the
# workspace, ffi-store-safe-check included, could have seen it. Compounding it:
# each root resolves the shared crates with its own feature set, and each root's
# payments/zaps forwarding is its own declaration.
#
# WHY IT IS NOT REDUNDANT WITH wasm-seam-check / wasm-chunk-check: those build
# the DEFAULT flavor. Nothing but this recipe compiles `fauna-wasm` excised —
# which is exactly how the root came to be declared excisable while not being
# so (see below).
#
# WHY PLAIN `cargo build`, NOT `wasm-pack`. wasm-pack is a RELEASE build plus
# wasm-bindgen-cli plus wasm-opt — the ~30-minute class of work `just web-check`
# does, far too heavy for three flavors on the check tier. A plain
# `cargo build --target wasm32-unknown-unknown` emits the cdylib `.wasm`
# directly, and that artifact already carries both witness axes: the kind
# strings AND the wasm-bindgen export names (`paymentsProvidersSet`, …), which
# are the web twin of the UniFFI faces the ffi column greps. Measured
# 2026-08-10 on this exact recipe: 79 s for a COLD default column (whole dep
# graph from scratch, under the build-slot queue), well under the ffi column's
# "a few minutes cold". So the full three-column witness costs about what a
# compile-only check would have, and unlike a compile-only check it can
# actually see a leak.
#
# THE DEFECT THAT MOTIVATED IT, so nobody re-litigates the cost: W1 declared
# `payments` excisable at both client flavor roots and shipped a structural pin
# that agreed. `fauna-wasm --no-default-features` did not compile at all, and
# nothing noticed for a whole release cycle for the simple reason that nothing
# ever built it. A flavor's manifest entry is a claim; the only thing that
# discharges it is a build of that flavor.
# ONE slot for the whole recipe (build-machine-resources.md § Build/e2e slot
# locks → The slot is the recipe's dataset lease). Every column builds an
# artifact into the cargo-target dataset and then scans it with `strings`, so
# each column is a produce→consume pair. Between two separate acquisitions the
# checkout holds no slot at all, and the pressure tier evicts exactly there —
# measured twice on 2026-08-20. The body's own
# {{slot_build}} lines stay: under this one they are reentrant no-ops
# (build-slot.py § FIFO ticket queue).
wasm-store-safe-check: i18n-generate providers-generate
    {{slot_build}} just _wasm-store-safe-check-impl

_wasm-store-safe-check-impl:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    WASM="$TARGET_DIR/wasm32-unknown-unknown/debug/fauna_wasm.wasm"
    # A `strings` scan of a VANISHED artifact is an EMPTY scan, and every
    # absence assertion below reads an empty scan as a clean excision — a
    # FALSE GREEN on the excision gate rather than a loud failure. So each
    # column asserts its artifact the moment its build returns: if it is gone,
    # the cargo-target dataset was reclaimed under us (build-machine-resources.md
    # § Build/e2e slot locks → the slot is the recipe's dataset lease).
    built() {
        [ -s "$1" ] && return 0
        echo "ERROR: $1 is missing right after the build that writes it —" >&2
        echo "  refusing to scan nothing, which every absence assertion here" >&2
        echo "  would read as a clean excision. Re-run; the build state rebuilds." >&2
        exit 1
    }

    # Both witness axes, per plane. The kind strings are what a nest would ever
    # see on the wire; the camelCase names are the JS faces the SPA could call.
    # An excised bundle must carry neither — absence of only one would leave
    # either a callable face over a renamed kind or a live kind behind a dead
    # face (`dynamic-features.md` § What "completely compiled away" means).
    PAY_PATS=('fauna\.payments\.' 'fauna\.tips\.' 'paymentsProvidersSet' 'paymentsClaimsRedeem')
    # `zaps` is a SUBSET member of `payments`, so it excises with the money
    # plane in the store-safe column and on its own in the Damus column.
    ZAP_PATS=('fauna\.nostr\.zap_signers\.' 'nostr\.zaps\.total' 'nostrZapSignersList' 'nostrZapTotal')
    # Criterion 1, the axis this column lacked until 2026-08-18 — the
    # wasm twin of the ffi column's own `PAY_ID_PATS`, added the same way and for
    # the same measured reason. PREFIXES from ui.yaml's
    # `gated_features.payments.id_prefixes`, so a new §4/§5/tip element is
    # covered the day it is added.
    #
    # ⚠ THE CARRIER HERE IS `#[wasm_bindgen]`, NOT `#[uniffi::export]`, and the
    # ffi column's green is no evidence about this one (§ The wasm root's own
    # column: one witness per flavor root). wasm-bindgen records each exported
    # item's doc comment in the `__wasm_bindgen_unstable` metadata section so the
    # CLI can emit it as JSDoc on the generated JS face — so the docstring text
    # is in the `.wasm` THIS recipe already greps, no wasm-pack needed. Measured
    # 2026-08-18 on the store-safe artifact BEFORE the fix: `post-tip-` **3**
    # (`tipAmount`/`tipCount`/`tipMore`) + `subscription-claim-` **1**
    # (`claimStatusLabel`), every one a doc line on an ungated inert formatter.
    #
    # A SUBSET of the catalog, measured rather than assumed, exactly as the ffi
    # column's array is. `subscription-provider-` is **0** in BOTH columns here —
    # no doc comment in fauna-wasm's graph spells a §4 provider id — and the
    # `p2p-share` pair is 0 for the structural reason that fauna-wasm has no
    # `p2p` feature. Either joins the day a doc comment here names it.
    # The price-and-route class (`dynamic-features.md` § Platform-family surface
    # excision → *The price-and-route class*) has five doc-comment carriers in
    # this graph — the `updateComposeSell`, `resolvePostUnlockOffer` and
    # `buyUnlockOffer` exports' docs spell `compose-sell-price`,
    # `compose-sell-subscribers-free`, `gated-post-price`, `gated-post-payment-link`
    # and `gated-post-buy-button` — each a `payments`-gated doc line since
    # 2026-10-07. They join this array (as FULL ids: the natural prefixes also
    # cover ids outside the plane) with the catalog row that lands the class:
    # this witness compiles for wasm32, which the ruling pass's machine could
    # not do, so the measurement is that row's.
    PAY_ID_PATS=('subscription-claim-' 'post-tip-')

    absent() {  # absent <flavor-label> <pattern>...
        local flavor="$1"; shift
        for pat in "$@"; do
            n="$(strings -a "$WASM" | grep -c "$pat" || true)"
            if [ "$n" != "0" ]; then
                echo "ERROR: the $flavor fauna-wasm flavor still carries $n '$pat' occurrence(s)." >&2
                echo "  That plane must be COMPLETELY compiled away (dynamic-features.md" >&2
                echo "  § What \"completely compiled away\" means: no wire senders, no re-enable path)." >&2
                echo "  Two likely causes: (1) a crate in fauna-wasm's graph re-enabled the" >&2
                echo "  feature through cargo's additive unification; (2) a wasm32-gated" >&2
                echo "  module (src/rpc.rs and friends) names the plane unconditionally —" >&2
                echo "  no host build compiles those, so ffi-store-safe-check cannot have" >&2
                echo "  caught it. That is the exact shape of the W1 defect. Check with:" >&2
                echo "    cargo tree -p fauna-wasm --target wasm32-unknown-unknown -e features -i fauna-protocol" >&2
                echo "  which must show fauna-wasm itself as the ONLY enabler." >&2
                echo "  (3) a DOC COMMENT on an UNGATED #[wasm_bindgen] item names the kind or" >&2
                echo "      the element id. wasm-bindgen records each exported item's docs in" >&2
                echo "      the __wasm_bindgen_unstable metadata section so the CLI can emit" >&2
                echo "      them as JSDoc on the generated face — so the prose ships in this" >&2
                echo "      .wasm AND in fauna_wasm.js. This was the whole of the element-id" >&2
                echo "      residue found and fixed 2026-08-18." >&2
                echo "      Fix: gate the id-naming doc line, do NOT reword it —" >&2
                echo "        #[cfg_attr(feature = \"payments\", doc = \" …post-tip-total…\")]" >&2
                echo "      which keeps the id greppable in source and out of the artifact." >&2
                echo "      Confirm which you have — the string itself says so:" >&2
                echo "      strings -a \$WASM | grep '$pat'" >&2
                exit 1
            fi
        done
    }

    present() {  # present <flavor-label> <pattern>...
        local flavor="$1"; shift
        for pat in "$@"; do
            n="$(strings -a "$WASM" | grep -c "$pat" || true)"
            if [ "$n" = "0" ]; then
                echo "ERROR: the $flavor fauna-wasm flavor carries no '$pat' — the absence" >&2
                echo "  assertions in the other columns are therefore vacuous. Either the plane" >&2
                echo "  was removed from that build (a product regression) or the strings were" >&2
                echo "  renamed and this witness needs its patterns updated." >&2
                exit 1
            fi
        done
    }

    # --- column 1: store-safe must reach neither money nor zaps --------------
    # Spelled with the COMPLEMENT feature, exactly as the ffi and nest columns
    # are, so a future non-registry default feature stays in both flavors by
    # construction rather than being silently dropped here.
    {{slot_build}} cargo build --locked -p fauna-wasm --target wasm32-unknown-unknown --no-default-features --features store-safe
    built "$WASM"
    absent "store-safe" "${PAY_PATS[@]}" "${ZAP_PATS[@]}" "${PAY_ID_PATS[@]}"

    # --- column 2: the Damus flavor keeps payments, drops zaps ---------------
    # The subset edge witnessed on the artifact rather than asserted from the
    # manifest: manifest text can prove `zaps` implies `payments`, and ONLY a
    # build can prove the converse fails. Without this column `zaps` could be a
    # plain alias of `payments` with every structural pin still green.
    {{slot_build}} cargo build --locked -p fauna-wasm --target wasm32-unknown-unknown --no-default-features --features store-safe,payments
    built "$WASM"
    absent "Damus (payments-without-zaps)" "${ZAP_PATS[@]}"
    present "Damus (payments-without-zaps)" "${PAY_PATS[@]}" "${PAY_ID_PATS[@]}"

    # --- column 3: the DEFAULT flavor must still ship both -------------------
    # Without this the absences above are indistinguishable from greps that
    # match nothing, and the escape hatch would read "green" while excising
    # planes that were never there.
    {{slot_build}} cargo build --locked -p fauna-wasm --target wasm32-unknown-unknown
    built "$WASM"
    present "DEFAULT" "${PAY_PATS[@]}" "${ZAP_PATS[@]}" "${PAY_ID_PATS[@]}"

    echo "wasm-store-safe-check: OK — payments+zaps in default, payments-only in damus, neither in store-safe (kind strings, wasm faces AND element ids)"

# ── the web SHELL's store-safe flavor (2026-08-16) ───────────────────────────
#
# The web family's compile condition is a **vite define** — `__FAUNA_PAYMENTS__`,
# fed by `FAUNA_WEB_PAYMENTS` (`apps/fauna-web/vite.config.ts`) — plus the
# isolated-module pattern that actually removes the surface
# (`dynamic-features.md` § Platform-family surface excision). Unlike every other
# shell there is no cargo crate and no manifest to read the flavor from: the
# switch is an environment variable at build time, so the structural pins for
# this leg are recipe- and source-shaped (`test_payments_excision_spine.py`'s web
# section), exactly as apple's had to be.
#
# ⚠ Deliberately NOT build-if-stale gated, unlike `just web`. A payments flavor
# flip changes no source file, so the stamp would report the tree fresh and hand
# back the OTHER flavor's `build/` — a false green that ships the plane in the
# artifact a store submission carries. (`just web-test` escapes the same trap
# only by accident: its onboarding wasm chunk swap dirties `static/`.)
#
# The wasm half of the excised web bundle is `just wasm-store-safe-check`'s
# column; this recipe is the SPA half, and both are needed for an excised web
# artifact (§ The feature-matrix test story — one witness per flavor root, built
# for that root's own target).
web-store-safe: wasm i18n-generate providers-generate
    deno install --config apps/fauna-web/deno.json
    {{slot_build}} bash -c 'cd apps/fauna-web && FAUNA_WEB_PAYMENTS=0 deno task build'

# The web APP-SHELL column of the store-safe family — the FIFTH app-shell
# witness, after tui, linux, apple and android. See `tui-store-safe-check` for
# why an app-shell column is not redundant with the library/server ones:
# criterion 1 ("no UI surface — the feature's element IDs absent") only lives
# where a shell renders.
#
# THREE THINGS ABOUT THIS COLUMN ARE WEB-SPECIFIC. Each was measured on
# 2026-08-16, before the leg was written, and each would silently break a
# verbatim copy of the tui/linux recipe.
#
# (1) THE SECOND AXIS IS FACE NAMES, NOT KIND STRINGS. The SPA never spells a
#     `fauna.payments.*` kind: every call goes through a wasm-bindgen face by
#     name, and the only occurrences of the kind strings in web source are
#     COMMENTS, which the vite build strips. Measured against the default
#     bundle: `subscription-provider-` 15, `subscription-claim-` 10, `post-tip-`
#     10 — and `fauna\.payments\.` **0**. Copying tui's pattern list verbatim
#     would add a pattern matching nothing in BOTH columns, which is exactly the
#     vacuity the second column exists to prevent. So web's criterion-2 axis is
#     the face names (`paymentsProvidersSet`, `paymentsClaimsMint`,
#     `paymentsKnownKinds`, `paymentsWebhookUrl`) — the browser twin of the
#     UniFFI symbols `ffi-store-safe-check` greps, and the same axis
#     `wasm-store-safe-check` greps on the `.wasm` side. (`tipAmount` sat here
#     too until it was dropped: its glue is deleted by the snapshot's own
#     `__wbindgen` exclusion, not by `FAUNA_WEB_PAYMENTS=0`, so it never
#     excised on this axis — a criterion-1-shaped tripwire wearing this
#     axis's clothes, not a genuine member of it.)
#
# (2) THE SCOPE IS THE SPA'S OWN OUTPUT, NOT ALL OF `build/`. SvelteKit copies
#     `static/` into `build/`, so the built tree also holds the wasm chunks
#     (`fauna_wasm_bg.wasm`, `fauna_wasm.js`, …). Those are the WASM root's
#     artifact and have their own column; greping them here would (a) make this
#     recipe pay for two full wasm builds, which is the cost that keeps apple's
#     column out of the mac check, and (b) fail column 1 for a plane this
#     recipe does not build. Scope: `_app/` + `index.html` + `service-worker.js`.
#
# (3) EACH COLUMN'S OUTPUT IS SNAPSHOTTED BEFORE THE NEXT BUILD RUNS. Both
#     flavors write the same `build/` directory, and `deno task build` does not
#     guarantee a clean one — a leftover chunk from column 1 would let column 2
#     pass on the wrong artifact. `rm -rf` before each build, copy out after.
#
# Element-id patterns are PREFIXES, not whole ids, so a new §4/§5 or tip element
# is covered on the day it is added.
web-store-safe-check: wasm i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    # ONE build grant for the whole body (justfile `slot_build_body`): its
    # builds share a product that must not be evicted between them.
    {{slot_build_body}}
    deno install --config apps/fauna-web/deno.json
    WORK="$(mktemp -d "{{tmp_root}}/web-store-safe-check.XXXXXX")"
    trap 'rm -rf "$WORK"' EXIT
    BUILD="apps/fauna-web/build"

    # The SPA's own output, excluding the wasm chunks copied in from `static/`
    # — see note (2) above.
    #
    # ⚠ AND excluding the wasm-bindgen JS GLUE, which note (2) does not cover
    # because vite INLINES it into `_app`. `$lib/wasm` imports
    # `../../static/fauna_wasm.js`, so the generated `WsRpcClient` class — every
    # method name included — is bundled as an app chunk rather than served as a
    # sibling file. Measured 2026-08-16: a correctly-excised store-safe bundle
    # still carried 2 `paymentsProvidersSet` occurrences, both of them that
    # class's own method definitions (`wsrpcclient_paymentsProvidersSet(this.
    # __wbg_ptr, …)`). Those bytes are the WASM root's artifact and they excise
    # only when `fauna-wasm` is built store-safe, which is
    # `just wasm-store-safe-check`'s column — greping them here would make this
    # recipe permanently red for a plane it does not build.
    #
    # ⚠ THAT REASONING IS EXACT FOR `paymentsProvidersSet` AND WAS FALSE FOR
    # `tipAmount` (measured 2026-08-18): `paymentsProvidersSet` really
    # does excise in the store-safe wasm flavor; `tipAmount`/`tipCount`/
    # `tipMore`/`claimStatusLabel` are UNGATED inert formatters, exported from
    # EVERY flavor of BOTH roots deliberately (the shape ratified at
    # `fauna-ffi` — only the id-naming DOC LINE is gated, never the function).
    # A `tipAmount` pattern on this axis was therefore satisfied only by the
    # `__wbindgen` deletion above — it was asserting the SPA's TypeScript
    # wrapper (`$lib/payments.ts`) tree-shakes out, never that the wasm face
    # excises, despite sitting in the same list as patterns that do. **Dropped
    # 2026-08-18:** this axis names only faces that genuinely excise;
    # a criterion-2 pattern whose sole witness is "the glue we already delete"
    # is a tripwire wearing this axis's clothes, not a member of it — the
    # `post-tip-`/`subscription-claim-`/`subscription-provider-` element-id
    # patterns below are criterion 1 and unaffected, they excise for real.
    #
    # The exclusion is by CONTENT (`__wbindgen`, present in every generated glue
    # file and in nothing hand-written), not by chunk name, because chunk names
    # are content hashes that change on every edit. It cannot hide a real leak
    # silently: if it ever swallowed the SPA's own call sites too, column 2 —
    # which requires each face PRESENT in the default bundle — goes red.
    snapshot() {
        mkdir -p "$WORK/$1"
        cp -r "$BUILD/_app" "$WORK/$1/_app"
        cp "$BUILD/index.html" "$BUILD/service-worker.js" "$WORK/$1/"
        grep -rl '__wbindgen' "$WORK/$1" | xargs -r rm -f
    }

    # --- column 1: the store-safe SPA carries neither the UI nor the glue -----
    rm -rf "$BUILD"
    (cd apps/fauna-web && FAUNA_WEB_PAYMENTS=0 deno task build)
    snapshot store-safe
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' \
               'subscription-tier-form-asking-price' 'compose-sell-asking-price' \
               'paymentsProvidersSet' 'paymentsClaimsMint' 'paymentsKnownKinds' \
               'paymentsWebhookUrl'; do
        # ⚠ `|| true` inside the braces, not after the pipe: grep exits 1 when it
        # finds nothing, and under `set -euo pipefail` that aborts the recipe —
        # on the column where finding nothing is the PASS. Same guard the tui and
        # linux recipes carry.
        n="$({ grep -ro "$pat" "$WORK/store-safe" || true; } | wc -l)"
        if [ "$n" != "0" ]; then
            echo "ERROR: the store-safe web bundle still carries $n '$pat' occurrence(s)." >&2
            echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
            echo "  reachable wire face for the plane — dynamic-features.md" >&2
            echo "  § What \"completely compiled away\" means, items 1-2." >&2
            echo "  Two causes are likely, and they need different fixes:" >&2
            echo "   • an element id survived: a RENDER is not behind" >&2
            echo "     \`{#if __FAUNA_PAYMENTS__}\`, or its component is imported from a" >&2
            echo "     module that is NOT itself behind the condition. PostSummary.tips is" >&2
            echo "     a deliberately ungated inert record, so a tip render compiles fine," >&2
            echo "     paints nothing, and still ships every id — gate the render, not the" >&2
            echo "     resolver feeding it." >&2
            echo "   • a face name survived: a payments function was defined outside" >&2
            echo "     \`\$lib/payments\` — in \$lib/rpc, \$lib/wasm or \$lib/value-format, all" >&2
            echo "     of which are unconditionally in the bundle. Move it back." >&2
            exit 1
        fi
    done

    # --- column 2: the DEFAULT SPA must still ship all of it ------------------
    # Without this the assertions above are indistinguishable from greps that
    # match nothing — a renamed id or face would read as a successful excision.
    rm -rf "$BUILD"
    (cd apps/fauna-web && deno task build)
    snapshot default
    for pat in 'subscription-provider-' 'subscription-claim-' 'post-tip-' \
               'subscription-tier-form-asking-price' 'compose-sell-asking-price' \
               'paymentsProvidersSet' 'paymentsClaimsMint' 'paymentsKnownKinds' \
               'paymentsWebhookUrl'; do
        n="$({ grep -ro "$pat" "$WORK/default" || true; } | wc -l)"
        if [ "$n" = "0" ]; then
            echo "ERROR: the DEFAULT web bundle carries no '$pat' — the store-safe" >&2
            echo "  assertion above is therefore vacuous. Either the surface was removed" >&2
            echo "  from the default build (a product regression) or it was renamed and" >&2
            echo "  this witness needs its patterns updated." >&2
            exit 1
        fi
        # Printed because the two-column counts are what `dynamic-features.md`
        # § The feature-matrix test story records for every shell, and a number
        # nobody prints gets re-measured by hand at every doc update.
        echo "  $pat: $n / 0"
    done
    echo "web-store-safe-check: OK — payments UI + faces present in default, absent in store-safe"

# CHECK-tier gate (merge-gate-check.md § Merge-gate check) — RELEASE-FLAVOR compile
# check over the app/bin crates shipped from the primary dev machine. Every heavy compile gate above builds DEV: test-compile-check,
# workspace-clippy-check and workspace-test-check all default to dev profile, and
# tui-store-safe-check/linux-store-safe-check build plain `cargo build` (also dev)
# — so a release-only compile break (a `cfg(any(debug_assertions, feature =
# "e2e-agent"))` seam missing its inert release twin) had no gate on ANY machine
# until a session's one-off workspace-scale release build caught fauna-tui's
# `drain_pending_ui_messages` E0425 (`git log --grep "drain_pending_ui_messages
# had no release twin"`) — the first thing ever to compile tui at release on this
# fleet.
#
# Same crate list as workspace-test-check's --exclude set, MINUS fauna-nest and
# fauna-linux: both ship in release (the Docker image; the Linux package) and
# workspace-test-check excludes them only because RUNNING their tests is
# expensive/OS-gated — neither reason applies to a plain `cargo check`. Everything
# still excluded is either windows-only (fauna-bridge-service,
# fauna-nest-service, fauna-cfapi,
# fauna-shellext-fixture) or a wasm32-target crate this native check cannot build
# at all (wasm-chunk-check / wasm-store-safe-check already cover those in their
# own wasm32 flavor).
# ⚠ This sentence also called `uniffi-bindgen-cs` windows-only — a SECOND reason
# for the same line as workspace-test-check's, and a false one: it is a
# C#-bindings generator that builds on Linux, which that recipe's copy called a
# vendored fork instead. Both readings were moot, because the name matched no
# package and so neither recipe ever excluded anything. Line removed 2026-09-12;
# the measurement is in workspace-test-check's comment.
# fauna-ctl and fauna-shell-ext WERE excluded here too, on the same "windows-only"
# sentence. That
# claim had already been refuted for the test gate the same day, and the question
# this comment used to defer — "is a release-profile compile of their dep trees
# too expensive to gate?" — turned out to cost 57 seconds cold: `cargo check
# --release --locked -p fauna-ctl -p fauna-shell-ext` exits 0. In the recipe below
# it is cheaper still, because every dep either crate pulls (fauna-core, fauna-ipc,
# fauna-i18n, tokio, reqwest, clap) is already in this gate's own selection.
# ⚠ What that DOES and does NOT buy, because the two crates differ:
#   * fauna-ctl was dev/diagnostic only and shipped in no artifact (USER-ratified
#     2026-06-20); it was deleted 2026-10-02 with the bridge service's pipe.
#   * fauna-shell-ext DOES ship, as the Windows `fauna_shell.dll` cdylib, and its
#     `[target.'cfg(windows)'.dependencies]` windows/windows-core deps plus every
#     #[cfg(windows)] body are compiled OUT on this host. So this gate covers its
#     PORTABLE slice only. The shipped artifact's release compile still happens
#     first in `.github/workflows/release.yml`, at release-cut time — as it does
#     for fauna-nest-service and fauna-bridge-service, which stay excluded because
#     they genuinely do not build here, and for the `cfg(windows)` half of
#     fauna-sync-agent (the shipped windows agent; this gate compiles its portable
#     slice, since the crate is an ordinary member here). Windows DOES run a
#     release-flavor gate — merge-gate-check-win.sh's gate 4, `windows-release-build`
#     — but its Rust half is `windows-ffi` alone (release-profile fauna-ffi, no
#     test-helpers) plus the C# app at Configuration=Release. None of the four
#     Rust crates the installer actually ships is in it, so their first release
#     compile is still release.yml, at cut time. Widening gate 4 is Windows work and
#     is captured as such, not guessed at here.
# Compile-only, deliberately no `-D warnings`: the release
# flavor currently carries real warnings (fauna-tui, fauna-linux, from code paths
# only the gated flavor reaches) that are a separate cleanup, not this gate's job.
release-flavor-compile-check:
    {{slot_build}} cargo check --release --locked --workspace --keep-going \
        --exclude fauna-bridge-service \
        --exclude fauna-nest-service \
        --exclude fauna-cfapi \
        --exclude fauna-shellext-fixture \
        --exclude fauna-wasm \
        --exclude fauna-wasm-content-index \
        --exclude fauna-wasm-onboarding \
        --exclude fauna-wasm-folders \
        --exclude fauna-wasm-backups \
        --exclude fauna-wasm-media \
        --exclude fauna-wasm-labeler-catalog \
        --exclude fauna-wasm-connected-apps \
        --exclude fauna-wasm-atproto-settings \
        --exclude fauna-wasm-launch

# Build the Go atproto PDS bridge binary (role "atproto.pds";
# docs/goal/behavior/atproto-pds-bridge.md § Architecture). Since S2 this is a
# CGO build like mail-bridge-build: the DID mint loop imports
# internal/mailfauna to HPKE-Open the sealed per-user identity-key blobs, so
# it links libfauna_ffi.so (S1's CGO_ENABLED=0 was a skeleton property, not a
# contract).
atproto-bridge-build: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    {{slot_build}} go -C bins/fauna-bridges build ./cmd/fauna-atproto-bridge
    # Convention 15 witness, on the artifact rather than on trust: every e2e
    # seam the tagged flavor carries must be absent from the production flavor —
    # the hostile-rotation capability (seize.go, `-tags fauna_e2e_seize`, keyed
    # on its FLAG NAME because that is the whole reachable surface) and, since
    # 2026-09-13, the harness redirect seams (`-tags fauna_e2e_fixtures`: fake
    # DNS, fake PLC directory, proxy fixtures and, since 2026-09-25, the
    # permission-set document fixtures — keyed on the ENV-VAR NAMES
    # the seams read, which a production twin never spells). Derived from the
    # built artifact, the shape `scripts/check-wasm-seam-exclusion.py`
    # established. e2e-automation-surface-gating.md § Implementation status
    # today → the Go bridges' leg.
    for lit in seize-did FAUNA_ATPROTO_FAKE_DNS_URL FAUNA_ATPROTO_PLC_DIRECTORY_URL FAUNA_ATPROTO_PROXY_FIXTURES FAUNA_ATPROTO_PERMISSION_SET_FIXTURES; do
        if grep -a -q -- "$lit" bins/fauna-bridges/fauna-atproto-bridge; then
            echo "ERROR: the production fauna-atproto-bridge carries the e2e seam '$lit' (convention 15)" >&2
            exit 1
        fi
    done

# The e2e flavor of the atproto bridge: the same binary plus the build-tagged
# hostile-rotation seam (`--seize-did`, `fauna_e2e_seize`) and the three
# harness redirect seams (`fauna_e2e_fixtures`: FAUNA_ATPROTO_PLC_DIRECTORY_URL,
# FAUNA_ATPROTO_FAKE_DNS_URL, FAUNA_ATPROTO_PROXY_FIXTURES — since 2026-09-13 —
# and FAUNA_ATPROTO_PERMISSION_SET_FIXTURES since 2026-09-25),
# written to a DISTINCT path so the two flavors can never be confused for one
# another (windows' single-slot marker dance is unnecessary here — a Go build
# is cheap and a second name is honest). Every tests/e2e-unified test that sets
# one of those seams requests the `atproto_bridge_e2e_binary` fixture, which
# builds this; no release path, no Dockerfile, and no other recipe sets a tag.
atproto-bridge-build-e2e: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    {{slot_build}} go -C bins/fauna-bridges build -tags fauna_e2e_seize,fauna_e2e_fixtures -o fauna-atproto-bridge-e2e ./cmd/fauna-atproto-bridge
    # The gate must switch BOTH ways, or the production witness above proves
    # nothing about the mechanism (a tag nobody honours also greps clean).
    for lit in seize-did FAUNA_ATPROTO_FAKE_DNS_URL FAUNA_ATPROTO_PLC_DIRECTORY_URL FAUNA_ATPROTO_PROXY_FIXTURES FAUNA_ATPROTO_PERMISSION_SET_FIXTURES; do
        if ! grep -a -q -- "$lit" bins/fauna-bridges/fauna-atproto-bridge-e2e; then
            echo "ERROR: the e2e fauna-atproto-bridge is missing the seam '$lit' the tags should compile in" >&2
            exit 1
        fi
    done

# Test the Go atproto PDS bridge (cgo since S2 — the mint loop's mailfauna
# unseal). Covers the atproto cmd package + its atprotoid/wsrpc dependencies'
# own suites via mail-bridge-test; this recipe scopes to the atproto-specific
# packages for the inner loop. the merge gate runs NO Go tests, so run this
# locally before trusting green.
atproto-bridge-test: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    # Every internal/atproto* package, discovered rather than hand-listed: a
    # written-out list silently drops each new package (it had already lost
    # internal/atprotoread), and this recipe is the only Go gate an atproto
    # session runs locally — the merge gate runs no Go tests at all.
    PKGS="./cmd/fauna-atproto-bridge/..."
    for d in bins/fauna-bridges/internal/atproto*/; do
        PKGS="$PKGS ./internal/$(basename "$d")/..."
    done
    # Atproto-bridge packages whose names don't carry the atproto prefix. The
    # glob above cannot find these, so they go here — the ONLY hand-listed part,
    # and the reason `just mail-bridge-test` (which runs ./... over the whole
    # module) stays the backstop for anything this list forgets.
    PKGS="$PKGS ./internal/safefetch/..."
    echo "atproto packages under test:$PKGS"
    go -C bins/fauna-bridges test $PKGS
    # The e2e-flavor seam files (`*_seam.go`, `-tags fauna_e2e_fixtures`) and
    # their tests compile only under the tag, so the untagged run above is the
    # PRODUCTION flavor — it proves the seams inert — and this run is the one
    # that exercises the seams themselves. Both halves, or the two-way gate is
    # checked in one direction only (convention 15).
    go -C bins/fauna-bridges test -tags fauna_e2e_fixtures ./internal/atprotoid/ ./cmd/fauna-atproto-bridge/

# Test under -race + cgo (Phase A spec § Bundled Task 5 covers the unit-level race detector).
mail-bridge-test-race: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    export CGO_ENABLED=1
    # Same cache hole as mail-bridge-test's `./...` leg — this run links
    # libfauna_ffi.so through LD_LIBRARY_PATH at run time, which Go's test
    # cache does not hash. Uncache it so the dev recipes don't drift apart.
    export GOFLAGS="${GOFLAGS:+$GOFLAGS }-count=1"
    {{slot_build}} go -C bins/fauna-bridges test -race ./...

# Scan the live Go mail-bridge for known vulnerabilities (govulncheck,
# https://go.dev/security/vuln/) and fail on any reachable advisory. The
# bridge is cgo (links libfauna_ffi.so), so govulncheck's source analysis
# needs the FFI headers (CGO_CFLAGS) on the cgo env — the same FFI-cdylib +
# cgo-env setup as `mail-bridge-test`. Wired into supply-chain.yml's
# govulncheck job. The legacy bins/fauna-bridge-imap terminator this used to
# scan was removed in the I6 cutover; fauna-mail-bridge is the
# live module. ./... stops at the vendored go-imap fork's module boundary.
mail-bridge-vulncheck: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    # GOTOOLCHAIN is DERIVED from bins/fauna-bridges/go.mod's `toolchain`
    # directive — the single source of truth for the Go version (§ Go toolchain
    # pin) — so this scan can never drift from it. It must be that exact version,
    # not GOTOOLCHAIN=auto, for TWO reasons:
    #   1. Fidelity: govulncheck must analyze the SAME toolchain the shipped image
    #      builds with (Dockerfile's golang:<ver>-bookworm, held equal to this
    #      directive by scripts/check_go_toolchain_sync.py). A scan of a different
    #      version is a supply-chain gate measuring the wrong artifact.
    #   2. Function: govulncheck is `go run …@latest`; built under a lower toolchain
    #      its type-checker can't parse the go1.26+ stdlib. Forcing GOTOOLCHAIN makes
    #      it build with the version it analyzes. (x/vuln is v1.6.0 as of 2026-07; the
    #      old "drop once x/vuln supports go1.26" note is moot — we pin for fidelity
    #      regardless of x/vuln's own module floor.)
    export GOTOOLCHAIN="go$(sed -n 's/^toolchain go//p' bins/fauna-bridges/go.mod)"
    {{slot_build}} go -C bins/fauna-bridges run golang.org/x/vuln/cmd/govulncheck@latest ./...

# go vet + forbidigo: bans any USE of emersion/go-message (spec § Components).
# forbidigo's main package is at the module root; pin v2.3.1 — v1 can't parse
# modern Go's pkgbits export format. Three things are load-bearing for the ban,
# each easy to get wrong (the recipe was silently broken on all three until
# 2026-06 — it never enforced the ban, and false-positived once a `</`-bearing
# regex literal landed in tests):
#   - STRUCTURED pattern `{p, pkg}`, not a bare regex. forbidigo matches `p`
#     against the usage identifier (e.g. `mail.Foo`) and `pkg` against its
#     resolved package path, AND'd together. A bare regex only sets `p`, so a
#     package-path regex can never match a usage identifier. `p: ".*"` = any
#     usage, narrowed to go-message by `pkg`.
#   - `-analyze_types`, or `pkg`/pkgText is never populated: forbidigo ignores
#     import declarations and, without type info, sees only literal source text.
#     (A bare/blank import with no usage still slips through — acceptable here,
#     go-message has no side-effect-import use.)
#   - `--` before the package list: args before `--` are patterns, after are
#     packages (forbidigo's main). Without it `./...` becomes a second pattern
#     (it matches `</` in HTML/XML regex literals) as well as the package list.
mail-bridge-lint:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_build_body}}
    go -C bins/fauna-bridges vet ./...
    go -C bins/fauna-bridges run github.com/ashanbrown/forbidigo/v2@v2.3.1 \
        -analyze_types \
        -set_exit_status \
        '{p: ".*", pkg: "^github.com/emersion/go-message(/|$$)"}' \
        -- ./...

# Run the ipld/codec-fixtures canonical-form corpus through the Go
# DAG-CBOR codec (internal/dagcbor) and assert byte-for-byte round-trip
# canonicality. The corpus is pinned to the commit named in
# bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT (a moving HEAD
# would let an upstream corpus change break unrelated CI). Wired into
# `just check-generated` so a canonicalisation regression fails CI.
# Float and tag (CID) fixtures are programmatically classified as
# expected-skip — DAG-CBOR strictness in this codec forbids both.
dagcbor-fixtures: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    fixtures_dir=$(bash scripts/fetch-dagcbor-fixtures.sh)
    DAGCBOR_FIXTURES_DIR="$fixtures_dir" \
        {{slot_build}} go -C bins/fauna-bridges test -tags=fixtures -run TestCodecFixtures -v ./internal/dagcbor/

# Run the ipld/codec-fixtures canonical-form corpus through the Rust
# DAG-CBOR codec (libs/fauna-cbor) and assert byte-for-byte round-trip
# canonicality. Mirrors the Go gate (recipe `dagcbor-fixtures`); same
# pinned commit (bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT).
dagcbor-fixtures-rust:
    #!/usr/bin/env bash
    set -euo pipefail
    fixtures_dir=$(bash scripts/fetch-dagcbor-fixtures.sh)
    DAGCBOR_FIXTURES_DIR="$fixtures_dir" \
        {{slot_build}} cargo test -p fauna-cbor --test codec_fixtures -- --nocapture

# Run wsrpc microbenchmarks. Smoke-run on a local httptest mock — does
# NOT require a real fauna-nest. Phase B.4 sets the baseline; later
# phases can add regression-tracking on top. The headline number to
# eyeball is BenchmarkValidateRecipientCall's ns/op: at localhost
# loopback latencies it should land in the low-microsecond range, well
# under SMTP-stage per-frame latencies (~10ms typical).
mail-bridge-bench: mail-bridge-ffi
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    {{slot_build}} go -C bins/fauna-bridges test -bench=. -benchmem -run=^$ ./internal/wsrpc/

# Debug-build alias.
mail-bridge-debug: mail-bridge-build

# Release build (-trimpath, stripped symbols).
mail-bridge-release: mail-bridge-ffi
    {{slot_build}} go -C bins/fauna-bridges build -trimpath -ldflags '-s -w' -o $(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')/release/fauna-mail-bridge ./cmd/fauna-mail-bridge

# Build the Windows Go mail-bridge (MDA: CalDAV + IMAP) — cgo-linked with llvm-mingw.
# Staged DLLs land beside the exe in target/; Track 2 wires them into the MSI Bridge
# feature. Mechanism + toolchain prereqs: `_windows-go-cgo-build` below.
windows-mail-bridge-build rust_target="aarch64-pc-windows-gnullvm" go_arch="arm64" profile="release":
    @just _windows-go-cgo-build "./cmd/fauna-mail-bridge" "fauna-mail-bridge.exe" "{{rust_target}}" "{{go_arch}}" "{{profile}}" ""

# Build the TEST-ONLY seal-helper for Windows (gnullvm cgo) — same module + same
# fauna_ffi.dll as windows-mail-bridge-build, just a different cmd. NOT a production
# artifact: the e2e harness (tests/e2e-unified) subprocesses it to perform the
# client-side wrapped-blob seal a Fauna app UI normally does, so the Windows-native
# CalDAV/IMAP serving e2e (test_caldav_imap_serving.py) can mint a known-password MUA
# credential headlessly — no Fauna app. Output: target/seal-helper-testonly.exe,
# run with fauna_ffi.dll + libunwind.dll beside it (the shared recipe re-stages both).
windows-seal-helper-build rust_target="aarch64-pc-windows-gnullvm" go_arch="arm64":
    @just _windows-go-cgo-build "./cmd/seal-helper-testonly" "seal-helper-testonly.exe" "{{rust_target}}" "{{go_arch}}" "release" ""

# Build the Windows Go atproto PDS bridge (role "atproto.pds") — the gnullvm-cgo
# twin of the Unix `atproto-bridge-build`. Same reason the mail-bridge needs one:
# the bridge's DID mint loop imports internal/mailfauna to HPKE-Open the sealed
# per-user identity-key blobs, so it is a cgo build that links fauna_ffi — and
# cgo cannot link the MSVC dll (see `_windows-go-cgo-build`).
windows-atproto-bridge-build rust_target="aarch64-pc-windows-gnullvm" go_arch="arm64" profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    just _windows-go-cgo-build "./cmd/fauna-atproto-bridge" "fauna-atproto-bridge.exe" "{{rust_target}}" "{{go_arch}}" "{{profile}}" ""
    # Convention 15 witness, on the artifact rather than on trust — the same check
    # the Unix `atproto-bridge-build` makes, because a windows-only build path is
    # exactly where a seam rides into a shipped binary unnoticed (release.yml's
    # `--features test-helpers` MSI, testing.md § convention 15). The seize seam is
    # keyed on its FLAG NAME (no `seize-did` in the binary means no invocation can
    # reach it); the three redirect seams on the ENV-VAR NAMES they read.
    for lit in seize-did FAUNA_ATPROTO_FAKE_DNS_URL FAUNA_ATPROTO_PLC_DIRECTORY_URL FAUNA_ATPROTO_PROXY_FIXTURES FAUNA_ATPROTO_PERMISSION_SET_FIXTURES; do
        if grep -a -q -- "$lit" target/fauna-atproto-bridge.exe; then
            echo "ERROR: the production fauna-atproto-bridge.exe carries the e2e seam '$lit' (convention 15)" >&2
            exit 1
        fi
    done

# The e2e flavor of the Windows atproto bridge: the same binary plus the
# build-tagged hostile-rotation seam (`--seize-did`, `fauna_e2e_seize`) and the
# three harness redirect seams (`fauna_e2e_fixtures`), at its own DISTINCT path
# so the two flavors can never be confused for one another. Every e2e test that
# sets a FAUNA_ATPROTO_* seam builds this via `atproto_bridge_e2e_binary`; no
# release path, no MSI, and no other recipe sets a tag.
windows-atproto-bridge-build-e2e rust_target="aarch64-pc-windows-gnullvm" go_arch="arm64" profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    just _windows-go-cgo-build "./cmd/fauna-atproto-bridge" "fauna-atproto-bridge-e2e.exe" "{{rust_target}}" "{{go_arch}}" "{{profile}}" "fauna_e2e_seize,fauna_e2e_fixtures"
    # The gate must switch BOTH ways, or the production witness above proves
    # nothing about the mechanism (a tag nobody honours also greps clean).
    for lit in seize-did FAUNA_ATPROTO_FAKE_DNS_URL FAUNA_ATPROTO_PLC_DIRECTORY_URL FAUNA_ATPROTO_PROXY_FIXTURES FAUNA_ATPROTO_PERMISSION_SET_FIXTURES; do
        if ! grep -a -q -- "$lit" target/fauna-atproto-bridge-e2e.exe; then
            echo "ERROR: the e2e fauna-atproto-bridge.exe is missing the seam '$lit' the tags should compile in" >&2
            exit 1
        fi
    done

# Shared implementation of the four Windows Go/cgo build recipes above: sources
# `_windows-go-cgo-env` (below — the cargo step, the one feature set and the cgo
# env, with the why in its header) and runs `go build -o` over {{cmd}}, staging
# the runtime DLLs beside the exe. Its go-TEST sibling, sourcing the same env,
# is `mail-bridge-test-win-cgo`.
_windows-go-cgo-build cmd out_name rust_target go_arch profile go_tags:
    #!/usr/bin/env bash
    set -euo pipefail
    ENV_FILE="$(mktemp)"
    just _windows-go-cgo-env "{{rust_target}}" "{{go_arch}}" "{{profile}}" >"$ENV_FILE"
    . "$ENV_FILE"
    rm -f "$ENV_FILE"
    OUT="$(pwd)/target/{{out_name}}"
    # Strip the Go binary (-s: symbol table, -w: DWARF) for dist builds — ~50%
    # smaller exe; dev (release) keeps symbols for debugging.
    GOLDFLAGS=""
    if [ "{{profile}}" = "dist" ]; then GOLDFLAGS="-s -w"; fi
    # Unquoted on purpose: an empty go_tags must vanish rather than become an
    # empty `-tags=` argument.
    GOTAGS=""
    if [ -n "{{go_tags}}" ]; then GOTAGS="-tags={{go_tags}}"; fi
    {{slot_build}} go -C bins/fauna-bridges build $GOTAGS -ldflags="$GOLDFLAGS" -o "$OUT" {{cmd}}
    # Stage runtime DLLs beside the exe: our fauna_ffi.dll + libunwind.dll (the one
    # llvm-mingw runtime dep of the gnullvm dll; everything else is OS UCRT api-sets).
    # Windows has no rpath/LD_LIBRARY_PATH equivalent an exe can carry (only the
    # process-wide PATH, which `mail-bridge-test-win-cgo` uses and a shipped exe
    # must not depend on), which is why the output lives in target/ next to its
    # DLLs rather than in bins/fauna-bridges/ like the Unix flavors.
    cp "$REL/fauna_ffi.dll" "$(pwd)/target/"
    cp "$(dirname "$(command -v "$CC_BIN")")/libunwind.dll" "$(pwd)/target/"
    echo "Built $OUT (+ staged fauna_ffi.dll, libunwind.dll in target/)"

# The cargo step + cgo environment every Windows Go/cgo recipe shares — the four
# build recipes through `_windows-go-cgo-build`, and `mail-bridge-test-win-cgo`
# directly. Builds the gnullvm fauna_ffi.dll, then PRINTS a sourceable env on
# stdout: the cgo exports (GOOS, GOARCH, CGO_ENABLED, CC, CGO_CFLAGS,
# CGO_LDFLAGS) plus two plain variables, `REL` (the profile dir holding
# fauna_ffi.dll and its import lib) and `CC_BIN`. Callers write it to a file and
# source it — the `mail-ffi-slot` idiom, because a failing `just … >file` stops
# `set -e` where `eval "$(just …)"` would sail on — so everything the cargo step
# itself prints goes to stderr.
#
# win/CI ONLY (needs Go + llvm-mingw + the *-pc-windows-gnullvm Rust target; the
# target is NOT in rust-toolchain.toml's `targets`, so a pin bump drops it and it
# must be re-added with `rustup target add aarch64-pc-windows-gnullvm`).
#
# Unlike the Unix bridge recipes (which link the host .so), Go's cgo cannot link
# the MSVC-built fauna_ffi.dll, so this builds a SEPARATE fauna_ffi.dll for the
# gnullvm target — whose toolchain IS llvm-mingw — and cgo links its GNU import
# lib (libfauna_ffi.dll.a) natively. The C# app keeps its own crt-static MSVC dll.
# `.cargo/config.toml` pins both halves (the gnullvm linker + cc-rs CC/CXX/AR
# keys, and the MSVC crt-static rustflags).
#
# ⚠ ONE feature set, ONE place. `--no-default-features --features labeler` must
# stay in lockstep with `mail-bridge-ffi`: --no-default-features matches the
# committed libs/fauna-mail-go bindings' (store-safe-less) symbol set, and
# --features labeler re-enables the community-labeler WASM executor
# (run_wasm_labeler_score + mail_to_labeler_input_bare +
# build_signed_labeler_metadata, which the seal-helper's `publish-labeler` mode
# wraps) that --no-default-features alone would drop. This line used to be
# copy-pasted per recipe and SILENTLY DRIFTED for weeks — the 2026-07-08 miss
# that shipped a labeler-less windows MDA *and* seal-helper, caught by a build
# failure rather than a gate (build-system.md § Generated-file parity gates).
# Collapsing the copies to this one line is what makes that drift class
# unrepresentable on the windows side; a stale binding still surfaces as a
# `go build` undefined-symbol error here, never silently.
#
# The committed libs/fauna-mail-go bindings are platform-agnostic and consumed
# as-is (regen happens on Linux/macOS/CI via `just mail-bridge-ffi`).
_windows-go-cgo-env rust_target go_arch profile:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    case "{{go_arch}}" in
      arm64) CC_BIN="aarch64-w64-mingw32-clang" ;;
      amd64) CC_BIN="x86_64-w64-mingw32-clang"  ;;
      *) echo "unsupported go_arch '{{go_arch}}' (want arm64|amd64)" >&2; exit 1 ;;
    esac
    # Same profile-name-vs-output-dir mapping `windows-ffi` does: cargo writes the
    # `dev` profile into `debug/`, every other profile into a dir of its own name.
    # Callers pass `release`/`dist`, both real named dirs today, but a bare
    # {{profile}} would resolve to a `dev/` dir cargo never writes and fail with
    # a confusing missing-import-lib link error rather than a clear one. Computed
    # BEFORE the gate below: the gate's --target needs this path.
    case "{{profile}}" in
        dev) PROFILE_DIR=debug ;;
        *)   PROFILE_DIR="{{profile}}" ;;
    esac
    REL="$(pwd)/target/{{rust_target}}/$PROFILE_DIR"
    # The cargo step is freshness-gated OUTSIDE the build slot (a warm tree must
    # not queue — build-machine-resources.md § Build/e2e slot locks), mirroring
    # `mail-bridge-ffi`'s composition and `_windows-ffi-flavor`'s vendor-source
    # note (below). --stamp, not the .dll, carries freshness (a cargo no-op
    # leaves the .dll untouched). Stamped on BOTH {{rust_target}} AND
    # {{profile}} — a single-axis stamp would read "fresh" across a
    # release<->dist switch and serve the wrong-profile .dll (the class
    # `_windows-ffi-flavor`'s own flavor guard exists to prevent one level up).
    # libs/fauna-mail-go excluded: it is this pipeline's own downstream
    # output, not a cargo input. Bare cargo is correct in the wrapped command
    # (the Git-Bash link.exe shadow only bites the MSVC linker; gnullvm uses
    # aarch64-w64-mingw32-clang). Its output goes to stderr: stdout is the env.
    {{py}} scripts/build-if-stale.py --label "windows-go-cgo-ffi-{{rust_target}}-{{profile}}" \
        --stamp "$(pwd)/target/windows-go-cgo-ffi-{{rust_target}}-{{profile}}.stamp" \
        --target "$REL/fauna_ffi.dll" \
        --source libs ${VENDOR_SOURCE:-} --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} cargo build --locked -p fauna-ffi --profile {{profile}} --no-default-features --features labeler --target {{rust_target}} >&2
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    printf 'export GOOS=windows GOARCH=%q CGO_ENABLED=1\n' "{{go_arch}}"
    printf 'export CC=%q\n' "$CC_BIN"
    printf 'export CGO_CFLAGS=%q\n' "$INCLUDES"
    printf 'export CGO_LDFLAGS=%q\n' "-L$REL -lfauna_ffi"
    printf 'REL=%q\n' "$REL"
    printf 'CC_BIN=%q\n' "$CC_BIN"

# Verify libs/fauna-mail-go is in sync with libs/fauna-mail + libs/fauna-mls
# sources (regenerate to a temp dir and diff against tracked output).
#
# ALSO runs the Go mail-bridge test suite.
# Nothing on any path ran `go test` for bins/fauna-bridges: PR CI never had a
# Go test step wired to fire (no automatic CI, build-system.md § CI enforcement),
# and the merge path only ever compiled the bridge, never ran its tests. Folding
# the tests in here (rather than a separate gate) mirrors the nest-feature-clippy
# precedent — this gate already builds the exact release fauna-ffi (--no-default-
# features --features labeler) and sets up the exact CGO_CFLAGS/CGO_LDFLAGS/
# LD_LIBRARY_PATH the tests need, so running them is a near-free superset of the
# compile check already here, NOT a second cold build. Measured 2026-07-21: with
# fauna-ffi already warm (as it is in the janitor's persistent $CARGO_TARGET_DIR/
# main), `just mail-bridge-test`'s two `go test` invocations cost ~8s total — the
# ~7m40s figure recorded elsewhere is almost entirely the cold release fauna-ffi
# build this gate already pays for, not the tests themselves. Uses the tracked
# libs/fauna-mail-go bindings directly (already verified fresh above) rather than
# mail-bridge-ffi's regenerate-in-place recipe, so this gate still never mutates
# the checkout it runs in (the janitor's iron-clad --locked/clean-tree invariant).
# Since 2026-09-19 the tests are every leg `mail-bridge-test` runs, through the
# `_mail-bridge-go-test-legs` recipe both call. Measured that day, warm: the
# whole gate is 12.3-12.5 s, up from 11.0-11.1 s over the old two legs.
mail-bridge-ffi-check:
    #!/usr/bin/env bash
    set -euo pipefail
    # Artifact/flavor build: per-invocation feature resolution, never
    # workspace-unified (.cargo/config.toml § feature unification).
    export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
    case "{{go_bindings}}" in
        regenerate|tracked) ;;
        *) echo "FAUNA_GO_BINDINGS must be 'regenerate' or 'tracked', not '{{go_bindings}}'" >&2
           exit 2 ;;
    esac
    if [ "{{go_bindings}}" = regenerate ]; then just _uniffi-bindgen-go; fi
    # Match mail-bridge-ffi's build flags — default features disabled,
    # labeler feature enabled. See that recipe's comment for rationale.
    {{slot_build}} cargo build --locked -p fauna-ffi --release --no-default-features --features labeler >/dev/null
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    # Copy into the same FLAVOR-PRIVATE slot `mail-bridge-ffi` owns and read from
    # there, for the reason the `mail-ffi-slot` comment gives. This gate's cargo
    # build is UNGATED, so it always restores the labeler flavor to the shared
    # slot first — which is exactly why the gate and the build disagreed in
    # OPPOSITE directions (the gate saw 10 namespaces, the build 26) and why the
    # symptom read as "the committed tree is stale" rather than "something else
    # wrote the slot". The private copy makes the read independent of whoever
    # writes the shared slot next, including a concurrent sibling landing between
    # the build above and the bindgen below. Always the `release` slot — this
    # gate deliberately never builds `dist` (single-homed against the flag line
    # above, matching `mail-bridge-ffi`'s default).
    # win's cdylib is fauna_ffi.dll, no "lib" prefix; the source name is probed,
    # the destination is this recipe's own.
    LIBNAME=libfauna_ffi.so
    [ -f "$TARGET_DIR/release/$LIBNAME" ] || LIBNAME=libfauna_ffi.dylib
    [ -f "$TARGET_DIR/release/$LIBNAME" ] || LIBNAME=fauna_ffi.dll
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_DIR="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    mkdir -p "$FFI_DIR"
    cp "$TARGET_DIR/release/$LIBNAME" "$FFI_DIR/$LIBNAME"
    LIB="$FFI_DIR/$LIBNAME"
    # The FRESHNESS half needs the generator; the COMPILE half below does not.
    # Under `FAUNA_GO_BINDINGS=tracked` this gate therefore keeps the half it can
    # still do and says so LOUDLY rather than reporting a narrower check under
    # the same green — the "a check that cannot tell 'the check broke' from 'the
    # tree is clean' must never report PASSED" posture. Freshness is not lost:
    # it is gated on the dev side by the merge-gate check's own
    # `mail-bridge-ffi-check` run, before the commit CI is testing ever landed.
    if [ "{{go_bindings}}" = tracked ]; then
        echo "libs/fauna-mail-go: freshness diff SKIPPED (FAUNA_GO_BINDINGS=tracked —"
        echo "  this machine builds no uniffi-bindgen-go). Gated dev-side by the"
        echo "  merge-gate check. Compiling the tracked binding below."
    else
        TMP="$(mktemp -d)"
        trap 'rm -rf "$TMP"' EXIT
        "$HOME/.cargo/bin/uniffi-bindgen-go" \
            --out-dir "$TMP" \
            "$LIB" >/dev/null
        # The tracked tree carries module-qualified cross-namespace imports (the
        # `_mail-bridge-ffi-bindgen` staging step rewrites them), so the comparand
        # must be qualified the same way or every run reds as spurious drift. This
        # is also where a generator upgrade that emits an unrecognized import shape
        # fails loudly: the script exits non-zero on a surviving bare import.
        {{py}} scripts/qualify-go-binding-imports.py "$TMP" >/dev/null
        if ! diff -rq libs/fauna-mail-go "$TMP" \
            --exclude README.md --exclude go.mod --exclude go.sum; then
            echo "ERROR: libs/fauna-mail-go is out of date — run 'just mail-bridge-ffi'"
            exit 1
        fi
        echo "libs/fauna-mail-go: up-to-date with sources"
    fi
    # Freshness alone is NOT enough: a binding can be perfectly fresh (byte-identical
    # to what the pipeline emits today) yet fail to COMPILE, and the diff above passes
    # that straight through because the generator reproduces it. The historical instance
    # was the bare cross-namespace import (one such import broke every deploy + tier_4; fixed by gating the export behind `value-format`, and closed as a CLASS on
    # 2026-07-30 by the qualification step above) — but the reason this compile stays is
    # that it is the only thing standing between "the bindings match" and "the bindings
    # work". the merge gate + PR CI skip the Docker image build, so without this step the Go
    # compile was exercised only by a real deploy. Mirror the Dockerfile's two
    # mail-bridge-builder commands against the .so we just built, so the merge/CI gate
    # catches the whole class before main.
    INCLUDES=""
    for d in libs/fauna-mail-go/*/; do INCLUDES="$INCLUDES -I$(pwd)/$d"; done
    export CGO_CFLAGS="$INCLUDES"
    FFI_SLOT_FILE="$(mktemp)"
    just mail-ffi-slot >"$FFI_SLOT_FILE"
    FFI_SLOT="$TARGET_DIR/$(cat "$FFI_SLOT_FILE")"
    rm -f "$FFI_SLOT_FILE"
    export CGO_LDFLAGS="-L$FFI_SLOT -lfauna_ffi -Wl,-rpath,$FFI_SLOT"
    {{cgo_sdkroot}}
    export LD_LIBRARY_PATH="$FFI_SLOT:${LD_LIBRARY_PATH:-}"
    echo "Compiling the Go mail-bridge + supervisor against the binding..."
    if ! go -C bins/fauna-bridges build -o /dev/null ./cmd/fauna-mail-bridge; then
        echo "ERROR: the Go mail-bridge fails to compile against libs/fauna-mail-go." >&2
        echo "  A fresh-but-uncompilable UniFFI Go binding. The bare cross-namespace import" >&2
        echo "  that used to cause this is rewritten by scripts/qualify-go-binding-imports.py," >&2
        echo "  so read the compiler error rather than assuming that class — and if it IS an" >&2
        echo "  unresolvable import, teach that script the shape the generator now emits." >&2
        exit 1
    fi
    if ! CGO_ENABLED=0 go -C bins/fauna-bridges build -o /dev/null ./cmd/fauna-supervisor; then
        echo "ERROR: the Go fauna-supervisor fails to compile (CGO_ENABLED=0) — same UniFFI" >&2
        echo "  Go binding class as above (the two failing Dockerfile mail-bridge-builder commands)." >&2
        exit 1
    fi
    echo "Go mail-bridge + supervisor: compile clean against the binding"
    # The two commands above compile every binding package the bridge LINKS
    # (fauna_ffi, fauna_mail, fauna_bridge_atproto, transitively). The other
    # generated packages are compiled by nothing at all — and that invisibility
    # is exactly what makes them landmines: the first import is what discovers
    # the breakage, in whatever session happens to need it. Compile them here
    # too, so a new one cannot rot unnoticed.
    #
    # There is no exclusion list any more, and adding one back is the wrong
    # move: `fauna_conversations` and `fauna_onboarding_machine` were skipped
    # here until 2026-07-30 for bare cross-namespace imports, which the
    # `qualify-go-binding-imports.py` step in `_mail-bridge-ffi-bindgen` now
    # rewrites for the whole class. `./...` over the module is therefore the
    # honest scope — a package that fails here is a real regression, and a NEW
    # unrewritable import shape has already failed the qualification step above.
    echo "Compiling every generated binding package..."
    if ! go -C libs/fauna-mail-go build ./...; then
        echo "ERROR: a generated binding package fails to compile." >&2
        echo "  Same UniFFI Go binding class as above. If the failure is an unresolvable" >&2
        echo "  bare cross-namespace import, teach scripts/qualify-go-binding-imports.py" >&2
        echo "  the shape the generator now emits — do NOT re-add a skip list, and do NOT" >&2
        echo "  gate the export (that is the per-export containment the script replaced)." >&2
        exit 1
    fi
    echo "Generated binding packages: compile clean"
    echo "Running the Go mail-bridge test suite..."
    # Every leg `mail-bridge-test` runs, from the one recipe both call, against
    # the cgo env exported above — never `just mail-bridge-test` itself, whose
    # `mail-bridge-ffi` dependency regenerates libs/fauna-mail-go in place
    # (see the header above).
    just _mail-bridge-go-test-legs
    echo "Go mail-bridge test suite: green"

# Build fauna-windows debug (WinUI 3 app — MSBuild, not `dotnet build`, because the XAML compiler crashes under x86 emulation on ARM64).
# `-restore` runs NuGet restore in the same MSBuild invocation; without it, regenerated FFI files (added by `windows-ffi`) leave project.assets.json stale and the build fails with NETSDK1004.
# Links the DEBUG (`dev`-profile) FFI, matching apple's dev loop
# (`mac-debug`/`swift-test` → `apple-ffi-host debug`): no CARGO_INCREMENTAL env
# and its fingerprint-flip footgun (dev is non-incremental fleet-wide since
# 2026-08-23 — build-machine-resources.md § Dev-profile incremental OFF), and
# the core gets debug_assert!/overflow checks. `windows-release`/`dist` stay on
# release — see § UniFFI cdylib rebuild cost (win) → Deeper unification.
windows-debug: (windows-ffi-test "dev") i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    # The MSBuild step is freshness-gated OUTSIDE the build slot (a warm tree must
    # not queue — build-machine-resources.md § Build/e2e slot locks), the same shape as
    # mail-bridge-ffi-lib. UNGATED until 2026-07-30, this recipe re-ran a full
    # WinUI MSBuild pass — and took a machine-wide build slot — on EVERY
    # invocation, including one that had just succeeded. That broke the
    # cheap-to-repeat contract every other builder recipe honours
    # (build-system.md § "`just <recipe>` is cheap to repeat"), and it is paid
    # twice per multiseat round: the handshake's step-C warm-build and the
    # announce's own `_ensure_local_seat_built` each did the whole ~9 min build
    # back to back (measured on a live tri-machine round, 2026-07-30).
    #
    # --stamp, not the .exe, carries freshness: MSBuild is incremental, so a
    # no-op build leaves FaunaApp.exe's mtime untouched and an .exe-keyed gate
    # would go PERMANENTLY stale after any mtime churn that doesn't relink (a
    # rebase does this to the whole tree). The .exe stays an existence check, so
    # deleting bin/ still forces a rebuild.
    #
    # bin/ and obj/ are EXCLUDED because they are this recipe's own output: the
    # stamp is committed with the PRE-run instant, so watching MSBuild's own
    # writes would re-stale the gate the moment it succeeded. Everything else
    # under the app tree IS watched, which is what makes the gate correct without
    # an enumerated source list — the generated FFI bindings
    # (FaunaApp.Core/Generated/uniffi), the staged fauna_ffi.dll, the i18n
    # Resources.resw and Generated/Providers.cs all live inside it, so a
    # regenerating dependency re-triggers MSBuild by construction.
    {{py}} scripts/build-if-stale.py --label windows-debug \
        --stamp target/windows-debug.stamp \
        --target apps/fauna-windows/FaunaApp/FaunaApp/bin/ARM64/Debug/net10.0-windows10.0.26100/FaunaApp.exe \
        --source apps/fauna-windows/FaunaApp \
        --source apps/fauna-windows/installer/PackageIdentity.props \
        --source Directory.Build.props --source global.json \
        --exclude '*/bin/*' --exclude '*/obj/*' \
        -- {{slot_build}} "{{msbuild_exe}}" \
            apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj \
            "-restore" "//p:Platform=ARM64" "//p:Configuration=Debug" "//verbosity:minimal"

# C# unit tests against FRESH bindings — the same pair the win merge-gate's
# `windows-cs-test-compile` gate runs, so a local green and a gate green are
# the same fact. Bare `dotnet test` after a rebase onto a libs/fauna-* change
# is the trap this retires (2026-07-23: a local green on stale gitignored
# bindings preceded a gate red). No WinUI XAML build needed (`dotnet test`,
# not `just windows-debug`/MSBuild) — measured ~19s warm end to end. Under the
# build slot, as the gate's own run is: `dotnet test` compiles the test project, then runs it.
windows-cs-test: (windows-ffi-test "dev")
    {{slot_build}} dotnet test apps/fauna-windows/FaunaApp/FaunaApp.Tests/FaunaApp.Tests.csproj

# A win Rust SERVICE-crate build, wrapped in the build slot pool. `cargo-win.cmd build -p <crate>` is neither a focused `check` nor a
# `test --lib` (the one bare exemption build-machine-resources.md § Build/e2e
# slot locks names), so hand-spelling it was never meant to run unwrapped — three
# live docs did anyway (two Rust-services examples, the installer's dist build),
# each a copy-paste trap for the next author. This recipe is the fix: call it by
# name instead. Freshness-gated OUTSIDE the slot (build-if-stale, `--stamp` mode
# since cargo's own no-op leaves target mtimes untouched — see that script's
# docstring), mirroring `windows-debug`/`windows-release`: a no-op rebuild costs
# one stamp check, never a slot wait. `{{crate}}` is a workspace member the win
# installer ships (`fauna-sync-agent`, `fauna-bridge-service`, `fauna-nest-service`, …);
# `{{features}}` is an optional comma-separated feature list. Source scope is the
# whole win-relevant Rust tree (safe direction: over-invalidation just costs an
# extra stamp check, under-invalidation would silently skip a real rebuild).
windows-service-build crate features="":
    #!/usr/bin/env bash
    set -euo pipefail
    FEATURES="{{features}}"
    FEATURE_ARGS=""
    if [ -n "$FEATURES" ]; then FEATURE_ARGS="--features $FEATURES"; fi
    {{py}} scripts/build-if-stale.py --label "windows-service-build-{{crate}}" \
        --stamp "target/.windows-service-build-{{crate}}.stamp" \
        --source apps/fauna-windows --source libs ${VENDOR_SOURCE:-} --source bins \
        --exclude '*/target/*' --exclude '*/bin/*' --exclude '*/obj/*' \
        -- {{slot_build}} bash -c "cmd //c \"scripts\\cargo-win.cmd build -p {{crate}} $FEATURE_ARGS\""

# Build fauna-windows release — the SHIPPED flavor: the production FFI
# (`windows-ffi`, i.e. release profile with NO `test-helpers`) plus
# Configuration=Release, which leaves both DEBUG and FaunaE2eAgent undefined. That
# pair is what `windows-release-build`, gate 4 of the win merge-gate check, exists
# to compile — the MSI's own build was witnessed by nothing until 2026-08-10.
#
# Freshness-gated OUTSIDE the build slot, exactly like `windows-debug` (a warm
# tree must not queue — build-machine-resources.md § Build/e2e slot locks). UNGATED until
# 2026-08-10: it took a machine-wide build slot on every invocation, including one
# that had just succeeded, breaking the cheap-to-repeat contract every other
# builder recipe honours. Measured warm: 12s ungated (MSBuild's own incremental
# no-op, inside the slot) against ~1s gated and no slot taken at all.
#
# --stamp, not the .exe, carries freshness, and bin/ + obj/ are excluded, for the
# same two reasons spelled out on `windows-debug` above: MSBuild is incremental so
# a no-op build leaves the .exe mtime untouched, and watching this recipe's own
# output would re-stale the gate the moment it succeeded. Everything else under
# the app tree IS watched — including `FaunaApp.Core/Generated/uniffi` and the
# staged `fauna_ffi.dll`, so a FLAVOR change correctly re-triggers this build
# rather than silently linking the other flavor's core.
windows-release: windows-ffi i18n-generate providers-generate
    {{py}} scripts/build-if-stale.py --label windows-release \
        --stamp target/windows-release.stamp \
        --target apps/fauna-windows/FaunaApp/FaunaApp/bin/ARM64/Release/net10.0-windows10.0.26100/FaunaApp.exe \
        --source apps/fauna-windows/FaunaApp \
        --source apps/fauna-windows/installer/PackageIdentity.props \
        --source Directory.Build.props --source global.json \
        --exclude '*/bin/*' --exclude '*/obj/*' \
        -- {{slot_build}} "{{msbuild_exe}}" \
            apps/fauna-windows/FaunaApp/FaunaApp/FaunaApp.csproj \
            "-restore" "//p:Platform=ARM64" "//p:Configuration=Release" "//verbosity:minimal"

# Release-flavor CHECK for the four Rust crates the installer ships for a
# Windows target — `fauna-shell-ext` (the `fauna_shell.dll` cdylib),
# `fauna-sync-agent`, `fauna-nest-service`, `fauna-bridge-service`
# (`.github/workflows/release.yml`'s "Build
# Rust binaries (Windows service crates)" step). Gate 4's own release
# compile (`windows-release` above) is `windows-ffi` alone — the FFI cdylib
# + the WinUI app — and covers none of these four, so their first release
# compile was the release cut itself. `check`,
# not `build`: the gate's job is the compile verdict, not a linked artifact
# — cargo has nothing to existence-check, so freshness rides the stamp
# alone (`build-if-stale.py`'s own `--stamp`-without-`--target` mode).
#
# Measured 2026-09-07 on a tree already warm from `windows-release` above
# (i.e. this is the DELTA these four crates add, not a from-scratch cold
# number): 3m06s / +~850 MB to `target/release` cold (they pull a much
# wider dependency slice than `fauna-ffi` alone — fauna-conversations,
# fauna-sync-engine, fauna-nest-http, …), 52s warm re-check. Comparable
# order of magnitude to the Linux dev machine's own portable-slice
# measurement for `fauna-shell-ext`+`fauna-ctl` (57s) — nowhere near gate 4's own ~8 min /
# ~5.6 G that earned its no-cadence design, so this needs no cadence either.
#
# ⚠ Residual: this host compiles `aarch64` only, matching every other gate
# here — `release.yml` also ships `x64` (see its build matrix), which no
# machine in this fleet cross-compiles or gates. Noted, not required: the
# two targets share the same source, and an `aarch64`-only break is still
# the overwhelmingly common case this gate exists to catch.
windows-shipped-services-release-check:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label windows-shipped-services-release-check \
        --stamp target/windows-shipped-services-release-check.stamp \
        --source apps/fauna-windows --source libs --source bins ${VENDOR_SOURCE:-} \
        --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/target/*' --exclude '*/bin/*' --exclude '*/obj/*' \
        -- {{slot_build}} bash -c "cmd //c \"scripts\\cargo-win.cmd check --release --locked -p fauna-shell-ext -p fauna-sync-agent -p fauna-nest-service -p fauna-bridge-service\""

# Build and launch the debug FaunaApp.exe
windows-run: windows-debug
    apps/fauna-windows/FaunaApp/FaunaApp/bin/ARM64/Debug/net10.0-windows10.0.26100/FaunaApp.exe

# Build the Windows e2e FlaUI automation bridge (C#/.NET console app under
# tests/e2e-unified/flaui-bridge). The windows driver calls this before
# launching the bridge with `dotnet run --no-build`; the build-if-stale gate
# skips the rebuild when no source changed. Plain console exe, so `dotnet
# build` is correct here — the MSBuild/XAML caveat is WinUI-only. `*.cs` is a
# top-level glob (the dir is flat), which auto-discovers sources while
# excluding the bin/ + obj/ build outputs. The build takes the build slot, freshness-gated
# OUTSIDE it like every sibling: a warm bridge never queues, and a stale one
# waits for `build` from inside the driver's e2e lane — the sanctioned order,
# which `drivers/windows.py`'s `_BRIDGE_BUILD_BUDGET_S` is sized for.
windows-flaui-bridge:
    #!/usr/bin/env bash
    set -euo pipefail
    {{py}} scripts/build-if-stale.py --label windows-flaui-bridge \
        --target tests/e2e-unified/flaui-bridge/bin/Debug/net10.0-windows/FauiBridge.exe \
        $(printf -- '--source %s ' tests/e2e-unified/flaui-bridge/*.cs) \
        --source tests/e2e-unified/flaui-bridge/FauiBridge.csproj \
        --source tests/e2e-unified/flaui-bridge/packages.lock.json \
        --source Directory.Build.props --source global.json \
        -- {{slot_build}} dotnet build tests/e2e-unified/flaui-bridge -c Debug

# Build fauna-linux debug binary
# fauna-sync-agent builds alongside: the app's spawner expects the agent binary
# beside fauna-desktop (systemd user unit in production, direct child spawn
# under e2e) — the linux twin of `mac-app` bundling the agent into Fauna.app.
#
# The cargo step is freshness-gated OUTSIDE the build slot (build-system.md §
# Build/e2e slot locks) — the same shape as mail-bridge-ffi-lib / windows-debug.
# UNGATED until 2026-07-31, `_ensure_client_built` (tests/e2e-unified/conftest.py)
# and a bare `just linux-debug` both took a machine-wide `build` slot and queued
# behind sibling builds even on a fully warm tree, since `{{slot_build}}` wraps
# cargo unconditionally and cargo's own (cheap) no-op check never gets a chance
# to run first.
#
# --stamp, not the two binaries, carries freshness: cargo is incremental, so a
# no-op build leaves fauna-desktop/fauna-sync-agent's mtimes untouched, and a
# binary-keyed gate would go permanently stale after any mtime churn that
# doesn't relink (every rebase does this). The binaries stay existence checks
# (`cargo clean -p` removes them but not the stamp). libs/ is watched whole
# (not an enumerated crate list) so any shared-Rust dependency bump is caught
# without maintaining a list; wasm-pack out-dirs are excluded since they are
# build OUTPUTS living inside libs/, never a cargo input.
#
# The target binary is `fauna-desktop`, NOT `fauna-linux`: the `fauna-linux`
# crate declares `[[bin]] name = "fauna-desktop"` (apps/fauna-linux/Cargo.toml)
# — a real gap this session's own live verification caught (the first version
# of this gate watched a binary path that can never exist, so it always read
# "missing" and always re-queued, silently defeating the fix).
linux-debug: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    {{py}} scripts/build-if-stale.py --label linux-debug \
        --stamp "$TARGET_DIR/linux-debug.stamp" \
        --target "$TARGET_DIR/debug/fauna-desktop" \
        --target "$TARGET_DIR/debug/fauna-sync-agent" \
        --source libs ${VENDOR_SOURCE:-} --source apps/fauna-linux --source bins/fauna-sync-agent \
        --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} cargo build -p fauna-linux -p fauna-sync-agent

# Build fauna-tui (the TUI client) debug binary
# fauna-sync-agent builds alongside (like linux-debug): tui's spawner expects the
# agent binary beside fauna-tui (systemd user unit in production, direct child
# spawn under e2e — A6, `sync_agent.rs`), so a `--client tui` sync-agent e2e run
# finds it there.
#
# Freshness-gated OUTSIDE the build slot — see linux-debug's comment above for
# the full rationale.
tui-debug: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET_DIR="${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')}"
    {{py}} scripts/build-if-stale.py --label tui-debug \
        --stamp "$TARGET_DIR/tui-debug.stamp" \
        --target "$TARGET_DIR/debug/fauna-tui{{exe_suffix}}" \
        --target "$TARGET_DIR/debug/fauna-sync-agent{{exe_suffix}}" \
        --source libs ${VENDOR_SOURCE:-} --source apps/fauna-tui --source bins/fauna-sync-agent \
        --source Cargo.lock --source rust-toolchain.toml \
        --exclude '*/pkg/*' --exclude '*/pkg-test/*' --exclude 'libs/fauna-mail-go/*' \
        -- {{slot_build}} cargo build -p fauna-tui -p fauna-sync-agent

# Build fauna-linux release binary (+ the sync agent it spawns — see linux-debug)
linux-release: i18n-generate providers-generate
    {{slot_build}} cargo build --locked -p fauna-linux -p fauna-sync-agent --release

# Run fauna-linux debug build
linux-run: i18n-generate providers-generate
    {{slot_build}} cargo build -p fauna-linux
    "$(cargo metadata --format-version 1 --no-deps | {{py}} -c 'import sys,json; print(json.load(sys.stdin)["target_directory"])')/debug/fauna-linux"

# Install fauna-linux to ~/.local (user) or $PREFIX
linux-install:
    apps/fauna-linux/install.sh

# Uninstall fauna-linux
linux-uninstall:
    apps/fauna-linux/uninstall.sh

# Build a .deb package (requires debhelper: apt install debhelper). Copies
# debian/ to the repo root — dpkg-buildpackage's expected location — since
# apps/fauna-linux/packaging/debian/{rules,control} is the source of truth
# (docs/goal/architecture/installers/linux-desktop.md § Debian Package). A
# real copy, not a symlink: dh's build-stamp/staging files land in the
# disposable repo-root copy instead of polluting the tracked source dir (a
# symlink let them leak through and, once, got silently replaced by a plain
# directory mid-build, breaking dh_installdocs's debian/control lookup).
linux-deb: i18n-generate providers-generate
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf debian
    cp -r apps/fauna-linux/packaging/debian debian
    dpkg-buildpackage -us -uc -b
    rm -rf debian

# Build Flatpak (requires flatpak-builder + org.gnome.Sdk//50; the manifest
# installs the repo's pinned rustup toolchain itself — builds since 2026-07-14.
# In a sandboxed tool environment add --disable-rofiles-fuse; see
# docs/goal/architecture/installers/linux-desktop.md § Flatpak)
linux-flatpak: i18n-generate providers-generate
    flatpak-builder --force-clean build-flatpak apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml

# Install Flatpak locally for testing
linux-flatpak-install: i18n-generate providers-generate
    flatpak-builder --user --install --force-clean build-flatpak apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml

# Run the locally installed Flatpak
linux-flatpak-run:
    flatpak run social.fauna.fauna

# Export Flatpak to a .flatpak bundle file for distribution
linux-flatpak-bundle: i18n-generate providers-generate
    flatpak-builder --force-clean --repo=flatpak-repo build-flatpak apps/fauna-linux/packaging/flatpak/social.fauna.fauna.yml
    flatpak build-bundle flatpak-repo fauna-linux.flatpak social.fauna.fauna

# Build macOS installer package (unsigned, for dev/testing). `profile`:
# forwarded to build.sh (`release` default, `dist` the shipping build —
# installers/macos.md § Size & build profile).
pkg-unsigned profile="release": i18n-generate providers-generate
    {{slot_build}} ./installer/macos/build.sh --profile {{profile}}

# Build macOS installer package (signed + notarized)
pkg profile="release": i18n-generate providers-generate
    {{slot_build}} ./installer/macos/build.sh --sign --profile {{profile}}

# Build macOS installer package (Developer-ID-signed, NOT notarized — for the
# § Identifier domain TCC measurement matrix and other pre-notary signed builds;
# needs only FAUNA_SIGN_IDENTITY + FAUNA_INSTALLER_IDENTITY)
pkg-sign-only profile="release": i18n-generate providers-generate
    {{slot_build}} ./installer/macos/build.sh --sign-only --profile {{profile}}

# Sign a release artifact with Ed25519 (requires FAUNA_RELEASE_SIGNING_KEY env var)
sign-release artifact:
    echo "$FAUNA_RELEASE_SIGNING_KEY" | {{slot_build}} cargo run -p fauna-sign -- --key-stdin --input {{artifact}} --output {{artifact}}.sig

# Run cargo-vet supply chain audit
vet:
    cargo vet

# Show diff for a crate update (usage: just vet-diff serde 1.0.203 1.0.204)
vet-diff crate old new:
    cargo vet diff {{crate}} {{old}} {{new}}

# Certify a crate version after review (usage: just vet-certify serde 1.0.204)
vet-certify crate version:
    cargo vet certify {{crate}} {{version}}

# Re-grandfather the exemption backlog so `cargo vet --locked` (the CI gate in
# supply-chain.yml + release.yml) passes again. RUN THIS when the gate has
# drifted RED: the fleet adds/bumps deps through the merge gate without running vet, so
# Cargo.lock outgrows the [exemptions] snapshot and the gate fails on the
# now-uncovered crates (165 such on 2026-06-28). This adds exemptions for the
# newly-unvetted crates and prunes entries for crates no longer in the tree —
# it does NOT review anything; the new exemptions are backlog for the burn-down
# (release-integrity.md § Dependency verification). A red gate gives zero signal
# (a new bad dep is indistinguishable from the stale ones), so keep it green.
vet-regen-exemptions:
    cargo vet regenerate exemptions

# Regenerate all i18n platform files from en.yaml (no-op when unchanged)
i18n-generate:
    @{{py}} scripts/build-if-stale.py --label i18n \
        --stamp i18n/.i18n-generate.stamp \
        --target apps/fauna-web/src/lib/i18n/strings.ts \
        --target apps/fauna-android/app/src/main/res/values/i18n_strings.xml \
        --target apps/fauna-windows/FaunaApp/FaunaApp/Strings/en-US/Resources.resw \
        --target libs/fauna-i18n/src/strings.rs \
        --target tests/e2e-unified/i18n/strings.py \
        --target apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/Generated/L.swift \
        --source i18n/strings/en.yaml \
        --source i18n/generator \
        -- uv run i18n/generator/generate.py

# Check that generated i18n files are up to date
i18n-check:
    uv run i18n/generator/generate.py --check

# Lint i18n strings for duplicates and near-duplicates
i18n-lint:
    uv run i18n/generator/lint_strings.py

# Regenerate per-client provider registry files (no-op when unchanged)
providers-generate:
    @{{py}} scripts/build-if-stale.py --label providers \
        --stamp i18n/.providers-generate.stamp \
        --target libs/fauna-provisioning/src/providers_generated.rs \
        --target apps/fauna-web/src/lib/generated/providers.ts \
        --target apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/Providers.swift \
        --target apps/fauna-android/app/src/main/kotlin/social/fauna/generated/Providers.kt \
        --target apps/fauna-windows/FaunaApp/FaunaApp/Generated/Providers.cs \
        --target tests/e2e-unified/generated/providers.py \
        --source i18n/providers.yaml \
        --source scripts/providers-generate.py \
        -- uv run scripts/providers-generate.py

# Check that generated provider registry files are up to date
providers-check:
    uv run scripts/providers-generate.py --check

# Regenerate per-platform element-id constants from ui.yaml (no-op when unchanged)
ui-ids-generate:
    @{{py}} scripts/build-if-stale.py --label ui-ids \
        --stamp tests/e2e-unified/.ui-ids-generate.stamp \
        --target libs/fauna-ui-ids/src/lib.rs \
        --target apps/fauna-web/src/lib/generated/uiIds.ts \
        --target apps/fauna-web/src/lib/components/payments/generatedIds.ts \
        --target apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/UiIds.swift \
        --target apps/fauna-android/app/src/main/kotlin/social/fauna/generated/UiIds.kt \
        --target apps/fauna-windows/FaunaApp/FaunaApp/Generated/UiIds.cs \
        --target tests/e2e-unified/generated/ui_ids.py \
        --source tests/e2e-unified/ui.yaml \
        --source scripts/ui-ids-generate.py \
        --source scripts/lint-ui-registry.py \
        --source scripts/ui_id_names.py \
        -- uv run scripts/ui-ids-generate.py

# Check that generated element-id constant files are up to date
ui-ids-check:
    uv run scripts/ui-ids-generate.py --check

# Verify ALL committed generated files are in sync with their sources.
# Fails if any generator would produce different output than what's in git.
# Run this in CI to catch PRs that edited a yaml without regenerating.
check-generated:
    @echo "=== i18n strings ==="
    just i18n-check
    @echo ""
    @echo "=== provider registry ==="
    just providers-check
    @echo ""
    @echo "=== element-id constants ==="
    just ui-ids-check
    @echo ""
    @echo "=== mail-bridge UniFFI Go bindings ==="
    just mail-bridge-ffi-check
    @echo ""
    @echo "=== mail-bridge DAG-CBOR codec canonicality (ipld/codec-fixtures) ==="
    just dagcbor-fixtures
    @echo ""
    @echo "=== fauna-cbor Rust DAG-CBOR codec canonicality (ipld/codec-fixtures) ==="
    just dagcbor-fixtures-rust

# Regenerate every generated artifact, unconditionally.
# Use after editing i18n/strings/en.yaml, i18n/providers.yaml, or libs/fauna-wasm.
# For day-to-day work, prefer the per-target recipes (i18n-generate, providers-generate, wasm)
# which auto-skip when sources are unchanged.
regen:
    uv run i18n/generator/generate.py
    uv run scripts/providers-generate.py
    uv run scripts/ui-ids-generate.py
    just _wasm-impl

# READ ui.yaml one page/component/element at a time. The spec is 3.31x the
# 262,144-byte Read-tool ceiling, so opening it whole is not possible — the read
# is refused outright and returns none of the file. The tool's own remedy, "use
# offset and limit", is the wrong advice for a spec: a blind byte range is not a
# node. Slices below are the file's own bytes with their line range, so
# `ui.yaml:NNN` citations still work.
#   just ui-show                    # table of contents
#   just ui-show pages.feed         # one page, verbatim
#   just ui-show feed               # bare key, when unambiguous
#   just ui-show --find recipient   # every key whose name matches
# A node over the cap renders as its child index rather than truncating.
ui-show *ARGS:
    {{py}} scripts/ui-show.py {{ARGS}}

# Lint ui.yaml: verify all 7 apps implement every required element (spec -> app)
ui-lint:
    uv run scripts/lint-ui-elements.py

# Lint ui.yaml the OTHER two directions (advisory, exits 0 unless --strict):
#   * page/component lists naming ids with no `elements:` registry row
#   * registry rows no page list uses
#   * ids an app renders that appear nowhere in ui.yaml (rule A deviations)
# Backlog measured 2026-08-10: 87 / 45 / 31. The first ("referenced-not-registered")
# closed to 0 the same day and is now GATED --strict on the cheap
# merge tier (both merge scripts). The other two are still open.
ui-registry-lint *ARGS:
    uv run scripts/lint-ui-registry.py {{ARGS}}

# Wire-schema evolution gate: diff libs/fauna-protocol/schemas/*.cddl against
# the merge base and block non-additive drift (removed/renamed/retyped keys,
# optional→required). Pure git + regex, stdlib-only, <1s. This is the only
# automatic executor of scripts/check-cddl-evolution.py — until 2026-08-19 the
# script ran solely in the dispatch-only ci.yml while both schema READMEs
# claimed it "runs in CI on every PR" (the excused-coverage class; the wire
# invariant it protects is version-compatibility.md's additive-everywhere rule).
cddl-evolution-check:
    {{py}} scripts/check-cddl-evolution.py

# Additive-only struct-evolution gate — the Rust-struct analogue of
# cddl-evolution-check above. `tools/check-additive-evolution` (a syn parser,
# not line heuristics) diffs libs/fauna-protocol's wire structs and
# libs/fauna-segment-store's at-rest structs against the merge base and blocks
# the same non-additive shapes (removed/renamed/retyped fields,
# optional→required), plus asserts every newly-added non-strict wire struct
# carries the rule-4 `extra` catch-all. Despite being fast (~1s warm, a
# workspace member reusing already-built deps), this is CHECK-tier, not cheap
# tier: it runs `cargo`, and merge-gates.md's synchronous path admits no
# compile regardless of measured cost (the CDDL sibling qualifies for cheap
# tier for the opposite reason — pure git + regex, no compiler involved).
# Wired into scripts/merge-gate-check.sh (runs on one machine only — both
# crates are cross-platform Rust with no per-target divergence a second
# machine's run would catch). Until this gate its only executor was the
# dispatch-only ci.yml `protocol-checks` job — no automatic path at all
# (transport.md § Schema and forward-compat discipline). --locked: heavy-gate
# recipes run inside the pinned check-tree checkout, which the next kick
# requires clean (merge-gate-check.md § Merge-gate check).
# *ARGS carries merge-gate-check.sh's `--base <LAST_GREEN>`: the check script builds in a tree PINNED at the tip under test, so
# there HEAD == origin/main == TIP and the tool's own default
# merge-base(HEAD, origin/main) would diff that tree against itself, passing
# unconditionally with zero violations ever reported. Bare `just
# additive-evolution-check` (no args) keeps the original merge-base behavior,
# meaningful when run by hand from a feature branch ahead of origin/main.
additive-evolution-check *ARGS:
    {{slot_build}} cargo run --locked -p check-additive-evolution -- {{ARGS}}

# The brand mark stays readable by the rasterizer that derives every app icon
# from it (scripts/_svg_flat_render.py → render-app-icons.py, render-appx-logos.py).
# Parse-only, stdlib, ~0.1s — cheap tier. This exists because NOTHING else reads
# those scripts until a human rebuilds icons: a 2026-07-29 mark change slipped the
# subset the rasterizer accepts and sat undetected into August while windows and
# macos kept shipping icons rendered from the PREVIOUS mark.
brand-mark-check:
    {{py}} -c "import sys; sys.path.insert(0, 'scripts'); import _svg_flat_render as r; \
        s, e = r.load_shapes('sites/fauna-social/public/favicon.svg'); \
        print(f'brand mark: {len(s)} shapes, viewBox {e}x{e} — readable')"

# Self-test the nest image's deployment shell (docker/nest-toml-overlay.sh): the
# artifact's writes into /data/nest.toml — static_dir (the bundled SPA path,
# reconciled every boot) and the cors_origins seed — must actually land in the
# [nest] table of the REAL config/default.toml, and must fail the boot loudly
# when they cannot. Gated on PR CI (the `installer-checks` job in ci.yml) AND the
# local-merge path that bypasses PR CI (path-scoped). No docker/sudo/network,
# ~0.3s. The class it guards: the overlay was a `sed` anchored on a
# `require_registration` line that config/default.toml stopped shipping
# — a sed address matching nothing is a silent no-op with exit 0, so
# every Docker nest first-booted 2026-07-13..24 served the nest info page at /app
# instead of the web SPA. `spa_security_headers.rs` tested the mount MECHANISM
# and never that the artifact configures it.
entrypoint-test:
    {{slot_tier_1}} pytest tests/docker/test_entrypoint_overlay.py -q

# Every shipped test suite that no other entry point runs — the tests a public
# clone carries and, until this recipe existed, had no documented way to run.
#
# ⚠ Why it exists, measured 2026-09-08 on the curated
# tree: 702 test files ship; 15 of them were named by no shipped `just` recipe
# and no CI job, so a contributor had no way to run them and no public red
# could ever fire on them. Nine of those fifteen were reachable from NOTHING,
# internal gates included — `i18n/generator/tests` (55 tests), the four
# `scripts/` linter/staleness self-tests (57), and `tests/e2e-unified/fakes/`
# (18) had never been wired anywhere at all. That is the same "green forever
# because it never runs" class the gate-scope ratchets were written to end, one
# noun over: a SUITE nothing invokes rather than a gate that selects nothing.
#
# The groups here are exactly the ones that pass in a fresh public clone
# (verified there, not just internally — no cargo, no nest binary, no driver,
# no network; the first three 130 tests / ~2.2s on 2026-09-08, the fourth
# joined 2026-09-09, the fifth 2026-09-13).
#
# The fifth is `tests/docker/test_entrypoint_overlay.py`: pure text (no docker,
# no network, no sudo), 16 tests green against the curated tree with pytest as
# the ONLY installed package. It was named by `entrypoint-test` alone, which no
# public job runs, so no public red could fire on it. `suite_reachable_check`
# now asks that half of the question as well: a shipped suite a recipe runs and
# no shipped workflow does is a finding unless it is declared local-only.
#
# The fourth group is the SHIPPED half of `tests/scripts/` — the suites that
# ship (the publish allowlist drop-paths the rest, each with its reason).
# They are also run internally on every merge by `features-lint-test` and
# `script-selftest`, so this recipe adds the public red, not the coverage. They
# could not be named here until 2026-09-09: the one
# assertion that reads THIS checkout rather than a fixture world — "the real
# `docs/features/` catalog is green", which needs the ledger the transform
# drop-paths — lived inside `test_features_lint.py` and made the whole file
# false by construction in a public clone. It now lives in its own drop-pathed
# `test_features_repo_catalog.py` on the internal gate, and the files below
# are named one by one on purpose: `tests/scripts/` as a directory would also
# name the drop-pathed internal-only suites here, and a new shipped suite that
# nobody adds is exactly what `suite_reachable_check` reds on — as
# `test_door_deploy_receive.py` did on 2026-09-19: it pins the front door's
# deploy wrapper, which ships, and it was named only by `script-selftest`, a
# recipe the public justfile prunes, so no public red could fire on it.
#
# Each root gets its OWN pytest run: collecting several together changes which
# `conftest.py` wins the bare module name `conftest` on `sys.path`, which is
# why a combined invocation reports spurious missing-tier-marker errors for
# files that are individually fine (measured 2026-09-08 — the same "one root,
# one run" lesson scripts/publish/suite_collect_check.py's docstring records).
shipped-suite-test:
    #!/usr/bin/env bash
    set -euo pipefail
    {{slot_tier_1_body}}
    if ! command -v pytest >/dev/null 2>&1; then
        echo "shipped-suite-test: SKIPPED — no pytest on PATH (see the machine setup guide)"
        exit 0
    fi
    pytest i18n/generator/tests -q
    pytest scripts/test_build_if_stale.py scripts/test_lint_cross_client.py \
        scripts/test_lint_winui_datatemplate_names.py \
        scripts/test_lint_winui_xbind_nested_calls.py \
        scripts/test_lint_winui_xbind_datatemplate_scope.py -q
    pytest tests/e2e-unified/fakes -q
    pytest tests/scripts/test_actuation_sweep_report.py \
        tests/scripts/test_cross_client.py tests/scripts/test_door_deploy_receive.py \
        tests/scripts/test_duplication_report.py \
        tests/scripts/test_features_catalog.py tests/scripts/test_features_lint.py \
        tests/scripts/test_textual_clones.py tests/scripts/test_ui_show.py -q
    pytest tests/docker/test_entrypoint_overlay.py -q

# Payments-excision spine (tier_1, merge-gate-check.md § Accepted gaps item (8)):
# `ui.yaml`'s gated_features: catalog stays consistent with the per-app store-safe
# witnesses, and no shared app-shell source paints a payments/zaps element id
# outside its excision mechanism (Views/Payments/ on windows, #if PAYMENTS,
# a #[cfg] gate, …) — the class that compiles clean in BOTH build flavors and
# still ships the id in a store-safe artifact. Pure text/regex analysis of
# ui.yaml, the justfile, and per-app generated ID sources — no cargo, no
# dotnet, no swift, no build slot — ~3s. Gated on the local-merge path (both
# both merge scripts call it); the recipe itself skips loudly
# where bare pytest is absent (next-open-test's guard, same reasoning here:
# the check is platform-agnostic text, so a lone machine's PATH gap costs
# nothing while the sibling machine's gate still covers it).
payments-excision-spine-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v pytest >/dev/null 2>&1; then
        echo "payments-excision-spine-check: SKIPPED — no pytest on PATH (see the machine setup guide)"
        exit 0
    fi
    {{slot_tier_1}} pytest tests/e2e-unified/tests/test_payments_excision_spine.py -q

# Lint WinUI: a DataTemplate root that is a bare layout container (Grid/Border/…)
# with an AutomationId must also have an AutomationProperties.Name, else UIA prunes
# the row and FlaUI counts 0.
windows-name-lint:
    uv run scripts/lint_winui_datatemplate_names.py

# Lint WinUI: an {x:Bind ...} expression must not nest one function call inside
# another's argument list ({x:Bind Foo(Bar(x))}) — the x:Bind compiler can't
# compile that and crashes XamlCompiler.exe opaquely on ARM64.
windows-xbind-lint:
    uv run scripts/lint_winui_xbind_nested_calls.py

# Lint WinUI: an {x:Bind ...} expression inside a DataTemplate that declares no
# x:DataType has no binding context — it silently resolves to nothing, or crashes
# the generated SetDataRoot if it's the template's only x:Bind. A sweep closed 70
# latent sites of this class; this gate keeps it
# un-reintroducible.
windows-xbind-datatemplate-scope-lint:
    uv run scripts/lint_winui_xbind_datatemplate_scope.py

# (`windows-apps-manifest-check` RETIRED 2026-07-22 — the nested apps/fauna-windows
# cargo workspace it guarded no longer exists. Its whole failure class was the
# two-workspace split: a member adding `<dep>.workspace = true` resolved against the
# ROOT manifest under a root build but broke every build through the apps manifest.
# One workspace = one [workspace.dependencies] table = the class is unrepresentable,
# so the gate has nothing left to catch. See build-system.md § Cargo target dir
# layout (win) → *Two workspaces, twice the compiles*.)

# Deadline-loop ratchet (e2e-conventions.md convention 6's deadline-loop
# rider): a per-file down-only ratchet on
# `tests/e2e-unified/actions/` deadline loops that exit without signalling
# (raising, or returning a value the caller must consume) and aren't
# `# deadline-ok:`-annotated as a documented-legal silent settle. AST-based,
# parse-only (~1s, pure stdlib, no build). See merge-gates.md § Local-merge
# gates.
deadline-loop-ratchet-check:
    {{py}} scripts/check_deadline_loop_ratchet.py

# Regenerate the deadline-loop-ratchet baseline from the current tree (same
# down-only-ratchet auto-shrink as sleep-ratchet-update). Refuses to write a
# rise over an existing baseline.
deadline-loop-ratchet-update:
    {{py}} scripts/check_deadline_loop_ratchet.py --update-baseline

# Negative-visibility ratchet (e2e-conventions.md convention 6's
# negative-visibility rider): a per-file down-only ratchet on EVERY bare
# `assert not …is_visible(…)` (the id `error-message` excepted — convention 2's
# rider owns it). Windows' bridge answers `!IsOffscreen`, so the read is vacuous
# wherever the element can sit below a fold: an element the defect really did
# paint reads False. Use `driver.is_absent(id, scope=…)` — `count == 0` on
# windows (exact, and it issues no bridge-hanging UIA scroll), `not is_visible`
# everywhere else — or annotate `# negative-visibility-ok: <reason>`. The
# baseline grandfathers reads on fixed pages and in other apps' tests.
# Parse-only (~1s, pure stdlib, no build). See merge-gates.md § Local-merge gates.
negative-visibility-ratchet-check:
    {{py}} scripts/check_negative_visibility_ratchet.py

# Regenerate the negative-visibility baseline from the current tree (same
# down-only-ratchet auto-shrink as sleep-ratchet-update). Refuses to write a
# rise over an existing baseline.
negative-visibility-ratchet-update:
    {{py}} scripts/check_negative_visibility_ratchet.py --update-baseline

# App-gate ratchet (testing.md convention 7): a per-file down-only ratchet on
# app-gated `pytest.skip` calls that have NOT been declared through
# tests/e2e-unified/helpers/app_surface.py. An undeclared gate is a test that
# quietly does not run on some app while reporting `s` in a summary line that
# reads like success. Parse-only (~1s).
app-gate-ratchet-check:
    {{py}} scripts/check_app_gate_ratchet.py

# List every undeclared app-gated skip, grouped by file (no baseline compare) —
# the picker for "which of these can I close next".
app-gate-ratchet-list:
    {{py}} scripts/check_app_gate_ratchet.py --list

# Regenerate the app-gate baseline after declaring or deleting gates. Refuses to
# write a rise over an existing baseline (same discipline as sleep-ratchet-update).
app-gate-ratchet-update:
    {{py}} scripts/check_app_gate_ratchet.py --update-baseline

# Dagcbor-bypass ratchet (serialization.md § How decode-strictness is actually
# enforced): a per-file down-only ratchet on raw `serde_ipld_dagcbor::from_*`
# call sites outside libs/fauna-cbor/ — decodes that bypass the pre-parse
# validator. A seventh site must be routed through fauna-cbor or argued in the
# goal doc, never silently inherit the boundary claim. Parse-only (~1s).
dagcbor-bypass-ratchet-check:
    {{py}} scripts/check_dagcbor_bypass_ratchet.py

# List every raw serde_ipld_dagcbor::from_* call site outside libs/fauna-cbor/
# (no baseline compare).
dagcbor-bypass-ratchet-list:
    {{py}} scripts/check_dagcbor_bypass_ratchet.py --list

# Regenerate the dagcbor-bypass baseline after routing a site through fauna-cbor
# or arguing a new one safe in serialization.md. Refuses to write a rise over an
# existing baseline (same discipline as sleep-ratchet-update).
dagcbor-bypass-ratchet-update:
    {{py}} scripts/check_dagcbor_bypass_ratchet.py --update-baseline

# Render the FEATURE CATALOG — docs/features/MATRIX.md, every page's `## Status`
# block, and matrix.json (the only thing the public site imports). Reads the pages'
# front-matter and coverage contracts plus the ledger the e2e runs write; nothing it
# emits is typed by hand. Idempotent, pure stdlib, sub-second, and deliberately NOT
# build-if-stale-gated: a page is both a source of the render and the target of its
# own status block, which an mtime gate cannot express — so it runs unconditionally
# and `features-lint` checks the committed output is current (the i18n-generate /
# i18n-check split). Owner: feature-catalog.md § Render.
features-render *ARGS:
    {{py}} scripts/features-render.py {{ARGS}}

# Which columns can each contract EVER complete, and which outcome blocks the rest?
# The parse-only half of the cell question (feature-catalog.md § Cell semantics):
# fullness is read per column, so a column short of full is short of it for one
# nameable outcome — a `(none)`, or citations every one of which is marked for other
# apps. A page completable on NO column is blank for a contract reason rather than a
# coverage one, so no run on any machine in any mode will move it. Writes nothing;
# add a slug for one page in full.
#
#   just features-parity
#   just features-parity drafts-survive
features-parity *ARGS:
    {{py}} scripts/features-render.py --parity {{ARGS}}

# The catalog gate — the six rules of feature-catalog.md § The catalog lint: the
# front-matter and every citation resolve; each contract-mapped test exists AND
# carries its `@pytest.mark.feature` (feature -> test); each marker-tagged test
# appears in some contract (test -> feature); the declared [app]/[nest] surface
# matches what the test actually drives; the committed render is current; and no
# declared absence is contradicted by a passing run. Parse-only, stdlib, ~2.1s on
# win (2026-09-05: one ast walk per module, and the tree scanned once per run,
# not once per caller) — cheap merge tier.
features-lint *ARGS:
    {{py}} scripts/features_lint.py {{ARGS}}

# Mode 5 (testing.md convention 7): informational inventory of PlatformDriver
# capability predicates (supports_unclean_kill and friends) each leaf app
# driver inherits rather than overrides directly. Not a gate — a coverage-debt
# list for a human to glance over; some inherited answers are correct by
# design (e.g. linux correctly inherits supports_unclean_kill).
app-gate-capability-debt:
    {{py}} scripts/check_app_gate_ratchet.py --capability-debt

# Run all duplication checks, output JSON to stdout
duplication-check:
    uv run scripts/lint_textual_clones.py
    uv run scripts/lint_cross_client.py

# Run all duplication checks and save findings to disk
duplication-check-save:
    uv run scripts/lint_textual_clones.py --save
    uv run scripts/lint_cross_client.py --save

# Detect copy-pasted code blocks
textual-clones *ARGS:
    uv run scripts/lint_textual_clones.py {{ARGS}}

# Check cross-client pattern violations
cross-client-check *ARGS:
    uv run scripts/lint_cross_client.py {{ARGS}}

# (Retired 2026-05-18: the cargo-target shared-cache GC was removed when each
# checkout got its own build-cache directory. Lifecycle is now handled by a
# session-startup script wired to a user-level hook.)
