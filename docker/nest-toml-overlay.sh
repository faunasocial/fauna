#!/bin/bash
#
# The deployment artifact's own writes into the nest's persisted /data/nest.toml.
#
# Split out of entrypoint.sh so it is directly runnable — and therefore testable
# without docker — against the real config/default.toml. That test
# (tests/docker/test_entrypoint_overlay.py, gated on the merge path via
# `just entrypoint-test`) is the gate this file exists to enable; see its module
# docstring for the eleven-day production regression that motivated it.
#
# Both keys are values NOT chosen by any human — product-invariant bucket (1)
# (docs/goal/principles.md § One configuration surface). They differ in lifetime,
# which is why they are two subcommands rather than one overlay:
#
#   • static_dir   — a hard-coded artifact constant: where this image's
#                    Dockerfile put the bundled web SPA. Reconciled on EVERY
#                    boot, deliberately. It can never legitimately differ per
#                    box, and a box first-booted while the write was broken must
#                    self-heal the moment it pulls a fixed image — a first-run-
#                    only write would leave every such box serving the info page
#                    at /app forever (works-out-of-the-box invariant).
#   • cors_origins — the artifact's boot SEED for a client-set choice
#                    (docs/goal/architecture/installers/docker.md § Environment
#                    Variables). First run only: once the admin's client writes
#                    the `nest_cors_origins` row it wins over this seed for good
#                    (node_policy_core::resolve_cors_origins), so re-seeding on
#                    every boot would churn a value nothing reads.
#
# Every write VERIFIES itself and exits non-zero with a diagnostic if the key is
# not present afterwards. That is the load-bearing property, not the write: the
# regression that shipped was a `sed` address matching nothing, which is a
# silent no-op with exit 0 that `set -euo pipefail` cannot catch. A failed boot
# is a far better outcome than a nest that serves a half-configured surface.

set -euo pipefail

# The bundled SPA's path inside the image — the artifact constant, paired with
# the Dockerfile's `COPY --from=web-builder … /usr/share/fauna-web/`.
STATIC_DIR="/usr/share/fauna-web"

_die() {
    echo "FATAL: $*" >&2
    exit 1
}

# True for a TOML table header line, allowing the leading whitespace TOML
# permits. `${line#"${line%%[![:space:]]*}"}` strips leading blanks — note the
# NEGATED class: `%%[[:space:]]*` would yield the empty string for an indented
# line, which is how an indented `[nest]` used to go unrecognised.
_is_table_header() {
    local t="${1#"${1%%[![:space:]]*}"}"
    case "$t" in
        \[*\]*) return 0 ;;
        *) return 1 ;;
    esac
}

# True when `$1` is the `[nest]` table header (whitespace-tolerant).
_is_nest_header() {
    local t="${1#"${1%%[![:space:]]*}"}"
    [ "${t%%[[:space:]]*}" = "[nest]" ]
}

# True when line `$1` assigns key `$2` (`key = …`), tolerating leading blanks.
_is_key_line() {
    local t="${1#"${1%%[![:space:]]*}"}"
    [ "${t#"$2 = "}" != "$t" ]
}

# Set `<key> = <value>` inside nest.toml's [nest] table, idempotently: replace
# the key's existing line if it has one, else insert it directly after the
# `[nest]` header (a table header cannot silently vanish the way an optional key
# did — that is the whole point of anchoring here). Rewritten with bash string
# ops rather than `sed` so a value containing sed metacharacters or delimiters
# is impossible to mis-escape.
#
# **Everything here — the search, the rewrite, and the verification — is scoped
# to the `[nest]` table span** (its header through the next table header).
# Whole-file matching would let a same-named key under *any other* table absorb
# the write: the replace branch would fire, land on the foreign line, leave
# `[nest]` without the key, and then a whole-file verification would pass — the
# same silent-success shape this script exists to eliminate, one level up.
# Unreachable with today's shipped config; pinned by
# `test_a_same_named_key_in_another_table_does_not_defeat_the_write`.
_upsert_nest_key() {
    local file="$1" key="$2" value="$3"
    local want="${key} = ${value}"

    [ -f "$file" ] || _die "nest.toml not found at ${file}"

    local tmp
    tmp="$(mktemp "${file}.overlay.XXXXXX")"
    # Preserve the original's owner+mode across the atomic replace: this runs as
    # root, but /data/nest.toml is fauna:fauna and only the first-run block
    # chowns /data, so a plain rename would hand the uid-1000 nest a file it no
    # longer owns on every subsequent boot.
    # stat-based rather than GNU --reference: the container is GNU coreutils,
    # but the entrypoint-test merge gate runs this script on the merge machine,
    # and macOS's BSD chown/chmod have no --reference.
    local ug mode
    if ug="$(stat -c '%u:%g' "$file" 2>/dev/null)"; then
        mode="$(stat -c '%a' "$file")"
    else
        ug="$(stat -f '%u:%g' "$file")"
        mode="$(stat -f '%Lp' "$file")"
    fi
    chown "$ug" "$tmp"
    chmod "$mode" "$tmp"

    # Pass 1 — decide, over the [nest] span ONLY, whether the key is already
    # there (replace) or absent (insert after the header).
    local have_nest=0 have_key=0 in_nest=0
    while IFS= read -r line || [ -n "$line" ]; do
        if _is_table_header "$line"; then
            if _is_nest_header "$line"; then
                in_nest=1
                have_nest=1
            else
                in_nest=0
            fi
        elif [ "$in_nest" -eq 1 ] && _is_key_line "$line" "$key"; then
            have_key=1
        fi
    done < "$file"

    if [ "$have_nest" -eq 0 ]; then
        rm -f "$tmp"
        _die "no [nest] table header in ${file} — cannot place '${key}'. \
The nest would boot without it and serve a half-configured surface, so this \
boot is refused instead. Check that config/default.toml still ships [nest]."
    fi

    # Pass 2 — rewrite. A key line outside [nest] belongs to another table and is
    # copied through untouched; this script reconciles [nest], nothing else.
    in_nest=0
    local inserted=0
    while IFS= read -r line || [ -n "$line" ]; do
        if _is_table_header "$line"; then
            _is_nest_header "$line" && in_nest=1 || in_nest=0
            printf '%s\n' "$line"
            if [ "$in_nest" -eq 1 ] && [ "$have_key" -eq 0 ] && [ "$inserted" -eq 0 ]; then
                printf '%s\n' "$want"
                inserted=1
            fi
        elif [ "$in_nest" -eq 1 ] && [ "$have_key" -eq 1 ] && _is_key_line "$line" "$key"; then
            # Replace in place — a wrong or stale value self-heals too.
            printf '%s\n' "$want"
            inserted=1
        else
            printf '%s\n' "$line"
        fi
    done < "$file" > "$tmp"

    if [ "$inserted" -eq 0 ]; then
        rm -f "$tmp"
        _die "could not place '${key}' in the [nest] table of ${file}. The nest \
would boot without it and serve a half-configured surface, so this boot is \
refused instead."
    fi

    mv "$tmp" "$file"

    # Verify, always — and verify **in the [nest] span**, not the whole file. The
    # regression this guards against was a write that reported success and did
    # nothing; a whole-file check would let a same-named key under another table
    # supply the evidence for a write that never landed.
    _nest_span_has "$file" "$want" \
        || _die "wrote '${want}' into the [nest] table of ${file} but it is not there afterwards"
}

# True when the [nest] table of `$1` contains exactly the line `$2` (leading and
# trailing whitespace ignored, matching how the writer emits it).
_nest_span_has() {
    local file="$1" want="$2" in_nest=0 line trimmed
    while IFS= read -r line || [ -n "$line" ]; do
        if _is_table_header "$line"; then
            _is_nest_header "$line" && in_nest=1 || in_nest=0
            continue
        fi
        [ "$in_nest" -eq 1 ] || continue
        trimmed="${line#"${line%%[![:space:]]*}"}"
        [ "$trimmed" = "$want" ] && return 0
    done < "$file"
    return 1
}

usage() {
    cat >&2 <<EOF
usage: $0 <subcommand> <nest.toml> [args]

  ensure-static-dir  <nest.toml>              reconcile the SPA path (every boot)
  seed-cors-origins  <nest.toml> <toml-array> seed CORS origins (first run only)
EOF
    exit 1
}

[ $# -ge 2 ] || usage

case "$1" in
    ensure-static-dir)
        [ $# -eq 2 ] || usage
        _upsert_nest_key "$2" static_dir "\"${STATIC_DIR}\""
        ;;
    seed-cors-origins)
        [ $# -eq 3 ] || usage
        _upsert_nest_key "$2" cors_origins "$3"
        ;;
    *)
        echo "unknown subcommand: $1" >&2
        usage
        ;;
esac
