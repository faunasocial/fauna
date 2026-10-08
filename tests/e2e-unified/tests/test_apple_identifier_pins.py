"""Every copy of an apple platform identifier agrees, and none is on the retired domain.

`installers/macos.md` § Identifier domain gives each identifier tier ONE owner —
`fauna_core::platform_ids` in Rust, `FaunaExtensionKit`'s `AppleIdentifiers` in Swift
(FFI-free, re-exported by `FaunaKit`, so the widget appex can link it too). Six
sites cannot consume either, because a plist cannot reference a constant: four
entitlements files, the appex's `NSExtensionFileProviderDocumentGroup`, the iOS
`BGTaskSchedulerPermittedIdentifiers` array — plus `Fauna-NSE`, a deliberately
dependency-free SPM target (a notification service extension gets ~30 s and must
not drag FaunaKit + the FFI xcframework in). Those literals are what this file
pins.

**Why it exists at all.** The 2026-08-12 sweep found the failure it prevents
already in the tree: `conftest.py` built the e2e iOS bundle as `com.fauna.ios`
while the shipping app had been `social.fauna.ios` since 2026-07-19 — a silent
drift between two copies of one identifier that nothing could see, because each
copy was internally consistent. A mismatch in the app-group tier is worse than
cosmetic: the app writes the File Provider capability into one keychain access
group and the extension reads another, so the extension fails closed with no
error that names the cause.

Static-only — reads repository files, builds nothing, launches nothing (tier_1).
It therefore runs on every machine, not just macOS build hosts, which is the
point: the identifiers it guards are edited from any development checkout.
"""
from __future__ import annotations

import os
import plistlib
import re
import subprocess

import pytest

pytestmark = pytest.mark.tier_1


def _repo_root() -> str:
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return os.path.normpath(result.stdout.strip())


REPO = _repo_root()

APP_GROUP = "group.social.fauna.shared"
#: The Apple Developer TEAM ID (from `security find-identity -v`), never the
#: enrollment id from the enrollment correspondence — the two look alike and
#: the mix-up would make
#: every macOS build prompt (installers/macos.md § Identifier domain).
TEAM_ID = "7457N3M72H"
#: The macOS spelling is a DERIVATION of the base id, not a third identifier:
#: since macOS 15 a Developer-ID-signed process reaches an app-group container
#: prompt-free only under a Team-ID-prefixed group id (measured 2026-08-23,
#: first-signed-build matrix). iOS uses the base id verbatim.
MACOS_APP_GROUP = TEAM_ID + "." + APP_GROUP
#: The macOS app's SECOND, app-only group: the keychain access group
#: `KeychainStore`'s data-protection-plane rows live in (decided 2026-08-25). Not
#: the shared group on purpose — the sandboxed File Provider appex is entitled to
#: that one, and it must never be able to read the identity seed. Keychain-only
#: (no container dir is ever resolved for it); macOS-only (iOS rows stay in the
#: app's default application-identifier group, which already excludes the appex).
MACOS_ACCOUNT_KEYCHAIN_GROUP = TEAM_ID + ".group.social.fauna.account"
WATCH_APP_GROUP = "group.social.fauna.watchkit"
KEYCHAIN_PUSH = "social.fauna.push"
KEYCHAIN_FILE_PROVIDER = "social.fauna.fileprovider"
#: `KeychainStore`'s account-credential store — one value for macOS, iOS, and
#: watchOS (decided 2026-08-24; installers/macos.md § Identifier domain).
KEYCHAIN_ACCOUNT = "social.fauna.account"
#: The spellings `KEYCHAIN_ACCOUNT` replaced, per platform. The store reads NONE
#: of them: the compat-remnant sweep removed the read-forward
#: (`version-compatibility.md` § Dimension 2, the fourth ratified exception — no
#: row under a retired spelling exists), and
#: `test_no_retired_keychain_service_is_read` below pins that it stays gone.
RETIRED_KEYCHAIN_ACCOUNT = {
    "macOS": ("social.fauna.fauna", "social.fauna.desktop"),
    "watchOS": ("social.fauna.watch",),
    "iOS": ("social.fauna.fauna", "social.fauna.ios"),
}
SYNC_AGENT_LABEL = "social.fauna.sync-agent"
BG_TASK_IDS = {
    "social.fauna.sync.upload",
    "social.fauna.sync.custodian",
    #: The home-screen widget's background count refresh (a `BGAppRefreshTask`,
    #: `apps/common.md` § Home-screen widget → the apple mechanism in `ios.md`).
    "social.fauna.widget.refresh",
}
#: The `UIBackgroundModes` entry each background task's *kind* needs declared
#: before `BGTaskScheduler.submit` accepts its request — Apple's Background
#: processing capability (`processing`) for a `BGProcessingTaskRequest`,
#: Background fetch (`fetch`) for a `BGAppRefreshTaskRequest`. It is the
#: `BG_TASK_IDS` census with one column added, and
#: `test_the_mode_table_matches_the_kind_the_scheduler_registers` derives the
#: same column from `BackgroundScheduler.registerTasks()` so the two cannot drift.
BG_TASK_MODE = {
    "social.fauna.sync.upload": "processing",
    "social.fauna.sync.custodian": "processing",
    "social.fauna.widget.refresh": "fetch",
}
#: The `BGTask` subclass a handler is cast to (`BackgroundScheduler.registerTasks`)
#: → the background mode its requests need.
BG_TASK_CLASS_MODE = {"BGProcessingTask": "processing", "BGAppRefreshTask": "fetch"}
#: The retired `social.fauna.sync.pull` slice (a one-shot pass over in-process
#: location bindings no app writes any more — `sync-engine-deployments.md`
#: § Apple apps — convergence design) must stay out of BOTH lists below.

APP_GROUPS_KEY = "com.apple.security.application-groups"

#: Every entitlements file that must name an app group, and the EXACT list its
#: platform takes (macOS: Team-ID-prefixed; iOS: the base id). An appex declares
#: BOTH the entitlement (grants the container) and
#: `NSExtensionFileProviderDocumentGroup` (selects it for the replica). The macOS
#: app alone also names the account keychain group; an appex never does.
APP_GROUP_PLISTS = [
    ("apps/fauna-apple/Fauna-macOS/Fauna-macOS.entitlements",
     APP_GROUPS_KEY, [MACOS_APP_GROUP, MACOS_ACCOUNT_KEYCHAIN_GROUP]),
    ("apps/fauna-apple/Fauna-iOS/Resources/Fauna-iOS.entitlements",
     APP_GROUPS_KEY, [APP_GROUP]),
    ("apps/fauna-apple/Fauna-FileProvider/Fauna-FileProvider.entitlements",
     APP_GROUPS_KEY, [MACOS_APP_GROUP]),
    ("apps/fauna-apple/Fauna-FileProvider/Fauna-FileProvider-iOS.entitlements",
     APP_GROUPS_KEY, [APP_GROUP]),
    # The widget appex reads the unread-count snapshot the app writes into the
    # shared container, and nothing else — the same shared group, never the
    # account keychain group (below).
    ("apps/fauna-apple/Fauna-Widget/Fauna-Widget.entitlements",
     APP_GROUPS_KEY, [MACOS_APP_GROUP]),
    ("apps/fauna-apple/Fauna-Widget/Fauna-Widget-iOS.entitlements",
     APP_GROUPS_KEY, [APP_GROUP]),
]

#: The per-user sync agent carries NO app-group claim (dropped 2026-08-25): its
#: data root, socket, logs and pin store are all in the user domain, never the
#: TCC-protected container (installers/macos.md § Identifier domain, item 6 —
#: a launchd-spawned process is prompted there on every instance, and neither
#: Allow nor Deny ever binds the next one). A re-added claim is a dead claim
#: that invites the next "why does it prompt" hunt; the headless witness that
#: the binary creates nothing under the container is
#: `agent_sigterm.rs::the_agent_binary_boots_in_the_user_domain_and_never_creates_the_container`.
SYNC_AGENT_ENTITLEMENTS = "installer/macos/fauna-sync-agent.entitlements"

#: Every entitlements file that is NOT the macOS app: none may name the account
#: keychain group, or the identity seed becomes readable from that process —
#: for the File Provider appex, the exact least-privilege boundary
#: `on-demand-files.md` § Apple File Provider binding draws ("BackupKey + bearer,
#: never the seed").
NOT_ACCOUNT_KEYCHAIN_PLISTS = [
    "apps/fauna-apple/Fauna-iOS/Resources/Fauna-iOS.entitlements",
    "apps/fauna-apple/Fauna-FileProvider/Fauna-FileProvider.entitlements",
    "apps/fauna-apple/Fauna-FileProvider/Fauna-FileProvider-iOS.entitlements",
    "apps/fauna-apple/Fauna-Widget/Fauna-Widget.entitlements",
    "apps/fauna-apple/Fauna-Widget/Fauna-Widget-iOS.entitlements",
    SYNC_AGENT_ENTITLEMENTS,
]

#: Each File Provider appex flavor's Info.plist must select the SAME container
#: its own entitlements grant — a mismatch puts the replica in a container the
#: extension is not entitled to, failing closed with no error naming the cause.
APPEX_DOCUMENT_GROUP_PAIRS = [
    ("apps/fauna-apple/Fauna-FileProvider/Info.plist", MACOS_APP_GROUP),
    ("apps/fauna-apple/Fauna-FileProvider/Info-iOS.plist", APP_GROUP),
]


def _read(rel: str) -> str:
    with open(os.path.join(REPO, rel), encoding="utf-8") as handle:
        return handle.read()


def _read_plist(rel: str):
    with open(os.path.join(REPO, rel), "rb") as handle:
        return plistlib.load(handle)


# ---------------------------------------------------------------------------
# 1. The app group: one id, named identically by every entitlement, the appex's
#    document-group key, and both language owners.
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "rel,key,expected", APP_GROUP_PLISTS, ids=lambda v: os.path.basename(str(v))
)
def test_every_entitlements_file_names_its_platforms_app_group(rel, key, expected):
    groups = _read_plist(rel).get(key)
    assert groups == expected, (
        f"{rel} declares {groups!r} under {key} but its platform's list is "
        f"{expected!r} (macOS: Team-ID-prefixed, iOS: the base id — "
        f"installers/macos.md § Identifier domain); a divergent spelling gets a "
        f"different container — the app provisions the File Provider capability "
        f"into one keychain access group and the extension reads another, "
        f"failing closed with no error naming the cause."
    )


def test_the_sync_agent_carries_no_app_group_claim():
    plist = _read_plist(SYNC_AGENT_ENTITLEMENTS)
    assert APP_GROUPS_KEY not in plist, (
        f"{SYNC_AGENT_ENTITLEMENTS} names {plist[APP_GROUPS_KEY]!r} under "
        f"{APP_GROUPS_KEY}, but the agent never opens the app-group container "
        f"(its state is in the user domain — installers/macos.md § Identifier "
        f"domain, item 6); a group claim it does not use is exactly the dead "
        f"claim that sent three sessions hunting for a TCC prompt."
    )


@pytest.mark.parametrize("rel", NOT_ACCOUNT_KEYCHAIN_PLISTS, ids=os.path.basename)
def test_only_the_macos_app_is_entitled_to_the_account_keychain_group(rel):
    groups = _read_plist(rel).get(APP_GROUPS_KEY, [])
    assert MACOS_ACCOUNT_KEYCHAIN_GROUP not in groups, (
        f"{rel} is entitled to {MACOS_ACCOUNT_KEYCHAIN_GROUP!r}, the access group "
        f"the identity seed lives in on macOS; only Fauna-macOS.entitlements may "
        f"name it — an appex or the agent reading the seed breaks the "
        f"least-privilege boundary of on-demand-files.md § Apple File Provider "
        f"binding."
    )
    assert MACOS_ACCOUNT_KEYCHAIN_GROUP not in _read(rel)


@pytest.mark.parametrize(
    "rel,expected", APPEX_DOCUMENT_GROUP_PAIRS,
    ids=lambda v: os.path.basename(str(v)),
)
def test_the_appex_document_group_selects_the_same_container(rel, expected):
    info = _read_plist(rel)
    declared = info["NSExtension"]["NSExtensionFileProviderDocumentGroup"]
    assert declared == expected, (
        f"{rel}'s NSExtensionFileProviderDocumentGroup is {declared!r} but "
        f"this flavor's entitlement grants {expected!r}; the replica would "
        f"live in a container the extension is not entitled to."
    )


def test_each_fp_appex_flavor_ships_its_own_info_plist():
    # The two FP targets MUST NOT share one Info.plist: the document group
    # forks per OS, so the iOS target points at Info-iOS.plist. A repoint back
    # to the shared file would silently give the iOS appex the macOS
    # (Team-ID-prefixed) document group.
    pbx = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    assert pbx.count('INFOPLIST_FILE = "Fauna-FileProvider/Info-iOS.plist";') == 2, (
        "the iOS File Provider target's two build configs must point at "
        "Info-iOS.plist (the base-id document group)"
    )
    assert pbx.count('INFOPLIST_FILE = "Fauna-FileProvider/Info.plist";') == 2, (
        "the macOS File Provider target's two build configs must point at "
        "Info.plist (the Team-ID-prefixed document group)"
    )


def test_rust_and_swift_owners_agree_on_the_app_group():
    rust = _read("libs/fauna-core/src/platform_ids.rs")
    swift = _read("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift")
    assert f'APPLE_TEAM_ID: &str = "{TEAM_ID}"' in rust
    assert f'APPLE_APP_GROUP: &str = "{APP_GROUP}"' in rust
    assert f'APPLE_MACOS_APP_GROUP: &str = "{MACOS_APP_GROUP}"' in rust
    # Swift forks the one constant with `#if os(macOS)` — both branch literals
    # must be present (each arm is compiled only for its own platform).
    assert f'appGroup = "{MACOS_APP_GROUP}"' in swift
    assert f'appGroup = "{APP_GROUP}"' in swift
    assert f'APPLE_WATCHKIT_APP_GROUP: &str = "{WATCH_APP_GROUP}"' in rust
    assert f'watchAppGroup = "{WATCH_APP_GROUP}"' in swift
    assert f'APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP: &str = "{MACOS_ACCOUNT_KEYCHAIN_GROUP}"' in rust
    assert f'accountKeychainGroup = "{MACOS_ACCOUNT_KEYCHAIN_GROUP}"' in swift
    # `KeychainStore` addresses the data-protection plane through the shared
    # owner, never a re-hardcoded literal.
    store = _read("apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/KeychainStore.swift")
    assert "AppleIdentifiers.accountKeychainGroup" in store
    assert MACOS_ACCOUNT_KEYCHAIN_GROUP not in store


def test_the_dependency_free_nse_repeats_the_shared_values_exactly():
    # `FaunaNSE` has no SPM dependencies by design, so it cannot import
    # AppleIdentifiers — these literals are the only copies that must be checked
    # rather than shared. They are the SAME keychain items `PushManager` writes.
    nse = _read("apps/fauna-apple/Fauna-NSE/NotificationService.swift")
    assert f'accessGroup = "{APP_GROUP}"' in nse
    assert f'keychainService = "{KEYCHAIN_PUSH}"' in nse


def test_keychain_store_delegates_the_account_service_to_the_shared_owner():
    # `KeychainStore.service` used to hand-roll `#if os(watchOS)` between a
    # platform-word leaf and a bundle-id echo — both banned by *How the LEAF is
    # spelled*. It must now be ONE shared constant, not a re-hardcoded literal
    # (that would silently reintroduce the same drift the D5-style dedups fix
    # everywhere else in this file).
    swift = _read("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift")
    assert f'account = "{KEYCHAIN_ACCOUNT}"' in swift

    store = _read("apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/KeychainStore.swift")
    assert "AppleIdentifiers.KeychainService.account" in store
    assert "os(watchOS)" not in store, (
        "KeychainStore must not special-case watchOS any more — macOS, iOS, "
        "and watchOS share one keychain-service leaf"
    )
    assert "social.fauna.watch" not in store
    assert '"social.fauna.fauna"' not in store


def test_no_retired_keychain_service_is_read():
    """The retired keychain-service spellings are never read, copied or swept.

    A refusal pin (the compat-remnant sweep, `version-compatibility.md`
    § Dimension 2): the store addresses the one current service and nothing
    else. `KeychainRetiredServiceTests` pins the behaviour; this pins that
    no source names a retired spelling to read it from.
    """
    swift = _read("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift")
    store = _read("apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/KeychainStore.swift")
    for source, text in (("AppleIdentifiers.swift", swift), ("KeychainStore.swift", store)):
        assert "retiredAccount" not in text and "retiredServices" not in text, (
            f"{source} names a retired keychain-service list again — the "
            f"read-forward was removed by the compat-remnant sweep"
        )
        for platform, spellings in RETIRED_KEYCHAIN_ACCOUNT.items():
            for spelling in spellings:
                assert f'"{spelling}"' not in text, (
                    f"{source} spells {platform}'s retired keychain service "
                    f"{spelling!r} as a string — nothing may read it"
                )


# ---------------------------------------------------------------------------
# 2. iOS background tasks: an id the app registers a handler for but never
#    declared is refused by the system at launch; an id declared but never
#    registered lets a still-pending request (submitted by an older build)
#    launch the app into a task with no handler, which terminates it.
# ---------------------------------------------------------------------------

def test_every_registered_background_task_id_is_permitted_by_the_plist():
    info = _read_plist("apps/fauna-apple/Fauna-iOS/Resources/Info.plist")
    permitted = set(info["BGTaskSchedulerPermittedIdentifiers"])
    missing = BG_TASK_IDS - permitted
    assert not missing, (
        f"{sorted(missing)} are registered in BackgroundScheduler but absent "
        f"from BGTaskSchedulerPermittedIdentifiers (declared: {sorted(permitted)}) "
        f"— BGTaskScheduler refuses to register a handler for an undeclared id, "
        f"so the task silently never runs."
    )
    extra = permitted - BG_TASK_IDS
    assert not extra, (
        f"{sorted(extra)} are declared in BGTaskSchedulerPermittedIdentifiers but "
        f"no BackgroundScheduler handler registers them — a request an older build "
        f"left pending would launch the app into a task with no handler. Drop the "
        f"retired id from the plist."
    )


def test_the_background_scheduler_consumes_the_shared_ids():
    scheduler = _read(
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/BackgroundScheduler.swift"
    )
    for name in ("upload", "custodianPull", "widgetRefresh", "backgroundUploadSession"):
        assert f"AppleIdentifiers.BackgroundTask.{name}" in scheduler, (
            f"BackgroundScheduler re-hardcodes the {name} task id instead of "
            f"consuming AppleIdentifiers — that is exactly how the plist and the "
            f"registration drift apart."
        )


def _scheduler_task_modes() -> dict[str, str]:
    """task id -> the `UIBackgroundModes` entry `registerTasks()` implies for it.

    A handler cast to `BGProcessingTask` / `BGAppRefreshTask` is the kind the
    matching request must have been submitted as (the cast traps otherwise), so
    that cast is the source-side truth of each id's kind.
    """
    scheduler = _read(
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/BackgroundScheduler.swift"
    )
    identifiers = _read(
        "apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift"
    )
    # `static let uploadTaskId = AppleIdentifiers.BackgroundTask.upload`
    leaf_of = dict(re.findall(
        r"static let (\w+TaskId) = AppleIdentifiers\.BackgroundTask\.(\w+)", scheduler
    ))
    # `public static let upload = "social.fauna.sync.upload"` inside the enum.
    enum_body = identifiers.split("public enum BackgroundTask {", 1)[1].split("\n    }", 1)[0]
    id_of = dict(re.findall(r'public static let (\w+) = "([^"]+)"', enum_body))
    return {
        id_of[leaf_of[const]]: BG_TASK_CLASS_MODE[cast]
        for const, cast in re.findall(
            r"forTaskWithIdentifier:\s*Self\.(\w+TaskId)\b.*?as!\s*(\w+)\)", scheduler, re.S
        )
    }


def test_the_mode_table_covers_exactly_the_task_ids():
    assert set(BG_TASK_MODE) == BG_TASK_IDS, (
        f"BG_TASK_MODE and BG_TASK_IDS name different tasks "
        f"({sorted(set(BG_TASK_MODE) ^ BG_TASK_IDS)}) — a new background task "
        f"needs its kind's background mode recorded here."
    )


def test_the_mode_table_matches_the_kind_the_scheduler_registers():
    registered = _scheduler_task_modes()
    assert registered == BG_TASK_MODE, (
        f"BackgroundScheduler.registerTasks() casts its handlers to kinds that "
        f"imply {registered}, but BG_TASK_MODE says {BG_TASK_MODE} — one of them "
        f"is stale, and the plist is pinned against BG_TASK_MODE."
    )


def test_the_plist_declares_the_background_mode_each_task_kind_needs():
    """`BGTaskScheduler.submit` refuses a request whose kind's mode is not declared.

    Apple's Background processing capability (`processing`) gates every
    `BGProcessingTaskRequest`, Background fetch (`fetch`) every
    `BGAppRefreshTaskRequest`; without the mode `submit` throws
    `BGTaskSchedulerErrorCodeNotPermitted`. The scheduler used to swallow that
    error (`try?`), so an undeclared mode meant the task was never scheduled and
    nothing said so. Both directions: a needed mode missing is that silent
    refusal; a task-kind mode declared with no task of the kind is a capability
    the app does not use (an App Review rejection).
    """
    info = _read_plist("apps/fauna-apple/Fauna-iOS/Resources/Info.plist")
    declared = set(info.get("UIBackgroundModes", []))
    needed = set(BG_TASK_MODE.values())
    missing = needed - declared
    assert not missing, (
        f"UIBackgroundModes {sorted(declared)} lacks {sorted(missing)} — "
        f"BGTaskScheduler.submit refuses every request of the kind that mode "
        f"gates ({ {i: m for i, m in BG_TASK_MODE.items() if m in missing} }), "
        f"so those tasks are never scheduled."
    )
    extra = (declared & set(BG_TASK_CLASS_MODE.values())) - needed
    assert not extra, (
        f"UIBackgroundModes declares {sorted(extra)} but no registered background "
        f"task is of the kind that mode gates — drop the unused capability."
    )


def test_the_background_scheduler_never_swallows_a_submit_error():
    """A refused `BGTaskScheduler.submit` must leave a trace, never vanish.

    `try? BGTaskScheduler.shared.submit(...)` is what hid the missing
    `processing` mode: the refusal was the only signal and it was discarded.
    """
    scheduler = _read(
        "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/BackgroundScheduler.swift"
    )
    assert "try? BGTaskScheduler.shared.submit" not in scheduler, (
        "BackgroundScheduler swallows a BGTaskScheduler.submit error again — "
        "log it (the schedule* methods share one logging submit)."
    )


# ---------------------------------------------------------------------------
# 3. The launchd label the app kickstarts must equal the one the .pkg installs.
# ---------------------------------------------------------------------------

def test_the_installed_launch_agent_label_is_the_one_the_app_kickstarts():
    postinstall = _read("installer/macos/scripts/sync/postinstall")
    assert f"<string>{SYNC_AGENT_LABEL}</string>" in postinstall
    assert f'install_launchagent "{SYNC_AGENT_LABEL}"' in postinstall

    swift = _read("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift")
    assert f'syncAgentLaunchAgent = "{SYNC_AGENT_LABEL}"' in swift
    spawner = _read("apps/fauna-apple/Fauna-macOS/Sync/SyncAgentSpawner.swift")
    assert "AppleIdentifiers.syncAgentLaunchAgent" in spawner, (
        "the spawner must consume the shared label — a spawner naming a label "
        "the postinstall never wrote kickstarts a job launchd has not loaded, "
        "which fails silently and looks like an agent that will not start."
    )


APP_LAUNCH_AGENT_LABEL = "social.fauna.FaunaMacOS"


def test_the_bundled_auto_start_agent_is_the_one_the_app_registers():
    """`SMAppService.agent(plistName:)` looks the plist up by NAME inside
    `Contents/Library/LaunchAgents`, and launchd keys the job by its `Label` —
    so the file name, its `Label`, the Swift constant and the Xcode copy phase
    are four copies of one identifier (`apps/macos.md` § App Lifecycle →
    *Auto-start at sign-in*). A drift in any of them fails `register()` at
    runtime with no error a user sees: auto-start silently never happens."""
    plist = _read_plist(
        f"apps/fauna-apple/Fauna-macOS/Resources/LaunchAgents/{APP_LAUNCH_AGENT_LABEL}.plist"
    )
    assert plist["Label"] == APP_LAUNCH_AGENT_LABEL
    assert plist["BundleProgram"] == "Contents/MacOS/Fauna"
    assert "--autostart" in plist["ProgramArguments"], (
        "the login launch must carry --autostart, or it opens the main window"
    )
    assert plist["RunAtLoad"] is True

    swift = _read("apps/fauna-apple/FaunaKit/Sources/FaunaExtensionKit/AppleIdentifiers.swift")
    assert f'appLaunchAgent = "{APP_LAUNCH_AGENT_LABEL}"' in swift
    autostart = _read("apps/fauna-apple/Fauna-macOS/App/AutoStart.swift")
    assert "AppleIdentifiers.appLaunchAgent" in autostart

    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    assert f"{APP_LAUNCH_AGENT_LABEL}.plist in Embed Launch Agent" in pbxproj
    assert "dstPath = Contents/Library/LaunchAgents;" in pbxproj


def test_no_installer_script_sweeps_a_retired_label():
    """The retired `com.fauna.*` launchd labels are named nowhere in the
    installer — no postinstall sweep, no uninstall loop, no per-user nest
    migration.

    A refusal pin (the compat-remnant sweep, `version-compatibility.md`
    § Dimension 2): no machine carries a pre-2026-08-12 label or a per-user
    nest install, so the `retire_legacy_labels` helper and
    `migrate_per_user_nest_to_system` were removed with their callers.
    """
    for rel in (
        "installer/macos/scripts/common.sh",
        "installer/macos/scripts/nest/postinstall",
        "installer/macos/scripts/bridge/postinstall",
        "installer/macos/scripts/sync/postinstall",
        "installer/macos/fauna-uninstall",
    ):
        text = _read(rel)
        assert "com.fauna." not in text, f"{rel} names a retired com.fauna.* label"
        assert "retire_legacy_labels" not in text, f"{rel} carries the retired-label sweep"
        assert "migrate_per_user_nest_to_system" not in text, (
            f"{rel} carries the per-user nest migration"
        )


# ---------------------------------------------------------------------------
# 4. The retired domain is gone from every apple surface.
# ---------------------------------------------------------------------------

#: Apple-owned trees. The android tree is deliberately absent: its Java package
#: `com.fauna.app` and the UniFFI Kotlin `package_name` `com.fauna.ffi` are
#: RATIFIED to stay on the legacy domain (`installers/android.md` § Store
#: identity — "renaming every source file buys nothing"), so they are not drift.
APPLE_TREES = [
    "apps/fauna-apple",
    "installer/macos",
]

#: Paths under those trees that legitimately still contain the old spelling.
_ALLOWED = re.compile(
    r"""(?x)
    ^apps/fauna-apple/(generated|FaunaFFISwift|FaunaFFI\.xcframework)/  # regenerated from Rust
    """
)

#: The deliberate exception in hand-written source: a retired identifier an
#: upgrade sweep or detection path must still name. Such code is required to say
#: so — but the "why" lives in the comment heading the block, not on the line
#: that repeats the label, so the marker is looked for in a window around it.
#: This is a drift GUARD, not a proof: it catches a new `com.fauna.*` written
#: somewhere nobody was thinking about retirement, which is the failure mode
#: that actually happened. It does not stop someone adding one inside an
#: existing sweep block, and is not meant to.
_LEGACY_MARKERS = ("legacy", "Legacy", "LEGACY", "retire", "Retire", "pre-rename",
                   "retired", "pre-A4", "domain sweep", "scaffolding")
_MARKER_WINDOW = 8


def _tracked_files(tree: str) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", tree],
        capture_output=True, text=True, check=True, cwd=REPO,
    ).stdout.split()
    return [p for p in out if not _ALLOWED.match(p)]


def test_no_apple_surface_carries_the_retired_domain_outside_a_sweep_path():
    offenders: list[str] = []
    for tree in APPLE_TREES:
        for rel in _tracked_files(tree):
            try:
                text = _read(rel)
            except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
                continue
            lines = text.splitlines()
            for index, line in enumerate(lines):
                if "com.fauna." not in line:
                    continue
                # Code naming a retired identifier must SAY it is retired —
                # that is what separates an upgrade sweep from missed drift. The
                # statement usually sits under the comment that explains it, so
                # look at the surrounding block rather than the single line.
                window = lines[max(0, index - _MARKER_WINDOW):index + _MARKER_WINDOW]
                if any(marker in text_line
                       for text_line in window
                       for marker in _LEGACY_MARKERS):
                    continue
                offenders.append(f"{rel}:{index + 1}: {line.strip()[:120]}")
    assert not offenders, (
        "these apple sites still name the retired `com.fauna.*` domain without "
        "marking it as legacy/retired (installers/macos.md § Identifier "
        "domain):\n  " + "\n  ".join(offenders)
    )


# ---------------------------------------------------------------------------
# 5. Export compliance: `ITSAppUsesNonExemptEncryption` declared on both app
#    targets, deliberately absent from the three appex targets — App Store
#    Connect's encryption declaration is read from the containing app's
#    Info.plist, not an embedded extension's.
# ---------------------------------------------------------------------------

EXPORT_COMPLIANCE_APP_PLISTS = [
    "apps/fauna-apple/Fauna-macOS/Resources/Info.plist",
    "apps/fauna-apple/Fauna-iOS/Resources/Info.plist",
]

EXPORT_COMPLIANCE_APPEX_PLISTS = [
    "apps/fauna-apple/Fauna-NSE/Info.plist",
    "apps/fauna-apple/Fauna-FileProvider/Info.plist",
    "apps/fauna-apple/Fauna-FileProvider/Info-iOS.plist",
    "apps/fauna-apple/Fauna-FileProviderUI/Info.plist",
]


@pytest.mark.parametrize(
    "rel", EXPORT_COMPLIANCE_APP_PLISTS,
    ids=lambda v: os.path.basename(os.path.dirname(v)),
)
def test_export_compliance_key_declared_on_app_targets(rel):
    info = _read_plist(rel)
    assert info.get("ITSAppUsesNonExemptEncryption") is True, (
        f"{rel} must declare ITSAppUsesNonExemptEncryption = true — Fauna ships "
        f"its own encryption (MLS, sealed-at-rest), not merely OS-provided "
        f"facilities, per docs/goal/architecture/export-compliance.md § What "
        f"each store form answers."
    )


@pytest.mark.parametrize(
    "rel", EXPORT_COMPLIANCE_APPEX_PLISTS,
    ids=lambda v: os.path.basename(os.path.dirname(v)),
)
def test_export_compliance_key_absent_from_appex_targets(rel):
    info = _read_plist(rel)
    assert "ITSAppUsesNonExemptEncryption" not in info, (
        f"{rel} is an app extension bundle — App Store Connect reads the "
        f"export-compliance declaration from the containing app's Info.plist, "
        f"not an embedded extension's; a key here is dead weight inviting the "
        f"two copies to drift apart."
    )


# ---------------------------------------------------------------------------
# 6. Bundle ids: ONE string for the whole Apple family, and every copy equal.
#
# `installers/macos.md` § Identifier domain (ratified 2026-08-22) gives Apple a
# SINGLE registry unit — "iOS + macOS + later tvOS/visionOS — ONE record" — so
# both apps ship the same bundle id. That is not a stylistic preference: Apple's
# universal purchase REQUIRES the two platforms to share one string, and the
# earlier per-platform spellings (`social.fauna.ios`, `social.fauna.desktop`)
# were retired for exactly that reason. Appexes take the parent-id prefix
# because xcodebuild's embedding validator refuses anything else.
# ---------------------------------------------------------------------------

SHIPPING_BUNDLE_ID = "social.fauna.fauna"

#: The per-platform leaves this family retired 2026-08-23. Unlike the launchd
#: and `.pkg` tiers, these need no upgrade sweep — no signed or notarized apple
#: artifact was ever distributed under them, so they persist on no user machine.
#: They are pinned as *absent* so a re-typed literal cannot quietly come back.
RETIRED_BUNDLE_LEAVES = ("social.fauna.desktop", "social.fauna.ios")


def test_both_apple_apps_ship_the_one_family_bundle_id():
    # Universal purchase needs iOS and macOS on ONE App Store record, which needs
    # ONE bundle id. Assert the whole set the .xcodeproj declares rather than a
    # substring: a single `in` check passes while a sibling target still carries
    # a retired leaf, which is the drift that would silently close the Mac App
    # Store off (`installers/macos.md` § Identifier domain).
    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    declared = set(re.findall(
        r"PRODUCT_BUNDLE_IDENTIFIER = \"?([\w.]+)\"?;", pbxproj))
    expected = {
        SHIPPING_BUNDLE_ID,
        f"{SHIPPING_BUNDLE_ID}.FileProvider",
        f"{SHIPPING_BUNDLE_ID}.FileProviderUI",
        # The home-screen widget (apps/common.md § Home-screen widget).
        f"{SHIPPING_BUNDLE_ID}.Widget",
    }
    assert declared == expected, (
        f"the .xcodeproj must declare exactly the one-family id and its three "
        f"appex children; got {sorted(declared)}. Universal purchase requires "
        f"iOS and macOS to share ONE bundle id, and an appex must be prefixed "
        f"by its host app's id or xcodebuild refuses to embed it."
    )


@pytest.mark.parametrize("rel", [
    "apps/fauna-apple/Fauna-macOS/Resources/Info.plist",
    "apps/fauna-apple/Fauna-iOS/Resources/Info.plist",
])
def test_no_app_plist_carries_a_retired_bundle_leaf(rel):
    text = _read(rel)
    offenders = [leaf for leaf in RETIRED_BUNDLE_LEAVES if leaf in text]
    assert not offenders, (
        f"{rel} still names the retired bundle leaf(s) {offenders}. Both apple "
        f"apps converged on {SHIPPING_BUNDLE_ID} (installers/macos.md § "
        f"Identifier domain); a leftover here is a copy that disagrees with the "
        f".xcodeproj while staying internally consistent — invisible drift."
    )


def test_the_e2e_macos_bundle_id_matches_the_shipping_app():
    # The macOS twin of the iOS pin below. Until 2026-08-23 only iOS was pinned,
    # so the macOS driver could drift from the .xcodeproj unobserved — and the
    # macOS driver's id is load-bearing twice over: it targets the app AND seeds
    # the artifact suite's per-launch staged identity (`_E2E_BUNDLE_ID_PREFIX`),
    # whose whole purpose is to be a SUBname of the shipping id.
    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    assert f"PRODUCT_BUNDLE_IDENTIFIER = {SHIPPING_BUNDLE_ID};" in pbxproj

    info = _read_plist("apps/fauna-apple/Fauna-macOS/Resources/Info.plist")
    assert info.get("CFBundleIdentifier") == SHIPPING_BUNDLE_ID, (
        "the macOS Info.plist's CFBundleIdentifier is the literal the built "
        "bundle actually carries — LaunchServices and the File Provider domain "
        "registration key on it, not on the build setting."
    )

    driver = _read("tests/e2e-unified/drivers/macos.py")
    assert f'BUNDLE_ID = "{SHIPPING_BUNDLE_ID}"' in driver
    assert f'_E2E_BUNDLE_ID_PREFIX = "{SHIPPING_BUNDLE_ID}.e2e"' in driver, (
        "the artifact suite stages each launch under a per-instance SUBname of "
        "the shipping id; if the prefix stops being a subname of BUNDLE_ID the "
        "staged copy is no longer the app under test."
    )

    # conftest must DERIVE the artifact bundle id from the driver, never repeat
    # it — the same rule the iOS synthetic bundle already follows.
    conftest = _read("tests/e2e-unified/conftest.py")
    assert "from drivers.macos import BUNDLE_ID as MACOS_BUNDLE_ID" in conftest
    assert '"bundle_id": MACOS_BUNDLE_ID,' in conftest, (
        "the macOS artifact fixture must take its bundle id from "
        "drivers.macos.BUNDLE_ID; a second literal here is the same drift that "
        "made the iOS suite prove an app nobody installs (2026-08-12)."
    )


def test_the_e2e_ios_bundle_id_matches_the_shipping_app():
    # The drift this whole file exists to prevent, pinned directly: the e2e
    # synthetic bundle and the driver that targets it must carry the bundle id
    # the .xcodeproj actually ships, or the suite proves an app nobody installs.
    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    shipping = SHIPPING_BUNDLE_ID
    assert f"PRODUCT_BUNDLE_IDENTIFIER = {shipping};" in pbxproj

    driver = _read("tests/e2e-unified/drivers/ios.py")
    assert f'BUNDLE_ID = "{shipping}"' in driver

    # conftest must DERIVE the synthetic bundle's id from the driver, never
    # repeat it — and must rewrite the plist every build.
    conftest = _read("tests/e2e-unified/conftest.py")
    assert "from drivers.ios import BUNDLE_ID as IOS_BUNDLE_ID" in conftest
    assert "<key>CFBundleIdentifier</key><string>{IOS_BUNDLE_ID}</string>" in conftest, (
        "the e2e iOS bundle must take its id from drivers.ios.BUNDLE_ID; a second "
        "literal here is exactly the drift that made the suite prove an app "
        "nobody installs."
    )
    assert 'if not info_plist.exists():' not in conftest, (
        "the synthetic Info.plist must be written UNCONDITIONALLY. `derived/` "
        "survives across runs, so a guarded write serves a bundle stamped with "
        "the PREVIOUS id while the driver launches the new one: simctl install "
        "succeeds and simctl launch fails FBSOpenApplicationServiceError code=4, "
        "a message that never mentions a stale bundle. Observed 2026-08-12."
    )


# ---------------------------------------------------------------------------
# iOS App Store signing (installers/ios.md § Signing). The signed export runs
# only on the macOS box that holds the profiles (test_ios_archive.py's export
# leg); these pins run everywhere, so nobody can quietly turn the tracked
# export options into an upload or the project into automatic signing — the
# two shapes that would let a build talk to Apple's servers.
# ---------------------------------------------------------------------------

EXPORT_OPTIONS = "apps/fauna-apple/ExportOptions-AppStore.plist"
#: The four iOS Release configs (Fauna-iOS, its File Provider pair, the widget)
#: — the ONLY configs that may carry device signing.
IOS_RELEASE_CONFIGS = (
    "FB0000000000000000000021",
    "FB0000000000000000000023",
    "FC0000000000000000000033",
    "FD0000000000000000000043",
)
_DEVICE_SIGNING_KEYS = (
    "CODE_SIGN_IDENTITY[sdk=iphoneos*]",
    "DEVELOPMENT_TEAM[sdk=iphoneos*]",
    "PROVISIONING_PROFILE_SPECIFIER[sdk=iphoneos*]",
)


def _app_store_profile_name(bundle_id: str) -> str:
    return f"Fauna AppStore {bundle_id}"


def _build_configs(pbxproj: str) -> dict[str, str]:
    return dict(re.findall(
        r"\t\t([0-9A-F]{24}) /\* \w+ \*/ = \{\n\t\t\tisa = XCBuildConfiguration;"
        r"\n\t\t\tbuildSettings = \{\n(.*?)\n\t\t\t\};", pbxproj, re.S))


def test_ios_export_options_never_upload_and_sign_manually():
    opts = _read_plist(EXPORT_OPTIONS)
    assert opts.get("method") == "app-store-connect", opts.get("method")
    assert opts.get("destination") == "export", (
        f"{EXPORT_OPTIONS} must export to disk; the store upload is a separate "
        f"act from a recorded public commit with its OWN options file "
        f"(installers/ios.md § Upload mechanics). Got {opts.get('destination')!r}."
    )
    assert opts.get("signingStyle") == "manual", opts.get("signingStyle")
    assert opts.get("teamID") == TEAM_ID, opts.get("teamID")
    assert opts.get("manageAppVersionAndBuildNumber") is False, (
        "manageAppVersionAndBuildNumber must be false: true makes the exporter "
        "query App Store Connect, and CFBundleVersion is pinned at 1 anyway."
    )
    assert opts.get("uploadSymbols") is False, opts.get("uploadSymbols")


def test_ios_export_options_name_a_profile_for_every_bundle():
    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    declared = set(re.findall(
        r"PRODUCT_BUNDLE_IDENTIFIER = \"?([\w.]+)\"?;", pbxproj))
    profiles = _read_plist(EXPORT_OPTIONS).get("provisioningProfiles", {})
    assert profiles == {b: _app_store_profile_name(b) for b in declared}, (
        f"{EXPORT_OPTIONS} must map every bundle the iOS app embeds to its App "
        f"Store profile — an export signs each embedded bundle. Got {profiles}."
    )


def test_device_signing_lives_only_in_the_four_ios_release_configs():
    pbxproj = _read("apps/fauna-apple/Fauna.xcodeproj/project.pbxproj")
    configs = _build_configs(pbxproj)
    for cid in IOS_RELEASE_CONFIGS:
        body = configs[cid]
        bundle = re.search(r"PRODUCT_BUNDLE_IDENTIFIER = \"?([\w.]+)\"?;", body).group(1)
        assert '"CODE_SIGN_IDENTITY[sdk=iphoneos*]" = "Apple Distribution";' in body, cid
        assert f'"DEVELOPMENT_TEAM[sdk=iphoneos*]" = {TEAM_ID};' in body, cid
        assert (f'"PROVISIONING_PROFILE_SPECIFIER[sdk=iphoneos*]" = '
                f'"{_app_store_profile_name(bundle)}";') in body, cid
    stray = sorted(
        cid for cid, body in configs.items()
        if cid not in IOS_RELEASE_CONFIGS and any(k in body for k in _DEVICE_SIGNING_KEYS))
    assert not stray, (
        f"device signing keys appear outside the four iOS Release configs: {stray}. "
        f"Debug, simulator and macOS builds keep the project-level ad-hoc '-'."
    )
    assert "CODE_SIGN_STYLE = Automatic" not in pbxproj, (
        "automatic signing needs an Apple account session on the build machine, "
        "which installers/ios.md § Signing forbids."
    )
    assert pbxproj.count("CODE_SIGN_STYLE = Manual;") >= 2, (
        "both project-level configs must keep CODE_SIGN_STYLE = Manual."
    )
