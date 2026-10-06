"""E2E test: Alice sends an encrypted message that only Bob can decrypt."""

from common import port_base_url, register_user
from playwright.sync_api import sync_playwright, expect

import pytest

# Web-client test (drives the SPA via Playwright); `web` marker scopes it to
# --client web so a non-web --include-independent run doesn't run it.
pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def generate_identity(page, base_url: str) -> str:
    """Navigate to Settings, generate a new identity, return the ActorId."""
    page.goto(f"{base_url}/app/settings")
    page.wait_for_load_state("networkidle")
    page.click("button:has-text('Generate new identity')")
    code = page.locator("code").first
    expect(code).to_be_visible(timeout=5_000)
    return code.inner_text()


def test_encrypted_email_roundtrip(two_nodes):
    """Alice sends an encrypted message to Bob. Bob sees it decrypted with 'Encrypted' badge."""
    port_a = two_nodes["port_a"]
    port_b = two_nodes["port_b"]
    base_a = port_base_url(port_a)
    base_b = port_base_url(port_b)

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)

        alice_console = []
        bob_console = []

        ctx_a = browser.new_context(base_url=base_a)
        ctx_b = browser.new_context(base_url=base_b)
        alice = ctx_a.new_page()
        bob = ctx_b.new_page()
        alice.on("console", lambda msg: alice_console.append(f"[{msg.type}] {msg.text}"))
        bob.on("console", lambda msg: bob_console.append(f"[{msg.type}] {msg.text}"))

        # Both generate identities
        alice_id = generate_identity(alice, base_a)
        bob_id = generate_identity(bob, base_b)
        print(f"Alice ID: {alice_id}")
        print(f"Bob ID:   {bob_id}")

        # Register users on their home nodes so they can get bearer tokens
        register_user(port_a, alice_id, admin_signing_key=two_nodes["admin_sk_a"])
        register_user(port_b, bob_id, admin_signing_key=two_nodes["admin_sk_b"])

        # Bob opens conversations (starts WebSocket listener)
        bob.goto(f"{base_b}/app/conversations")
        bob.wait_for_load_state("networkidle")

        # Alice opens conversations and composes an encrypted message
        alice.goto(f"{base_a}/app/conversations")
        alice.wait_for_load_state("networkidle")

        alice.fill('[data-testid="to"]', bob_id)
        alice.fill('[data-testid="subject"]', "Secret from Alice")
        alice.fill('[data-testid="body"]', "This message is encrypted.")

        # Set recipient node URL to Bob's node
        node_url_input = alice.locator('input[placeholder="Recipient node URL"]')
        node_url_input.clear()
        node_url_input.fill(base_b)

        # Enable encryption
        encrypt_toggle = alice.locator('[data-testid="encrypt-toggle"]')
        encrypt_toggle.check()

        # Send
        alice.click('[data-testid="send"]')

        # Verify no send errors
        alice.wait_for_timeout(2000)
        error_el = alice.locator(".error")
        if error_el.count() > 0:
            error_text = error_el.first.inner_text()
            print(f"Send error: {error_text}")
            print(f"Alice console: {alice_console}")

        # Bob should receive the message
        inbox_item = bob.locator('[data-testid="inbox-item"]').first
        expect(inbox_item).to_be_visible(timeout=15_000)

        # Verify subject is visible
        subject = bob.locator(
            '[data-testid="inbox-item"] [data-testid="subject"]'
        ).first
        expect(subject).to_contain_text("Secret from Alice")

        # Verify body was decrypted correctly
        body = bob.locator(
            '[data-testid="inbox-item"] [data-testid="msg-body"]'
        ).first
        expect(body).to_contain_text("This message is encrypted.")

        # Verify the "Encrypted" badge is shown
        encrypted_badge = bob.locator(
            '[data-testid="inbox-item"] [data-testid="encrypted-badge"]'
        ).first
        expect(encrypted_badge).to_be_visible()

        # Verify the "Signed" badge is also shown (encrypted emails are also signed)
        signed_badge = bob.locator(
            '[data-testid="inbox-item"] [data-testid="signed-badge"]'
        ).first
        expect(signed_badge).to_be_visible()

        # Verify sender matches Alice
        sender = bob.locator(
            '[data-testid="inbox-item"] [data-testid="sender"]'
        ).first
        expect(sender).to_contain_text(alice_id[:16])

        print(f"Alice console: {alice_console}")
        print(f"Bob console: {bob_console}")

        ctx_a.close()
        ctx_b.close()
        browser.close()
