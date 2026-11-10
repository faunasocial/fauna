"""The Windows app as the Microsoft Store installed it: the OS facts that say so,
and the registered layout the mechanism is proven against before the Store serves.

The ruling this module serves is ``docs/goal/architecture/packaging-witnesses.md``
§ Implementation status today, the bullet on how the Windows Store install is
driven. In short:

* **The subject is read from the OS, never inferred.** ``Get-AppxPackage``
  answers for the package ``FaunaSocial.FaunaSocial`` (the reserved identity,
  ``installers/windows.md`` § Identity & coexistence): a Store delivery reports
  ``SignatureKind`` ``Store`` and ``IsDevelopmentMode`` false. A layout registered
  with ``Add-AppxPackage -Register`` carries the same identity, publisher and
  family name and reports neither, which is why both facts are checked
  (:func:`store_subject_problems`).
* **The process carries the identity only when the OS started it as the
  package's application** — by activation (the Start-menu entry, the AUMID
  ``<PackageFamilyName>!FaunaApp``), never by starting the exe from the package's
  folder, which runs with no identity at all (measured on Windows 2026-09-28,
  ``installers/windows.md`` § Store distribution). So the running process's
  package full name is the third fact (:func:`process_identity_problems`), read by
  the bridge off the process it activated.
* **A registered layout is never the witness.** It stands in for the Store
  delivery so the mechanism (activation, the UIA-only drive, the subject checks)
  is proven before the Store serves; the node that colours ``get-the-app``'s
  windows cell refuses it.

Every probe here is a ``pwsh`` call reading or changing this user's own package
catalogue — one set per user, which is why the module that uses this holds the
box's ``real-desktop-session`` lock (``tests/real_session/conftest.py``, the
Windows arm).
"""

from __future__ import annotations

import json
import os
import subprocess
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path

#: The package identity Partner Center reserved — the one name a Store delivery
#: and a locally registered Store layout share (`installers/windows.md`
#: § Identity & coexistence).
IDENTITY_NAME = "FaunaSocial.FaunaSocial"

#: Partner Center's publisher for that identity. The family name's suffix is a hash
#: of it, so a layout packed with any other publisher is a different app.
STORE_PUBLISHER = "CN=E8868D60-047A-46A3-B608-0D4CA88AB791"

#: The family name both derive (`installers/windows.md` § Identity & coexistence):
#: the same on a Store delivery and on a layout registered under the reservation,
#: which is why a family name alone never says which one is installed.
STORE_FAMILY_NAME = "FaunaSocial.FaunaSocial_hra91aebets7r"

#: The manifest's `Application Id` (`apps/fauna-windows/installer/store/
#: AppxManifest.xml.in`), the half of the AUMID after the `!`.
APPLICATION_ID = "FaunaApp"

#: The manifest's `Executable`, relative to the package's install location.
EXECUTABLE_RELPATH = Path("App") / "FaunaApp.exe"

#: The flavour the Store ships from 0.1.6 (`installers/windows.md` § Store
#: distribution), and the recipe that builds its layout under the real identity.
STORE_FLAVOUR = "store-no-payments"
STORE_LAYOUT_RECIPE = (
    f'just windows-store-package {{arch}} {STORE_FLAVOUR} {IDENTITY_NAME} "{STORE_PUBLISHER}"'
)

#: The architectures `windows-store-package` builds a layout for, as a package full
#: name spells them. An x64 layout registers and runs under emulation on an Arm
#: box, which is the x64 leg of the Store witness (`packaging-witnesses.md`
#: § Implementation status today, the bullet on how the Windows Store install is
#: driven).
STORE_ARCHES = ("x64", "arm64")

# ── store-install: what the Store's flavour leaves out, and what it keeps ──────
#
# `docs/features/store-install.md`: the Microsoft Store takes `store-no-payments`
# (`dynamic-features.md` § The store flavors), whose build removes every XAML under
# `Views\Payments\` (`FaunaApp.csproj`, the `FaunaExcisePayments` item group) and
# keeps `Views\P2pShare\`. Each `STORE_ABSENT_*` tuple is asserted ABSENT on the
# screen a full build paints it on, and `test_windows_store_install_pins.py` holds
# every id in them to a `Views\Payments` XAML that spells it, so no absence is of an
# id nothing ever drew. Each tuple is one screen's share of one outcome.

#: Outcome 1: no payment provider to connect and no code to hand out — the own
#: profile's Tiers tab, where a full build hosts both sections …
STORE_ABSENT_PROVIDER_AND_MINT = (
    "subscription-provider-section", "subscription-provider-add-button",
    "subscription-claim-section", "subscription-claim-mint-button",
)
#: … and no code to redeem (Settings → Subscriptions).
STORE_ABSENT_REDEEM = ("subscription-claim-redeem-input", "subscription-claim-redeem-button")
#: Outcome 2, the author half: the tier editor's three money fields.
STORE_ABSENT_TIER_MONEY = (
    "subscription-tier-form-price-hint", "subscription-tier-form-asking-price",
    "subscription-tier-form-payment-url",
)
#: Outcome 2, the buyer half: another author's priced tier, on their Tiers tab.
STORE_ABSENT_OFFER_MONEY = ("subscription-offer-price", "subscription-offer-payment-link")
#: Outcome 3, the author half: the sale controls under compose.
STORE_ABSENT_SELL = ("compose-sell-price", "compose-sell-asking-price", "compose-sell-subscribers-free")
#: Outcome 3, the buyer half: a post sold elsewhere, beside its badge.
STORE_ABSENT_SOLD_POST_MONEY = ("gated-post-price", "gated-post-buy-button", "gated-post-payment-link")
#: Outcome 4: a post card's tip surface …
STORE_ABSENT_TIPS = ("post-tip-total", "post-tip-count", "post-tip-list-button")
#: … and the zap-signer section of Settings → Nostr.
STORE_ABSENT_ZAP_SIGNER = (
    "nostr-zap-signer-add-btn", "nostr-zap-signer-pubkey-input", "nostr-zap-signer-empty",
)

#: Every id asserted absent, for the pin.
STORE_ABSENT = (
    STORE_ABSENT_PROVIDER_AND_MINT + STORE_ABSENT_REDEEM + STORE_ABSENT_TIER_MONEY
    + STORE_ABSENT_OFFER_MONEY + STORE_ABSENT_SELL + STORE_ABSENT_SOLD_POST_MONEY
    + STORE_ABSENT_TIPS + STORE_ABSENT_ZAP_SIGNER
)

#: Outcome 5, the Microsoft Store half: the in-person share is KEPT — its two
#: entry buttons on Settings → Folders, and this device's compare code once the
#: share panel opens. Spelled in `Views\P2pShare\`, which this flavour keeps.
STORE_KEPT_IN_PERSON_SHARE = ("offline-share-button", "offline-receive-button", "offline-share-own-code")

#: Ids the outcome nodes read as PRESENT on a store build — the tabs and buttons
#: they click and each screen's anchor. The pin holds each to a XAML outside
#: `Views\Payments\`, since an id only that folder spells is gone from the build.
#: The Settings rail rows they click (`settings-nav-row[<key>]`, composed in
#: `SettingsShellPage.xaml.cs`) are pinned by `STORE_SETTINGS_RAIL_KEYS` instead.
STORE_DRIVEN_IDS = (
    "feed-tab", "profile-tab", "contacts-tab", "settings-tab",
    "profile-posts-tab", "profile-tiers-tab", "subscription-tiers-section",
    "subscription-tier-create-button", "subscription-tier-form-save",
    "subscription-tier-form-cancel", "subscription-mine-section",
    "compose-gate-tier-select", "compose-text-field",
    "contact-actor-id-field", "contact-actor-id-lookup", "contact-actor-id-result",
    "contact-actor-id-open-profile-button", "subscription-offer-row", "gated-post-badge",
    "post-card", "nostr-link-mode", "nostr-unlink-button", "folder-add-button",
    "offline-share-cancel-button",
)

#: The Settings rail rows the outcome nodes open, by key.
STORE_SETTINGS_RAIL_KEYS = ("subscription-settings", "nostr", "folders")

#: How long one catalogue call may take. Registering a ~350 MB loose layout is
#: the slow one; a healthy read answers in about a second.
_PS_BUDGET_S = 600


@dataclass(frozen=True)
class PackageFacts:
    """What this user's package catalogue says about one installed package."""

    name: str
    publisher: str
    family_name: str
    full_name: str
    signature_kind: str
    is_development_mode: bool
    status: str
    install_location: str

    @property
    def aumid(self) -> str:
        """The application's user-model id — what its Start-menu entry activates."""
        return f"{self.family_name}!{APPLICATION_ID}"

    @property
    def executable(self) -> Path:
        return Path(self.install_location) / EXECUTABLE_RELPATH


def _ps(script: str, timeout: float = _PS_BUDGET_S) -> tuple[int, str, str]:
    r = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=timeout,
    )
    return r.returncode, (r.stdout or "").strip(), (r.stderr or "").strip()


def pick_package(raw: dict | list[dict], arch: str | None = None) -> dict:
    """The one package of ``Get-AppxPackage``'s answer the run drives.

    Two architectures of one name can both be installed, and then the answer is a
    list: the package of ``arch`` (default the host's own) is taken, else the first.
    A single package is taken whatever its architecture — the fixtures' own checks
    (``registered_from``, the subject checks) decide whether it is theirs."""
    if not isinstance(raw, list):
        return raw
    wanted = f"_{arch or host_arch()}_"
    return next((r for r in raw if wanted in r.get("PackageFullName", "")), raw[0])


def package_facts(name: str = IDENTITY_NAME, arch: str | None = None) -> PackageFacts | None:
    """This user's installed package ``name``, or ``None`` when there is none;
    when both architectures are installed, the one of ``arch`` (``pick_package``).

    The enums are stringified on the PowerShell side: ``ConvertTo-Json`` writes an
    enum as its number, and ``SignatureKind`` 4 says nothing to a reader of a
    failure message."""
    rc, out, err = _ps(
        f"$p = Get-AppxPackage -Name '{name}'; if ($p) {{ $p | Select-Object Name, Publisher, "
        "PackageFamilyName, PackageFullName, "
        "@{n='SignatureKind';e={\"$($_.SignatureKind)\"}}, IsDevelopmentMode, "
        "@{n='Status';e={\"$($_.Status)\"}}, InstallLocation | ConvertTo-Json -Compress }"
    )
    if rc != 0:
        raise RuntimeError(f"Get-AppxPackage {name} failed (rc={rc}): {err or out}")
    if not out:
        return None
    raw = pick_package(json.loads(out), arch)
    return PackageFacts(
        name=raw["Name"],
        publisher=raw["Publisher"],
        family_name=raw["PackageFamilyName"],
        full_name=raw["PackageFullName"],
        signature_kind=str(raw["SignatureKind"]),
        is_development_mode=bool(raw["IsDevelopmentMode"]),
        status=str(raw["Status"]),
        install_location=raw["InstallLocation"],
    )


def store_subject_problems(facts: PackageFacts) -> list[str]:
    """Every way ``facts`` falls short of a Microsoft Store delivery of the app —
    empty when it is one.

    ``SignatureKind`` alone is not enough and neither is ``IsDevelopmentMode``: a
    registered layout reports ``None`` and development mode, a sideloaded
    developer-signed package reports ``Developer`` without it, and only a package
    the Store signed and delivered reports ``Store`` with development mode off. The
    identity and publisher are the reservation's, so a package of another name
    reaching here is a harness bug, named rather than assumed away."""
    problems = []
    if facts.name != IDENTITY_NAME or facts.publisher != STORE_PUBLISHER:
        problems.append(
            f"the package is {facts.name} by {facts.publisher}, not the reserved "
            f"{IDENTITY_NAME} by {STORE_PUBLISHER}"
        )
    if facts.signature_kind != "Store":
        problems.append(
            f"SignatureKind is {facts.signature_kind!r}, not 'Store' — the Store did not "
            f"sign what is installed"
        )
    if facts.is_development_mode:
        problems.append(
            "IsDevelopmentMode is true — a layout registered with Add-AppxPackage "
            "-Register, never a Store delivery"
        )
    if facts.status != "Ok":
        problems.append(f"the package's Status is {facts.status!r}, not 'Ok'")
    return problems


def process_identity_problems(package_full_name: str | None, facts: PackageFacts) -> list[str]:
    """Why the running app is not running AS ``facts``' package — empty when it is.

    The process's package full name is the OS's answer (``GetPackageFullName`` on
    the process the bridge activated). None means it runs with no identity, which
    is what starting the exe directly from the install location gives."""
    if not package_full_name:
        return [
            "the running app carries no package identity — it was not started as the "
            "package's application (activation), so nothing it does speaks for the install"
        ]
    if package_full_name != facts.full_name:
        return [
            f"the running app carries the identity {package_full_name}, not the installed "
            f"package's {facts.full_name}"
        ]
    return []


def host_arch() -> str:
    """This box's package architecture, as a package full name spells it."""
    native = os.environ.get("PROCESSOR_ARCHITEW6432") or os.environ.get("PROCESSOR_ARCHITECTURE", "")
    return "arm64" if native.upper() == "ARM64" else "x64"


def selected_layout_arch(option: str | None) -> str:
    """The architecture whose staged layout the development-mode nodes register:
    the run's ``--store-layout-arch`` when given, else this box's own.

    The option is the test harness's alone — the app reads nothing of it
    (convention 15)."""
    if not option:
        return host_arch()
    if option not in STORE_ARCHES:
        raise ValueError(
            f"--store-layout-arch {option!r}: windows-store-package builds a layout for "
            f"{' or '.join(STORE_ARCHES)} only"
        )
    return option


def layout_recipe(arch: str) -> str:
    """The recipe that stages ``arch``'s store-no-payments layout under the reserved
    identity — what a fixture tells the reader to run when the stage is missing."""
    return STORE_LAYOUT_RECIPE.format(arch=arch)


#: Words a hypervisor's virtual firmware puts in ``Win32_ComputerSystem``'s
#: Manufacturer or Model (the virtualization host: "the virtualization host ARM Virtual Machine"; Hyper-V:
#: "Virtual Machine"; VMware: "VMware, Inc."; QEMU/KVM; VirtualBox: "innotek GmbH").
_VIRTUAL_MACHINE_WORDS = ("virtual", "parallels", "vmware", "qemu", "kvm", "xen", "innotek")


def machine_is_virtual(manufacturer: str, model: str) -> bool:
    """Whether ``Win32_ComputerSystem``'s Manufacturer and Model name a virtual
    machine. It fails closed: a reading that names no hypervisor, an empty one
    included, is a person's own PC as far as the Store witness's venue is concerned
    (convention 12's Windows arm, the Store-witness exception)."""
    reading = f"{manufacturer} {model}".lower()
    return any(word in reading for word in _VIRTUAL_MACHINE_WORDS)


def this_machine_is_virtual() -> tuple[bool, str]:
    """``machine_is_virtual`` of this box, and the reading it was decided on."""
    rc, out, err = _ps(
        "$c = Get-CimInstance Win32_ComputerSystem; \"$($c.Manufacturer)`t$($c.Model)\""
    )
    if rc != 0:
        return False, f"Win32_ComputerSystem unreadable (rc={rc}): {err or out}"
    manufacturer, _, model = out.partition("\t")
    return machine_is_virtual(manufacturer, model), f"{manufacturer!r} / {model!r}"


def staged_layout_manifest(repo: Path, arch: str | None = None) -> Path:
    """Where ``windows-store-package`` leaves the loose layout it packed — the
    manifest ``Add-AppxPackage -Register`` takes (the packer derives the stage from
    its ``--out-dir``, ``build/installer``: ``scripts/build-store-package.py``)."""
    return repo / "build" / "installer" / "store-stage" / (arch or host_arch()) / "AppxManifest.xml"


def layout_identity(manifest: Path) -> tuple[str, str]:
    """The (Name, Publisher) a staged layout's manifest declares."""
    root = ET.parse(manifest).getroot()
    identity = next(el for el in root.iter() if el.tag.endswith("}Identity") or el.tag == "Identity")
    return identity.get("Name", ""), identity.get("Publisher", "")


def register_layout(manifest: Path, arch: str | None = None) -> PackageFacts:
    """Register a staged loose layout for this user (deployment path 3 of
    ``tests/real_session/test_store_package_registration.py``: Developer Mode, no
    signing, no elevation) and return what the catalogue then says about the
    package of ``arch`` (default the host's own), the layout's architecture."""
    rc, out, err = _ps(f"$ErrorActionPreference='Stop'; Add-AppxPackage -Register '{manifest}'")
    if rc != 0:
        raise RuntimeError(f"Add-AppxPackage -Register {manifest} failed (rc={rc}):\n{out}\n{err}")
    facts = package_facts(arch=arch)
    if facts is None:
        raise RuntimeError(
            f"Add-AppxPackage -Register {manifest} reported success but no {IDENTITY_NAME} "
            "is installed"
        )
    return facts


def remove_package(name: str = IDENTITY_NAME) -> None:
    """Remove this user's ``name`` package — which also stops its processes and
    deletes the package's own (virtualized) application data."""
    _ps(f"Get-AppxPackage -Name '{name}' | Remove-AppxPackage -ErrorAction SilentlyContinue")


def registered_from(facts: PackageFacts, manifest: Path) -> bool:
    """Whether ``facts`` is the registration of exactly this staged layout — a
    registered layout's install location is the folder it was registered from."""
    try:
        return Path(facts.install_location).resolve() == manifest.parent.resolve()
    except OSError:
        return False
