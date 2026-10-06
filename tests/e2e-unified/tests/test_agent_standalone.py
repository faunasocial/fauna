#!/usr/bin/env python3
"""Standalone test: does the Linux test agent actually poll and push state?

Runs without pytest, bridges, or AT-SPI. Just a fake HTTP server + the app binary.
"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from http.server import HTTPServer, BaseHTTPRequestHandler

import pytest

pytestmark = pytest.mark.tier_2

command_queue = []
received_states = []
lock = threading.Lock()


class FakeBridge(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/app/commands":
            with lock:
                if command_queue:
                    cmd = command_queue.pop(0)
                    body = json.dumps(cmd).encode()
                    self.send_response(200)
                else:
                    body = b""
                    self.send_response(204)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length else b"{}"
        data = json.loads(body)
        if self.path == "/app/state":
            with lock:
                received_states.append(data)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            body = b'{"received":true}'
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *args):
        pass


def main():
    # 1. Start fake bridge
    server = HTTPServer(("127.0.0.1", 0), FakeBridge)
    port = server.server_address[1]
    bridge_url = f"http://127.0.0.1:{port}"
    print(f"Fake bridge on {bridge_url}")
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()

    # 2. Launch app with FAUNA_E2E_BRIDGE
    tmp = tempfile.mkdtemp(prefix="agent-test-")
    env = {
        **os.environ,
        "FAUNA_E2E_BRIDGE": bridge_url,
        "XDG_DATA_HOME": os.path.join(tmp, "data"),
        "XDG_CONFIG_HOME": os.path.join(tmp, "config"),
    }
    os.makedirs(env["XDG_DATA_HOME"], exist_ok=True)
    os.makedirs(env["XDG_CONFIG_HOME"], exist_ok=True)

    binary = os.environ.get("CARGO_TARGET_DIR", "target") + "/release/fauna-desktop"
    print(f"Launching {binary}")
    # This script runs standalone (no pytest, no conftest.py — see the module
    # docstring), so the usual conftest.py `sys.path.insert(0, .../e2e-unified)`
    # never runs; bootstrap it ourselves so `drivers.port_util` resolves.
    _e2e_unified_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    if _e2e_unified_dir not in sys.path:
        sys.path.insert(0, _e2e_unified_dir)
    from drivers.port_util import popen_group_kwargs, reap_descendants_of
    proc = subprocess.Popen(
        [binary], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()` is
    # `{}` there, so without this the app's only protection is a sweep this
    # standalone script never runs (testing.md § point 9). No-op off Windows.
    reap_descendants_of(proc.pid)

    # 3. Wait for initial state push
    print("Waiting for initial state push...")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        with lock:
            if received_states:
                break
        if proc.poll() is not None:
            stderr = proc.stderr.read().decode(errors="replace")
            print(f"APP CRASHED (exit {proc.returncode})")
            print(f"STDERR:\n{stderr[-2000:]}")
            server.shutdown()
            return
        time.sleep(0.2)

    with lock:
        if not received_states:
            print("FAIL: No state received after 10s")
            proc.terminate()
            proc.wait()
            server.shutdown()
            return
        initial = received_states[-1]

    print(f"OK: Got initial state. auth={initial.get('state',{}).get('session',{}).get('authenticated')}")
    print(f"    nav={initial.get('state',{}).get('nav',{}).get('stack',[{}])[0].get('view')}")

    # 4. Send a patch command
    with lock:
        received_states.clear()
        command_queue.append({
            "id": "test-cmd-1",
            "action": "patch",
            "state": {"session": {"authenticated": True}},
        })

    print("Sent patch command, waiting for ack...")
    deadline = time.monotonic() + 5
    acked = False
    while time.monotonic() < deadline:
        with lock:
            for s in received_states:
                if s.get("last_command_id") == "test-cmd-1":
                    auth = s.get("state", {}).get("session", {}).get("authenticated")
                    print(f"OK: Acked test-cmd-1. auth={auth}")
                    acked = True
                    break
        if acked:
            break
        time.sleep(0.2)

    if not acked:
        print("FAIL: Command not acknowledged after 5s")
        with lock:
            for s in received_states:
                print(f"  state push: lastCmd={s.get('last_command_id')} auth={s.get('state',{}).get('session',{}).get('authenticated')}")

    # 5. Cleanup
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    stderr = proc.stderr.read().decode(errors="replace")
    if stderr.strip():
        print(f"APP STDERR:\n{stderr[-2000:]}")
    server.shutdown()


if __name__ == "__main__":
    main()
