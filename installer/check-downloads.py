#!/usr/bin/env python3
"""The downloads table's release check (stdlib only).

`installer/downloads.json` is the table fauna.social/apps renders: each cell
links a released file. A release workflow runs this over the directory of
files it is about to publish, just before it publishes them:

    check-downloads.py installer/downloads.json <workflow> <dir> [<version>]

and it refuses, by name, in both directions:

  * a file the table names for this workflow that is not in <dir> — the page
    would link a file the release does not carry;
  * a file in <dir> that the table neither names nor lists under
    `not_listed[<workflow>]` — a new download the page would not offer.

Each workflow checks only its own cells (a cell's `workflow`), because two
workflows publish into the same GitHub Release.

A cell's `file` may carry the placeholder `{version}` for a file whose name
includes the release version (`Fauna-{version}.dmg`), so the table is never
retyped per release; the page fills it with the version it links, and this check
with <version>, the one the workflow is publishing.
"""

import fnmatch
import json
import sys
from pathlib import Path


def check(manifest: dict, workflow: str, files: list[str],
          version: str | None = None) -> list[str]:
    if manifest.get("schema") != 1:
        return [f"downloads.json schema {manifest.get('schema')!r} is not 1"]
    columns = {c["id"] for c in manifest.get("columns", [])}
    named = set()
    errors = []
    for row in manifest.get("rows", []):
        for column, cells in row.get("cells", {}).items():
            if column not in columns:
                errors.append(f"row {row.get('label')!r}: column {column!r} is not in `columns`")
            for cell in cells:
                has_file, has_href = "file" in cell, "href" in cell
                if has_file == has_href:
                    errors.append(f"row {row.get('label')!r} / {column}: a cell links "
                                  "exactly one of `file` or `href`")
                if has_file and cell.get("workflow") == workflow:
                    if "{version}" not in cell["file"]:
                        named.add(cell["file"])
                    elif version is None:
                        errors.append(f"{cell['file']}: the cell names a {{version}} file, "
                                      "but no version was given to the check")
                    else:
                        named.add(cell["file"].replace("{version}", version))
    skipped = manifest.get("not_listed", {}).get(workflow, [])
    present = set(files)
    for name in sorted(named - present):
        errors.append(f"{name}: the table links it, but {workflow} is not publishing it — "
                      "drop the cell or build the file")
    for name in sorted(present - named):
        if not any(fnmatch.fnmatchcase(name, pattern) for pattern in skipped):
            errors.append(f"{name}: {workflow} publishes it, but the table neither links "
                          "it nor lists it under not_listed — add its cell or its pattern")
    return errors


def main(argv: list[str]) -> int:
    if len(argv) not in (4, 5):
        print("usage: check-downloads.py <downloads.json> <workflow> <dir> [<version>]",
              file=sys.stderr)
        return 2
    manifest = json.loads(Path(argv[1]).read_text(encoding="utf-8"))
    files = sorted(p.name for p in Path(argv[3]).iterdir() if p.is_file())
    errors = check(manifest, argv[2], files, argv[4] if len(argv) == 5 else None)
    for line in errors:
        print(f"downloads: {line}")
    if errors:
        print(f"downloads: {len(errors)} error(s) — installer/downloads.json is the table "
              "fauna.social/apps shows")
        return 1
    print(f"downloads: the table and the {len(files)} files {argv[2]} publishes agree")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
