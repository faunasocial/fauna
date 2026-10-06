"""Read a nest's own console claim banner — the admin's out-of-band channel.

Every unclaimed nest boot prints the bare claim code and, beside it, the claim
URI `fauna://claim?code=<code>&nest=<64-hex nest_actor_id>`
(`bins/fauna-nest/src/claim.rs::claim_banner_lines`). That printed `nest=` root
is the ONLY place a test can learn the box's real `nest_actor_id` the way a real
admin does, and it is what `docs/goal/behavior/onboarding.md` § 3a means by "the
identity the console vouched for".

⚠ **A claim URI's `nest=` must be the box's REAL root, never arbitrary bytes.**
`wizard_submit_claim_code` holds it as the first-contact identity before it
claims (`hold_first_contact_identity`), and the claim connection then graduates
against it — so a fabricated root fails the graduation outright rather than
producing a claimed-but-mispinned box. Tests that want a *successful* claim must
read the real root from here; only a test whose subject IS the refusal supplies a
different one.

Lifted out of `test_web_claim_pin_wasm_witness.py` when the platform-neutral
witness needed the same read (priority #2 — one copy, not one per app).
"""

from __future__ import annotations

import re
import time
from pathlib import Path

#: The banner line's shape. Anchored on both the scheme and the 64-hex root so a
#: truncated or partially-flushed line is never matched as a short root.
CLAIM_URI_RE = re.compile(
    r"CLAIM URI:\s*fauna://claim\?code=[^&\s]+&nest=([0-9a-f]{64})"
)


def read_real_nest_actor_id_hex(nest, *, timeout: float = 15.0) -> str:
    """The box's REAL `nest_actor_id`, read off its own console claim banner.

    Printed once the nest's identity is final, before `/health` is ever answered
    (`main.rs`'s boot order), so it is already in the log by the time any nest
    fixture returns; the poll is a safety margin for flush timing, not a wait on
    a state transition (convention 14 — the loop ends on the line appearing,
    never on a fixed delay).
    """
    log_path = nest["log_path"]
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            text = Path(log_path).read_text()
        except FileNotFoundError:
            text = ""
        m = CLAIM_URI_RE.search(text)
        if m:
            return m.group(1)
        time.sleep(0.2)
    raise AssertionError(
        f"no claim banner (CLAIM URI: fauna://claim?...) found in {log_path} "
        f"within {timeout}s — the nest either never printed one (is it actually "
        f"unclaimed?) or its stdout is not being captured to that path"
    )


def claim_uri(code: str, nest_actor_id_hex: str) -> str:
    """The console's own claim-URI form, assembled for a test to paste.

    Built here rather than inline so the two witnesses of
    `onboarding.md` § 3a agree on the exact shape the parser accepts.
    """
    return f"fauna://claim?code={code}&nest={nest_actor_id_hex}"
