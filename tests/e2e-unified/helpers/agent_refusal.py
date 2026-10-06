"""Read a test-agent command's refusal off whichever loud channel the app has.

`docs/goal/architecture/e2e-conventions.md` § convention 11: a test agent
honours a command or fails loudly, and never silently drops one. The apps do not
share one loud channel, and they are not meant to:

* **web** runs its commands as in-page JS through `window.__fauna_callCommand`,
  so a decline THROWS. The throw propagates out of `_execute_js` into the driver
  call, which is strictly louder than a state field, and convention 2 already
  sanctions that channel for auth failures.
* **the native apps** have no call stack reaching the driver. They stamp a
  dedicated nav-independent refusal slot that `error_text()` reads ahead of the
  page's own banner: tui's `App::refused_agent_command`, linux's
  `SharedState::agent_command_failure`, windows' `App.AgentCommandFailure`, and
  android's and apple's `AppMessages.refusedAgentCommand`.

A test that drives a command it EXPECTS to decline must read both channels.
Otherwise the same correct behaviour is green on one family and red on the
other: web's decline raises out of a bare `call_command` and fails the test at
the very step that wanted the decline.
"""

from __future__ import annotations


def refusal_from(app, action: str, payload: dict) -> str:
    """Drive `action` and return its refusal text, or `""` if it acked green.

    The ack is deliberately NOT the observable. Every bridge acks by design, so
    a clean return proves nothing about whether the arm honoured, refused, or
    dropped the command. Use this only for a command the test expects to
    DECLINE; a command that must succeed should let a throw propagate.
    """
    try:
        app.driver.call_command(action, payload)
    except Exception as exc:  # web's loud channel: the throw IS the surface
        return f"{type(exc).__name__}: {exc}"
    return app.error_text() or ""
