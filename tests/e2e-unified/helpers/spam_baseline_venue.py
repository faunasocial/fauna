"""Shared setup for the spam-baseline witnesses on a dedicated mail venue
(`dedicated_mail_nest`): open the deployment's mail gate so the MDA — the
baseline's aggregation holder — serves, and stand up opted-in contributors
(`docs/goal/behavior/mail-spam.md` § Cold start, § Encrypted-mode interaction).

Consumers: `tests/test_spam_baseline_withdrawal.py`,
`tests/test_spam_baseline_standing.py`.
"""

import socket

from helpers.waiting import wait_until


def _accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0):
            return True
    except OSError:
        return False


def open_mail_gate_and_wait_for_the_bridges(venue, budget_s: float = 120.0) -> None:
    """The admin opens the deployment's mail gate; return once both bridges serve.

    Opening the gate races each bridge's first config fetch (the race
    `helpers/mail_wire.py::_open_mail_gate_and_wait_for_the_mta` documents):
    a bridge whose fetch lands after the gate opened serves at once and never
    exits, while one that had already idled exits for rebind and needs the venue
    to play the supervisor. So wait, per role, for whichever state it reaches,
    and rebind only the ones that exited.
    """
    venue.admin_opens_mail_gate()
    roles = {"mta": (venue.mta, venue.mx_port), "mda": (venue.mda, venue.imaps_port)}

    def _states():
        states = {}
        for role, (bridge, port) in roles.items():
            if _accepts(port):
                states[role] = "serving"
            elif bridge.proc.poll() is not None:
                states[role] = "exited"
            else:
                return None
        return states

    states = wait_until(
        _states,
        budget_s,
        interval=0.5,
        diagnose=lambda: "a mail bridge neither served nor exited for rebind after the gate opened",
    )
    exited = {role for role, state in states.items() if state == "exited"}
    if exited:
        venue.rebind_after_enable(mta="mta" in exited, mda="mda" in exited)


def baseline_contributors(nest, seal_helper_binary, tag: str, n: int = 3):
    """`n` fresh opted-in contributors, each with a sealed model, a holder copy
    and a keyless grant to the nest's own aggregation holder."""
    from conftest import _provision_baseline_contributor

    return [
        _provision_baseline_contributor(
            nest_instance=nest, seal_helper_binary=seal_helper_binary,
            local_part=f"baseline-{tag}-{i}", token=f"qz{tag}{i}wx", spam_messages=5,
        )[0]
        for i in range(n)
    ]
