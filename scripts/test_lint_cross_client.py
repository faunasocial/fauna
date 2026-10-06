"""Unit tests for scripts/lint_cross_client.py.

Run with: uv run pytest scripts/test_lint_cross_client.py -v
"""
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import lint_cross_client as lcc


def test_check_markers_in_window_all_present():
    lines = ["actor_id field", "timestamp here", "signature line"]
    result = lcc.check_markers_in_window(lines, ["actor_id", "timestamp", "signature"], 30)
    assert result == (1, 3)


def test_check_markers_in_window_one_missing():
    lines = ["actor_id field", "timestamp here"]
    assert lcc.check_markers_in_window(lines, ["actor_id", "timestamp", "signature"], 30) is None


def test_file_with_negative_marker_is_suppressed(tmp_path: Path):
    """If a file contains any negative marker, it should NOT be flagged."""
    file = tmp_path / "fake.rs"
    file.write_text(
        "actor_id timestamp signature\n"
        "fauna_client_core::auth::build_auth_request(&kp)\n"
    )
    hit = lcc.scan_file_with_negatives(
        file,
        markers=["actor_id", "timestamp", "signature"],
        negative_markers=["fauna_client_core::auth::build_auth_request"],
        window_size=30,
    )
    assert hit is None


def test_file_without_negative_marker_is_flagged(tmp_path: Path):
    file = tmp_path / "fake.rs"
    file.write_text(
        "actor_id timestamp signature\n"
        "my own inline sign impl\n"
    )
    hit = lcc.scan_file_with_negatives(
        file,
        markers=["actor_id", "timestamp", "signature"],
        negative_markers=["fauna_client_core::auth::build_auth_request"],
        window_size=30,
    )
    assert hit is not None


def test_exclude_files_regex(tmp_path: Path):
    """Files matching exclude_files regex are skipped before scanning."""
    base = tmp_path / "apps" / "fauna-apple" / "FaunaFFISwift" / "Sources"
    base.mkdir(parents=True)
    generated = base / "FaunaFFI.swift"
    generated.write_text("actor_id timestamp signature sign(\n")

    # Scan should return 0 findings because the generated file is excluded.
    pattern = {
        "id": "auth-body-construction",
        "description": "test",
        "markers": ["actor_id", "timestamp", "signature", "sign"],
        "exclude": [],
        "exclude_files": [r"apps/fauna-apple/FaunaFFISwift/Sources/FaunaFFI\.swift$"],
        "negative_markers": [],
        "severity": "high",
    }
    findings = lcc.scan_client(
        "apple",
        str(tmp_path.relative_to(tmp_path)),
        {".swift"},
        pattern,
        tmp_path,
    )
    assert findings == []
