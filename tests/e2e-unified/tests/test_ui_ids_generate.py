"""tier_1: the element-id constant generator's three load-bearing properties.

Pure in-process analysis of `scripts/ui-ids-generate.py` against the real
`ui.yaml`. No nest binary, no driver, no build.

Owner doc: `docs/goal/architecture/build-system.md` § Generated element-id constants.

What each test guards, and why it exists rather than being assumed:

  * **the collision guard actually fires.** Two ids differing only where a separator
    was (`a-b_c` vs `a-b-c`) derive one constant name, so one of them would silently
    vanish from every app's constant set while the lint kept demanding it. ui.yaml
    already contained one such pair when the generator was written — the onboarding
    page `launch_instance_chooser` against the registry element
    `launch-instance-chooser` — so this is a live shape, not a hypothetical. The
    guard is red-verified here against a synthetic pair; asserting only that today's
    real ui.yaml is collision-free would pass just as well with the guard deleted.

  * **every declared element id reaches every platform.** The generator's whole
    purpose is that an app can render any declared id through a constant; a target
    silently missing ids is the failure that would send an app back to literals for
    exactly the ids nobody noticed.

  * **page/onboarding block names stay out.** `collect_declared` deliberately also
    returns top-level `pages:`/`onboarding:` keys, which are block *names*
    (`feed`, `dns_config`) rather than elements. Emitting them would be junk surface,
    and it is what creates the collision above.
"""

import importlib.util
import os
import re

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_SCRIPT = os.path.join(_REPO, "scripts", "ui-ids-generate.py")


@pytest.fixture(scope="module")
def gen():
    spec = importlib.util.spec_from_file_location("ui_ids_generate", _SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="module")
def doc(gen):
    """ui.yaml as the generator itself loads it.

    Split out because `load_ids` takes the parsed doc rather than reading the
    file itself — the same two-step `main()` does (`doc = load_ui_doc()` then
    `ids = load_ids(doc)`). Calling `load_ids()` bare raised TypeError at
    fixture setup, which errored out every test in this module.
    """
    return gen.load_ui_doc()


@pytest.fixture(scope="module")
def ids(gen, doc):
    return gen.load_ids(doc)


def test_the_real_ui_yaml_declares_ids_and_all_are_element_shaped(gen, ids):
    assert len(ids) > 1000, f"suspiciously few ids collected: {len(ids)}"
    assert ids == sorted(ids), "output must be deterministic — sorted"
    assert len(set(ids)) == len(ids), "duplicate ids in the collected set"
    for element_id in ids:
        assert gen.KEBAB.fullmatch(element_id), f"not element-shaped: {element_id}"


def test_page_and_onboarding_block_names_are_excluded(gen, ids):
    """The 43 snake_case block names `collect_declared` returns are not elements."""
    for block_name in ("feed", "devices", "conversations", "dns_config", "launch_instance_chooser"):
        assert block_name not in ids, f"block name leaked into the id set: {block_name}"
    # …and the element it would have collided with is present under its own name.
    assert "launch-instance-chooser" in ids


def test_the_collision_guard_fires_on_a_separator_only_difference(gen):
    """Red-verify: without the guard these two collapse into one constant."""
    colliding = ["nest-place-editor", "nest-place_editor"]
    assert gen.scream(colliding[0]) == gen.scream(colliding[1])
    with pytest.raises(SystemExit) as excinfo:
        gen.check_collisions(colliding)
    message = str(excinfo.value)
    assert "nest-place-editor" in message and "nest-place_editor" in message, (
        "the error must name BOTH ids — naming only the derived constant leaves the "
        f"reader to search ui.yaml for the pair: {message}"
    )


def test_todays_real_id_set_is_collision_free(gen, ids):
    gen.check_collisions(ids)


@pytest.fixture(scope="module")
def gated(gen, doc, ids):
    """ui.yaml's `gated_features:` resolved to ids — the emitters' second argument."""
    return gen.load_gated(doc, ids)


@pytest.fixture(scope="module")
def emitted(gen, ids, gated):
    """`target name -> [(path, content)]`, exactly as `main()` drives the emitters.

    The shape this module used to assume — `TARGETS[name] == (path, emit)` with
    `emit(ids)` returning one string — predates gated features. `TARGETS` now
    maps a name straight to the emitter, and an emitter returns a LIST of
    `(path, content)` pairs because one target can write several files. Deriving
    it once here keeps the drift in a single place next time.
    """
    return {name: emit(ids, gated) for name, emit in gen.TARGETS.items()}


def test_every_declared_id_reaches_every_platform_target(ids, emitted):
    """Every id must reach each platform as a literal — no silent drops.

    Joined across a target's files rather than checked per file: a gated id can
    legitimately live in a different file from the ungated bulk, so per-file
    would fail on a correct split.
    """
    for name, files in emitted.items():
        blob = "\n".join(content for _, content in files)
        missing = [i for i in ids if f'"{i}"' not in blob and f"'{i}'" not in blob]
        assert not missing, f"{name} dropped {len(missing)} ids, e.g. {missing[:5]}"


def test_generated_files_on_disk_are_in_sync_with_ui_yaml(emitted):
    """`just check-generated` covers this too; pinning it here fails faster and locally."""
    stale = []
    for files in emitted.values():
        for path, content in files:
            out = os.path.join(_REPO, path)
            if not os.path.exists(out):
                stale.append(f"{path} (missing)")
                continue
            with open(out, encoding="utf-8") as handle:
                if handle.read() != content:
                    stale.append(path)
    assert not stale, f"run `just ui-ids-generate` — out of date: {stale}"


def test_every_target_carries_the_do_not_edit_banner(emitted):
    for name, files in emitted.items():
        for path, content in files:
            first_line = content.splitlines()[0]
            assert "DO NOT EDIT" in first_line, (
                f"{name} ({path}) lost its banner: {first_line!r}"
            )


def test_derived_names_are_valid_identifiers_in_each_convention(gen, ids):
    ident = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
    for element_id in ids:
        for derive in (gen.scream, gen.camel, gen.pascal):
            name = derive(element_id)
            assert ident.fullmatch(name), f"{element_id} derived an invalid identifier: {name}"
