"""The Store upload is built by a PRODUCTION recipe, and the packer refuses anything else.

`docs/goal/architecture/release-integrity.md` § Release signing → *A store upload is
built from a recorded public commit* requires the Microsoft Store package to be built
from a clean public clone. Until this file, the only builder in the tree that staged a
Store payload was the MSI test helper (`test_installer.py::_build_msi`), which
compiles the e2e automation surface in on purpose (`-p:FaunaE2eAgent=true` over the
`windows-ffi-test` bindings) because the installed-MSI journey tests drive the app
through it. A Store payload staged that way would hand every user the test agent —
convention 15 (`e2e-automation-surface-gating.md`) — and the packer's default payload
root pointed straight at that stage.

So three things are pinned here, all by text and in-process calls (no build, no
`makeappx`, every machine):

1. `just windows-store-package` builds the shipped flavour — production FFI, no
   `FaunaE2eAgent`, self-contained, each Rust package in its own cargo invocation —
   into a stage of its own, and wipes the app's `bin/` + `obj/` first because a
   flavour flip touches no source file (the trap `windows-store-safe-check` found).
2. The packer's default payload root is that production stage, never the MSI's.
3. The packer refuses a payload that carries the automation surface or lacks the
   native core, unless a caller explicitly opts into a test-flavour payload.

The packing half (does `makeappx` accept the result) is `test_store_package_pack.py`.
"""

import importlib.util
import os
import re
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[5]
_JUSTFILE = _REPO / "justfile"
_PACKER = _REPO / "scripts" / "build-store-package.py"
_RECIPE = "windows-store-package"


def _recipe_body(name: str) -> str:
    """The indented body of justfile recipe `name`, delegates resolved.

    A body whose last non-blank line is a `[{{slot_build}}] just _<name> [args]` call
    resolves through it, so a pin reads the content the slot-lease split moved into
    the delegate. Adapted from `test_ffi_flavor_split.py::_recipe_body` (which takes
    no delegate arguments) — duplicated per this suite's no-cross-import-between-
    test-files convention.
    """
    text = _JUSTFILE.read_text(encoding="utf-8")
    m = re.search(rf"^{re.escape(name)}(?:\s+[^:]*)?:.*\n((?:[ \t]+.*\n|\n)*)", text, re.MULTILINE)
    assert m, f"no recipe named {name!r} in the justfile"
    body = m.group(1)
    lines = [line for line in body.splitlines() if line.strip()]
    if lines:
        delegate = re.match(r"^\s*(?:\{\{slot_build\}\})?\s*just\s+(_\S+)(?:\s+.*)?$", lines[-1])
        if delegate:
            prefix = "\n".join(lines[:-1])
            return (prefix + "\n" if prefix else "") + _recipe_body(delegate.group(1))
    return body


def _code_lines(body: str) -> list[str]:
    """The recipe's executable lines — comments carry the trap names on purpose."""
    return [l for l in body.splitlines() if l.strip() and not l.strip().startswith("#")]


@pytest.fixture(scope="module")
def packer():
    spec = importlib.util.spec_from_file_location("build_store_package", _PACKER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---------------------------------------------------------------------------
# 1. The recipe builds the shipped flavour
# ---------------------------------------------------------------------------

def test_recipe_never_builds_the_test_flavour():
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    for forbidden in ("FaunaE2eAgent", "windows-ffi-test", "test-helpers", "Configuration=Debug"):
        assert forbidden not in code, (
            f"`just {_RECIPE}` names {forbidden!r} — that compiles the e2e automation "
            "surface into the package every Store user installs (convention 15)"
        )


def test_recipe_builds_the_production_ffi_at_the_dist_profile():
    """Installer artifacts build at the size-optimised `dist` profile; `release` is
    the dev/test inner loop (installers/windows.md § Size & build profile)."""
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    for ffi in ("windows-ffi", "windows-ffi-store-safe", "windows-ffi-store-no-payments"):
        assert re.search(rf"\bjust {ffi} dist\b", code), (
            f"`just {_RECIPE}` must stage the production FFI at the dist profile "
            f"(`{ffi} dist` for its flavour) — the C# bindings and fauna_ffi.dll the "
            "app publishes come from that one recipe"
        )
    assert not re.search(r"\bjust windows-ffi(?:-store-safe|-store-no-payments)? release\b", code)


# Each flavour's three settings, the table the public Store workflow's
# `Resolve the flavour` step repeats (test_public_workflows_shape.py pins the two
# against each other). The sync agent's only default feature is `p2p-share`, the
# share plane's ingest half: store-safe drops it, store-no-payments keeps it.
_FLAVOURS = {
    "store-safe": ("windows-ffi-store-safe", "--no-default-features", "-p:FaunaStoreSafe=true"),
    "store-no-payments": ("windows-ffi-store-no-payments", "--no-default-features --features p2p-share",
                          "-p:FaunaExcisePayments=true"),
    "full": ("windows-ffi", "", ""),
}


@pytest.mark.parametrize("flavour", sorted(_FLAVOURS))
def test_recipe_builds_each_flavour_from_one_table(flavour):
    """One case arm per flavour sets the FFI, the agent's features and the app's
    MSBuild property together — a flavour that set two of the three would ship an
    app and an agent from different flavours."""
    body = _recipe_body(_RECIPE)
    arm = re.search(rf"^\s*{re.escape(flavour)}\)\n(.*?);;", body, re.MULTILINE | re.DOTALL)
    assert arm, f"`just {_RECIPE}` has no `{flavour})` arm"
    ffi, agent, publish = _FLAVOURS[flavour]
    text = arm.group(1)
    assert re.search(rf"\bjust {ffi} dist\b", text), f"the {flavour} arm stages no `{ffi}`"
    assert f'AGENT_FEATURES="{agent}"' in text, f"the {flavour} arm's sync agent: {text!r}"
    assert f'PUBLISH_FLAVOR="{publish}"' in text, f"the {flavour} arm's app property: {text!r}"
    validate = re.search(r'case "\$FLAVOR" in\n\s*([^)]+)\) ;;', body)
    assert validate and flavour in validate.group(1).split("|"), (
        f"`just {_RECIPE}` refuses the {flavour} flavour before building it"
    )


def test_recipe_publishes_self_contained_release():
    code = _publish_code()
    for needed in ("-t:Publish", "Configuration=Release", "SelfContained=true",
                   "WindowsAppSDKSelfContained=true"):
        assert needed in code, (
            f"`just {_PUBLISH}`'s app publish lacks {needed!r}: the Store package must "
            "run on a box with no .NET / Windows App SDK runtime installed"
        )


def test_recipe_publishes_with_msbuild_never_the_dotnet_cli():
    """The WinUI app's XAML compiler runs in-process only under MSBuild.exe; under
    `dotnet publish` it is the out-of-process net472 XamlCompiler.exe, which exited 1
    with no diagnostic on both Store legs of the 0.1.5 dry run (run 39492438668)."""
    code = _publish_code()
    assert "dotnet publish" not in code, f"`just {_PUBLISH}` must publish with MSBuild"
    assert re.search(r'^\s*"\{\{msbuild_exe\}\}" [^\n]*-restore -t:Publish\b', code, re.M), (
        f"`just {_PUBLISH}` must run MSBuild.exe -restore -t:Publish"
    )


def test_recipe_builds_agent_and_shell_ext_in_separate_cargo_invocations():
    """One invocation naming several packages unifies their features (release.yml's
    own rule), and the shipped agent must carry only the feature set it asks for."""
    code = _code_lines(_recipe_body(_RECIPE))
    agent = [l for l in code if "-p fauna-sync-agent" in l]
    shell = [l for l in code if "-p fauna-shell-ext" in l]
    assert agent and shell, f"`just {_RECIPE}` must build fauna-sync-agent and fauna-shell-ext"
    both = [l for l in code if "-p fauna-sync-agent" in l and "-p fauna-shell-ext" in l]
    assert not both, f"one cargo invocation builds both packages: {both[0].strip()}"
    for line in agent + shell:
        assert "--profile dist" in line and "--locked" in line and "--release" not in line, (
            f"a shipped binary is a `--profile dist --locked` build "
            f"(installers/windows.md § Size & build profile): {line.strip()}"
        )


_PUBLISH = "windows-app-publish"


def _publish_code() -> str:
    """The app publish `just windows-store-package` runs: its call of the one
    publish recipe (the MSI's app is published by it too), with that recipe's
    executable lines."""
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    assert re.search(rf'^\s*just {_PUBLISH} "\$ARCH" "\$PUBLISH_FLAVOR"\s*$', code, re.M), (
        f"`just {_RECIPE}` must publish the app through `just {_PUBLISH}`, in the "
        "flavour's MSBuild property"
    )
    return "\n".join(_code_lines(_recipe_body(_PUBLISH)))


def test_recipe_wipes_the_app_build_before_publishing():
    """A flavour flip touches no source file, so an incremental publish can hand back
    the PREVIOUS flavour's assembly — the MSI helper's e2e-agent build, sitting in
    the very same `bin/Release/<tfm>/<rid>/publish` directory."""
    code = _publish_code()
    for sub in ("bin", "obj"):
        assert re.search(rf"rm -rf [^\n]*FaunaApp/FaunaApp/{sub}\b", code) or \
               re.search(rf"rm -rf [^\n]*\$APP/{sub}\b", code), (
            f"`just {_RECIPE}` must wipe the app's {sub}/ before the publish"
        )


def test_recipe_stages_its_own_payload_and_packs_from_it():
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    assert "build/installer/stage/" not in code, (
        f"`just {_RECIPE}` touches the MSI's stage dir, which the test-flavour MSI "
        "helper writes — the Store payload needs a stage of its own"
    )
    assert "scripts/build-store-package.py" in code and "--payload-root" in code, (
        f"`just {_RECIPE}` must hand its own stage to the packer explicitly"
    )
    assert "--allow-test-surface" not in code, (
        f"`just {_RECIPE}` must never opt out of the packer's automation-surface check"
    )


def test_recipe_stages_the_licence_files_with_its_flavours_own_notices():
    """The package root carries LICENSE-MIT.txt, LICENSE-APACHE.txt and the notices
    of this flavour's closure (shipped-notices.md § Per channel, the Microsoft
    Store row), written after the publish whose restore wrote project.assets.json."""
    import tomllib

    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    assert 'just shipped-licences "$STAGE" .txt' in code
    # one package per arch: its notices are resolved for the triple it builds
    assert re.search(r'shipped-notices\.py "\$NOTICES_ARTIFACT" "\$STAGE/THIRD-PARTY-NOTICES\.txt" --target "\$TARGET"', code)
    assert code.index("just windows-app-publish") < code.index("shipped-notices.py") < code.index("build-store-package.py")
    arms = dict(re.findall(r"^\s*([\w-]+)\)\s+NOTICES_ARTIFACT=([\w-]+)\s*;;", code, re.M))
    assert set(arms) == {"full", "store-safe", "store-no-payments"}, arms
    artifacts = tomllib.loads((_REPO / "installer" / "shipped-notices.toml").read_text(encoding="utf-8"))["artifact"]
    for flavour, artifact in arms.items():
        assert artifacts[artifact]["channel"] == "Microsoft Store package (MSIX)", (flavour, artifact)
    assert len(set(arms.values())) == 3, "two flavours share one closure's notices"


@pytest.mark.parametrize("missing", ["LICENSE-MIT.txt", "LICENSE-APACHE.txt", "THIRD-PARTY-NOTICES.txt"])
def test_stage_refuses_a_payload_without_its_licence_files(packer, tmp_path, missing):
    files = {k: v for k, v in _CLEAN.items() if k != missing}
    root = _payload(tmp_path / "payload", files)
    with pytest.raises(SystemExit) as exc:
        packer._stage(str(tmp_path / "pack"), str(root), "<Package/>")
    assert missing in str(exc.value)


@pytest.mark.parametrize("arch,rid", [("arm64", "win-arm64"), ("x64", "win-x64")])
def test_recipe_handles_both_store_architectures(arch, rid):
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    assert arch in code and rid in code, (
        f"`just {_RECIPE}` has no {arch} leg (RID {rid}); a Store release ships both "
        "desktop architectures (installers/windows.md § Store distribution)"
    )


# ---------------------------------------------------------------------------
# 2. The packer's default payload is the production stage
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("arch", ["arm64", "x64"])
def test_packer_default_payload_root_is_not_the_msi_test_stage(packer, arch):
    root = os.path.normpath(packer._default_payload_root(arch))
    msi_stage = os.path.normpath(os.path.join(str(_REPO), "build", "installer", "stage"))
    assert not root.startswith(msi_stage), (
        f"the packer's default payload root {root} is the MSI's stage, which the "
        "test-flavour MSI helper writes; a bare packer run would package the e2e agent"
    )
    code = "\n".join(_code_lines(_recipe_body(_RECIPE)))
    rel = os.path.relpath(root, str(_REPO)).replace(os.sep, "/")
    assert rel.replace(f"/{arch}", "/") in code.replace("$ARCH", "").replace("{{arch}}", ""), (
        f"the packer's default payload root ({rel}) is not where `just {_RECIPE}` "
        "stages — the two must name one directory"
    )


# ---------------------------------------------------------------------------
# 3. The packer refuses a test-flavour or incomplete payload
# ---------------------------------------------------------------------------

def _payload(root: Path, files: dict[str, bytes]) -> Path:
    for rel, data in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    return root


def _pe_with_exports(names: list[str], data: bytes = b"") -> bytes:
    """A minimal PE32+ image: one section holding an export directory that names
    `names`, plus `data` as non-exported bytes. Enough for an export-table reader;
    nothing ever loads it."""
    import struct
    sect_rva, sect_raw = 0x1000, 0x200
    body = bytearray(40 + 4 * len(names))            # export directory + AddressOfNames
    name_rvas = []
    for name in names:
        name_rvas.append(sect_rva + len(body))
        body += name.encode() + b"\0"
    body += data
    struct.pack_into("<I", body, 24, len(names))      # NumberOfNames
    struct.pack_into("<I", body, 32, sect_rva + 40)   # AddressOfNames
    for i, rva in enumerate(name_rvas):
        struct.pack_into("<I", body, 40 + 4 * i, rva)
    dos = bytearray(64)
    dos[0:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 64)
    coff = struct.pack("<HHIIIHH", 0xAA64, 1, 0, 0, 0, 240, 0x2022)
    opt = bytearray(240)
    struct.pack_into("<H", opt, 0, 0x20B)             # PE32+
    struct.pack_into("<I", opt, 108, 16)              # NumberOfRvaAndSizes
    struct.pack_into("<II", opt, 112, sect_rva, 40)   # data directory 0: exports
    section = struct.pack("<8sIIIIIIHHI", b".edata", len(body), sect_rva, len(body),
                          sect_raw, 0, 0, 0, 0, 0x40000040)
    headers = bytes(dos) + b"PE\0\0" + coff + bytes(opt) + section
    return headers.ljust(sect_raw, b"\0") + bytes(body)


# What real Windows builds measured 2026-10-07, kept as stand-ins: the production
# assembly carries an UNGATED member named StageImageFromTestAgent (so a bare
# `TestAgent` substring is the wrong marker), and the production fauna_ffi.dll
# carries seam NAMES in its data with zero `for_test` EXPORTS (the test flavour
# exports 135) — so the FFI marker must read the export table, not raw bytes.
_CLEAN = {
    "App/FaunaApp.exe": b"MZ apphost",
    "App/FaunaApp.dll": b"MZ\0FaunaApp.Views\0MainPage\0StageImageFromTestAgent\0",
    "App/fauna_ffi.dll": _pe_with_exports(
        ["uniffi_fauna_ffi_fn_method_nestclient_connect"],
        data=b"Self::clear_for_test\0inject_inbound_for_test\0"),
    "fauna-sync-agent.exe": b"MZ agent",
    "fauna_shell.dll": b"MZ shell",
    "LICENSE-MIT.txt": b"stand-in",
    "LICENSE-APACHE.txt": b"stand-in",
    "THIRD-PARTY-NOTICES.txt": b"stand-in",
}
_TEST_AGENT_DLL = b"MZ\0FaunaApp.Testing\0TestAgent\0PaintedErrorObserver\0"


def test_surface_scan_passes_a_production_payload(packer, tmp_path):
    root = _payload(tmp_path, _CLEAN)
    assert packer.automation_surface_hits(str(root)) == []


def test_surface_scan_flags_the_test_agent_in_the_app_assembly(packer, tmp_path):
    root = _payload(tmp_path, {**_CLEAN, "App/FaunaApp.dll": _TEST_AGENT_DLL})
    hits = packer.automation_surface_hits(str(root))
    assert any("FaunaApp.dll" in h and "FaunaApp.Testing" in h for h in hits), hits


def test_surface_scan_flags_ffi_test_seam_exports(packer, tmp_path):
    root = _payload(tmp_path, {
        **_CLEAN,
        "App/fauna_ffi.dll": _pe_with_exports([
            "uniffi_fauna_ffi_fn_method_nestclient_connect",
            "UNIFFI_META_FAUNA_CONVERSATIONS_METHOD_CONVERSATIONSMANAGER_INJECT_INBOUND_FOR_TEST",
        ]),
    })
    hits = packer.automation_surface_hits(str(root))
    assert any("fauna_ffi.dll" in h and "INJECT_INBOUND_FOR_TEST" in h for h in hits), hits


def test_pe_export_reader_reads_names_not_data(packer, tmp_path):
    path = tmp_path / "x.dll"
    path.write_bytes(_pe_with_exports(["alpha", "beta"], data=b"gamma_for_test\0"))
    assert packer.pe_export_names(str(path)) == ["alpha", "beta"]
    junk = tmp_path / "junk.dll"
    junk.write_bytes(b"not a PE at all")
    assert packer.pe_export_names(str(junk)) == []


def test_stage_refuses_a_test_flavour_payload(packer, tmp_path):
    root = _payload(tmp_path / "payload", {**_CLEAN, "App/FaunaApp.dll": _TEST_AGENT_DLL})
    with pytest.raises(SystemExit) as exc:
        packer._stage(str(tmp_path / "pack"), str(root), "<Package/>")
    assert "automation surface" in str(exc.value)


def test_stage_takes_a_test_flavour_payload_only_when_asked(packer, tmp_path):
    root = _payload(tmp_path / "payload", {**_CLEAN, "App/FaunaApp.dll": _TEST_AGENT_DLL})
    packer._stage(str(tmp_path / "pack"), str(root), "<Package/>", allow_test_surface=True)
    assert (tmp_path / "pack" / "App" / "FaunaApp.dll").exists()


@pytest.mark.parametrize("missing", ["App/fauna_ffi.dll", "App/FaunaApp.exe"])
def test_stage_refuses_a_payload_without_the_app_or_its_native_core(packer, tmp_path, missing):
    """`FaunaApp.csproj` includes fauna_ffi.dll only `Condition="Exists(...)"`, so a
    publish that ran before the FFI was staged succeeds — and ships an app that dies
    at its first P/Invoke. The packer is the last place that can see the hole."""
    files = {k: v for k, v in _CLEAN.items() if k != missing}
    root = _payload(tmp_path / "payload", files)
    with pytest.raises(SystemExit) as exc:
        packer._stage(str(tmp_path / "pack"), str(root), "<Package/>")
    assert os.path.basename(missing) in str(exc.value)


def test_packer_cli_exposes_the_opt_out_under_an_unmissable_name():
    out = subprocess.run([sys.executable, str(_PACKER), "--help"],
                         capture_output=True, text=True, timeout=60)
    assert out.returncode == 0, out.stderr
    assert "--allow-test-surface" in out.stdout
