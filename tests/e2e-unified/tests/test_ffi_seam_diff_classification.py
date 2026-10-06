"""Classification rules of the flavor-diff seam witness's difference-coverage
assert (`scripts/check-ffi-seam-diff.py`; e2e-automation-surface-gating.md
§ Implementation status today, the binding-face witness bullet).

The assert reads every declaration present only in the test-flavored binding
face as a seam that owes a witness. Some of those declarations are not seams at
all — a member of a `*ForTest` type with a generic name (`json()`), the
`FooForTestInterface` companion, uniffi's `FfiConverter*` lowering objects, and
pure-data records/enums that a seam takes or returns — and until 2026-10-06 the
first android release run in weeks reported every one of them as an
unwitnessed seam. These fixtures pin both halves: those classes are accounted
for, and a real non-test-named callable still fails.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SCRIPT = _REPO / "scripts" / "check-ffi-seam-diff.py"


def _floor() -> list[dict[str, str]]:
    spec = importlib.util.spec_from_file_location("check_ffi_seam_diff", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.KNOWN_SEAMS


def _camel(snake: str) -> str:
    head, *rest = snake.split("_")
    return head + "".join(p[:1].upper() + p[1:] for p in rest)


# Production face shared by every case: one product type and function.
_PROD = """\
open class FfiProduct : FfiProductInterface {
    override fun `render`(): kotlin.String { return "" }
}
fun `productCall`(): kotlin.String { return "" }
"""


def _run(tmp_path: Path, extra_test_kotlin: str) -> subprocess.CompletedProcess:
    """Run the witness on a pair whose difference is the floor list, one
    `*ForTest` method, and `extra_test_kotlin`."""
    floor = []
    for e in _floor():
        if e["kind"] == "type":
            floor.append(f"open class {e['name']} {{\n}}")
        else:
            floor.append(f"fun `{_camel(e['name'])}`() {{\n}}")
    test_dir = tmp_path / "test"
    prod_dir = tmp_path / "prod"
    test_dir.mkdir()
    prod_dir.mkdir()
    (prod_dir / "face.kt").write_text(_PROD)
    (test_dir / "face.kt").write_text(
        _PROD
        + "\n".join(floor)
        + "\nfun `injectForTest`() {\n}\n"
        + extra_test_kotlin
    )
    return subprocess.run(
        [
            sys.executable,
            str(_SCRIPT),
            "--lang",
            "kotlin",
            "--test-tree",
            str(test_dir),
            "--production-tree",
            str(prod_dir),
        ],
        capture_output=True,
        text=True,
    )


def test_the_floor_alone_is_fully_accounted(tmp_path):
    r = _run(tmp_path, "")
    assert r.returncode == 0, r.stderr
    assert "difference fully accounted" in r.stdout


def test_members_and_companions_of_a_for_test_type_are_accounted(tmp_path):
    r = _run(
        tmp_path,
        """\
public interface FfiTallyForTestInterface {
    fun `json`(): kotlin.String
    fun `observe`(`frame`: List<FfiPainted>)
}
open class FfiTallyForTest : FfiTallyForTestInterface {
    override fun `json`(): kotlin.String { return "" }
    override fun `observe`(`frame`: List<FfiPainted>) {}
}
""",
    )
    assert r.returncode == 0, r.stderr


def test_a_generic_member_name_also_declared_at_top_level_still_fails(tmp_path):
    """Membership excuses a name only when EVERY declaration of it sits inside
    an accounted type: a top-level `json()` is a free function, a callable of
    its own."""
    r = _run(
        tmp_path,
        """\
open class FfiTallyForTest {
    fun `json`(): kotlin.String { return "" }
}
fun `json`(): kotlin.String { return "" }
""",
    )
    assert r.returncode == 1
    assert "json()" in r.stderr


def test_converters_records_and_enum_variants_are_accounted(tmp_path):
    r = _run(
        tmp_path,
        """\
data class FfiServeTally (
    val `served`: kotlin.ULong
)
sealed class FfiOutcome {
    data class Handled(
        val `resultJson`: kotlin.String?) : FfiOutcome()
    object NotMine : FfiOutcome()
}
enum class FfiMode {
    OPEN;
}
public object FfiConverterTypeFfiServeTally: FfiConverterRustBuffer<FfiServeTally> {
}
public object FfiConverterMapStringULong: FfiConverterRustBuffer<Map<String, ULong>> {
}
""",
    )
    assert r.returncode == 0, r.stderr


def test_a_non_test_named_callable_type_still_fails(tmp_path):
    """The data-type exemption must not reach an object type: an `open class`
    with methods is a callable surface and owes a witness."""
    r = _run(
        tmp_path,
        """\
open class FfiRedirector {
    fun `redirect`(`url`: kotlin.String) {}
}
""",
    )
    assert r.returncode == 1
    assert "ffiredirector" in r.stderr
    assert "redirect()" in r.stderr


def test_a_non_test_named_free_function_still_fails(tmp_path):
    r = _run(tmp_path, "fun `setHiddenOverride`(`on`: kotlin.Boolean) {\n}\n")
    assert r.returncode == 1
    assert "sethiddenoverride()" in r.stderr


def test_a_method_on_a_record_is_held_to_the_witness(tmp_path):
    """A record is data, but a method uniffi exports on it is a callable."""
    r = _run(
        tmp_path,
        """\
data class FfiServeTally (
    val `served`: kotlin.ULong
) {
    fun `resetCounters`() {}
}
""",
    )
    assert r.returncode == 1
    assert "resetcounters()" in r.stderr
