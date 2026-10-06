"""Read back what a copy button put on the clipboard.

Two ways a driver can answer this: a real OS clipboard read
(``PlatformDriver.get_clipboard_text`` — only tui (OSC 52) and windows (raw
Win32 clipboard) implement it, ``drivers/base.py``'s ``NotImplementedError``
default), or the button's own ``copied`` attribute — the value it reports
having written (the ``CopyButton``/``account-actor-id-copy-btn`` contract
``test_settings.py::_copy_and_read_back`` already reads for tui/linux/web/
macos/ios). ``read_clipboard_or_copied`` picks by driver CAPABILITY, not by
app name, so a caller needing both aliases mint (outcome 13) and settings
credential (outcome 10) checks writes the branch once.
"""
from helpers.waiting import wait_until


def read_copied_attr(driver, button_id: str, index: int = 0, *, timeout: float = 15.0) -> str:
    """Wait for `button_id`'s `copied` attribute to hold what it wrote."""
    return wait_until(
        lambda: driver.get_attr(button_id, "copied", index) or None,
        timeout,
        diagnose=lambda: (
            f"{button_id} never reported what it copied; {driver.diagnose(button_id)}"
        ),
    )


def read_clipboard_or_copied(app, button_id: str, index: int = 0, *, timeout: float = 15.0) -> str:
    """Read what `button_id` (already clicked) put on the clipboard: a real
    clipboard read where the driver can do it, else its `copied` attr. Polls
    until the read is non-empty either way (the copy can lag the click by a
    beat), leaving the equality check to the caller."""
    if app.driver.is_tui() or app.driver.is_windows():
        return wait_until(
            lambda: app.driver.get_clipboard_text() or None,
            timeout,
            diagnose=lambda: f"{button_id}: the clipboard never got a value",
        )
    return read_copied_attr(app.driver, button_id, index, timeout=timeout)
