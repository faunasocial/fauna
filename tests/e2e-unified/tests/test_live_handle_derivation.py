"""tier_1: a live-box test derives the admin handle from the SECRET.

The defect this pins (2026-08-16, user-directed — *"this detail needs to be a
part of the test"*): every `live_box` module signed in with
``FAUNA_LIVE_SECRET_HEX`` and then demanded the *same account's handle* a second
time as ``FAUNA_LIVE_HANDLE`` / ``FAUNA_LIVE_MAIL_ADDRESS``. That value exists in
no artifact in the repo or on any dev machine — every docstring and the
`justfile` carry ``test@example.com`` as an example — so the standing "run this
against the live box" ask could not be self-served: a session had to interrupt a
human for a value the box already knows. After the 2026-08-16 example.com release
that alone blocked the ActivityPub journey, with the binary built, the secret
located and the box verified clean.

Pinned here, headlessly (no box, no binary, no driver):

  1. ``FAUNA_LIVE_HANDLE`` is an OVERRIDE, never a precondition — the derivation
     runs without it, and honours it when set (including bare-localpart form).
  2. The probe typed into the wizard is a VALID handle on the box's own host —
     an invalid one would red at ``FormatInvalid`` and never reach the silent
     challenge that does the real work.
  3. The derivation returns the NEST's answer, and the localpart it yields is
     the AP username (handle-verbatim — `activitypub.md` § Username derivation).
  4. A handle-less account is REFUSED, loudly, rather than silently deriving the
     probe as if it were real — the one way this mechanism could fabricate a
     handle and then assert against it.
  5. ``test_activitypub_live.py`` no longer gates on a handle variable — the
     regression that would silently re-impose the out-of-band knowledge. This is
     the red-verify's headless twin: on the pre-change module this assertion
     fails, because ``FAUNA_LIVE_HANDLE`` was in its skipif.

Port/host handling mirrors `fauna_core::resolve::qualify_handle`
(`libs/fauna-core/src/resolve.rs:371`), the shipped qualifier this is the Python
twin of.
"""

import ast
import re
from pathlib import Path

import pytest

from helpers.live_box_door import marks_live_box
from helpers.live_handle import (
    PROBE_LOCALPART,
    derive_handle,
    handle_override,
    nest_host,
    qualify,
    sign_in_handle,
)

pytestmark = pytest.mark.tier_1

_LIVE_URL = "https://example.com"
_AP_LIVE_MODULE = Path(__file__).parent / "test_activitypub_live.py"

# The handle charset the nest registers under: 3–63 lowercase alphanumerics and
# hyphens, no edge hyphens (`nest/public-mode.md` § User Registration).
_HANDLE_RE = re.compile(r"^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])?$")


class _FakeApp:
    """The narrowest stand-in for the driver seam the derivation reads: a
    ``session.handle`` that may take a few polls to arrive, exactly as the real
    one does (it lands on the account fetch that FOLLOWS auth, not with the
    shell)."""

    def __init__(self, values):
        self._values = list(values)
        self.driver = self

    def get_state(self, key):
        assert key == "session.handle", f"unexpected state read: {key!r}"
        return self._values.pop(0) if len(self._values) > 1 else self._values[0]


@pytest.fixture(autouse=True)
def _no_ambient_override(monkeypatch):
    """The suite may run on a machine where the live vars are exported; every
    test here states its own override world."""
    monkeypatch.delenv("FAUNA_LIVE_HANDLE", raising=False)
    monkeypatch.delenv("FAUNA_LIVE_MAIL_ADDRESS", raising=False)


# ── 1. the override is an override ─────────────────────────────────────────


def test_the_derivation_runs_with_no_handle_variable_set():
    """The whole point: URL + SECRET alone must be enough."""
    assert handle_override() == ""
    app = _FakeApp(["admin"])
    assert derive_handle(app, _LIVE_URL) == ("admin@example.com", "admin")


@pytest.mark.parametrize("var", ["FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS"])
def test_an_explicit_handle_still_wins_over_the_box(monkeypatch, var):
    monkeypatch.setenv(var, "someone@example.com")
    # A driver that would answer differently — the override must not consult it.
    app = _FakeApp(["derived-instead"])
    assert derive_handle(app, _LIVE_URL) == ("someone@example.com", "someone")
    assert sign_in_handle(_LIVE_URL) == "someone@example.com"


def test_a_bare_override_is_qualified_with_the_box_host(monkeypatch):
    monkeypatch.setenv("FAUNA_LIVE_HANDLE", "someone")
    app = _FakeApp([""])
    assert derive_handle(app, _LIVE_URL) == ("someone@example.com", "someone")


def test_FAUNA_LIVE_HANDLE_wins_over_the_mail_address_alias(monkeypatch):
    monkeypatch.setenv("FAUNA_LIVE_HANDLE", "chosen@example.com")
    monkeypatch.setenv("FAUNA_LIVE_MAIL_ADDRESS", "fallback@example.com")
    assert handle_override() == "chosen@example.com"


# ── 2. what gets typed into the wizard ─────────────────────────────────────


def test_the_probe_is_a_valid_handle_on_the_boxs_own_host():
    """An invalid probe would red the check at `FormatInvalid` and never reach
    the silent challenge, so the derivation would never happen at all."""
    assert _HANDLE_RE.match(PROBE_LOCALPART), (
        f"the probe localpart {PROBE_LOCALPART!r} is outside the handle charset"
    )
    assert sign_in_handle(_LIVE_URL) == f"{PROBE_LOCALPART}@example.com"


@pytest.mark.parametrize(
    ("url", "host"),
    [
        ("https://example.com", "example.com"),
        ("https://example.com/", "example.com"),
        ("http://localhost:3000", "localhost:3000"),
        ("https://box.example.test:8443/api", "box.example.test:8443"),
    ],
)
def test_the_handle_domain_is_the_nests_host_including_a_nondefault_port(url, host):
    """`qualify_handle`'s node-address half: a non-default port is part of the
    handle domain, so a local box qualifies as `me@localhost:3000`."""
    assert nest_host(url) == host
    assert qualify("me", url) == f"me@{host}"


def test_an_already_qualified_handle_passes_through_untouched():
    assert qualify("me@elsewhere.test", _LIVE_URL) == "me@elsewhere.test"


# ── 3. the derived value is the nest's, and it is the AP username ──────────


def test_the_derivation_waits_for_the_account_fetch_rather_than_the_shell():
    """`session.handle` arrives on the account fetch that FOLLOWS auth, so an
    empty first read is normal, not a failure."""
    app = _FakeApp(["", "", "admin"])
    assert derive_handle(app, _LIVE_URL, timeout=30.0) == ("admin@example.com", "admin")


def test_the_localpart_is_the_activitypub_username():
    """`activitypub.md` § Username derivation: the username mints from the
    Fauna handle VERBATIM, and the nest stores bare localparts — so the
    derivation's second return value is what `preferredUsername` must equal."""
    app = _FakeApp(["admin"])
    _handle, username = derive_handle(app, _LIVE_URL)
    assert username == "admin"


def test_a_nest_returned_full_handle_is_not_double_qualified():
    app = _FakeApp(["admin@example.com"])
    assert derive_handle(app, _LIVE_URL) == ("admin@example.com", "admin")


# ── 4. it refuses rather than fabricates ───────────────────────────────────
#
# The two refusal tests pass a deliberately SHORT `timeout=3.0` rather than a
# `helpers/budgets.py` constant (the sleep-ratchet's advisory hint): here the
# value never arrives by construction — a fake driver returns a fixed answer —
# so the budget bounds nothing real and a generous one would only make a tier_1
# test slow. The budget that matters is the production default, exercised by the
# green paths above.


def test_a_handle_less_account_is_refused_not_derived_as_the_probe():
    """The one way this mechanism could invent a handle: the onboarding machine
    substitutes the TYPED handle when the registered one is empty
    (`machine.rs` ``current_handle.unwrap_or(typed_handle)``), so a handle-less
    account would hand the probe back looking exactly like a real answer."""
    app = _FakeApp([PROBE_LOCALPART])
    with pytest.raises(AssertionError, match="handle-less"):
        derive_handle(app, _LIVE_URL, timeout=3.0)


def test_a_handle_that_never_arrives_fails_with_a_diagnosis_not_a_hang():
    app = _FakeApp([""])
    with pytest.raises(AssertionError, match="could not derive the admin handle"):
        derive_handle(app, _LIVE_URL, timeout=3.0)


# ── 5. the live module does not re-impose the out-of-band knowledge ────────


def test_the_live_ap_module_does_not_gate_on_a_handle_variable():
    """The regression guard, and the headless twin of the red-verify: the
    pre-change module named FAUNA_LIVE_HANDLE in its skipif, so this fails
    against it and passes only once the handle is derived.

    Scoped to the module's own gate (the `skipif` call, and any `_REQUIRED*`
    tuple), not the whole file — the docstring and the claim-fresh refusal
    legitimately mention the variable as an override. The gate has demanded no
    env tuple since its seed became per-box resolved (2026-10-05): it is
    `URL and SECRET`, and `test_live_seed_resolution.py` pins the seed half.
    """
    tree = ast.parse(_AP_LIVE_MODULE.read_text())

    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        if not any(getattr(t, "id", "").startswith("_REQUIRED") for t in node.targets):
            continue
        names = {e.value for e in ast.walk(node.value) if isinstance(e, ast.Constant)}
        assert not names & {"FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS"}, (
            f"the live AP test's required env set drifted to {names!r}; the handle "
            "must stay derived, not demanded"
        )

    markers = next(
        (
            node.value
            for node in tree.body
            if isinstance(node, ast.Assign)
            and any(getattr(t, "id", None) == "pytestmark" for t in node.targets)
        ),
        None,
    )
    assert markers is not None, "test_activitypub_live.py lost its pytestmark"
    gate = ast.dump(markers)
    for var in ("FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS"):
        assert var not in gate, (
            f"{var} is back in the live AP test's skip gate — that is the exact "
            "defect this pins: the run would again be blocked on a value no "
            "artifact records"
        )
    # A gate can also demand the handle behind a helper name, where the variable
    # itself never appears in the AST (the pre-change module's own shape:
    # `... or not _handle_present()`). Pin the call graph too, so removing the
    # variable from the *reason string* alone cannot green this.
    called = {
        node.func.id
        for node in ast.walk(markers)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
    }
    assert "_handle_present" not in called, (
        "the live AP test's skip gate calls `_handle_present()` again — the "
        "handle is derived from the secret, so no gate may require it"
    )


# ── 6. one rule across the suite, declared rather than rediscovered ────────

# Every module that keys on a handle env var, and whether its handle CAN be
# derived from the secret. Derivable == the module signs into an identity the
# box already has registered, which is the only state a silent challenge can
# answer for. `False` needs a reason, and the reason is the value here — so a
# new `live_box` module cannot quietly re-impose the out-of-band knowledge by
# simply not being listed.
_HANDLE_ENV_MODULES = {
    # Derivable — signs in as the box's existing admin.
    "tests/test_activitypub_live.py": True,
    # Derivable on a claimed box (it signs in); on an UNCLAIMED box the claim
    # needs an explicit override, which `helpers.live_admin.reach_admin_shell`
    # enforces at the claim page — the probe can never claim.
    "tests/test_live_box_bootstrap.py": True,
    "tests/live/test_private_relay_hetzner.py": True,
    # Derivable at the point that matters: its handle's ONE consumer is
    # `ob.fill_handle(...)`, so the nest replaces the localpart at sign-in and
    # only the domain (from `nest_url()`) is load-bearing. It never claims —
    # it fails fast on an unclaimed box — so the probe cannot register anything.
    "tests/test_filesync_multiseat_live.py": True,
    # NOT derivable — factory-resets, then RE-CLAIMS. After the reset the
    # identity is unregistered, so the handle is one the test chooses, not one
    # the box can report. Deriving pre-reset would work and is strictly better
    # (a wrong env value silently RENAMES the account at re-claim today), but it
    # changes a destructive live test, and per this row's own gotcha that may
    # only be proven against a throwaway box — which does not exist yet
    # (`testing.md` § Implementation status → Gap 3, "Not yet done"). On a
    # staging box the handle comes from its staging-box file (the identity it
    # was provisioned under, `live_box_door.mailbox`), so nothing is exported;
    # the var stays an override.
    "tests/test_mail_enable_live_nest.py": False,
    "tests/test_mail_zero_cheat_live.py": False,
    "tests/test_caldav_live_nest.py": False,
    "tests/test_caldav_autoschedule_live_nest.py": False,
    "tests/live/test_dns01_cert_renewal_hetzner.py": False,
    # NOT derivable — never signs into the app at all (no driver, which is
    # exactly the property track 14's CD gate depends on: it is the one
    # `cd_suite` member the toolchain-light runner can execute). There is no
    # session to read a handle off, and the address is paired with a password
    # that is out-of-band regardless.
    "tests/test_mail_port25_inbound_live.py": False,
}


def test_every_handle_env_consumer_is_classified():
    """A new module keying on the handle vars must declare itself derivable or
    say why not — the ratchet that keeps "derive, don't demand" a suite rule
    rather than one test's local fix."""
    root = Path(__file__).parent.parent
    found = set()
    for path in sorted(root.rglob("test_*.py")):
        text = path.read_text()
        # Only `live_box` modules: a tier_1 pin naming the vars (this file,
        # `test_live_seed_resolution.py`) is not a consumer of them.
        if not marks_live_box(ast.parse(text)):
            continue
        if "FAUNA_LIVE_HANDLE" in text or "FAUNA_LIVE_MAIL_ADDRESS" in text:
            found.add(path.relative_to(root).as_posix())

    undeclared = found - set(_HANDLE_ENV_MODULES)
    assert not undeclared, (
        f"these modules key on a handle env var but are not classified: "
        f"{sorted(undeclared)}. Declare each in `_HANDLE_ENV_MODULES` — True if "
        "it signs in as an identity the box already has (then use "
        "`helpers.live_handle.derive_handle` instead of demanding the var), "
        "False plus the reason it cannot."
    )
    stale = set(_HANDLE_ENV_MODULES) - found
    assert not stale, (
        f"classified modules that no longer mention a handle env var: "
        f"{sorted(stale)} — drop them from `_HANDLE_ENV_MODULES`"
    )


@pytest.mark.parametrize(
    "rel", sorted(m for m, derivable in _HANDLE_ENV_MODULES.items() if derivable)
)
def test_a_derivable_module_does_not_require_the_handle_var(rel):
    """Declared derivable ⇒ the var is an override, so it must not appear in the
    module's required-env tuple. This is what makes the classification load-
    bearing rather than a comment."""
    tree = ast.parse((Path(__file__).parent.parent / rel).read_text())
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        names = {getattr(t, "id", "") for t in node.targets}
        if not any(n.startswith("_REQUIRED") or n.endswith("_REQUIRED") for n in names):
            continue
        required = {e.value for e in node.value.elts if isinstance(e, ast.Constant)}
        assert not required & {"FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS"}, (
            f"{rel} declares its handle derivable but still REQUIRES it via "
            f"{sorted(names)}: {sorted(required)}"
        )
