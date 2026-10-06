"""Per-section fingerprint of WebAssembly modules — WHERE two builds of a chunk differ.

release-integrity.md § Release signing → Web-app verifiability: when
`just web-verify` reports a `*_bg.wasm` chunk as changed, a whole-file hash
says only THAT the bytes differ. This prints, per module, its size and every
section's id, (custom) name, size and a short SHA-256, so two runs — the image
build's (the `web-tree-reproducibility.yml` probe prints it) and a local
`just wasm` of the same commit — diff section by section: a changed `code`
section with a changed size points at the optimizer (wasm-pack's binaryen
`wasm-opt`), a changed `data` section at embedded strings. It also decodes the
`producers` custom section (the tool versions the module records) and lists any
absolute path the module embeds outside the remap placeholders (`/fauna`,
`/cargo`, `/rustc/<hash>`) — a build-environment path that escaped the remap.

Usage: wasm-sections.py <file-or-dir> [...]   (a directory: every *.wasm under it)

Stdlib only; run as `uv run --no-project python scripts/wasm-sections.py`.
"""

import hashlib
import re
import sys
from pathlib import Path

SECTION_NAMES = {
    0: "custom", 1: "type", 2: "import", 3: "function", 4: "table", 5: "memory",
    6: "global", 7: "export", 8: "start", 9: "element", 10: "code", 11: "data",
    12: "datacount", 13: "tag",
}

# An absolute path a build environment could leak. The remap placeholders
# (`/fauna/…`, `/cargo/…`) and rustc's own `/rustc/<hash>/…` are expected and
# are not matched: none of them starts with one of these roots, and a root
# segment INSIDE one (`/cargo/registry/src/…`) is not a path start.
LEAKED_PATH = re.compile(
    rb"(?<![\w.\-/])/(?:home|root|usr|src|work|tmp|build|runner|opt|var)/[\x21-\x7e]{1,100}"
)
MAX_LEAKS = 20


def _uleb(buf: bytes, pos: int) -> tuple[int, int]:
    value = shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, pos
        shift += 7


def _name(buf: bytes, pos: int) -> tuple[str, int]:
    length, pos = _uleb(buf, pos)
    return buf[pos:pos + length].decode("utf-8", "replace"), pos + length


def _producers(payload: bytes) -> list[str]:
    """`field: name version, …` per field of a `producers` custom section."""
    out = []
    fields, pos = _uleb(payload, 0)
    for _ in range(fields):
        field, pos = _name(payload, pos)
        count, pos = _uleb(payload, pos)
        values = []
        for _ in range(count):
            name, pos = _name(payload, pos)
            version, pos = _name(payload, pos)
            values.append(f"{name} {version}".strip())
        out.append(f"{field}: {', '.join(values)}")
    return out


def describe(path: Path, label: str) -> list[str]:
    buf = path.read_bytes()
    lines = [f"{label}  size={len(buf)}  sha256={hashlib.sha256(buf).hexdigest()[:16]}"]
    if buf[:4] != b"\0asm":
        return lines + ["  not a WebAssembly module"]
    pos, index = 8, 0
    while pos < len(buf):
        section_id = buf[pos]
        size, body = _uleb(buf, pos + 1)
        payload = buf[body:body + size]
        kind = SECTION_NAMES.get(section_id, f"id{section_id}")
        if section_id == 0:
            custom, start = _name(payload, 0)
            kind = f"custom:{custom}"
            if custom == "producers":
                lines.extend(f"  producers {p}" for p in _producers(payload[start:]))
        lines.append(f"  [{index:2}] {kind:<28} size={size:<10} sha256={hashlib.sha256(payload).hexdigest()[:16]}")
        pos, index = body + size, index + 1
    leaks = sorted({m.decode("ascii") for m in LEAKED_PATH.findall(buf)})
    for leak in leaks[:MAX_LEAKS]:
        lines.append(f"  path {leak}")
    if len(leaks) > MAX_LEAKS:
        lines.append(f"  path … {len(leaks) - MAX_LEAKS} more")
    return lines


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__.split("\n\n")[2], file=sys.stderr)
        return 2
    for arg in argv:
        root = Path(arg)
        files = sorted(root.rglob("*.wasm")) if root.is_dir() else [root]
        for f in files:
            label = f.relative_to(root).as_posix() if root.is_dir() else f.name
            print("\n".join(describe(f, label)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
