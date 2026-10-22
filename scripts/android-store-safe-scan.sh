#!/usr/bin/env bash
# The android store-safe excision scan — ONE pattern list for every artifact
# that has to prove it (dynamic-features.md § What "completely compiled away"
# means, items 1-2): the store-safe witness APK (`just android-store-safe-check`,
# both columns) and the Play upload AABs (`just android-bundle-store-safe`,
# `just android-bundle-store-no-payments`).
#
#   android-store-safe-scan.sh absent      <artifact.apk|artifact.aab> <workdir>
#   android-store-safe-scan.sh present     <artifact.apk|artifact.aab> <workdir>
#   android-store-safe-scan.sh no-payments <artifact.apk|artifact.aab> <workdir>
#
# `absent` is the store-safe column: no payments / zaps / p2p-share element id
# in the dex, no kind string anywhere. `present` is the default column: the
# same surface must be there, or the `absent` column is vacuous. `no-payments`
# is the `store-no-payments` column (dynamic-features.md § The App-Store escape
# hatch → *The store flavors*; `android-bundle-store-no-payments` scans with it
# too): the payments and zaps patterns absent, the p2p-share ones present.
#
# Three mechanics, each a measured trap (read `android-store-safe-check`'s
# header in the justfile for the long form):
#
#  1. **Unpack first.** APK and AAB are ZIPs: dex string constants are
#     deflate-compressed inside them, and `strings` over the packed bytes
#     reports 0 for everything — which reads exactly like a perfect excision.
#     The two archives keep the same members at different paths: an APK at
#     `classes*.dex` + `lib/*/*.so`, a bundle under its `base/` module.
#  2. **Element ids are scanned in the dex only; kind strings everywhere.**
#     Ids are a claim about the SHELL (Compose paints them, they live in the
#     dex); the packaged `.so` carries UniFFI doc comments that NAME
#     `post-tip-*` while painting nothing (2026-08-16, the first run). Kind
#     strings are criterion 2's "absent means absent, prose included".
#  3. **Positive controls in both modes.** An always-rendered element id in the
#     dex and a kept plane's kind string in the native library must be found,
#     so a scan reading nothing (a layout change, a zero-file unpack) fails
#     here instead of passing as an excision. This is what lets the Play
#     bundle prove its own absence without a second, default-flavor bundle.
set -euo pipefail

MODE="${1:?usage: android-store-safe-scan.sh <absent|present> <artifact> <workdir>}"
ARTIFACT="${2:?missing artifact path}"
WORK="${3:?missing work directory}"
case "$MODE" in absent|present|no-payments) ;; *) echo "ERROR: mode must be absent, present or no-payments, got '$MODE'" >&2; exit 2 ;; esac
[ -f "$ARTIFACT" ] || { echo "ERROR: no artifact at $ARTIFACT" >&2; exit 1; }

case "$ARTIFACT" in
    *.apk) DEX_GLOB='classes*.dex';           SO_GLOB='lib/*/*.so' ;;
    *.aab) DEX_GLOB='base/dex/classes*.dex';  SO_GLOB='base/lib/*/*.so' ;;
    *) echo "ERROR: $ARTIFACT is neither an .apk nor an .aab" >&2; exit 2 ;;
esac

# Emit every printable string in the named archive members. `-a` because a dex
# is not a recognised object format.
archive_strings() {
    local out="$1"; shift
    rm -rf "$out" && mkdir -p "$out"
    unzip -q -o "$ARTIFACT" "$@" -d "$out"
    # A silent zero-file unpack would make every assertion below vacuous.
    [ -n "$(find "$out" -type f -print -quit)" ] || {
        echo "ERROR: unpacking '$*' from $ARTIFACT produced no files." >&2
        exit 1
    }
    find "$out" -type f -print0 | xargs -0 strings -a
}
mkdir -p "$WORK"
archive_strings "$WORK/scan-dex" "$DEX_GLOB" > "$WORK/scan-dex.txt"
archive_strings "$WORK/scan-all" "$DEX_GLOB" "$SO_GLOB" > "$WORK/scan-all.txt"
archive_strings "$WORK/scan-so" "$SO_GLOB" > "$WORK/scan-so.txt"

# Positive controls: present in every flavor. `page-heading` / `error-message`
# are the two ids android paints on the most screens; `fauna.feed.` is a plane
# store-safe keeps (only the kids flavor excises it).
for ctl in 'page-heading' 'error-message'; do
    if ! grep -qF -- "$ctl" "$WORK/scan-dex.txt"; then
        echo "ERROR: the dex of $ARTIFACT carries no '$ctl' element id — the scan is" >&2
        echo "  reading nothing, so every absence it reports below would be vacuous." >&2
        exit 1
    fi
done
if ! grep -q 'fauna\.feed\.' "$WORK/scan-so.txt"; then
    echo "ERROR: the native library of $ARTIFACT carries no 'fauna.feed.' kind string —" >&2
    echo "  the scan is reading nothing, so a kind-string absence would be vacuous." >&2
    exit 1
fi

# The `p2p-share` member's catalog prefixes ride the element-id column: its
# ceremony render is gated on `BuildConfig.P2P_SHARE`, and its plane half
# (`share-transfer-` / `share-serve-`) is painted nowhere on android, so all
# four must be absent from the store-safe dex — and only the two android
# paints can be required present in the default one. `offline-receive-` is the
# one that caught a minified-build defect (2026-09-30, fixed 2026-10-08): with
# the `uniffi.*` binding packages unkept, the release shrinker folded the ceremony's gates to
# false and the default dex carried no `offline-receive-` id at all —
# `offline-share-own-code` alone survived, which `offline-share-` by itself
# could not tell apart from a working build. `fauna.tips.` is a kind with no
# default-build sender to require.
#
# Split by member, so the `no-payments` mode can take the money half absent and
# the sharing half present from the same lists the other two modes read.
PAY_PATS=('subscription-provider-' 'subscription-claim-' 'post-tip-' 'nostr-zap-signer-' 'fauna\.payments\.' 'fauna\.nostr\.zap_signers\.' 'subscription-tier-form-price-hint' 'subscription-tier-form-asking-price' 'subscription-tier-form-payment-url' 'compose-sell-price' 'compose-sell-asking-price' 'compose-sell-subscribers-free' 'subscription-tier-price' 'subscription-offer-price' 'subscription-offer-payment-link' 'gated-post-price' 'gated-post-payment-link' 'gated-post-buy-button')
PAY_ABSENT_ONLY_PATS=('fauna\.tips\.')
P2P_PATS=('offline-share-' 'offline-receive-')
P2P_ABSENT_ONLY_PATS=('share-transfer-' 'share-serve-')

case "$MODE" in
    absent)      ABSENT=("${PAY_PATS[@]}" "${PAY_ABSENT_ONLY_PATS[@]}" "${P2P_PATS[@]}" "${P2P_ABSENT_ONLY_PATS[@]}"); PRESENT=() ;;
    present)     ABSENT=(); PRESENT=("${PAY_PATS[@]}" "${P2P_PATS[@]}") ;;
    no-payments) ABSENT=("${PAY_PATS[@]}" "${PAY_ABSENT_ONLY_PATS[@]}"); PRESENT=("${P2P_PATS[@]}") ;;
esac
scan_for() {  # the strings file a pattern is read from
    case "$1" in
        fauna*) echo "$WORK/scan-all.txt" ;;   # criterion 2 — prose included
        *)      echo "$WORK/scan-dex.txt" ;;   # criterion 1 — the shell's own surface
    esac
}
for pat in "${ABSENT[@]}"; do
    n="$(grep -c "$pat" "$(scan_for "$pat")" || true)"
    if [ "$n" != "0" ]; then
        echo "ERROR: the $MODE-scanned Android artifact still carries $n '$pat' occurrence(s)." >&2
        echo "  ($ARTIFACT)" >&2
        echo "  An excised app artifact must contain NO UI surface (element ids) and NO" >&2
        echo "  wire senders (kind strings) for the plane — dynamic-features.md" >&2
        echo "  § What \"completely compiled away\" means, items 1-2." >&2
        echo "  Two causes, in order of likelihood:" >&2
        echo "   * a RENDER with no BuildConfig.PAYMENTS (or, for a p2p-share id," >&2
        echo "     BuildConfig.P2P_SHARE) condition. It compiles fine —" >&2
        echo "     the data it reads is an ungated inert record (PostSummary.tips is" >&2
        echo "     the known one) — so it paints nothing and still ships every id it" >&2
        echo "     would have painted. Gate the render, not just the resolver." >&2
        echo "   * the release shrinker kept the folded branch (a keep rule, or a reference that made it" >&2
        echo "     reachable). Move that surface into app/src/payments/ (or p2pShare/) instead, where" >&2
        echo "     the absence is the compiler's doing rather than the optimizer's." >&2
        exit 1
    fi
done
for pat in "${PRESENT[@]}"; do
    n="$(grep -c "$pat" "$(scan_for "$pat")" || true)"
    if [ "$n" = "0" ]; then
        echo "ERROR: the $MODE-scanned Android artifact carries no '$pat'." >&2
        echo "  In the default column that makes the store-safe assertion vacuous; in the" >&2
        echo "  no-payments column it means the flavor lost the sharing plane it KEEPS." >&2
        echo "  Either the surface was removed (a product regression — e.g. a sharing" >&2
        echo "  render gated on BuildConfig.PAYMENTS) or it was renamed and this scan" >&2
        echo "  needs its patterns updated." >&2
        echo "  ($ARTIFACT)" >&2
        exit 1
    fi
done
echo "android-store-safe-scan: $MODE — ${#ABSENT[@]} absent, ${#PRESENT[@]} present payments/zaps/p2p-share patterns, controls found  $ARTIFACT"
