#!/usr/bin/env python3
"""Two devices behind two NATs: does our build ever leave the relayed path?

The measurement behind `docs/goal/behavior/p2p.md` § NAT hole punching. It
stands up, on throwaway docker networks:

    peer A (lan-a) ── gateway A ──┐
                                  ├── wan ── relay (`spawn_relay`)
    peer B (lan-b) ── gateway B ──┘

With `--discovery on` (the default, the shape that ships) the relay also
serves the substrate's QUIC address discovery on its default UDP port
(`RELAY_QAD_PORT`) on the WAN side, reachable from both gateways' outside
addresses, as the production relay does (`p2p.md` § The relay → *Address
discovery*); `--discovery off` serves the relay protocol only, the shape that
shipped before. Each gateway
counts the UDP its LAN sends to that port, so a run says whether the
endpoints asked at all, and the UDP crossing straight between the two outside
addresses — sent, let in, and dropped for want of a mapping — so it says
whether a hole punch was tried and where it died (`COUNTERS` lines).

Each gateway translates its LAN onto its own WAN address with the host's own
`nft` (`masquerade` for a gateway that keeps one outside port per inside
socket; `masquerade fully-random` for one that picks a new port per
destination — symmetric). The WAN network is `--internal`: nothing on it
routes to either LAN, so the only way between the peers is through both
gateways, as on the internet. Peer B dials peer A with no direct candidate
(what `compose_facts` publishes today) and streams for `--secs`; the probe
(`bins/fauna-iroh-relay/examples/nat_probe.rs`) prints every change of
`PeerConn::path()`.

Every working process runs a binary from the host's own `/usr` (mounted
read-only, run through the host's loader) — the probe, `nft`, `ip` — so the
container image only supplies the namespaces and an idle `sleep`: the runtime
base image the root `Dockerfile` already pins. Gateways and peers get
`NET_ADMIN` inside their own network namespace only; nothing touches the
host's network.

Exit status grades the run against `--expect` per mode, so the script is the
regression test the goal doc cites: a build that starts hole punching (or
stops) fails it and owes the doc a correction.

    just p2p-nat-probe                    # build the probe, run every mode
    uv run --no-project python scripts/p2p-nat-probe.py --mode cone --expect direct
    uv run --no-project python scripts/p2p-nat-probe.py --discovery off --expect relay
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# The root Dockerfile's runtime base (`FROM debian:bookworm-slim@…`).
# One literal: the secret scan reads a `sha256:`-introduced 64-hex run as a content digest only when the prefix and the digest share a string.
IMAGE = "debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"
PREFIX = "fnp"
WAN, LAN_A, LAN_B = "10.231.0", "10.231.1", "10.231.2"
RELAY_IP = f"{WAN}.10"
RELAY_PORT = 3340
# `fauna_iroh_relay::DISCOVERY_PORT` — where `nat_probe relay --discovery`
# binds the address-discovery server, as the production relay does.
RELAY_QAD_PORT = 7842
NAT_MODES = ("cone", "symmetric", "mixed")
SEED_A, SEED_B = 0xA1, 0xB2
# The host's dynamic loader, by architecture — the host's /usr is what runs.
LOADERS = {
    "aarch64": "ld-linux-aarch64.so.1",
    "x86_64": "ld-linux-x86-64.so.2",
}
MODES = {
    "cone": "masquerade",
    "symmetric": "masquerade fully-random",
    # Control: the gateways route without translating, and every box knows the
    # way to both LANs — each peer's own address is reachable, so a direct path
    # MUST form. Proves the testbed carries direct UDP and the probe sees it.
    "routed": "no translation",
}
GATEWAY_WAN = {"a": f"{WAN}.2", "b": f"{WAN}.3"}


def docker(*args: str, check: bool = True, capture: bool = True) -> str:
    out = subprocess.run(
        ["docker", *args], check=False, capture_output=capture, text=True
    )
    if check and out.returncode != 0:
        raise SystemExit(f"docker {' '.join(args)} failed:\n{out.stderr}")
    return out.stdout if capture else ""


def host_cmd(binary: str, *args: str) -> list[str]:
    """Run a host binary inside a container through the host's own loader."""
    arch = os.uname().machine
    libdir = f"/hostusr/lib/{arch}-linux-gnu"
    loader = f"{libdir}/{LOADERS[arch]}"
    return [loader, "--library-path", libdir, binary, *args]


def teardown() -> None:
    names = docker("ps", "-aq", "--filter", f"name=^{PREFIX}-", check=False).split()
    if names:
        docker("rm", "-f", *names, check=False)
    for net in ("wan", "lan-a", "lan-b"):
        docker("network", "rm", f"{PREFIX}-{net}", check=False)


def run_box(name: str, net: str, ip: str, mounts: list[str], admin: bool) -> None:
    args = [
        "run", "-d", "--name", f"{PREFIX}-{name}", "--network", f"{PREFIX}-{net}",
        "--ip", ip, "--user", "0:0", "-v", "/usr:/hostusr:ro",
        # A device has its platform's trust roots; the endpoint loads them at bind.
        "-v", "/etc/ssl/certs:/etc/ssl/certs:ro",
    ]
    for m in mounts:
        args += ["-v", m]
    if admin:
        args += ["--cap-add", "NET_ADMIN", "--sysctl", "net.ipv4.ip_forward=1"]
    args += ["--entrypoint", "/bin/sleep", IMAGE, "infinity"]
    docker(*args)


def exec_in(name: str, *cmd: str, detach: bool = False) -> str:
    flags = ["-d"] if detach else []
    return docker("exec", *flags, f"{PREFIX}-{name}", *cmd)


def setup(mode_a: str, mode_b: str, discovery: bool, probe: Path, out: Path) -> None:
    docker("network", "create", "--internal", "--subnet", f"{WAN}.0/24", f"{PREFIX}-wan")
    docker("network", "create", "--subnet", f"{LAN_A}.0/24", f"{PREFIX}-lan-a")
    docker("network", "create", "--subnet", f"{LAN_B}.0/24", f"{PREFIX}-lan-b")
    probe_mount = [f"{probe}:/probe/nat_probe:ro", f"{out}:/out"]

    lans = {"a": LAN_A, "b": LAN_B}
    routed = mode_a == "routed"
    run_box("relay", "wan", RELAY_IP, probe_mount, admin=routed)
    if routed:
        for side, lan in lans.items():
            exec_in("relay", *host_cmd("/hostusr/bin/ip", "route", "add",
                                       f"{lan}.0/24", "via", GATEWAY_WAN[side]))
    exec_in(
        "relay",
        *host_cmd("/probe/nat_probe", "relay", RELAY_IP, str(RELAY_PORT), "/out",
                  *(["--discovery"] if discovery else []), str(SEED_A), str(SEED_B)),
        detach=True,
    )

    for side, mode in (("a", mode_a), ("b", mode_b)):
        lan = lans[side]
        gw = f"gw-{side}"
        run_box(gw, f"lan-{side}", f"{lan}.2", [], admin=True)
        docker("network", "connect", "--ip", GATEWAY_WAN[side],
               f"{PREFIX}-wan", f"{PREFIX}-{gw}")
        if routed:
            other = "b" if side == "a" else "a"
            exec_in(gw, *host_cmd("/hostusr/bin/ip", "route", "add",
                                  f"{lans[other]}.0/24", "via", GATEWAY_WAN[other]))
        else:
            ruleset = (
                "add table ip nat; "
                "add chain ip nat post { type nat hook postrouting priority srcnat; }; "
                f"add rule ip nat post ip saddr {lan}.0/24 ip daddr != {lan}.0/24 "
                f"{MODES[mode]}"
            )
            exec_in(gw, *host_cmd("/hostusr/sbin/nft", ruleset))
        # Counters, read back by `gateway_counts`: what this LAN asks the relay's
        # address-discovery port; the UDP it sends straight at the other
        # gateway's outside address (a hole-punch attempt) and what of the other
        # gateway's UDP is let through to it; and what of that UDP hits this
        # gateway with no mapping to take it inside (dropped here).
        other_wan = GATEWAY_WAN["b" if side == "a" else "a"]
        exec_in(gw, *host_cmd("/hostusr/sbin/nft", (
            "add table ip probe; "
            "add chain ip probe fw { type filter hook forward priority 0; }; "
            "add chain ip probe in { type filter hook input priority 0; }; "
            f"add rule ip probe fw ip daddr {RELAY_IP} udp dport {RELAY_QAD_PORT} "
            'counter comment "qad"; '
            f'add rule ip probe fw ip daddr {other_wan} meta l4proto udp counter comment "punch_out"; '
            f'add rule ip probe fw ip saddr {other_wan} meta l4proto udp counter comment "punch_in"; '
            f'add rule ip probe in ip saddr {other_wan} meta l4proto udp counter comment "punch_unmapped"; '
            # A home router drops what the WAN sends it unasked: dropped, the
            # packet never confirms a conntrack entry that would steal the
            # tuple the inside peer's own punch is about to masquerade onto.
            f'add rule ip probe in ip saddr {WAN}.0/24 ct state new drop'
        )))
        peer = f"peer-{side}"
        run_box(peer, f"lan-{side}", f"{lan}.10", probe_mount, admin=True)
        exec_in(peer, *host_cmd("/hostusr/bin/ip", "route", "replace", "default",
                                "via", f"{lan}.2"))

    root = out / "relay-root.der"
    for _ in range(100):
        if root.exists():
            break
        time.sleep(0.1)
    else:
        raise SystemExit("relay never wrote its root cert")


def gateway_counts(side: str) -> dict[str, int]:
    """Gateway `side`'s packet counters (see `setup`), by name."""
    listing = exec_in(f"gw-{side}", *host_cmd("/hostusr/sbin/nft", "list", "table",
                                              "ip", "probe"))
    counts = {}
    for ln in listing.splitlines():
        if "counter packets" in ln and 'comment "' in ln:
            name = ln.split('comment "')[1].split('"')[0]
            counts[name] = int(ln.split("counter packets")[1].split()[0])
    return counts


def measure(mode_a: str, mode_b: str, discovery: bool, secs: int,
            probe: Path) -> tuple[str, float | None, float | None,
                                  dict[str, dict[str, int]]]:
    """One run: the dialer's final path; when it connected and when it first
    saw a direct path (seconds from the dial — the listener's clock starts at
    accept, so only the dialer's is graded — or None); each gateway's counters."""
    teardown()
    with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as tmp:
        out = Path(tmp)
        os.chmod(out, 0o777)
        try:
            setup(mode_a, mode_b, discovery, probe, out)
            url = f"https://{RELAY_IP}:{RELAY_PORT}"
            listen = host_cmd("/probe/nat_probe", "listen", str(SEED_A), url,
                              "/out/relay-root.der")
            # The accepting side reports its own view of the same connection.
            listener = subprocess.Popen(
                ["docker", "exec", f"{PREFIX}-peer-a", *listen],
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            )
            time.sleep(1)
            dial = subprocess.run(
                ["docker", "exec", f"{PREFIX}-peer-b",
                 *host_cmd("/probe/nat_probe", "dial", str(SEED_B), url,
                           "/out/relay-root.der", str(SEED_A), str(secs))],
                capture_output=True, text=True, check=False,
            )
            dial = dial.stdout + dial.stderr
            print(dial, end="")
            time.sleep(1)
            counts = {side: gateway_counts(side) for side in ("a", "b")}
            teardown()
            heard, _ = listener.communicate(timeout=30)
            for ln in heard.splitlines():
                print(f"  listener: {ln}")
            final = next(
                (ln.split("kind=")[1].split()[0] for ln in dial.splitlines()
                 if ln.startswith("FINAL")),
                "none",
            )
            direct_at = [float(ln.split("t=")[1].split()[0])
                         for ln in dial.splitlines()
                         if ln.startswith("PATH")
                         and ln.split("kind=")[1].split()[0] != "relay"]
            connected = next((float(ln.split("t=")[1].split()[0])
                              for ln in dial.splitlines()
                              if ln.startswith("CONNECTED")), None)
            return final, connected, min(direct_at, default=None), counts
        finally:
            teardown()


def parse_expect(text: str | None) -> dict[str, str]:
    """`relay`/`direct` grades every NAT mode alike; `cone=direct,symmetric=relay`
    grades per mode (an unnamed mode is not graded)."""
    if not text:
        return {}
    if "=" not in text:
        pairs = [(m, text) for m in NAT_MODES]
    else:
        pairs = [tuple(p.split("=", 1)) for p in text.split(",")]
    expect = {}
    for mode, want in pairs:
        if mode not in NAT_MODES or want not in ("relay", "direct"):
            raise SystemExit(f"bad --expect entry {mode}={want} "
                             f"(modes {', '.join(NAT_MODES)}; classes relay, direct)")
        expect[mode] = want
    return expect


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--mode", choices=["routed", "cone", "symmetric", "mixed", "all"],
                    default="all")
    ap.add_argument("--secs", type=int, default=60)
    ap.add_argument("--discovery", choices=["off", "on"], default="on",
                    help="whether the relay serves QUIC address discovery "
                         "(on = the shape that ships)")
    ap.add_argument("--expect", default=None,
                    help="grade the run: `relay` or `direct` for every NAT mode, or "
                         "per mode as `cone=direct,symmetric=relay,mixed=relay` "
                         "(the routed control must always end direct)")
    target = os.environ.get("CARGO_TARGET_DIR", "target")
    ap.add_argument("--probe", type=Path,
                    default=Path(target) / "debug/examples/nat_probe")
    a = ap.parse_args()
    expect = parse_expect(a.expect)
    discovery = a.discovery == "on"
    probe = a.probe.resolve()
    if not probe.exists():
        print(f"probe binary missing: {probe} (cargo build -p fauna-iroh-relay "
              "--features relay --example nat_probe)", file=sys.stderr)
        return 2
    if subprocess.run(["docker", "image", "inspect", IMAGE], capture_output=True).returncode:
        docker("pull", IMAGE, capture=False)

    # (gateway A, gateway B): the control, then both port-keeping, both
    # symmetric, and one of each.
    pairs = {
        "routed": ("routed", "routed"),
        "cone": ("cone", "cone"),
        "symmetric": ("symmetric", "symmetric"),
        "mixed": ("cone", "symmetric"),
    }
    chosen = list(pairs) if a.mode == "all" else [a.mode]
    failed = False
    for mode in chosen:
        ga, gb = pairs[mode]
        print(f"== mode {mode}: gateway A {MODES[ga]!r}, gateway B {MODES[gb]!r}, "
              f"discovery {a.discovery}, {a.secs}s stream")
        final, connected, direct_at, counts = measure(ga, gb, discovery, a.secs, probe)
        cls = {"relay": "relay", "none": "none"}.get(final, "direct")
        first = "never" if direct_at is None else f"{direct_at:.1f}"
        conn = "never" if connected is None else f"{connected:.1f}"
        print(f"RESULT mode={mode} discovery={a.discovery} final={final} "
              f"connect_t={conn} first_direct_t={first}")
        for side in ("a", "b"):
            print(f"COUNTERS mode={mode} gateway={side} " + " ".join(
                f"{k}={v}" for k, v in sorted(counts[side].items())))
        if cls == "none":
            print(f"NO CONNECTION mode={mode}: the probe never connected")
            failed = True
        else:
            want = "direct" if mode == "routed" else expect.get(mode)
            if want and cls != want:
                print(f"MISMATCH mode={mode}: expected {want}, got {cls}")
                failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
