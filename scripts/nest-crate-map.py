#!/usr/bin/env python3
"""The nest's module graph and the crate-cut plan it is measured against.

Owner: docs/goal/architecture/nest-crate-layout.md. Stdlib-only; run as `just nest-crate-map <cmd>`.

  map   [--all]        Each top-level module of bins/fauna-nest/src: production lines (the `#[cfg(test)]`
                       items and `tests/` files stripped), total lines, its feature gate, and the other
                       top-level modules it references (`crate::x` paths, `use crate::{...}` lists,
                       `super::x` from a top-level file); then the strongly connected components.
                       Prints the largest modules and the hubs; --all prints every module.
  plan                 Evaluates PLAN (the crate membership the goal doc ratified) against the graph:
                       per-crate production lines, every back-edge (a reference from a crate to one it
                       may not depend on) with its reference-site count, and the allowed cross-crate
                       counts. A back-edge is work the cut must do; zero back-edges is a buildable DAG.
  mono <mono_items.json>
                       Aggregates a `-Zdump-mono-stats-format=json` dump of the nest lib per top-level
                       module (items, instantiations, estimated size), separating dependency generics
                       instantiated by nest code from the nest's own items.

The reference scan is textual: it counts path sites, not symbols, so a module referenced only through a
re-export is missed and a doc-comment link is counted. It is the map a cut starts from, not the compiler.
"""
import argparse
import json
import os
import re
import sys
from collections import defaultdict

SRC = "bins/fauna-nest/src"
IDENT = r"[A-Za-z_][A-Za-z0-9_]*"

# ---------------------------------------------------------------------------------------------
# The plan. Order = allowed dependency direction: a crate may depend on every crate its entry
# lists. Siblings never depend on each other. `nest` is the binary and its wiring.
# A module listed under a crate is the WHOLE top-level module; the three sub-module moves the
# goal doc names (segments::records_db → db, routes' post core → social, state's sub-state
# types staying in core) are not expressible here and show up as back-edges to do.
# ---------------------------------------------------------------------------------------------
LAYERS = [
    ("base", []),
    ("db", ["base"]),
    ("core", ["base", "db"]),
    ("mail", ["base", "db", "core"]),
    ("social", ["base", "db", "core"]),
    ("files", ["base", "db", "core"]),
    ("web", ["base", "db", "core"]),
    ("admin", ["base", "db", "core"]),
    ("nostr", ["base", "db", "core"]),
    ("activitypub", ["base", "db", "core"]),
    ("bluesky", ["base", "db", "core"]),
    ("test-hooks", ["base", "db", "core", "mail", "social", "files", "web", "admin", "nostr", "activitypub", "bluesky"]),
    ("nest", ["base", "db", "core", "mail", "social", "files", "web", "admin", "nostr", "activitypub", "bluesky", "test-hooks"]),
]
ALLOWED = {c: set(d) | {c} for c, d in LAYERS}

PLAN = {
    "base": """api_error rpc_errors config ssrf blob_store s3_blob_store dns_verifier mail_deliverability video rate_limit
        streaming federation_sig http_range services oauth_as_error identity_op_grant nest_identity admin sweeper registration
        served_name spam_model_seal unix_signal text_heuristic bridge_rate_limit restore export partition_scan""",
    "db": """db deployment_key succession_ownership bridge_approval_test_support payload_store sidecar_tokens cursor_seal nest_kek
        mail_dkim_key media_ticket_secret oauth_session_secret oauth_issuer_key room_read_key atproto_authoring_key
        change_signature""",
    "core": """routes state ws dispatch_core rpc_router bridge_method_allowlist auth auth_core challenge_auth anonymous_rate_limit
        failed_credential_throttle pre_identity_allowlist segments storage acme acme_http01 self_signed_cert push
        bridge_push_registry delegation_registry federation_pool federation_channel federation_router security_notify nest_link
        sidecar_channel token_store bulk_byte_token ttl_gc age_attest principal_session test_support backup web_content
        plugin_runner oauth_as_state oauth_as_rate_limit oauth_as_ceremony oauth_as_client oauth_as_token events_webhook
        atproto_oracle succession_pull rpc_hold_test_hook rescore_drain_test_hook post_fanout_test_hook account_lock_test_hook
        custody_hosting_worker segment_backup delegation_runner moderation_withhold folder_authz link_preview region_tier
        feature_gate media_ticket media_proxy_routes custody_admission custody_fleet bridge_management bridge_dm_gate
        discovery_core account_core invite_core pending_actions principal_handlers""",
    "mail": """bridge_routing_handlers bridge_imap_handlers bridge_blob_handlers bridge_atproto_handlers bridge_caldav_handlers
        bridge_carddav_handlers bridge_import_handlers bridge_export_handlers bridge_list_handlers
        bridged_conversation_handlers bridges_ui_handlers email_handlers mail_enable mail_from_ownership mail_submitter_proof
        mail_body_plane mail_export_blobs outbound_bounce outbound_retry outbound_dmarc outbound_tlsrpt outbound_mta_sts
        mta_sts_advance dav_pre_split caldav_bridge carddav_bridge webdav_bridge well_known_dav spam_baseline nat_mode_core
        mode_commit host_maintenance nat_mode_handlers bridge_withdraw bridge_round_trip""",
    "social": """conversations_handlers posts_handlers feed_handlers feed_routes profile_handlers personalization_handlers
        notifications_handlers contacts_handlers search_handlers spam_handlers moderation_handlers content_policy_handlers
        content_index_handlers drafts_handlers mls_replica_handlers label_handlers labeler_handlers interact_routes
        subscription_handlers subscription_routes family_handlers invite_handlers session_handlers account_handlers
        events_doors generation_escrow_handlers pending_action_handlers eviction region_relay records_door post_author_oracle
        profile_author_oracle authoring_oracle link_preview_handlers tip_handlers payment_core payment_handlers payment_routes
        push_handlers auth_handlers transport_policy_handlers web_app_origin
        federation_handlers follow_federation follow_delivery moderation_federation abuse_report_federation outbox
        peer_query exchange_originator nest_sync_worker room_post_view post_delete_redrive discovery discovery_handlers
        inbox_handlers""",
    "files": """folder_handlers folder_public folder_deposit files_handlers filesync_handlers sync_handlers chunk_relay chunk_routes
        blob_routes share_routes share_handlers video_routes custody_hosting_handlers custody_receipt_handlers
        delegation_handlers backup_handlers recovery_handlers export_routes admin_export_routes media_handlers stats_handlers""",
    "web": """web_handlers web_files_projection oauth_as_routes oauth_as_gates oauth_as_oidc oauth_as_permission_sets
        oauth_issuer_handlers oauth_issuer_routes oauth_consent_handlers plugins_handlers plugin_ingress
        webdav_principal_admission""",
    "admin": """admin_ws_handlers node_policy_core node_policy_handlers dns_handlers tls_handlers domain_expiry
        domain_expiry_handlers cert_nudge lan_cert desktop_serve claim claim_core claim_handlers factory_reset degraded_serve
        build_identity update_channel updater_report log_plane router_status relay_admission identity_domain_core
        pair_handlers membership_lapse trend_sweeper principals_handlers atproto_blob_sweeper""",
    "nostr": "nostr",
    "activitypub": "activitypub",
    "bluesky": "bluesky",
    "test-hooks": """protocol_test push_test_hooks content_seal_test_hook content_seal_epoch_test_hook outbound_tlsrpt_test_hook
        channel_refusal_test_hook outbound_dmarc_test_hook dns_seed_test_hook region_relay_test_hook region_status_test_hook
        custody_hosting_test_hook generation_escrow_test_hook delegation_lease_test_hook outbound_drain_test_hook web_domain_activate_test_hook
        mta_sts_test_hook segment_backup_test_hook link_preview_test_hook mail_portability_test_hook
        tls_cert_withhold_test_hook bridge_status_test_hook snapshot_scheduler_test_hook tlsa_test_hook
        capability_withhold_test_hook cert_nudge_test_hook eviction_test_hook invite_request_prune_test_hook
        acme_issuance_error_test_hook atproto_identity_test_hook rename_cert_test_hook domain_expiry_test_hook
        release_test_hook spam_baseline_cadence_test_hook recovery_landing_test_hook web_paywall_test_hook
        web_render_fault_test_hook version_prune_test_hook pending_actions_test_hook web_blank_site_test_hook
        outbound_clock_test_hook keypackage_age_test_hook share_link_age_test_hook mta_sts_advance_test_hook
        mail_domain_gc_test_hook mail_health_state_test_hook mass_mailing_test_hook membership_lapse_test_hook
        blocklist_self_check_test_hook spam_history_gc_test_hook""",
    "nest": "<lib.rs> <main.rs> bridge_legs bridge_schema",
}


# ---------------------------------------------------------------------------------------------
# Scanning
# ---------------------------------------------------------------------------------------------
def declared_modules(lib_lines):
    """name -> cfg text (or None) for every `mod x;` in lib.rs, reading the `#[cfg(...)]` just above it."""
    mods = {}
    pending = None
    mod_re = re.compile(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?mod\s+(" + IDENT + r")\s*;")
    cfg_re = re.compile(r"^\s*#\[cfg\((.*)\)\]")
    for line in lib_lines:
        m = cfg_re.match(line)
        if m:
            pending = m.group(1)
            continue
        m = mod_re.match(line)
        if m:
            mods[m.group(1)] = pending
            pending = None
            continue
        if not line.strip().startswith("#["):
            pending = None
    return mods


def module_files(name):
    out = []
    f = os.path.join(SRC, name + ".rs")
    d = os.path.join(SRC, name)
    if os.path.isfile(f):
        out.append(f)
    if os.path.isdir(d):
        for root, _, fs in os.walk(d):
            out += [os.path.join(root, x) for x in fs if x.endswith(".rs")]
    return out


def strip_tests(text):
    """Drop `#[cfg(test)]` items: a `mod x { ... }` block (brace-matched), a `mod x;` or a `use`."""
    out = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.strip() == "#[cfg(test)]":
            nxt = lines[i + 1] if i + 1 < len(lines) else ""
            if re.match(r"\s*(pub(\([a-z]+\))? )?mod\s+\w+\s*\{", nxt):
                depth = 0
                j = i + 1
                started = False
                while j < len(lines):
                    depth += lines[j].count("{") - lines[j].count("}")
                    if "{" in lines[j]:
                        started = True
                    if started and depth <= 0:
                        break
                    j += 1
                i = j + 1
                continue
            if re.match(r"\s*(pub(\([a-z]+\))? )?(mod\s+\w+\s*;|use\b)", nxt):
                i += 2
                continue
        out.append(line)
        i += 1
    return "\n".join(out)


def is_test_file(path):
    return "/tests/" in path or path.endswith("/tests.rs")


def brace_list_idents(text, start):
    depth = 1
    i = start
    buf = []
    items = []
    while i < len(text) and depth > 0:
        c = text[i]
        if c == "{":
            depth += 1
            if depth == 2:
                buf.append(c)
        elif c == "}":
            depth -= 1
            if depth == 0:
                items.append("".join(buf))
            elif depth >= 1:
                buf.append(c)
        elif c == "," and depth == 1:
            items.append("".join(buf))
            buf = []
        else:
            buf.append(c)
        i += 1
    out = []
    for it in items:
        m = re.match(IDENT, it.strip())
        if m:
            out.append(m.group(0))
    return out


CRATE_PATH = re.compile(r"\bcrate::(" + IDENT + r")\b")
USE_CRATE_BRACE = re.compile(r"\buse\s+crate::\{")
SUPER_PATH = re.compile(r"\bsuper::(" + IDENT + r")\b")


def references(files, strip):
    refs = defaultdict(int)
    for f in files:
        if strip and is_test_file(f):
            continue
        text = open(f, encoding="utf-8", errors="replace").read()
        if strip:
            text = strip_tests(text)
        for m in CRATE_PATH.finditer(text):
            refs[m.group(1)] += 1
        for m in USE_CRATE_BRACE.finditer(text):
            for x in brace_list_idents(text, m.end()):
                refs[x] += 1
        if os.path.dirname(f) == SRC and os.path.basename(f) not in ("lib.rs", "main.rs"):
            for m in SUPER_PATH.finditer(text):
                refs[m.group(1)] += 1
    return refs


def count_lines(files, strip):
    n = 0
    for f in files:
        if strip and is_test_file(f):
            continue
        text = open(f, encoding="utf-8", errors="replace").read()
        if strip:
            text = strip_tests(text)
        n += len(text.splitlines())
    return n


def build_graph():
    lib_path = os.path.join(SRC, "lib.rs")
    lib_lines = open(lib_path, encoding="utf-8").read().splitlines()
    mods = declared_modules(lib_lines)
    graph = {}
    for name, feat in mods.items():
        files = module_files(name)
        refs = references(files, strip=True)
        graph[name] = {
            "feature": feat,
            "lines": count_lines(files, strip=False),
            "lib_lines": count_lines(files, strip=True),
            "edges": {k: v for k, v in refs.items() if k in mods and k != name},
        }
    lib_refs = references([lib_path], strip=True)
    graph["<lib.rs>"] = {"feature": None, "lines": len(lib_lines), "lib_lines": len(lib_lines),
                         "edges": {k: v for k, v in lib_refs.items() if k in mods}}
    main_text = open(os.path.join(SRC, "main.rs"), encoding="utf-8").read()
    main_edges = defaultdict(int)
    for m in re.finditer(r"\bfauna_nest::(" + IDENT + r")\b", main_text):
        if m.group(1) in mods:
            main_edges[m.group(1)] += 1
    graph["<main.rs>"] = {"feature": None, "lines": len(main_text.splitlines()),
                          "lib_lines": len(main_text.splitlines()), "edges": dict(main_edges)}
    return graph


def sccs_of(graph):
    sys.setrecursionlimit(10000)
    index, low, onstack, stack, out = {}, {}, set(), [], []
    counter = [0]

    def strong(v):
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        onstack.add(v)
        for w in graph[v]["edges"]:
            if w not in index:
                strong(w)
                low[v] = min(low[v], low[w])
            elif w in onstack:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                onstack.discard(w)
                comp.append(w)
                if w == v:
                    break
            out.append(comp)

    for v in list(graph):
        if v not in index:
            strong(v)
    return out


# ---------------------------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------------------------
def cmd_map(args):
    graph = build_graph()
    mods = [n for n in graph if not n.startswith("<")]
    rev = defaultdict(set)
    for v, d in graph.items():
        for w in d["edges"]:
            rev[w].add(v)
    lib_total = sum(graph[n]["lib_lines"] for n in mods)
    all_total = sum(graph[n]["lines"] for n in mods)
    print(f"modules: {len(mods)}  production lines: {lib_total}  all lines: {all_total}  "
          f"lib.rs: {graph['<lib.rs>']['lines']}  main.rs: {graph['<main.rs>']['lines']}")
    comps = sccs_of(graph)
    big = [c for c in comps if len(c) > 1]
    print(f"strongly connected components: {len(comps)}, with more than one module: {len(big)}")
    for c in sorted(big, key=len, reverse=True):
        print(f"  {len(c)} modules, {sum(graph[x]['lib_lines'] for x in c)} production lines")
        if args.all:
            print("   " + " ".join(sorted(c)))
    shown = sorted(mods, key=lambda n: -graph[n]["lib_lines"])
    if not args.all:
        shown = shown[:30]
    print(f"\n{'production':>10s} {'all':>7s}  {'module':34s} {'feature':22s} {'out':>4s} {'in':>4s}")
    for n in shown:
        d = graph[n]
        print(f"{d['lib_lines']:10d} {d['lines']:7d}  {n:34s} {(d['feature'] or '-')[:22]:22s} "
              f"{len(d['edges']):4d} {len(rev[n]):4d}")
    print("\nmost referenced (in-degree):")
    for n in sorted(mods, key=lambda n: -len(rev[n]))[:25]:
        print(f"  in={len(rev[n]):3d} out={len(graph[n]['edges']):3d} production={graph[n]['lib_lines']:6d}  {n}")
    if args.json:
        json.dump({"modules": graph, "sccs": comps}, open(args.json, "w"), indent=1)
        print(f"\nwrote {args.json}")


def cmd_plan(args):
    graph = build_graph()
    crate_of = {}
    for c, text in PLAN.items():
        for x in text.split():
            if x in crate_of:
                sys.exit(f"module {x} listed twice in PLAN")
            crate_of[x] = c
    missing = sorted(n for n in graph if n not in crate_of)
    extra = sorted(x for x in crate_of if x not in graph)
    if missing or extra:
        print("PLAN out of step with the tree —")
        if missing:
            print("  modules not in PLAN:", " ".join(missing))
        if extra:
            print("  PLAN names no module:", " ".join(extra))
        sys.exit(1)
    lines = {c: 0 for c in PLAN}
    for n, c in crate_of.items():
        lines[c] += graph[n]["lib_lines"]
    total = sum(lines.values())
    print(f"{'crate':12s} {'production lines':>16s}   share   modules")
    for c, _ in LAYERS:
        print(f"{c:12s} {lines[c]:16d}   {100 * lines[c] / total:5.1f}%   {len(PLAN[c].split())}")
    cross = defaultdict(int)
    back = defaultdict(list)
    for n, d in graph.items():
        cn = crate_of[n]
        for w, k in d["edges"].items():
            cw = crate_of[w]
            if cw == cn:
                continue
            cross[(cn, cw)] += k
            if cw not in ALLOWED[cn]:
                back[(cn, cw)].append((n, w, k))
    total_back = sum(k for lst in back.values() for _, _, k in lst)
    print(f"\nback-edges: {total_back} reference sites over {len(back)} crate pairs "
          "(each is a move, an inversion or a trait the cut must land)")
    for (a, b), lst in sorted(back.items(), key=lambda x: -sum(k for _, _, k in x[1])):
        s = sum(k for _, _, k in lst)
        items = ", ".join(f"{n}->{w}:{k}" for n, w, k in sorted(lst, key=lambda x: -x[2]))
        print(f"  {a:>11s} -> {b:<11s} {s:4d}  {items}")
    print("\nallowed cross-crate reference sites:")
    for (a, b), k in sorted(cross.items(), key=lambda x: -x[1]):
        if b in ALLOWED[a]:
            print(f"  {a:>11s} -> {b:<11s} {k}")


def cmd_mono(args):
    data = json.load(open(args.file))
    items = data if isinstance(data, list) else data.get("items", data)
    mods = set(declared_modules(open(os.path.join(SRC, "lib.rs"), encoding="utf-8").read().splitlines()))
    by = defaultdict(lambda: [0, 0, 0])
    total = [0, 0, 0]
    for it in items:
        name = it.get("name", "")
        inst = it.get("instantiation_count", 0)
        size = it.get("total_estimate", it.get("size_estimate", 0))
        m = re.match(r"(?:<)?(?:fauna_nest::)?(" + IDENT + r")::", name)
        if m and m.group(1) in mods:
            key = m.group(1)
        else:
            mm = re.match(r"(?:<)?(" + IDENT + r")::", name)
            key = "[dep] " + (mm.group(1) if mm else name[:30])
        for i, v in enumerate((1, inst, size)):
            by[key][i] += v
            total[i] += v
    dep = sum(v[2] for k, v in by.items() if k.startswith("[dep]"))
    print(f"items {total[0]}  instantiations {total[1]}  estimated size {total[2]}  "
          f"dependency generics instantiated by nest code: {100 * dep / total[2]:.1f}% of the size")
    print(f"{'module or [dep] crate':40s} {'items':>7s} {'inst':>7s} {'size':>10s} {'size%':>6s}")
    for k, (c, i, s) in sorted(by.items(), key=lambda x: -x[1][2])[: args.top]:
        print(f"{k:40s} {c:7d} {i:7d} {s:10d} {100 * s / total[2]:6.2f}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    pm = sub.add_parser("map")
    pm.add_argument("--all", action="store_true")
    pm.add_argument("--json", help="also write the full graph here")
    sub.add_parser("plan")
    pn = sub.add_parser("mono")
    pn.add_argument("file")
    pn.add_argument("--top", type=int, default=40)
    args = p.parse_args()
    if not os.path.isdir(SRC):
        sys.exit(f"run from the repository root: {SRC} not found")
    {"map": cmd_map, "plan": cmd_plan, "mono": cmd_mono}[args.cmd](args)


if __name__ == "__main__":
    main()
