"""Phase-3 perf gate — the tier_3 IMAP FETCH-latency benchmark that CONFIRMS the
release microbench's preliminary GO (tracked internally; Phase 3
perf-gate measurement).

## What this measures and why

Phase 3 (`2026-07-04-capability-mediated-content-processing-design.md` § Phasing
→ Phase 3; `encryption-at-rest.md` § Capability tiering by storage mode) would
seal mail at rest in BOTH storage modes, retiring the plaintext-at-rest posture.
The only per-message serve-path cost that change adds is the encrypted-mode
`OpenMailRecord` HPKE-open every *body* FETCH already pays in production today:

  * plaintext mode serves the record verbatim — no crypto
    (`bins/fauna-bridges/internal/mda/imap/fetch.go:386`, `pt = ct.EncryptedBody`);
  * encrypted mode HPKE-opens each envelope
    (`fetch.go:388`, `opener.OpenMailRecord(...)`).

Everything else on the FETCH path — the `fetch_message_ciphertext` WS-RPC
round-trip (serves bytes verbatim in BOTH modes, no server-side unseal), the
segment-store read, the shared-Rust section extraction — is identical. So the
**encrypted − plaintext FETCH-latency delta measured here IS the Phase-3
per-message serve cost**, end-to-end through real binaries with the release
`libfauna_ffi.so` (`conftest.py` rpaths the bridge at `$TARGET_DIR/release`), so
the `OpenMailRecord` open matches the release microbench.

The release microbench already answered the crux (the open is sub-millisecond:
~50–165 µs typical mail, ~0.5 ms for 200 KiB). This e2e answers:

  (a) the open as a measured *fraction* of the real end-to-end FETCH round-trip; and
  (b) whether a larger mailbox inflates the FETCH round-trip itself — a size
      effect independent of the crypto.

## ⚠ Methodology notes

  * **The nest is a RELEASE build** — the bench nests stand up on the
    `bench_nest_binary` fixture (`build_node(release=True)`), NOT the debug
    `nest_binary` every functional test uses. A debug nest's absolute round-trip
    and mailbox-scaling are debug-amplified and understate the crypto-delta
    *fraction* (delta ÷ round-trip); release makes the fraction representative.
    The crypto DELTA itself is valid on either profile — the open runs in the
    bridge against the RELEASE FFI, and the nest serves ciphertext verbatim in
    both modes (a common-mode cost that cancels in the delta).
  * A sufficiently large mailbox can still saturate the nest's WS outbound reply
    queue during the SELECT/FETCH phase (`fauna_nest::ws: outbound queue
    saturated`), tearing down the bridge↔nest connection so
    `fetch_message_metadata` hits its 5 s deadline. That is a crypto-INDEPENDENT
    nest-transport/scaling wall (it reproduces in plaintext mode), orthogonal to
    Phase-3. The benchmark treats it as a recorded finding, not a failure.

## Shape

Two DEDICATED nests (storage mode is write-once): `bench_mda_plaintext` and
`bench_mda_encrypted` each stand up their own nest + MDA + fresh MSEK recipient
(conftest `_bench_mda_impl`). Bulk-ingest is IMAP **APPEND** (not SMTP): APPEND
seals in encrypted mode / stores plaintext in plaintext mode — the SAME
mode-conditional at-rest shape as inbound SMTP (`imap/append.go`), but one
AUTH'd IMAPS connection instead of per-message SMTP+STARTTLS. Latency is sampled
with `BODY.PEEK[]` (read-only: isolates the serve+decrypt path from the
non-PEEK ``\\Seen``-flag write, a mode-INDEPENDENT nest write; the crypto delta
is unaffected either way).

Building the mailbox is bounded by a per-tier wall-clock **append budget**
(debug-nest APPEND is ~hundreds of ms each), so the default 1k/10k target stays
bounded: a tier that can't be reached in budget is measured at the count reached
and recorded as `reached < target`. Per-tier measure failures (the scaling wall
above) are caught and recorded, keeping the test green as long as the crypto
delta was measured at ≥1 size in both modes.

## Opt-in

`FAUNA_BENCH=1`-gated. Env knobs:
  * `FAUNA_BENCH_SIZES`   — comma-sep mailbox size targets (default `1000,10000`).
  * `FAUNA_BENCH_SAMPLE`  — FETCHes sampled per (mode × size × cold/warm) pass
                            (default `200`).
  * `FAUNA_BENCH_BUDGET`  — per-tier append wall-clock budget, seconds (default `240`).
  * `FAUNA_BENCH_OUT`     — path to also write the results table as JSON
                            (written after each mode so partial data survives).

The recorded delta + a ratify/amend of the GO go into the NEXT § Phase-3 section.
"""

import json
import os
import statistics
import time

import pytest

from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_fetch_body_cmd,
    _imap_read_tagged,
    _imaps_connect,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.skipif(
        os.environ.get("FAUNA_BENCH") != "1",
        reason="opt-in perf benchmark; set FAUNA_BENCH=1 to run "
        "(APPENDs up to 1k/10k messages — minutes, not an inner-loop test)",
    ),
]


# ── Tunables (env-overridable so a smoke run can shrink them) ────────────────
def _sizes() -> list[int]:
    raw = os.environ.get("FAUNA_BENCH_SIZES", "1000,10000")
    return [int(x) for x in raw.split(",") if x.strip()]


def _sample() -> int:
    return int(os.environ.get("FAUNA_BENCH_SAMPLE", "200"))


def _budget() -> float:
    return float(os.environ.get("FAUNA_BENCH_BUDGET", "240"))


_BODY_TYPICAL = 2 * 1024        # 2 KiB — dominant "typical mail" size
_BODY_LARGE = 200 * 1024        # 200 KiB — the large-body tail (microbench's top row)
_N_LARGE = 20                   # how many 200 KiB bodies to seed for the large-body delta
_FILLER = b"The quick brown fox jumps over the lazy dog. 0123456789abcdef\r\n"  # 63 B


def _bench_message(recipient: str, idx: int, target_bytes: int) -> bytes:
    """A syntactically-plausible RFC822 message padded to ~`target_bytes`. The
    IMAP APPEND literal is length-prefixed, so any body bytes round-trip
    verbatim (no dot-stuffing); the bridge seals the whole literal in encrypted
    mode."""
    header = (
        b"From: bench@example.com\r\n"
        b"To: " + recipient.encode() + b"\r\n"
        b"Subject: bench message " + str(idx).encode() + b"\r\n"
        b"Message-ID: <bench-" + str(idx).encode() + b"@mda.fauna.test>\r\n"
        b"Date: Wed, 15 May 2026 12:00:00 +0000\r\n"
        b"\r\n"
    )
    remaining = max(0, target_bytes - len(header))
    reps = remaining // len(_FILLER) + 1
    body = (_FILLER * reps)[:remaining]
    return header + body


def _pct(values: list[float], frac: float) -> float:
    """Nearest-rank percentile (no interpolation) — robust for small samples."""
    if not values:
        return 0.0
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round(frac * (len(s) - 1)))))
    return s[k]


def _even_seqs(n: int, sample: int) -> list[int]:
    """`sample` sequence numbers evenly spaced across [1, n] (1-based)."""
    m = min(sample, n)
    if m <= 1:
        return [1] if n >= 1 else []
    return sorted({1 + (i * (n - 1)) // (m - 1) for i in range(m)})


def _peek_fetch(sock, buf, tag: str, seq: int, deadline: float):
    """`FETCH <seq> BODY.PEEK[]` → (status, body_bytes). Read-only (no `\\Seen`
    write) so the timing isolates the serve+decrypt path."""
    return _imap_fetch_body_cmd(sock, buf, tag, f"FETCH {seq} BODY.PEEK[]", deadline)


def _append_up_to(handle, target_count: int, current_count: int, body_size: int,
                  tag_prefix: str, budget_s: float) -> int:
    """APPEND `_bench_message`s over one fresh AUTH'd IMAPS connection until the
    mailbox holds `target_count` messages OR the wall-clock `budget_s` is spent,
    whichever first. Returns the count actually reached. Seq numbers are the
    1-based arrival order (no expunge), so message k has seq k."""
    conn_deadline = time.monotonic() + max(budget_s, 60.0) + 60.0
    started = time.monotonic()
    sock, buf = _imaps_connect(handle, conn_deadline)
    i = current_count
    with sock:
        assert _imap_auth_plain(
            sock, buf, "au", handle.recipient_username, handle.recipient_password, conn_deadline
        ) == "OK", "AUTH PLAIN must succeed for the bench recipient"
        while i < target_count:
            if time.monotonic() - started > budget_s:
                break  # append budget spent — measure at the count reached
            i += 1
            msg = _bench_message(handle.recipient_username, i, body_size)
            deadline = time.monotonic() + 120.0  # per-APPEND budget
            status, uid = _imap_append(sock, buf, f"{tag_prefix}{i}", "INBOX", msg, deadline)
            assert status == "OK", f"APPEND #{i} must succeed; got {status}"
            if i % 500 == 0:
                print(f"  [{handle.storage_mode}] appended {i}/{target_count} "
                      f"({body_size}B) in {time.monotonic() - started:.0f}s", flush=True)
        sock.sendall(b"az LOGOUT\r\n")
    return i


def _measure_pass(handle, seqs: list[int], expect_len: dict[int, int] | None,
                  label: str) -> dict:
    """Fresh connect → SELECT → cold FETCH pass over `seqs` → warm FETCH pass
    over the same `seqs`. Returns per-pass p50/p95 (µs). Correctness: every FETCH
    is OK and (for the typical-body sample) the returned body length matches the
    APPENDed length — the encrypted arm's first full-stack seal→open proof.

    Raises on a FETCH that is not OK (the caller records that as the scaling
    wall) so a torn-down connection surfaces as a recorded finding."""
    deadline = time.monotonic() + 1800.0
    sock, buf = _imaps_connect(handle, deadline)
    cold: list[float] = []
    warm: list[float] = []
    with sock:
        assert _imap_auth_plain(
            sock, buf, "m0", handle.recipient_username, handle.recipient_password, deadline
        ) == "OK", "AUTH PLAIN must succeed"
        sock.sendall(b"m1 SELECT INBOX\r\n")
        st, _ = _imap_read_tagged(sock, buf, "m1", deadline)
        assert st == "OK", f"SELECT INBOX must succeed; got {st}"

        for phase, acc in (("c", cold), ("w", warm)):
            for j, seq in enumerate(seqs):
                deadline = time.monotonic() + 120.0
                t0 = time.perf_counter()
                status, body = _peek_fetch(sock, buf, f"{phase}{j}", seq, deadline)
                dt = (time.perf_counter() - t0) * 1e6  # µs
                assert status == "OK", f"[{label}] FETCH seq {seq} status {status}"
                assert body is not None, f"[{label}] FETCH seq {seq} returned no body"
                if expect_len is not None and seq in expect_len:
                    assert len(body) == expect_len[seq], (
                        f"[{label}] FETCH seq {seq}: body length {len(body)} != "
                        f"APPENDed {expect_len[seq]} (seal→open must round-trip verbatim)"
                    )
                acc.append(dt)
        sock.sendall(b"mz LOGOUT\r\n")

    return {
        "n_sampled": len(seqs),
        "cold_p50": _pct(cold, 0.50), "cold_p95": _pct(cold, 0.95),
        "cold_mean": statistics.fmean(cold) if cold else 0.0,
        "warm_p50": _pct(warm, 0.50), "warm_p95": _pct(warm, 0.95),
        "warm_mean": statistics.fmean(warm) if warm else 0.0,
    }


def _run_mode(handle, sizes: list[int], sample: int, budget_s: float) -> dict:
    """Build the mailbox incrementally toward each target size (2 KiB bodies)
    under the append budget, measuring cold+warm FETCH latency at the reached
    count; then seed `_N_LARGE` 200 KiB bodies and measure the large-body
    latency. Per-tier measure failures are caught + recorded (scaling wall)."""
    mode = handle.storage_mode
    typ_len = len(_bench_message(handle.recipient_username, 1, _BODY_TYPICAL))
    out: dict = {"mode": mode, "typical_body_len": typ_len, "by_size": {}}

    count = 0
    for target in sizes:
        count = _append_up_to(handle, target, count, _BODY_TYPICAL, "t", budget_s)
        rec: dict = {"target": target, "reached": count}
        seqs = _even_seqs(count, sample)
        try:
            m = _measure_pass(handle, seqs, {s: typ_len for s in seqs},
                              label=f"{mode}/target={target}/reached={count}")
            rec.update(m)
        except (AssertionError, OSError) as e:
            rec["error"] = f"{type(e).__name__}: {e}"
            print(f"  [{mode}] measure at reached={count} FAILED (recorded): {rec['error']}",
                  flush=True)
        out["by_size"][target] = rec
        if count < target:
            print(f"  [{mode}] target {target} NOT reached in budget "
                  f"(stopped at {count}); skipping larger tiers", flush=True)
            break  # can't reach larger tiers if this one blew the budget

    # Large-body tail (best-effort): append _N_LARGE × 200 KiB, then measure.
    large_start = count + 1
    large_len = len(_bench_message(handle.recipient_username, large_start, _BODY_LARGE))
    count = _append_up_to(handle, count + _N_LARGE, count, _BODY_LARGE, "L", budget_s)
    out["large_body_len"] = large_len
    if count >= large_start:
        large_seqs = list(range(large_start, count + 1))
        try:
            out["large"] = _measure_pass(handle, large_seqs,
                                         {s: large_len for s in large_seqs},
                                         label=f"{mode}/large")
        except (AssertionError, OSError) as e:
            out["large"] = {"error": f"{type(e).__name__}: {e}"}
    return out


def _fmt(v) -> str:
    return f"{v:8.1f}" if isinstance(v, (int, float)) else f"{str(v):>8}"


def _row(scope: str, phase: str, p: dict | None, e: dict | None) -> tuple[str, dict]:
    """Format one comparison row; tolerates a missing/errored side."""
    def g(d, k):
        return d.get(k) if d and "error" not in d else None
    pp50, pp95 = g(p, f"{phase}_p50"), g(p, f"{phase}_p95")
    ep50, ep95 = g(e, f"{phase}_p50"), g(e, f"{phase}_p95")
    d50 = (ep50 - pp50) if (pp50 is not None and ep50 is not None) else "n/a"
    d95 = (ep95 - pp95) if (pp95 is not None and ep95 is not None) else "n/a"
    line = (f"{f'{scope} {phase}':>16} | {_fmt(pp50 if pp50 is not None else 'ERR')} "
            f"{_fmt(pp95 if pp95 is not None else 'ERR')} | "
            f"{_fmt(ep50 if ep50 is not None else 'ERR')} "
            f"{_fmt(ep95 if ep95 is not None else 'ERR')} | {_fmt(d50)} {_fmt(d95)}")
    return line, {"scope": scope, "phase": phase, "pt_p50": pp50, "pt_p95": pp95,
                  "enc_p50": ep50, "enc_p95": ep95, "delta_p50": d50, "delta_p95": d95}


def _write_json(path, payload):
    if path:
        with open(path, "w") as f:
            json.dump(payload, f, indent=2, default=str)


def test_imap_fetch_latency_bench(bench_mda_plaintext, bench_mda_encrypted):
    """The two-mode FETCH-latency benchmark. Green ⇒ the encrypted-mode serve
    path (seal→open) works end-to-end and the crypto delta was measured at ≥1
    size in both modes; scaling walls at larger sizes are recorded, not fatal."""
    sizes = _sizes()
    sample = _sample()
    budget = _budget()
    out_path = os.environ.get("FAUNA_BENCH_OUT")

    payload = {"sizes": sizes, "sample": sample, "budget_s": budget}

    pt = _run_mode(bench_mda_plaintext, sizes, sample, budget)
    payload["plaintext"] = pt
    _write_json(out_path, payload)  # persist after the plaintext arm

    enc = _run_mode(bench_mda_encrypted, sizes, sample, budget)
    payload["encrypted"] = enc

    # ── Assemble a human-readable results block. ──
    lines = ["", "=" * 82,
             "PHASE-3 IMAP FETCH-LATENCY BENCHMARK (µs per BODY.PEEK[] FETCH)",
             f"sizes={sizes} sample={sample} append_budget={budget}s "
             f"typical={pt['typical_body_len']}B large={pt.get('large_body_len')}B",
             "nest=RELEASE (bench_nest_binary); round-trip representative; crypto DELTA = "
             "bridge release-FFI open (nest serves verbatim common-mode).",
             "-" * 82,
             f"{'scope':>16} | {'pt.p50':>8} {'pt.p95':>8} | "
             f"{'enc.p50':>8} {'enc.p95':>8} | {'Δp50':>8} {'Δp95':>8}",
             "-" * 82]
    rows = []
    seen = set()
    for target in sizes:
        p, e = pt["by_size"].get(target), enc["by_size"].get(target)
        pr = (p or {}).get("reached")
        er = (e or {}).get("reached")
        scope = f"N={target}"
        if pr is not None and pr != target:
            scope = f"N~{pr}(→{target})"
        for phase in ("cold", "warm"):
            line, r = _row(scope, phase, p, e)
            lines.append(line)
            rows.append(r)
        seen.add(target)
    for phase in ("cold", "warm"):
        line, r = _row("200KiB", phase, pt.get("large"), enc.get("large"))
        lines.append(line)
        rows.append(r)
    lines += ["=" * 82,
              "Δ = encrypted − plaintext = the per-message OpenMailRecord serve cost.",
              "(b) size scaling: compare plaintext p50 across N (crypto-independent).",
              "reached<target or ERR ⇒ append-budget/scaling wall (see caveats).",
              "=" * 82]
    report = "\n".join(lines)
    print(report)

    payload["rows"] = rows
    _write_json(out_path, payload)
    if out_path:
        print(f"[bench] wrote JSON results to {out_path}")

    # Green condition: the crypto delta was measured (a completed pass) at the
    # SMALLEST target in BOTH modes — that pass also asserted seal→open
    # correctness inline. Larger-tier walls are recorded, not gated.
    smallest = min(sizes)
    pt0, enc0 = pt["by_size"].get(smallest), enc["by_size"].get(smallest)
    assert pt0 and "error" not in pt0 and pt0.get("warm_p50"), (
        f"plaintext must complete a clean pass at the smallest target {smallest}; got {pt0}")
    assert enc0 and "error" not in enc0 and enc0.get("warm_p50"), (
        f"encrypted must complete a clean pass at the smallest target {smallest}; got {enc0}")
