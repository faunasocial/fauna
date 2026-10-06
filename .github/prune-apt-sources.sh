#!/usr/bin/env bash
# Drop the third-party apt sources the hosted runner image ships and this
# build never uses.
#
# Why this exists: `apt-get update` exits non-zero if ANY configured source
# fails, even one nothing here installs from. The runner image pre-registers
# Microsoft's `packages.microsoft.com/repos/azure-cli`, which INTERMITTENTLY
# answers `403 Forbidden` -> `E: The repository ... is no longer signed.`.
#
# Measured on this repository's first pull-request run (2026-08-28): it red-ed
# `linux-desktop` at its only apt step, while `rust` -- same workflow, same
# commit, same minute, a different runner -- got through BOTH of its apt steps
# and went on to a clean workspace clippy. Every package either job actually
# wants resolves fine from the Ubuntu archive.
#
# Intermittent is the reason to fix it rather than re-run: a red that lands on
# a random job for a reason unrelated to the change under test is the kind CI
# users learn to dismiss, which is exactly how a real red gets dismissed too.
#
# Removing the source beats `apt-get update || true`: a genuine failure on an
# Ubuntu archive source still reds the job, which is the whole point of running
# update at all. Removing a *source* uninstalls nothing -- the image's
# pre-installed tooling is already on disk and is untouched by this.
#
# Idempotent, and safe on an image that has none of these: `grep -l` finding
# nothing exits 1, but the pipeline's status is `xargs`'s, and `xargs -r` with
# empty input exits 0.
#
# ci.yml invokes this as `bash .github/prune-apt-sources.sh`, so it needs no
# executable bit -- deliberately, because it is INJECTED into the public tree
# from scripts/publish/public-files/ and the exec-bit gate reads git's modes at
# the SOURCE path, which never matches an injected file's destination path.
# Depending on the bit here would mean depending on mode preservation through
# two copy layers that nothing checks.
set -euo pipefail

# Ubuntu's own archive sources live in /etc/apt/sources.list and
# /etc/apt/sources.list.d/ubuntu.sources; neither matches, so both survive.
readonly UNUSED='packages\.microsoft\.com'

sudo grep -rlE "$UNUSED" /etc/apt/sources.list.d/ 2>/dev/null \
  | sudo xargs -r rm -v -f
