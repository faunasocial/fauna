"""Tests for scripts/lint_textual_clones.py textual clone detection."""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "scripts"))
from lint_textual_clones import (
    CloneGroup,
    normalize_lines,
    fingerprint_file,
    cluster_clones,
    filter_clones,
    scan_directory,
)


def test_normalize_rust_strips_line_comments():
    lines = ["let x = 5; // comment", "let y = 10; // another"]
    result = normalize_lines(lines, ".rs")
    assert result[0] == "let x = 5;"
    assert result[1] == "let y = 10;"


def test_normalize_rust_strips_block_comments():
    # Inline block comment
    lines_inline = ["let x = /* value */ 5;"]
    result = normalize_lines(lines_inline, ".rs")
    assert result[0] == "let x = 5;"

    # Multiline block comment
    lines_multi = [
        "let x = 5;",
        "/* this is",
        "   a block comment */",
        "let y = 10;",
    ]
    result = normalize_lines(lines_multi, ".rs")
    assert result[0] == "let x = 5;"
    assert result[1] == ""
    assert result[2] == ""
    assert result[3] == "let y = 10;"


def test_normalize_python_strips_comments():
    lines = ["x = 5  # comment", "y = 10"]
    result = normalize_lines(lines, ".py")
    assert result[0] == "x = 5"
    assert result[1] == "y = 10"


def test_normalize_collapses_whitespace():
    lines = ["  let   x  =  5 ;  "]
    result = normalize_lines(lines, ".rs")
    assert result[0] == "let x = 5 ;"


def test_normalize_empty_lines_preserved_as_empty():
    lines = ["let x = 5;", "", "   ", "let y = 10;"]
    result = normalize_lines(lines, ".rs")
    assert result[0] == "let x = 5;"
    assert result[1] == ""
    assert result[2] == ""
    assert result[3] == "let y = 10;"


def test_fingerprint_produces_hashes():
    lines = ["line one", "line two", "line three", "line four", "line five"]
    hashes = fingerprint_file(lines, min_lines=3)
    # 5 lines with window 3 => windows at 0,1,2 => 3 hashes
    assert len(hashes) == 3
    # Each entry is (hash_str, start_line_0indexed)
    for h, start in hashes:
        assert isinstance(h, str)
        assert len(h) == 64  # sha256 hex digest
        assert isinstance(start, int)


def test_cluster_finds_duplicates():
    # Two files with the same 5-line block should produce 1 group
    block = ["fn foo() {", "let x = 1;", "let y = 2;", "let z = 3;", "}"]
    h1 = fingerprint_file(block, min_lines=5)
    h2 = fingerprint_file(block, min_lines=5)

    all_hashes = {}
    for h, start in h1:
        all_hashes.setdefault(h, []).append(("file_a.rs", start))
    for h, start in h2:
        all_hashes.setdefault(h, []).append(("file_b.rs", start))

    groups = cluster_clones(all_hashes, min_lines=5)
    assert len(groups) == 1
    assert len(groups[0].locations) == 2
    files = {loc[0] for loc in groups[0].locations}
    assert files == {"file_a.rs", "file_b.rs"}


def test_cluster_no_false_positive_for_unique():
    block_a = ["fn foo() {", "let x = 1;", "let y = 2;", "let z = 3;", "}"]
    block_b = ["fn bar() {", "let a = 9;", "let b = 8;", "let c = 7;", "}"]
    h1 = fingerprint_file(block_a, min_lines=5)
    h2 = fingerprint_file(block_b, min_lines=5)

    all_hashes = {}
    for h, start in h1:
        all_hashes.setdefault(h, []).append(("file_a.rs", start))
    for h, start in h2:
        all_hashes.setdefault(h, []).append(("file_b.rs", start))

    groups = cluster_clones(all_hashes, min_lines=5)
    assert len(groups) == 0


def test_filter_drops_ignored_patterns():
    group = CloneGroup(
        normalized_lines=["impl Default for MyStruct {", "fn default() -> Self {", "Self {}", "}", "}"],
        locations=[("a.rs", 1, 5), ("b.rs", 10, 14)],
    )
    ignore_patterns = [{"pattern": "impl Default for", "reason": "boilerplate"}]
    result = filter_clones([group], ignore_patterns)
    assert len(result) == 0


def test_filter_keeps_non_ignored():
    group = CloneGroup(
        normalized_lines=["let sig = hex::decode(sig_hex);", "let key = VerifyingKey::from_bytes(&bytes);", "key.verify(&msg, &sig);"],
        locations=[("a.rs", 1, 3), ("b.rs", 10, 12)],
    )
    ignore_patterns = [{"pattern": "impl Default for", "reason": "boilerplate"}]
    result = filter_clones([group], ignore_patterns)
    assert len(result) == 1


def test_end_to_end_with_temp_files():
    block = """\
fn verify_sig(actor: &[u8; 32], ts: u64, sig_hex: &str) {
    let sig_bytes = hex::decode(sig_hex).unwrap();
    let signature = Signature::from_slice(&sig_bytes).unwrap();
    let key = VerifyingKey::from_bytes(actor).unwrap();
    let mut msg = Vec::new();
    msg.extend_from_slice(actor);
}
"""
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        f1 = d / "alpha.rs"
        f2 = d / "beta.rs"
        f1.write_text("// header\n" + block + "\nfn other() {}\n")
        f2.write_text("// different header\n" + block + "\nfn something_else() {}\n")

        findings = scan_directory(
            path=d,
            min_lines=5,
            lang_filter=["rs"],
            ignore_patterns=[],
            root=d,
        )
        assert len(findings) >= 1
        # Check that both files appear in the first finding's locations
        all_files = set()
        for f in findings:
            for loc in f.locations:
                all_files.add(Path(loc["file"]).name)
        assert "alpha.rs" in all_files
        assert "beta.rs" in all_files
