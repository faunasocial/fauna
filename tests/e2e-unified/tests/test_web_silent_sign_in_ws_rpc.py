"""End-to-end probe: the web SPA's silent sign-in (`silentSignIn`) runs the
challenge/verify ceremony over the **anonymous WS-RPC** kinds
`fauna.auth.{challenge,verify}` — NOT the deleted HTTP twins
`POST /api/v1/auth/{challenge,verify}`.

Track C of the web auth-bootstrap migration (`transport.md` § Pre-identity
(anonymous) connection; the HTTP twins were deleted in the WS-RPC-everywhere rip-out, `transport-connection.md` § Pre-identity).
The web twin of Track B's `mintBearer` regression (`test_web_ws_rpc_echo.py`,
which covers the `fauna.auth.handshake` mint).

The SPA exposes `window.__fauna_silentSignIn(nestUrl?)` (always-installed in
`+layout.svelte`, inert in production) that runs `silentSignIn` for the stored
identity. The test installs a `fetch` spy that records any hit to the HTTP auth
twins, runs the ceremony, and asserts (a) a verified actor came back and (b) the
spy recorded ZERO HTTP auth-twin hits — the regression that keeps the deleted
path from creeping back in.

tier_3: a real `fauna-nest` is spun by `logged_in_app` → `nest_instance`, and the
WS upgrade is proxied through to it by `spa_url`. `logged_in_app` only
`set_state`s the web session client-side, so the actor is not yet registered on
the nest; `verify` rejects unregistered actors (→ null). We first call
`__fauna_rpcEcho` (which mints a bearer via `fauna.auth.handshake`, auto-
registering the actor on the open nest) so the subsequent `verify` succeeds —
the same order a real launch follows.
"""
from __future__ import annotations

import pytest

from helpers.app_surface import declared_absence

# Web-only (the `window.__fauna_silentSignIn` hook is SPA-specific). Deselected
# under non-web `--client` by the conftest marker-platform filter; the in-body
# `is_web()` skip still guards a no-`--client` run.
pytestmark = [pytest.mark.web, pytest.mark.tier_3]


def test_web_silent_sign_in_rides_ws_rpc(logged_in_app):
    """Web's `silentSignIn` runs challenge/verify over anonymous WS-RPC and
    never touches the deleted HTTP `/auth/challenge|verify` twins."""
    driver = logged_in_app.driver
    if not driver.is_web():
        declared_absence(
            driver,
            capability="the window.__fauna_silentSignIn automation hook",
            doc="testing.md § Cross-app e2e conventions, point 15 (web's "
            "window.__fauna_* hooks are a web-only automation surface; native "
            "apps carry their own compiled-in TestAgent instead)",
        )

    # `__fauna_rpcEcho` lazily mints a bearer (`fauna.auth.handshake`), which
    # auto-registers the actor on the open nest — a prerequisite for `verify` to
    # resolve (it rejects unregistered actors with `fauna.auth.not_registered`).
    # Then install a fetch spy over the auth twins and run the ceremony.
    script = (
        "(async () => {"
        "  if (typeof window.__fauna_silentSignIn !== 'function') {"
        "    return {ok: false, error: 'window.__fauna_silentSignIn hook missing'};"
        "  }"
        "  if (typeof window.__fauna_rpcEcho === 'function') {"
        "    await window.__fauna_rpcEcho('00');"  # mint bearer -> auto-register
        "  }"
        "  const authHits = [];"
        "  const origFetch = window.fetch;"
        "  window.fetch = function (input, init) {"
        "    const u = typeof input === 'string' ? input : (input && input.url) || '';"
        "    if (u.indexOf('/api/v1/auth/challenge') >= 0 ||"
        "        u.indexOf('/api/v1/auth/verify') >= 0) authHits.push(u);"
        "    return origFetch.apply(this, arguments);"
        "  };"
        "  try {"
        "    const verified = await window.__fauna_silentSignIn();"
        "    return {ok: true, verified: verified, authHttpHits: authHits};"
        "  } catch (e) {"
        "    return {ok: false, error: String((e && e.message) || e), authHttpHits: authHits};"
        "  } finally {"
        "    window.fetch = origFetch;"
        "  }"
        "})()"
    )
    result = driver.eval_js(script)

    assert result is not None, (
        "eval_js returned nothing — the SPA didn't evaluate the silent-sign-in hook"
    )
    assert result.get("ok") is True, (
        f"web silent sign-in failed: {result.get('error')!r}"
    )
    assert result.get("verified"), (
        "silentSignIn returned null for a logged-in (registered) actor — "
        "the challenge/verify ceremony did not resolve the actor"
    )
    assert result.get("authHttpHits") == [], (
        "silentSignIn still hit the deleted HTTP auth twin "
        f"(should ride anonymous WS-RPC): {result.get('authHttpHits')!r}"
    )
