"""Nostr — Zap signers (the NIP-57 trust root the payee designates).

The *Zap signers* section of the standalone Nostr page (docs/goal/behavior/
monetization.md § Zap receipts — the trust model, ratified 2026-07-29; the page
slot is docs/goal/ui/nostr.md § Layout & flow item 7).

Why the control exists at all: a kind-9735 zap receipt is signed by the
*recipient's* LNURL/wallet server — not the sender's, and not any key Fauna
knows a priori — and anyone may mint one naming any recipient. **A zap
receipt's own valid signature therefore proves nothing and must buy it
nothing.** The trust root is instead a payee-designated signer list, and
"which wallet provider do I trust to speak for my money" is a genuine user
choice, so it is app UI + nest state per § Product invariants — never a config
file. A payee who has designated nobody believes nobody: that is the ratified
out-of-the-box default (no zap is silently believed), which is why the empty
roster is a *stated* state (`nostr-zap-signer-empty`) rather than a blank list.

tier_3 (full stack — a real `fauna-nest` binary): the designation is nest state
(`nostr_zap_signers`) behind the User-class, caller-scoped
`fauna.nostr.zap_signers.{list,add,remove}` kinds, and the normalize-on-write
rule below lives in the handler. A mocked backend cannot catch drift between
the shared `NostrZapSignerClient` and those handlers.

tui is the lead app for new UI, by standing project ordering; the other six
follow in the usual batched trickle-down, and their markers are added as each
leg lands (web's UI landed 2026-08-28; its marker followed in web's catalog
trickle-down pass).

Covers (nostr.md § Layout & flow item 7 / § Element IDs):
  * the section renders for a linked account, and the empty roster states that
    nothing is believed rather than rendering a bare list;
  * designate a signer (`nostr-zap-signer-add-btn`) → a roster row appears
    (`nostr-zap-signer-item`);
  * **the rendered row is the STORED row, not the typed input** — the nest
    normalizes to lowercase on write and only that form ever matches a receipt,
    so a client echoing the user's uppercase paste would show a correct-looking
    roster that believes nothing;
  * undesignate (`nostr-zap-signer-remove`) → the roster empties and the
    "nothing is believed" state returns. Removal is de-escalation and is
    deliberately ungated (dynamic-features.md § per-surface composition, ruling
    (i)): a tier that can only tighten must never trap a user in a
    configuration they can no longer undo.
"""
import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.web,
]

# A syntactically valid 64-hex signer pubkey, typed UPPERCASE on purpose: the
# nest lowercases on write, so this is what proves the row is rendered from the
# reply rather than echoed from the input.
SIGNER_UPPER = "AB" * 32
SIGNER_STORED = SIGNER_UPPER.lower()


@pytest.mark.feature("nostr")
def test_zap_signer_section_renders_with_a_stated_empty_state(logged_in_app):
    """The section renders for a linked account, and an empty roster SAYS that
    no zap receipt is believed instead of rendering a bare list."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_zap_signer_section(), (
        f"Zap signers section should render for a linked account: "
        f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
    )
    # Known-empty roster start (the session nest is shared).
    app.nostr.clear_zap_signers()
    assert app.nostr.wait_for_zap_signer_count(0), (
        f"roster should start empty. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_zap_signer_empty_state(True), (
        f"an empty roster must state that nothing is believed, not render a "
        f"blank list: {app.driver.diagnose('nostr-zap-signer-empty')}"
    )


@pytest.mark.feature("nostr")
def test_designate_and_undesignate_round_trip(logged_in_app):
    """Designate a signer → a roster row appears carrying the STORED (lowercased)
    pubkey → undesignate → the roster empties and the stated empty state
    returns. Exercises `fauna.nostr.zap_signers.{list,add,remove}` end-to-end."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_zap_signer_section(), (
        f"Zap signers section should render: "
        f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
    )
    app.nostr.clear_zap_signers()
    assert app.nostr.wait_for_zap_signer_count(0), "roster should start empty"

    app.nostr.add_zap_signer(SIGNER_UPPER, label="my wallet provider")
    assert app.nostr.wait_for_zap_signer_count(1), (
        f"designating a signer should create one roster row. "
        f"error: {app.nostr.page_error_text()!r}"
    )
    # The empty state is gone precisely because something IS believed now.
    assert app.nostr.wait_for_zap_signer_empty_state(False), (
        "the 'nothing is believed' line must not survive a designation"
    )
    # The row renders the reply's stored form. The nest lowercases on write and
    # only that form ever matches a receipt, so echoing the typed input here
    # would show a correct-looking roster that believes nothing.
    row = app.nostr.zap_signer_texts()[0]
    assert SIGNER_UPPER not in row, (
        f"the row must render the STORED pubkey, not the typed input; the "
        f"uppercase paste leaked through: {row!r}"
    )
    assert SIGNER_STORED[:8] in row.lower(), (
        f"the row should identify the designated signer, got {row!r}"
    )

    app.nostr.remove_zap_signer(0)
    assert app.nostr.wait_for_zap_signer_count(0), (
        f"undesignating should empty the roster. "
        f"error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_zap_signer_empty_state(True), (
        "emptying the roster must restore the 'nothing is believed' state"
    )
