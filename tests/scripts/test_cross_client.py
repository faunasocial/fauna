"""Tests for scripts/lint_cross_client.py cross-app pattern detection."""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))
from lint_cross_client import check_markers_in_window, load_catalog, scan_file, scan_client


def test_check_markers_all_present_in_window():
    """All markers within 30 lines -> match."""
    lines = [""] * 5
    lines.append("let actor_id = get_actor();")
    lines.append("let timestamp = now();")
    lines.append("let bytes = timestamp.to_be_bytes();")
    lines.append("let sig = key.sign(&msg);")
    lines.extend([""] * 5)

    result = check_markers_in_window(
        lines, ["actor_id", "timestamp", "to_be_bytes", "sign"], window_size=30
    )
    assert result is not None
    start, end = result
    assert start >= 1  # 1-indexed
    assert end >= start
    assert end - start + 1 <= 30


def test_check_markers_missing_one():
    """Missing marker -> None."""
    lines = [
        "let actor_id = get_actor();",
        "let timestamp = now();",
        "let bytes = timestamp.to_be_bytes();",
        # Missing "sign"
    ]
    result = check_markers_in_window(
        lines, ["actor_id", "timestamp", "to_be_bytes", "sign"], window_size=30
    )
    assert result is None


def test_check_markers_spread_too_wide():
    """Markers spread over 40 lines, window=30 -> None."""
    lines = [""] * 50
    lines[0] = "let actor_id = get_actor();"
    lines[10] = "let timestamp = now();"
    lines[35] = "let bytes = timestamp.to_be_bytes();"
    lines[45] = "let sig = key.sign(&msg);"

    result = check_markers_in_window(
        lines, ["actor_id", "timestamp", "to_be_bytes", "sign"], window_size=30
    )
    assert result is None


def test_scan_file_detects_inlined_pattern():
    """Temp file with all markers -> match."""
    content = """\
fn authenticate(key: &SigningKey) {
    let actor_id = hex::encode(key.verifying_key().as_bytes());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut msg = Vec::new();
    msg.extend_from_slice(actor_id.as_bytes());
    msg.extend_from_slice(&timestamp.to_be_bytes());
    let signature = key.sign(&msg);
    send_request(actor_id, timestamp, signature);
}
"""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".rs", delete=False) as f:
        f.write(content)
        f.flush()
        result = scan_file(
            Path(f.name),
            ["actor_id", "timestamp", "to_be_bytes", "sign"],
            window_size=30,
        )
    assert result is not None
    start, end = result
    assert start >= 1
    assert end >= start


def test_scan_file_no_match():
    """Temp file with some markers -> None."""
    content = """\
fn do_something() {
    let actor_id = "abc";
    let result = compute(actor_id);
    println!("{}", result);
}
"""
    with tempfile.NamedTemporaryFile(mode="w", suffix=".rs", delete=False) as f:
        f.write(content)
        f.flush()
        result = scan_file(
            Path(f.name),
            ["actor_id", "timestamp", "to_be_bytes", "sign"],
            window_size=30,
        )
    assert result is None
