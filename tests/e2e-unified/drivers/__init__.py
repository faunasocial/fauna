from .base import PlatformDriver


def create_driver(app_name: str) -> PlatformDriver:
    """Create a PlatformDriver for the given app name.

    Valid names: web, android, ios, macos, windows, linux, tui

    macOS and iOS are both driven by the **in-process automation server** (the
    app hosts an `InProcessAutomationServer` + `AutomationRegistry` on a
    per-instance FAUNA_E2E_AGENT_PORT — docs/goal/architecture/apps/apple-e2e-automation.md).
    The legacy cross-process XCUITest/AutomationMode bridge was retired once iOS
    reached in-process parity (no AutomationMode → no host reboots, no
    machine-wide serialization).
    """
    # The feature catalog attributes an outcome to every app whose driver the test
    # launched (feature-catalog.md § The marker). This factory is the one door all
    # ~146 driver constructions come through, so noting it here means no test — and
    # no multi-seat fixture — has to remember. `feature_ledger` is stdlib-only and
    # imports nothing from `drivers`, so this cannot cycle.
    from helpers import feature_ledger
    feature_ledger.note_app(app_name)

    if app_name == "web":
        from .web import WebBridgeDriver
        return WebBridgeDriver()
    elif app_name == "android":
        from .android import AndroidBridgeDriver
        return AndroidBridgeDriver()
    elif app_name == "ios":
        from .ios import IosInProcessDriver
        return IosInProcessDriver()
    elif app_name == "macos":
        from .macos import MacosInProcessDriver
        return MacosInProcessDriver()
    elif app_name == "windows":
        from .windows import WindowsBridgeDriver
        return WindowsBridgeDriver()
    elif app_name == "linux":
        from .linux import LinuxBridgeDriver
        return LinuxBridgeDriver()
    elif app_name == "tui":
        from .tui import TuiDriver
        return TuiDriver()
    else:
        raise ValueError(f"Unknown app: {app_name}")
