"""A co-resident helper that tries to lie about where a connection comes from.

Run inside a container that shares the nest's network namespace, so every
connection it opens to the nest arrives from loopback — the position a mail
bridge or any other helper process holds. For each connection accepted on
``--listen`` it dials ``--upstream``, writes a PROXY-v2 header naming
``--forge`` as the source, and then splices bytes both ways, so a test client on
the host can speak TLS / WS-RPC to the nest through it unchanged.

The header carries NO router-auth TLV: a helper cannot read the router's secret
(``test_proxy_router_auth.py`` witnesses that), so a header without it is the
best forgery a helper can write. Stdlib only — it runs in a stock python image.
"""

import argparse
import ipaddress
import socket
import struct
import threading

SIGNATURE = b"\r\n\r\n\x00\r\nQUIT\n"
VERSION_PROXY = 0x21  # version 2, command PROXY
TCP4 = 0x11


def proxy_v2_header(src: str, src_port: int, dst: str, dst_port: int) -> bytes:
    addr = (
        ipaddress.IPv4Address(src).packed
        + ipaddress.IPv4Address(dst).packed
        + struct.pack("!HH", src_port, dst_port)
    )
    return SIGNATURE + bytes([VERSION_PROXY, TCP4]) + struct.pack("!H", len(addr)) + addr


def _pump(src: socket.socket, dst: socket.socket) -> None:
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def serve(listen: tuple[str, int], upstream: tuple[str, int], forge: str) -> None:
    lsock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lsock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lsock.bind(listen)
    lsock.listen(64)
    print(f"proxy_forger listening on {listen}, forging {forge}", flush=True)
    while True:
        client, _ = lsock.accept()
        up = socket.create_connection(upstream)
        up.sendall(proxy_v2_header(forge, 40000, upstream[0], upstream[1]))
        threading.Thread(target=_pump, args=(client, up), daemon=True).start()
        threading.Thread(target=_pump, args=(up, client), daemon=True).start()


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", required=True, help="host:port")
    ap.add_argument("--upstream", required=True, help="host:port")
    ap.add_argument("--forge", required=True, help="the IPv4 source to claim")
    a = ap.parse_args()
    lh, lp = a.listen.rsplit(":", 1)
    uh, up_ = a.upstream.rsplit(":", 1)
    serve((lh, int(lp)), (uh, int(up_)), a.forge)
