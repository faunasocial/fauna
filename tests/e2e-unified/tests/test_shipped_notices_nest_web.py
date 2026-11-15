"""tier_1: the nest image and the web app it serves stage their licence files.

shipped-notices.md § Per channel, the Nest image and Web app rows: the image
carries LICENSE-MIT, LICENSE-APACHE and a generated THIRD-PARTY-NOTICES in
``/usr/share/doc/fauna/`` and labels its licences ``MIT OR Apache-2.0``; the web
app serves ``THIRD-PARTY-NOTICES.txt`` beside its entry page, with the two
licence texts. These are static properties of the checked-in Dockerfile and
justfile (as ``test_nest_image_revision_label.py`` reasons): whether the staging
can silently stop needs no image build. The image itself is read by
``tests/platform/docker/test_image_licence_files.py`` (tier_4).

No test here opens a generated notices file: it is dependency-authored text
(shipped-notices.md § How the notices file is produced).
"""

import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)


def _read(rel):
    with open(os.path.join(_REPO_ROOT, rel), encoding="utf-8") as fh:
        return fh.read()


def _stages(text):
    """Stage name → its body (the lines up to the next FROM)."""
    out, name, body = {}, None, []
    for line in text.splitlines():
        m = re.match(r"^FROM\s+(\S+)(?:\s+AS\s+(\S+))?", line)
        if m:
            if name is not None:
                out[name] = "\n".join(body)
            name, body = (m.group(2) or "<final>"), [line]
        elif name is not None:
            body.append(line)
    if name is not None:
        out[name] = "\n".join(body)
    return out


def _final_stage(text):
    return _stages(text)["<final>"]


def _runs(body):
    """Each RUN instruction with its continuation lines joined."""
    runs, cur = [], None
    for line in body.splitlines():
        if cur is not None:
            cur += " " + line.strip()
            if not line.rstrip().endswith("\\"):
                runs.append(cur)
                cur = None
        elif line.startswith("RUN "):
            if line.rstrip().endswith("\\"):
                cur = line.strip()
            else:
                runs.append(line.strip())
    return runs


def test_the_web_notices_stage_generates_the_web_apps_three_files_offline():
    stages = _stages(_read("Dockerfile"))
    assert "web-notices" in stages, "Dockerfile has no web-notices stage"
    body = stages["web-notices"]
    assert body.startswith("FROM rust-wasm AS web-notices")
    gen = [r for r in _runs(body) if "shipped-notices.py web-app" in r]
    assert len(gen) == 1, "web-notices must run the generator for the web-app artifact once"
    run = gen[0]
    assert "--network=none" in run, "the generator RUN must have no network (fetch first, then generate)"
    assert "/notices-web/THIRD-PARTY-NOTICES.txt" in run
    assert "cp LICENSE-MIT /notices-web/LICENSE-MIT.txt" in run
    assert "cp LICENSE-APACHE /notices-web/LICENSE-APACHE.txt" in run
    assert any(r.startswith("RUN") and "cargo fetch --locked" in r for r in _runs(body))


def test_the_nest_notices_stage_covers_the_images_binaries_for_its_own_triple():
    stages = _stages(_read("Dockerfile"))
    assert "nest-notices" in stages, "Dockerfile has no nest-notices stage"
    body = stages["nest-notices"]
    assert "COPY --from=bridges-builder /fauna-mail-bridge /fauna-supervisor /fauna-atproto-bridge bins/fauna-bridges/" in body, (
        "the Go binaries must sit where installer/shipped-notices.toml's nest-image entry names them"
    )
    gen = [r for r in _runs(body) if "shipped-notices.py nest-image" in r]
    assert len(gen) == 1
    run = gen[0]
    assert "--network=none" in run
    assert '--host-target "$triple"' in run, "the stage runs at the build platform; the native groups resolve for the image's triple"
    assert "/notices-nest/THIRD-PARTY-NOTICES" in run
    assert "cp LICENSE-MIT LICENSE-APACHE /notices-nest/" in run


def test_the_manifest_names_the_go_binaries_the_stage_copies():
    manifest = _read("installer/shipped-notices.toml")
    for b in ("fauna-mail-bridge", "fauna-supervisor", "fauna-atproto-bridge"):
        assert f'"bins/fauna-bridges/{b}"' in manifest


def test_the_served_tree_and_the_image_carry_the_files_and_the_label():
    text = _read("Dockerfile")
    stages = _stages(text)
    web_tree = stages["web-tree"]
    assert "COPY --from=web-builder /src/apps/fauna-web/build/ /" in web_tree
    assert "COPY --from=web-notices /notices-web/ /" in web_tree, (
        "the exported web tree (what the asset manifest hashes) must hold the licence files the image serves"
    )
    web_export = stages["web-export"]
    assert "COPY --from=web-tree / /web-tree/" in web_export
    assert "COPY --from=rust-wasm /opt/wasm32-cc/version.txt /wasm32-cc-version.txt" in web_export, (
        "the compiler version is exported beside the tree, never inside it"
    )
    final = _final_stage(text)
    assert "COPY --from=web-export /web-tree/ /usr/share/fauna-web/" in final, (
        "the image must serve exactly the exported web tree, and build the export stage itself"
    )
    assert "COPY --from=nest-notices /notices-nest/ /usr/share/doc/fauna/" in final
    assert re.search(r'org\.opencontainers\.image\.licenses="MIT OR Apache-2\.0"', final), (
        "the image's licences label must read MIT OR Apache-2.0"
    )


def test_the_web_notices_recipe_writes_the_same_three_files_and_web_manifest_runs_it():
    justfile = _read("justfile")
    m = re.search(r"^web-notices tree=\"apps/fauna-web/build\":\n((?:    .*\n|\n)+)", justfile, re.MULTILINE)
    assert m, "the justfile has no web-notices recipe"
    body = m.group(1)
    assert "scripts/shipped-notices.py web-app '{{tree}}/THIRD-PARTY-NOTICES.txt'" in body
    assert "just shipped-licences '{{tree}}' .txt" in body, "the licence texts go through the one helper every channel uses"
    wm = re.search(r"^web-manifest:.*\n((?:    .*\n)+)", justfile, re.MULTILINE)
    assert wm
    lines = [l.strip() for l in wm.group(1).splitlines()]
    assert lines.index("just web-notices") < next(i for i, l in enumerate(lines) if "web-asset-manifest.py write" in l), (
        "the hosted tree's manifest must be written after its licence files, so it names them"
    )
    wv = re.search(r"^web-verify manifest:\n((?:    .*\n)+)", justfile, re.MULTILINE)
    assert wv and "web-notices" in wv.group(1), "a verifier's rebuild must add the licence files a manifest names"
