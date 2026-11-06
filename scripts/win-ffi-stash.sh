#!/usr/bin/env bash
# usage: scripts/win-ffi-stash.sh save|restore <stash> <bindgen-stamp> <flavor-dll> <rid>
#
# Keep one windows FFI flavor's staged bindings beside its private dll, so a warm
# switch back to it is a restage and never a build. `_windows-ffi-flavor` stages
# every flavor into ONE fixed pair of paths, and `_windows-ffi-bindgen` takes a
# build slot unconditionally, so without this a run that needs two flavors queues
# for `build` on a tree with nothing to compile — or, in an e2e run's in-hold
# re-check, is refused (build-slot-pool.md § Cross-pool acquisition order → the
# warm pass). Apple's stash, `_apple-ffi-flavor`'s local fast path, is the same
# mechanism.
#
#   save     after the bindgen gate: copy the staged generation aside, unless the
#            stash already holds the one the stamp vouches for.
#   restore  put this flavor's generation back; exits non-zero, touching nothing,
#            unless the stash is the generation the stamp vouches for.
#
# ⚠ A restore leaves FRESH mtimes, exactly what a regeneration leaves, and that is
# the opposite of apple's `cp -p` on purpose. Apple's two apps build in separate
# trees; here every flavor shares one `bin/` and `obj/`, and the app takes the dll
# `PreserveNewest` (FaunaApp.csproj). A restored dll keeping its old mtime loses to
# the other flavor's copy already in `bin/`, and the app ships the wrong core with
# no error at all.
#
# The dll lands LAST. The bindgen gate's existence check on it is what turns a
# restore that died half way into a regeneration.
set -euo pipefail
action=$1
stash=$2
stamp=$3
flavor_dll=$4
rid=$5

generated=apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi
fake_bases=apps/fauna-windows/FaunaApp/FaunaApp.Tests/Generated/FakeBases.g.cs
staged_dll=apps/fauna-windows/FaunaApp/FaunaApp/runtimes/$rid/native/fauna_ffi.dll

stash_is_current() {
    [ -f "$stash/.stashed" ] && [ -f "$stamp" ] && [ ! "$stamp" -nt "$stash/.stashed" ]
}

case "$action" in
    save)
        stash_is_current && exit 0
        rm -rf "$stash.tmp" "$stash"
        mkdir -p "$stash.tmp"
        cp -R "$generated" "$stash.tmp/uniffi"
        cp "$fake_bases" "$stash.tmp/FakeBases.g.cs"
        touch "$stash.tmp/.stashed"
        mv "$stash.tmp" "$stash"
        ;;
    restore)
        stash_is_current
        [ -f "$stash/FakeBases.g.cs" ] && [ -d "$stash/uniffi" ]
        rm -rf "$generated"
        cp -R "$stash/uniffi" "$generated"
        cp "$stash/FakeBases.g.cs" "$fake_bases"
        mkdir -p "$(dirname "$staged_dll")"
        cp "$flavor_dll" "$staged_dll"
        ;;
    *)
        echo "win-ffi-stash: unknown action '$action' (expected save or restore)" >&2
        exit 2
        ;;
esac
