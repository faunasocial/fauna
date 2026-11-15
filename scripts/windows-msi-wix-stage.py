#!/usr/bin/env python3
"""Stage the WiX Toolset install-time code's files for the MSI's notices.

    uv run --no-project python scripts/windows-msi-wix-stage.py <bundled-dir>

Owner: docs/goal/architecture/shipped-notices.md § How the notices file is
produced, the installer-tooling bullet. The MSI carries code of WiX's own —
the custom-action libraries its Util and Firewall extensions put into the
package and the dialog set of its UI extension — so the `windows-msi`
artifact's manifest entry names a `bundled` component, *WiX Toolset
install-time code*, whose `binary` and `files` live under `wix/`. The package
holding that code does not exist when the notices are written, so
`just windows-msi-notices` runs this first: it copies, into
`<bundled-dir>/wix/`, each file the component names, from where the WiX the
build uses keeps it, at the component's version —

  - the Util extension's library from the WiX extension cache: the checkout's
    own `.wix/extensions/` (what `wix extension add` without `-g` writes, as
    release.yml's "Install WiX" step does) or the user profile's (`-g`);
  - `OSMFEULA.txt` from the `wix` tool package in the .NET tool store, beside
    the `wix` on PATH (a `--global` or `--tool-path` install), else under
    DOTNET_CLI_HOME's or the user profile's `.dotnet/tools/`;

The licence's standard text is not staged: the manifest names it as the
component's `standard_texts`, and the generator prints it from
`supply-chain/licence-texts/` itself (the package's own file names the licence
without carrying it).

A file with no source at that version fails the run: a job without WiX at the
pinned version fails here, never with a notices entry whose files are missing.
The files are copied byte for byte and never read; the lines printed name
files and paths only.

Standard library only. Exit 0 = staged; 1 = a source missing; 2 = usage.
"""

from __future__ import annotations

import os
import shutil
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "installer" / "shipped-notices.toml"
ARTIFACT = "windows-msi"
SUBDIR = "wix"


class StageError(RuntimeError):
    pass


def wix_component(manifest: dict) -> dict:
    """The windows-msi artifact's bundled component whose files sit under wix/."""
    comps = manifest.get("artifact", {}).get(ARTIFACT, {}).get("bundled", {}).get("components", [])
    found = [c for c in comps if str(c.get("binary", "")).startswith(f"{SUBDIR}/")]
    if len(found) != 1:
        raise StageError(f"the {ARTIFACT} artifact names {len(found)} bundled components under {SUBDIR}/, not one")
    return found[0]


def sources(name: str, version: str, *, repo: Path, home: Path, wix_on_path: str | None, cli_home: str | None) -> list[Path]:
    """Where a WiX build at `version` keeps the file `name`, in the order read."""
    if name.endswith(".wixext.dll"):
        rel = Path(".wix") / "extensions" / name[: -len(".dll")] / version / "wixext6" / name
        return [repo / rel, home / rel]
    if name == "OSMFEULA.txt":
        store = Path(".store") / "wix" / version / "wix" / version / name
        tools = [Path(wix_on_path).parent] if wix_on_path else []
        if cli_home:
            tools.append(Path(cli_home) / ".dotnet" / "tools")
        tools.append(home / ".dotnet" / "tools")
        return [t / store for t in tools]
    raise StageError(f"no source is known for {SUBDIR}/{name} (an extension library or OSMFEULA.txt)")


def stage(comp: dict, dest: Path, *, repo: Path, home: Path, wix_on_path: str | None, cli_home: str | None) -> list[str]:
    version = comp["version"]
    out = dest / SUBDIR
    lines = []
    for rel in [comp["binary"], *comp.get("files", [])]:
        if not rel.startswith(f"{SUBDIR}/") or "/" in rel[len(SUBDIR) + 1:]:
            raise StageError(f"{rel}: the component's files sit directly under {SUBDIR}/")
        name = rel[len(SUBDIR) + 1:]
        tried = sources(name, version, repo=repo, home=home, wix_on_path=wix_on_path, cli_home=cli_home)
        src = next((p for p in tried if p.is_file()), None)
        if src is None:
            raise StageError(f"no {name} for WiX {version} at: " + "; ".join(str(p) for p in tried))
        out.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, out / name)
        lines.append(f"windows-msi-wix-stage: staged {SUBDIR}/{name} from {src}")
    return lines


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1:
        print(__doc__.split("\n\n")[1].strip(), file=sys.stderr)
        return 2
    dest = Path(argv[0])
    try:
        comp = wix_component(tomllib.loads(MANIFEST.read_text(encoding="utf-8")))
        lines = stage(comp, dest, repo=REPO, home=Path.home(), wix_on_path=shutil.which("wix"),
                      cli_home=os.environ.get("DOTNET_CLI_HOME"))
    except StageError as e:
        print(f"windows-msi-wix-stage: FAILED — {e}")
        return 1
    for ln in lines:
        print(ln)
    print(f"windows-msi-wix-stage: OK — {len(lines)} files of {comp['name']} {comp['version']} in {dest / SUBDIR}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
