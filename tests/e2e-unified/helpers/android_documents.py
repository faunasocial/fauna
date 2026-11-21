"""The android documents provider as a journey drives it — the shared steps.

Android's on-demand surface is a SAF ``DocumentsProvider`` over a replica the
app owns (``on-demand-files-android.md`` § Android SAF DocumentsProvider
binding); no host process reaches it, so every android leg drives it through
the instrumentation bridge's ``documents_*`` verbs (``drivers/android.py``),
which call the provider through a ``ContentResolver`` in the app's own process,
exactly as another app's picker does. The steps every such journey repeats —
the "Show in Files" gesture that presents a set, finding the presented set,
finding a document in it, waiting for a residency — live here, so the deposit
witness and the on-demand witnesses read one shape.
"""

from __future__ import annotations

from typing import Callable

from helpers.waiting import wait_until

# Ceilings a green run never pays (convention 14).
PRESENT_WINDOW = 90.0
LIST_WINDOW = 180.0
RESIDENCY_WINDOW = 120.0


def show_in_files(app, folder: str) -> None:
    """MUTATION (UI): turn the owner row's "Show in Files" toggle ON as a flip.

    The toggle reads ON by default, but ON is also the enrol gesture — the
    shared ``ensure_place`` writes this device's place only on a flip to ON
    (``DevicesVM.setFolderOnDemand``), and the plan presents a set only where
    this device's place accepts (``on-demand-files-apple.md`` § *Auto-appear
    default-ON*, which the android plan shares). A folder created off this
    device holds no place here, so a default-ON row is flipped off and on."""
    b = open_row(app, folder)
    assert b.on_demand_toggle_visible(), (
        "an owner row must render folder-on-demand-toggle on android; "
        f"{app.driver.diagnose('folder-on-demand-toggle')} error={app.error_text()!r}"
    )
    if b.on_demand_toggle_state() == "on":
        assert b.toggle_on_demand() == "off", f"the flip off did not settle; error={app.error_text()!r}"
    assert b.toggle_on_demand() == "on", f"the flip on did not settle; error={app.error_text()!r}"


def set_shown(app, folder: str, shown: bool) -> None:
    """MUTATION (UI): flip the row's "Show in Files" toggle to ``shown``."""
    b = open_row(app, folder)
    want = "on" if shown else "off"
    if b.on_demand_toggle_state() != want:
        assert b.toggle_on_demand() == want, (
            f"the flip to {want!r} did not settle on {folder!r}; error={app.error_text()!r}"
        )


def open_row(app, folder: str):
    """Leave the page and come back, then expand ``folder``'s row — one row
    open at a time, whatever an earlier gesture left expanded (the expand is a
    toggle). Returns the Folders page's action object."""
    b = app.backups
    app.mail_settings.navigate()
    b.navigate_folders()
    b.find_and_expand_folder(folder)
    return b


def presented_set(drv, folder: str) -> str:
    """The set's document id in the provider's root, as a picker lists it."""
    found: dict = {}

    def presented() -> bool:
        for child in drv.documents_children():
            if folder in child.get("name", ""):
                found.update(child)
                return True
        return False

    wait_until(
        presented, PRESENT_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"the provider never presented {folder!r}; root lists {drv.documents_children()!r}"
        ),
    )
    return found["id"]


def set_hidden(drv, folder: str) -> None:
    """Poll the provider's root until it no longer presents ``folder``."""
    wait_until(
        lambda: not any(folder in c.get("name", "") for c in drv.documents_children()),
        PRESENT_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"the provider still presents {folder!r} after its row was switched off; "
            f"root lists {drv.documents_children()!r}"
        ),
    )


def listed(drv, set_doc: str, name: str) -> dict:
    """``name``'s row in the set, re-querying the set's directory until it
    lists — each re-query is the browser's refresh, which pulls the set
    (``FaunaDocumentsProvider.queryChildDocuments``). Nothing else is asked of
    the device: a file another device adds arrives by itself."""
    found: dict = {}

    def rows() -> bool:
        hits = [c for c in drv.documents_children(set_doc) if c.get("name") == name]
        if len(hits) == 1:
            found.update(hits[0])
            return True
        return False

    wait_until(
        rows, LIST_WINDOW, interval=2.0,
        diagnose=lambda: f"{name!r} never listed once in the set; it lists {drv.documents_children(set_doc)!r}",
    )
    return found


def await_residency(
    drv, set_doc: str, document_id: str, settled: Callable[[dict], bool], what: str,
    *, window: float = RESIDENCY_WINDOW,
) -> dict:
    """Re-query the set (the browser's refresh: a pull, then the kept-root
    sweep and the eviction observation) until ``document_id``'s residency
    satisfies ``settled``; returns that reading. ``what`` names the state for
    the failure message."""
    last: dict = {}

    def reached() -> bool:
        drv.documents_children(set_doc)
        last.clear()
        last.update(drv.documents_residency(document_id))
        return settled(last)

    wait_until(
        reached, window, interval=2.0,
        diagnose=lambda: (
            f"{document_id} never reached {what}; last residency {last!r}; "
            f"parks {drv.documents_parks()!r}"
        ),
    )
    return dict(last)


def recorded(r: dict) -> bool:
    """A recorded body: the nest acked it and the host demoted it to the cache root."""
    return r.get("listed") and r.get("hydrated") and r.get("root") == "cache"


def placeholder(r: dict) -> bool:
    """Listed, with its bytes on no disk of this device."""
    return r.get("listed") and not r.get("hydrated") and r.get("root") is None


def kept(r: dict) -> bool:
    """An edit the nest has not recorded: its body rests in the kept root."""
    return r.get("listed") and r.get("root") == "kept"
