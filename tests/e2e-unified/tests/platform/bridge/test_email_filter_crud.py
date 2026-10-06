"""E2E test: Email filter CRUD — create and delete email filters.

Alice generates an identity, registers, and navigates to the settings page.
She creates an email filter, verifies it appears in the list, then
deletes it and verifies it's gone.
"""

from common import port_base_url, register_user
from playwright.sync_api import sync_playwright, expect

import pytest

# Drives the web SPA via Playwright — a web-client test, not client-independent.
# The `web` marker is authoritative in conftest's --client deselection, so this is
# dropped under a non-web run (e.g. `--client windows --include-independent`)
# instead of running a web test inside it.
pytestmark = [pytest.mark.tier_3, pytest.mark.web]


def generate_identity(page, base_url: str) -> str:
    """Navigate to Settings, generate a new identity, return the ActorId."""
    page.goto(f"{base_url}/app/settings")
    page.wait_for_load_state("networkidle")
    page.click("button:has-text('Generate new identity')")
    code = page.locator("code").first
    expect(code).to_be_visible(timeout=5_000)
    return code.inner_text()


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_create_and_delete(two_nodes):
    """Alice creates an email filter and then deletes it.

    Verifies:
    1. Initial state shows "No email filters configured"
    2. Clicking "Add Filter" shows the form
    3. Creating a filter adds it to the list with correct name and action
    4. Creating a second filter shows both
    5. Deleting the first filter removes it, second remains
    6. Deleting the last filter returns to "No email filters" message
    """
    port_a = two_nodes["port_a"]
    base_a = port_base_url(port_a)

    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        console_logs = []

        ctx = browser.new_context(base_url=base_a)
        alice = ctx.new_page()
        alice.on("console", lambda msg: console_logs.append(f"[{msg.type}] {msg.text}"))

        try:
            # --- Step 1: Generate identity and register ---
            alice_id = generate_identity(alice, base_a)
            print(f"Alice ID: {alice_id}")

            register_user(port_a, alice_id, admin_signing_key=two_nodes["admin_sk_a"])
            alice.evaluate("localStorage.setItem('fauna_registered', 'true')")

            alice.goto(f"{base_a}/app/settings")
            alice.wait_for_load_state("networkidle")

            # --- Step 2: Verify initial empty state ---
            no_filters = alice.locator("text=No email filters configured")
            expect(no_filters).to_be_visible(timeout=10_000)
            print("No filters initially")

            # --- Step 3: Open the filter form ---
            alice.click('[data-testid="add-filter-btn"]')
            filter_name_input = alice.locator('[data-testid="filter-name-input"]')
            expect(filter_name_input).to_be_visible(timeout=5_000)
            print("Filter form opened")

            # --- Step 4: Create first filter (block spam sender) ---
            alice.fill('[data-testid="filter-name-input"]', "Block spam sender")
            alice.locator('[data-testid="filter-rule-type"]').select_option("SenderIs")
            alice.fill('[data-testid="filter-rule-value"]', "spammer@evil.com")
            alice.locator('[data-testid="filter-action-select"]').select_option("Discard")
            alice.click('[data-testid="create-filter"]')

            # Wait for filter to appear in list
            filter_item = alice.locator('[data-testid="filter-item"]').first
            expect(filter_item).to_be_visible(timeout=10_000)

            filter_name = filter_item.locator('[data-testid="filter-name"]')
            expect(filter_name).to_contain_text("Block spam sender")

            filter_action = filter_item.locator('[data-testid="filter-action"]')
            expect(filter_action).to_contain_text("Discard")
            print("First filter created: Block spam sender (Discard)")

            # --- Step 5: Create second filter (allow trusted domain) ---
            alice.click('[data-testid="add-filter-btn"]')
            alice.fill('[data-testid="filter-name-input"]', "Allow trusted")
            alice.locator('[data-testid="filter-rule-type"]').select_option("SenderDomain")
            alice.fill('[data-testid="filter-rule-value"]', "trusted.com")
            alice.locator('[data-testid="filter-action-select"]').select_option("Allow")
            alice.click('[data-testid="create-filter"]')

            # Wait for two filters
            filters = alice.locator('[data-testid="filter-item"]')
            expect(filters).to_have_count(2, timeout=10_000)
            print("Two filters now exist")

            # --- Step 6: Delete the first filter ---
            first_filter = alice.locator('[data-testid="filter-item"]').first
            first_filter.locator('[data-testid="filter-delete"]').click()

            # Wait for only one filter
            expect(filters).to_have_count(1, timeout=10_000)

            # Verify the remaining filter is "Allow trusted"
            remaining = alice.locator('[data-testid="filter-item"]').first
            remaining_name = remaining.locator('[data-testid="filter-name"]')
            expect(remaining_name).to_contain_text("Allow trusted")
            print("First filter deleted, 'Allow trusted' remains")

            # --- Step 7: Delete the last filter ---
            remaining.locator('[data-testid="filter-delete"]').click()

            # Verify back to empty state
            no_filters = alice.locator("text=No email filters configured")
            expect(no_filters).to_be_visible(timeout=10_000)
            print("All filters deleted, back to empty state")

            print("Email filter CRUD: PASSED")

        except Exception:
            print(f"\n=== FAILURE DEBUG ===")
            print(f"Console ({len(console_logs)} messages):")
            for line in console_logs[-30:]:
                print(f"  {line}")
            raise
        finally:
            ctx.close()
            browser.close()
