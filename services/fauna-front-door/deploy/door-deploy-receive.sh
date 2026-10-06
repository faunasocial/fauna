#!/bin/sh
# Forced command for the restricted deploy key (front-door.md § The box).
#
# authorized_keys entry for the dedicated deploy user:
#   command="/usr/local/bin/door-deploy-receive",restrict,no-pty,no-agent-forwarding,no-port-forwarding,no-X11-forwarding ssh-ed25519 AAAA…
#
# The key can do exactly two things, both confined to /srv/fauna:
#   1. receive an rsync into a fresh release dir:
#        rsync -az --delete ... deploy@box:/srv/fauna/{site,app}/releases/<stamp>/
#   2. atomically flip `current` onto a completed release and prune:
#        ssh deploy@box flip {site,app} <stamp>
# It can therefore replace CONTENT, and nothing else — not certificates,
# not units, not binaries, not the system — and it can read nothing.
#
# SSH_ORIGINAL_COMMAND is attacker-controlled text, and confining a
# hand-parsed `rsync --server` line is a known way to get this wrong. So this
# wrapper never runs the client's command. It accepts ONE exact shape — the
# six words `rsync -az --delete` sends — and then starts an rsync whose every
# argument is either a literal from this file or a value checked below:
#
#   rsync --server -logDtprze.<caps> --delete \
#         --munge-links --no-specials --no-devices . <ROOT>/<kind>/releases/<stamp>/
#
#   * the option word is the fixed string `-az` produces; what follows `e.` is
#     the client's capability letters (they vary by rsync version), which rsync
#     reads as the argument of `-e`, never as options;
#   * no other word is accepted anywhere, so `--sender` (the read direction),
#     `--daemon`, and every option that names a path (`--log-file`,
#     `--backup-dir`, `--link-dest`, …) are unrepresentable rather than listed;
#   * <stamp> is drawn from an alphabet with no `/` and no `.`, so a target is
#     exactly one directory deep and `..` cannot be written;
#   * the three receiver-side refusals are OURS, not the client's: `-az` sends
#     `-l` (keep symlinks) and `-D` (keep devices and specials), so without
#     them a push lands whatever shape it likes inside the release — a link
#     aimed at anything the door can read (its own certificate keys), or a
#     FIFO that parks a request on `open`. `--munge-links` lands an incoming
#     link as a dangling `/rsyncd-munged/…`, `--no-specials` drops the FIFO,
#     `--no-devices` the rest. They are receiver-side only, so a plain
#     `rsync -az --delete` deploy still round-trips unchanged. The door
#     contains the same two shapes at its own end (`src/vhost.rs` — it serves
#     only regular files inside the canonical docroot); neither layer is the
#     only one, because neither alone covers both arms;
#   * the release path AND its `releases` parent must each resolve to
#     themselves, so a symlink planted where a release would go — or one
#     component up, where `mkdir -p` would otherwise create through it before
#     anything was checked — is refused rather than followed.
#
# `rrsync` was considered and not used: it needs python3 on a box that
# otherwise has none, confines a key to one root (so not to two releases dirs,
# and not out of `current`), and checks paths lexically.
#
# Pinned by tests/scripts/test_door_deploy_receive.py, including a round trip
# through a real rsync client — change the client flags in the deploy
# workflows and that test says what the new option word is.
set -eu

ROOT=/srv/fauna
RSYNC=/usr/bin/rsync
KEEP=3
PATH=/usr/bin:/bin
export PATH

refuse() {
    echo "deploy: $1" >&2
    exit 1
}

# $1 kind, $2 stamp → sets $rel to the release dir, or refuses.
release_dir() {
    case "$1" in
        site|app) ;;
        *) refuse "bad kind" ;;
    esac
    case "$2" in
        *[!0-9TZa-z-]*|"") refuse "bad stamp" ;;
    esac
    # Checked BEFORE anything is created inside it: the `mkdir -p` below would
    # otherwise follow a planted `releases` link and create at its target.
    require_real "$ROOT/$1/releases"
    rel="$ROOT/$1/releases/$2"
}

# A path must be a real directory at its own address: not a symlink itself,
# and with no symlink anywhere above it either (`readlink -f` canonicalizes
# every component, so a planted parent moves the answer).
require_real() {
    [ ! -L "$1" ] && [ -d "$1" ] && [ "$(readlink -f -- "$1")" = "$1" ] \
        || refuse "release path is not a plain directory"
}

# Split into words with no globbing and no shell interpretation.
set -f
# shellcheck disable=SC2086
set -- ${SSH_ORIGINAL_COMMAND:-}

case "${1:-}" in
    rsync)
        [ "$#" -eq 6 ] || refuse "command not permitted"
        [ "$2" = "--server" ] && [ "$4" = "--delete" ] && [ "$5" = "." ] \
            || refuse "command not permitted"
        caps=${3#-logDtprze.}
        [ "$caps" != "$3" ] || refuse "command not permitted"
        case "$caps" in
            *[!A-Za-z]*) refuse "command not permitted" ;;
        esac
        case "$6" in
            "$ROOT"/site/releases/*/) kind=site ;;
            "$ROOT"/app/releases/*/) kind=app ;;
            *) refuse "refused target" ;;
        esac
        stamp=${6#"$ROOT/$kind/releases/"}
        release_dir "$kind" "${stamp%/}"
        [ -L "$rel" ] && refuse "release path is not a plain directory"
        mkdir -p "$rel"
        require_real "$rel"
        exec "$RSYNC" --server "$3" --delete \
            --munge-links --no-specials --no-devices . "$rel/"
        ;;
    flip)
        [ "$#" -eq 3 ] || refuse "command not permitted"
        kind=$2
        stamp=$3
        release_dir "$kind" "$stamp"
        [ -e "$rel" ] || refuse "no such release"
        require_real "$rel"
        # Atomic: build the symlink beside the target, rename over `current`.
        ln -sfn "releases/$stamp" "$ROOT/$kind/current.new"
        mv -Tf "$ROOT/$kind/current.new" "$ROOT/$kind/current"
        # Prune all but the newest $KEEP releases (never the live one).
        ls -1t "$ROOT/$kind/releases" | tail -n +$((KEEP + 1)) | while read -r old; do
            [ "$old" = "$stamp" ] && continue
            rm -rf "$ROOT/$kind/releases/$old"
        done
        echo "deploy: $kind now serves $stamp"
        ;;
    *)
        refuse "command not permitted"
        ;;
esac
