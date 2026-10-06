"""E2E test: Alice sends two emails to Bob over SMTP; Bob sorts them.

Both users share a single node. A SPA-aware static server serves the web app.
Bob's inbox is set to "open" mode via direct DB update so messages are delivered.
"""

import http.server
import os
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request

from common import CLAIM_CODE, get_repo_root, register_user, wait_for_node
from common.auth import claim_admin
from drivers.port_util import popen_group_kwargs, reap_descendants_of
from playwright.sync_api import sync_playwright, expect

import pytest

# Web-client test (drives the SPA via Playwright); `web` marker scopes it to
# --client web so a non-web --include-independent run doesn't run it.
pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def find_free_port() -> int:
    with socket.socket() as s:
        s.bind(("", 0))
        return s.getsockname()[1]


class SPAHandler(http.server.SimpleHTTPRequestHandler):
    """Serve static files; fall back to index.html for client-side routes."""

    def __init__(self, *args, spa_dir: str, **kwargs):
        self._spa_dir = spa_dir
        super().__init__(*args, directory=spa_dir, **kwargs)

    def do_GET(self):
        path = self.translate_path(self.path)
        if os.path.exists(path) and not os.path.isdir(path):
            return super().do_GET()
        if os.path.isdir(path) and os.path.exists(os.path.join(path, "index.html")):
            return super().do_GET()
        self.path = "/index.html"
        return super().do_GET()

    def log_message(self, format, *args):
        pass


def start_spa_server(build_dir: str, port: int) -> http.server.HTTPServer:
    handler = lambda *args, **kwargs: SPAHandler(*args, spa_dir=build_dir, **kwargs)
    server = http.server.HTTPServer(("127.0.0.1", port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/")
            return server
        except Exception:
            time.sleep(0.1)
    raise TimeoutError(f"SPA server on port {port} did not start")


def set_inbox_open(db_path: str, actor_id_hex: str):
    """Set an actor's inbox mode to 'open' directly in the SQLite DB."""
    actor_bytes = bytes.fromhex(actor_id_hex)
    conn = sqlite3.connect(db_path)
    conn.execute(
        "INSERT OR REPLACE INTO inbox_modes (actor_id, mode) VALUES (?, ?)",
        (actor_bytes, "open"),
    )
    conn.commit()
    conn.close()


def get_subjects_in_order(page) -> list[str]:
    items = page.locator('[data-testid="inbox-item"] [data-testid="subject"]')
    count = items.count()
    return [items.nth(i).inner_text() for i in range(count)]


def test_smtp_two_emails_sort(node_binary):
    """Alice sends two emails to Bob on the same node. Bob sorts by subject."""
    repo = get_repo_root()
    build_dir = str(repo / "apps" / "fauna-web" / "build")

    api_port = find_free_port()
    web_port = find_free_port()
    api_base = f"http://127.0.0.1:{api_port}"
    web_base = f"http://127.0.0.1:{web_port}"

    tmp = tempfile.mkdtemp(prefix="fauna-smtp-sort-")
    db_path = os.path.join(tmp, "nest.db")

    # Write claim code file
    claim_code_path = os.path.join(tmp, "claim-code")
    with open(claim_code_path, "w") as f:
        f.write(CLAIM_CODE)

    node_proc = subprocess.Popen(
        [node_binary, "--bind", f"127.0.0.1:{api_port}",
         "--db", db_path,
         "--cors-origin", web_base],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        **popen_group_kwargs(),
    )
    reap_descendants_of(node_proc.pid)

    spa_server = start_spa_server(build_dir, web_port)

    try:
        wait_for_node(api_port)

        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            ctx_a = browser.new_context()
            ctx_b = browser.new_context()
            alice = ctx_a.new_page()
            bob = ctx_b.new_page()

            console_logs = {"alice": [], "bob": []}
            alice.on("console", lambda msg: console_logs["alice"].append(f"[{msg.type}] {msg.text}"))
            bob.on("console", lambda msg: console_logs["bob"].append(f"[{msg.type}] {msg.text}"))

            # --- Alice: set up identity ---
            alice.goto(f"{web_base}/app/settings")
            alice.wait_for_load_state("networkidle")
            alice.reload()
            alice.wait_for_load_state("networkidle")
            alice.click("button:has-text('Generate new identity')")
            alice_code = alice.locator("code").first
            expect(alice_code).to_be_visible(timeout=5_000)
            alice_id = alice_code.inner_text()
            print(f"Alice ID: {alice_id}")

            # --- Bob: set up identity ---
            bob.goto(f"{web_base}/app/settings")
            bob.wait_for_load_state("networkidle")
            bob.reload()
            bob.wait_for_load_state("networkidle")
            bob.click("button:has-text('Generate new identity')")
            bob_code = bob.locator("code").first
            expect(bob_code).to_be_visible(timeout=5_000)
            bob_id = bob_code.inner_text()
            print(f"Bob ID:   {bob_id}")

            # Admit both actors. The client generates the keypair locally, but a
            # handshake never provisions an account any more (the auto-provision
            # branch is deleted — `public-mode.md` § User Registration), so the
            # admin admits each one, exactly as on a closed nest. Fixture setup
            # arranging a precondition, not the behavior under test.
            admin = claim_admin(api_port, CLAIM_CODE)
            for actor_hex in (alice_id, bob_id):
                register_user(api_port, actor_hex, admin_signing_key=admin["signing_key"])

            # Set Bob's inbox to "open" so Alice's messages are delivered directly
            set_inbox_open(db_path, bob_id)

            # Bob opens conversations (starts WS listener)
            bob.goto(f"{web_base}/app/conversations")
            bob.wait_for_load_state("networkidle")

            # Alice goes to conversations
            alice.goto(f"{web_base}/app/conversations")
            alice.wait_for_load_state("networkidle")

            # --- Alice sends "Zebra report" ---
            alice.fill('[data-testid="to"]', bob_id)
            alice.press('[data-testid="to"]', 'Enter')
            expect(alice.locator('.resolve-status.resolved')).to_be_visible(timeout=5_000)
            alice.fill('[data-testid="subject"]', "Zebra report")
            alice.fill('[data-testid="body"]', "Details about zebras.")
            expect(alice.locator('[data-testid="send"]')).to_be_enabled(timeout=2_000)
            alice.click('[data-testid="send"]')
            alice.wait_for_timeout(2000)

            # --- Alice sends "Alpha update" ---
            alice.fill('[data-testid="to"]', bob_id)
            alice.press('[data-testid="to"]', 'Enter')
            expect(alice.locator('.resolve-status.resolved')).to_be_visible(timeout=5_000)
            alice.fill('[data-testid="subject"]', "Alpha update")
            alice.fill('[data-testid="body"]', "Everything about alphas.")
            expect(alice.locator('[data-testid="send"]')).to_be_enabled(timeout=2_000)
            alice.click('[data-testid="send"]')
            alice.wait_for_timeout(2000)

            # --- Bob should see both messages ---
            bob_items = bob.locator('[data-testid="inbox-item"]')
            expect(bob_items).to_have_count(2, timeout=15_000)

            # --- Sort by Subject A-Z ---
            sort_select = bob.locator('[data-testid="inbox-sort"]')
            expect(sort_select).to_be_visible(timeout=5_000)
            sort_select.select_option("subject-asc")
            bob.wait_for_timeout(500)
            subjects_asc = get_subjects_in_order(bob)
            assert subjects_asc == ["Alpha update", "Zebra report"], (
                f"Expected A-Z order, got: {subjects_asc}"
            )

            # --- Sort by Subject Z-A ---
            sort_select.select_option("subject-desc")
            bob.wait_for_timeout(500)
            subjects_desc = get_subjects_in_order(bob)
            assert subjects_desc == ["Zebra report", "Alpha update"], (
                f"Expected Z-A order, got: {subjects_desc}"
            )

            # --- Sort by Date: Oldest first ---
            sort_select.select_option("date-asc")
            bob.wait_for_timeout(500)
            subjects_date_asc = get_subjects_in_order(bob)
            assert subjects_date_asc[0] == "Zebra report", (
                f"Expected oldest-first to start with 'Zebra report', got: {subjects_date_asc}"
            )

            # --- Sort by Date: Newest first ---
            sort_select.select_option("date-desc")
            bob.wait_for_timeout(500)
            subjects_date_desc = get_subjects_in_order(bob)
            assert subjects_date_desc[0] == "Alpha update", (
                f"Expected newest-first to start with 'Alpha update', got: {subjects_date_desc}"
            )

            ctx_a.close()
            ctx_b.close()
            browser.close()
    finally:
        spa_server.shutdown()
        node_proc.kill()
        node_proc.wait()
