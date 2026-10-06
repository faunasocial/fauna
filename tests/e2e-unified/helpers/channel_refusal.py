"""Arm and clear the nest's per-channel envelope refusal
(`bins/fauna-nest/src/channel_refusal_test_hook.rs`, `--features test-hooks`).

The seam that lets a journey stage a succession sweep which is genuinely
*partial* — the only way `recovery-kit-sweep-retry-button` renders at all
(`settings.md` § Recovery kit → *Finishing an unfinished group sweep*: the
button's render gate is unfinished work). Which class to refuse, and why the
selector is an envelope CLASS rather than a count of sends, is the Rust module's
own doc; the short version is that the sweep's three per-channel wire steps are
Commit → Application → Commit, and only failing the middle one leaves the group
in the resumable state the retry was written for.

Lives in ``helpers/`` rather than in one journey because it is a nest surface,
not a tui one: the web leg of the same coverage stages its owing sweep exactly
the same way.
"""
from __future__ import annotations

import json
import urllib.request

#: The envelope classes the hook understands — `ChannelRefusal` in the Rust.
APPLICATION = "application"
COMMIT = "commit"
ALL = "all"


def refuse_channel_envelopes(port: int, channel_id_hex: str, envelope: str) -> None:
    """Make `channel_id_hex` refuse every `envelope`-class send.

    The refusal is consulted before the envelope is ingested, so a refused send
    consumes no seq and no storage: the channel is left exactly where it was,
    which is what makes the staged fault a *flake* the client may safely retry
    rather than a hole in the log.
    """
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/v1/test/conversations/channel/{channel_id_hex}/refuse",
        data=json.dumps({"envelope": envelope}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req).read()


def allow_channel_envelopes(port: int, channel_id_hex: str) -> None:
    """Disarm `channel_id_hex` — the step a journey takes between staging the
    partial sweep and proving the retry can finish it."""
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/v1/test/conversations/channel/{channel_id_hex}/clear",
        data=b"",
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req).read()
