"""The typed-code consent given app-less: the approving account's own WS-RPC
session types the code, and the card is answered — exactly what a Fauna app's
*Connect an app* entry does (``docs/goal/behavior/authorization-server.md``
§ Consent → the typed code).

Two shapes, one per subject:

- **the nest's half** (``approve_typed_code``) — the bare ``resolve_consent``
  the API suite's consent-start tests drive (``tests/api/
  test_oauth_consent_starts.py``), where what is under test is the issuer's
  typed-code door and a loopback client asks for nothing a grant carries;
- **the production approving path** (``answer_typed_code_as``) — the code
  claimed on the user's session, then the consent machine's shared
  ``answer_consent`` through the e2e harness library
  (``fauna_ffi.harness_answer_consent``): the authoring delegation provisioned
  and every consent-time grant (the ``identity.op`` classes among them) minted,
  recorded and deposited before the resolve, as every app's approve does. The
  cli journeys (``tests/test_cli_mcp.py``) take this one: there the approving
  app is a helper seat that only seeds the grant, and a cli witness launches no
  app driver, so § The marker attributes it ``nest``
  (``docs/goal/architecture/feature-catalog.md`` § The two surfaces). What a
  person sees on the card is witnessed per app through the app's UI
  (``tests/test_connected_apps.py``).
"""

from clients.ws_rpc_admin_client import WsRpcAdminClient


def user_session(nest: dict, user: dict) -> WsRpcAdminClient:
    """``user``'s own authenticated WS-RPC connection to ``nest`` — the
    connection a Fauna app signed in as ``user`` holds. Use it as a context
    manager."""
    return WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def pending_consents(ws: WsRpcAdminClient, client_id: str) -> list[dict]:
    """The account's pending cards from ``client_id`` — its requests tray."""
    rows = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
    return [row for row in rows if row["client_id"] == client_id]


def lookup_typed_code(ws: WsRpcAdminClient, user_code: str) -> dict:
    """Type ``user_code``: claim its request onto this account's card list and
    return the card."""
    looked_up = ws.call("fauna.oauth.consent.lookup_code", {"user_code": user_code})
    consent = looked_up.get("consent")
    assert consent, f"the typed code {user_code!r} found no request: {looked_up!r}"
    return consent


def approve_typed_code(ws: WsRpcAdminClient, user_code: str) -> dict:
    """The nest's half: type ``user_code`` and resolve its card with the bare
    ``resolve_consent`` — no grant minted; the card, once resolved."""
    consent = lookup_typed_code(ws, user_code)
    resolved = ws.call(
        "fauna.bridges.atproto.resolve_consent",
        {"consent_id": consent["consent_id"], "approved": True},
    )
    assert resolved["resolved"], f"the looked-up request did not resolve: {resolved!r}"
    return consent


def answer_typed_code_as(nest: dict, user: dict, user_code: str) -> tuple[dict, dict]:
    """The production approving path, app-less: ``user`` types ``user_code`` on
    a session of their own, then approves the card through the shared
    ``answer_consent``. Returns the card and the answer — ``{"resolved":
    bool}`` (``False`` when the nest resolves nothing, a member approving an
    admin string) or ``{"refused": "<reason>"}``, the card's error line."""
    import fauna_ffi

    with user_session(nest, user) as ws:
        consent = lookup_typed_code(ws, user_code)
    answer = fauna_ffi.harness_answer_consent(
        nest["url"], bytes(user["signing_key"]), bytes(consent["consent_id"])
    )
    return consent, answer


def approve_typed_code_as(nest: dict, user: dict, user_code: str) -> dict:
    """``answer_typed_code_as``, which must resolve; the card."""
    consent, answer = answer_typed_code_as(nest, user, user_code)
    assert answer == {"resolved": True}, (
        f"the approve minted and resolved the card {consent!r}: {answer!r}"
    )
    return consent
