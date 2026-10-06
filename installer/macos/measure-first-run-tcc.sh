#!/bin/bash
# Observe macOS app-group TCC activity (kTCCServiceSystemPolicyAppData) for the
# signed-build first-run measurement — the pre-registered outcome matrix in
# docs/goal/architecture/installers/macos.md § Identifier domain.
#
# This exists as a script because the `log` predicate quoting does not survive
# zsh interpolation when composed inline from another tool or shell layer.
#
# Usage (run as the GUI user, in a real terminal):
#   ./measure-first-run-tcc.sh reset       Clear Fauna's sticky TCC AppData
#                                          decision (bundle-scoped). Run between
#                                          measurement arms — answers are sticky.
#   ./measure-first-run-tcc.sh reset-all   Clear the WHOLE per-user AppData
#                                          service (needed for the bare agent,
#                                          whose path subject tccutil cannot name
#                                          by bundle id). ⚠ Also clears every
#                                          other app's AppData decision for this
#                                          user — say so before running it.
#   ./measure-first-run-tcc.sh watch       Live-stream TCC AppData events while
#                                          you launch Fauna.app / kickstart the
#                                          agent. Ctrl-C to stop.
#   ./measure-first-run-tcc.sh report [WINDOW]
#                                          Verdict lines from the unified log for
#                                          the last WINDOW (default 30m):
#                                          PROMPTED / SILENT per Fauna subject.
set -euo pipefail

APP_BUNDLE_ID="social.fauna.fauna"
# One predicate for watch + report: TCC subsystem events that concern either the
# AppData service or any Fauna subject (bundle id or the bare agent's path).
PREDICATE='subsystem == "com.apple.TCC" AND (eventMessage CONTAINS "AppData" OR eventMessage CONTAINS[c] "fauna")'

case "${1:-}" in
  reset)
    tccutil reset SystemPolicyAppData "$APP_BUNDLE_ID"
    echo "reset: SystemPolicyAppData decisions cleared for $APP_BUNDLE_ID"
    ;;
  reset-all)
    tccutil reset SystemPolicyAppData
    echo "reset-all: SystemPolicyAppData decisions cleared for EVERY app (this user)"
    ;;
  watch)
    echo "streaming TCC AppData events — launch the app / kickstart the agent now (Ctrl-C to stop)"
    log stream --style compact --info --predicate "$PREDICATE"
    ;;
  report)
    WINDOW="${2:-30m}"
    echo "== raw TCC AppData events, last $WINDOW =="
    log show --last "$WINDOW" --style compact --info --predicate "$PREDICATE" \
      | grep -E "AUTHREQ_|BUNDLE_ATTRIBUTION|AppData" || true
    echo
    echo "== verdict =="
    PROMPTS=$(log show --last "$WINDOW" --style compact --info --predicate "$PREDICATE" \
      | grep "AUTHREQ_PROMPTING" | grep -ci "fauna" || true)
    if [ "${PROMPTS:-0}" -gt 0 ]; then
      echo "PROMPTED: $PROMPTS AppData authorization prompt(s) attributed to a Fauna subject."
      echo "Matrix arm (a) [unprefixed group]: this CONFIRMS the model — try arm (b), not the data-root fork."
      echo "Matrix arm (b) [Team-ID-prefixed group]: this REFUTES the expectation — the data-root fork opens."
    else
      echo "SILENT: no AppData prompt attributed to a Fauna subject in the window."
      echo "(Confirm the app/agent actually touched the group container in this window before"
      echo " reading SILENT as authorized — an idle process proves nothing.)"
    fi
    ;;
  *)
    sed -n '2,24p' "$0"
    exit 1
    ;;
esac
