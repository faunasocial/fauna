"""Structural validation of `bins/fauna-nest/install.sh` — no root, no install.

Mirrors the Windows MSI structural-validation pattern
(`platform/windows/test_installer_structure.py`): parse the installer source
directly instead of actually running an install, so these tests need no
privilege and mutate no system state. `test_installer.py`'s `TestNestInstaller`
covers the real, root-gated install/uninstall behavior separately.
"""

import os
import re
import shlex
import subprocess
import tomllib

import pytest

pytestmark = pytest.mark.tier_1


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return result.stdout.strip()


@pytest.fixture(scope="module")
def install_sh_text():
    path = os.path.join(_repo_root(), "bins", "fauna-nest", "install.sh")
    with open(path) as f:
        return f.read()


def _function_body(text, name):
    m = re.search(r"^" + re.escape(name) + r"\(\)\s*\{\n(.*?)^\}\n", text, re.DOTALL | re.MULTILINE)
    assert m, f"{name}() function not found in install.sh"
    return m.group(1)


def _find_matching_fi(lines, if_line_idx):
    """Index of the `fi` at the same indentation as the `if` at if_line_idx."""
    if_indent = len(lines[if_line_idx]) - len(lines[if_line_idx].lstrip(" "))
    for i in range(if_line_idx + 1, len(lines)):
        stripped = lines[i].strip()
        indent = len(lines[i]) - len(lines[i].lstrip(" "))
        if indent == if_indent and stripped == "fi":
            return i
    return None


# ── Claim-code format: install.sh must mirror libs/fauna-core exactly ──
#
# claim.rs::ensure_claim_code_at preserves an existing claim-code file rather
# than overwriting it, so a native (install.sh) deploy that mints an
# off-format code is stuck with it forever — not self-healing. Parsed from
# the canonical Rust source rather than hardcoded, so a future format change
# there fails this test until install.sh is updated too.

def _canonical_claim_code_constants():
    core_src = os.path.join(_repo_root(), "libs", "fauna-core", "src")
    with open(os.path.join(core_src, "claim_code.rs")) as f:
        claim_code_rs = f.read()
    with open(os.path.join(core_src, "human_code.rs")) as f:
        human_code_rs = f.read()

    code_len = re.search(r"const CODE_LEN: usize = (\d+);", claim_code_rs)
    group = re.search(r"const GROUP: usize = (\d+);", claim_code_rs)
    alphabet = re.search(r'pub const ALPHABET: &\[u8; \d+\] = b"([^"]+)";', human_code_rs)
    assert code_len and group and alphabet, "could not parse claim-code constants from fauna-core"
    return int(code_len.group(1)), int(group.group(1)), alphabet.group(1)


class TestClaimCodeFormat:
    def test_length_matches_canonical_code_len(self, install_sh_text):
        code_len, _group, _alphabet = _canonical_claim_code_constants()
        body = _function_body(install_sh_text, "generate_claim_code")
        head_c = re.search(r"head -c (\d+)", body)
        assert head_c, "no 'head -c N' in generate_claim_code()"
        assert int(head_c.group(1)) == code_len, (
            f"install.sh mints a {head_c.group(1)}-char claim code, but the "
            f"canonical format (fauna_core::claim_code::CODE_LEN) is {code_len} "
            f"chars — a native install would permanently mint an off-format code."
        )

    def test_alphabet_matches_canonical_alphabet(self, install_sh_text):
        _code_len, _group, alphabet = _canonical_claim_code_constants()
        body = _function_body(install_sh_text, "generate_claim_code")
        tr_alpha = re.search(r"tr -dc '([^']+)'", body)
        assert tr_alpha, "no 'tr -dc' alphabet in generate_claim_code()"
        assert tr_alpha.group(1) == alphabet, (
            f"install.sh's claim-code alphabet {tr_alpha.group(1)!r} does not "
            f"match the canonical fauna_core::human_code::ALPHABET {alphabet!r}"
        )

    def test_grouping_matches_canonical_group(self, install_sh_text):
        _code_len, group, _alphabet = _canonical_claim_code_constants()
        body = _function_body(install_sh_text, "generate_claim_code")
        fold = re.search(r"fold -w(\d+)", body)
        assert fold, "no 'fold -w N' in generate_claim_code()"
        assert int(fold.group(1)) == group

    def test_generated_code_matches_the_canonical_shape(self, install_sh_text):
        """Extract the exact generation pipeline and run it for real (no root,
        no FAUNA_USER/chown — /dev/urandom is world-readable) to prove the
        shipped line, not just its substrings, produces the right shape."""
        body = _function_body(install_sh_text, "generate_claim_code")
        m = re.search(r"code=\$\(LC_ALL=C tr .*?\)", body, re.DOTALL)
        assert m, "could not extract the claim-code generation pipeline"

        result = subprocess.run(
            ["bash", "-c", m.group(0) + '; echo "$code"'],
            capture_output=True, text=True, timeout=10,
        )
        assert result.returncode == 0, result.stderr
        code = result.stdout.strip()
        assert re.fullmatch(r"[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}", code), (
            f"generated code {code!r} does not match the canonical 8-char / "
            f"2-group / hyphenated / no-I-O-0-1 shape"
        )


# ── --remove-data gating: userdel must ride the same gate as the data dir ──

class TestUninstallUserRemovalGating:
    """A bare `--uninstall` (no `--remove-data`) must leave the fauna system
    user in place, exactly like it leaves /var/lib/fauna in place — otherwise
    the data directory is orphaned under a nonexistent UID."""

    def test_userdel_is_nested_inside_the_remove_data_conditional(self, install_sh_text):
        body = _function_body(install_sh_text, "do_uninstall")
        lines = body.splitlines()

        if_idx = next((i for i, l in enumerate(lines) if "if $REMOVE_DATA; then" in l), None)
        assert if_idx is not None, "REMOVE_DATA conditional not found in do_uninstall()"
        fi_idx = _find_matching_fi(lines, if_idx)
        assert fi_idx is not None, "no matching 'fi' found for the REMOVE_DATA conditional"

        userdel_idx = next((i for i, l in enumerate(lines) if "userdel " in l), None)
        assert userdel_idx is not None, "userdel call not found in do_uninstall()"

        assert if_idx < userdel_idx < fi_idx, (
            f"userdel (line {userdel_idx} of do_uninstall()) must be nested "
            f"inside the `if $REMOVE_DATA; then ... fi` block (lines "
            f"{if_idx}-{fi_idx}) — a bare --uninstall must not delete the "
            f"fauna system user while leaving /var/lib/fauna behind"
        )


# ── install_default_config(): the installer writes wiring, never a choice ──
#
# The rendered nest.toml is artifact-written IPC: paths, the listen address and
# the pre-claim NAT-mode seed. A domain, an ACME contact and mail settings are
# an admin's choices, made in the app and kept in nest state — so the script
# takes no flag for them and writes no key for them
# (docs/goal/principles.md § One configuration surface;
# docs/goal/architecture/installers/linux-nest.md § Installer flags).
# These run the real function (no root needed — it only touches the temp
# CONFIG_FILE) against the real template, so template/script drift is caught.

# The flags that carried a human's choice into nest.toml, all removed.
_REMOVED_CHOICE_FLAGS = (
    "--domain", "--acme", "--acme-email", "--email", "--email-domain", "--smtp-bind",
)


class TestInstallDefaultConfigWritesWiringOnly:
    def _run(self, install_sh_text, tmp_path, *, mode="", blob_dir=""):
        body = _function_body(install_sh_text, "install_default_config")
        config_file = tmp_path / "nest.toml"
        data_dir = tmp_path / "data"
        template = os.path.join(_repo_root(), "config", "default.toml")
        # `set -u`: a variable the function reads but the script no longer
        # declares fails here instead of silently expanding empty.
        script = "\n".join([
            "set -eu",
            f"DEFAULT_CONFIG={shlex.quote(template)}",
            f"CONFIG_FILE={shlex.quote(str(config_file))}",
            f"DATA_DIR={shlex.quote(str(data_dir))}",
            'BIND="0.0.0.0:3000"',
            f"MODE={shlex.quote(mode)}",
            f"BLOB_DIR={shlex.quote(blob_dir)}",
            "install_default_config() {",
            body,
            "}",
            "install_default_config",
        ])
        result = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, timeout=10,
        )
        assert result.returncode == 0, result.stderr
        return config_file

    def test_no_flag_carries_a_human_choice(self, install_sh_text):
        for flag in _REMOVED_CHOICE_FLAGS:
            assert not re.search(rf"^\s*{re.escape(flag)}\)", install_sh_text, re.M), (
                f"install.sh still parses {flag} — a domain, an ACME contact and "
                f"mail settings are chosen in the app, never passed to the installer"
            )
            assert not re.search(rf"^\s*{re.escape(flag)}\b", install_sh_text, re.M), (
                f"install.sh --help still lists {flag}"
            )

    def test_rendered_config_names_no_domain_contact_or_mail(self, install_sh_text, tmp_path):
        config_file = self._run(install_sh_text, tmp_path, mode="private")
        raw = config_file.read_text()
        assert raw.count("[acme]") == 1, (
            f"expected exactly the template's one [acme] table, got:\n{raw}"
        )
        with open(config_file, "rb") as f:
            parsed = tomllib.load(f)
        assert parsed["nest"]["mode"] == "private", "the NAT-mode seed still lands"
        assert "domain" not in parsed["nest"], (
            f"the installer must not seed [nest].domain — the nest learns its "
            f"domain at claim: {parsed['nest']}"
        )
        assert "email" not in parsed["acme"], f"no ACME contact is written: {parsed['acme']}"
        assert "email" not in parsed, f"no [email] table is written: {sorted(parsed)}"


# ── Claim code: root must never write through a link the service user planted ──
#
# $DATA_DIR (/var/lib/fauna) is owned by the fauna service user, so a compromised
# nest can plant $DATA_DIR/claim-code as a dangling symlink to a root-read path;
# a later root re-install must not write, chown or chmod through it.

class TestClaimCodeSymlinkSafety:
    def _run_generate(self, install_sh_text, data_dir):
        body = _function_body(install_sh_text, "generate_claim_code")
        user = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
        group = subprocess.run(["id", "-gn"], capture_output=True, text=True, check=True).stdout.strip()
        script = (
            "set -e\n"
            f"DATA_DIR={shlex.quote(str(data_dir))}\n"
            f"FAUNA_USER={shlex.quote(user)}\nFAUNA_GROUP={shlex.quote(group)}\n"
            "generate_claim_code() {\n" + body + "}\n"
            "generate_claim_code\n"
        )
        return subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=20)

    def test_planted_dangling_symlink_is_never_written_through(self, install_sh_text, tmp_path):
        data_dir = tmp_path / "data"
        data_dir.mkdir()
        target = tmp_path / "root-owned-target"
        (data_dir / "claim-code").symlink_to(target)

        result = self._run_generate(install_sh_text, data_dir)

        assert not target.exists(), "installer wrote the claim code through a planted symlink"
        assert result.returncode != 0, "installer should refuse a symlinked claim-code path"
        assert (data_dir / "claim-code").is_symlink(), "the planted link must be left for the admin, not replaced"

    def test_regular_install_writes_a_regular_0640_file(self, install_sh_text, tmp_path):
        data_dir = tmp_path / "data"
        data_dir.mkdir()

        result = self._run_generate(install_sh_text, data_dir)

        assert result.returncode == 0, result.stderr
        claim = data_dir / "claim-code"
        assert claim.is_file() and not claim.is_symlink()
        assert oct(claim.stat().st_mode & 0o777) == "0o640"
        assert re.fullmatch(r"[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}", claim.read_text().strip())

    def test_only_root_write_into_data_dir_is_a_rename(self, install_sh_text):
        # install(1)/chmod/chown by path set owner and mode through a link the
        # service user swapped in; the file is finished in a root-owned stage
        # directory and renamed into place, which replaces a link, never follows it.
        body = _function_body(install_sh_text, "generate_claim_code")
        code = "\n".join(l for l in body.splitlines() if not l.lstrip().startswith("#"))
        for cmd in ("install", "chmod", "chown", "cp", "tee"):
            for line in code.splitlines():
                assert not (re.search(rf"\b{cmd}\b", line) and '"$claim_path"' in line), (
                    f"generate_claim_code hands {cmd} the final claim path: {line.strip()}"
                )
        assert re.search(r'\bmv\s+-fT\s+"\$stage/claim-code"\s+"\$claim_path"', code), (
            "the claim code must reach $claim_path by `mv -fT` from a root-owned stage directory"
        )
        assert "set on the fd" not in body, "the comment claims install(1) acts on the fd; it does not on every platform"

    def test_stage_directory_is_removed(self, install_sh_text, tmp_path):
        data_dir = tmp_path / "data"
        data_dir.mkdir()
        assert self._run_generate(install_sh_text, data_dir).returncode == 0
        assert [p.name for p in tmp_path.iterdir()] == ["data"], "stage directory left behind"


# ── Blob dir: the guard must refuse a link in ANY component under $DATA_DIR ──

class TestBlobDirGuard:
    def _run_guard(self, install_sh_text, data_dir, blob_spelling):
        body = _function_body(install_sh_text, "refuse_linked_blob_dir")
        script = (
            "set -e\n"
            f"DATA_DIR={shlex.quote(str(data_dir))}\n"
            "refuse_linked_blob_dir() {\n" + body + "}\n"
            f"refuse_linked_blob_dir {shlex.quote(blob_spelling)}\n"
        )
        return subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=20)

    @pytest.fixture
    def data_dir(self, tmp_path):
        data = tmp_path / "data"
        (data / "real").mkdir(parents=True)
        (data / "blobs").symlink_to("real")
        (data / "mid").symlink_to("real")
        (data / "plain").mkdir()
        return data

    def test_final_component_link_is_refused(self, install_sh_text, data_dir):
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/blobs").returncode != 0

    def test_trailing_slash_spelling_is_refused(self, install_sh_text, data_dir):
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/blobs/").returncode != 0

    def test_intermediate_component_link_is_refused(self, install_sh_text, data_dir):
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/mid/x").returncode != 0

    def test_non_canonical_spellings_are_refused(self, install_sh_text, data_dir):
        assert self._run_guard(install_sh_text, data_dir, f"/{data_dir}/blobs").returncode != 0
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/plain/../blobs").returncode != 0

    def test_plain_paths_pass(self, install_sh_text, data_dir, tmp_path):
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/plain").returncode == 0
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/not-yet").returncode == 0
        assert self._run_guard(install_sh_text, data_dir, str(tmp_path / "elsewhere")).returncode == 0

    def test_chown_of_the_blob_dir_does_not_follow_a_link(self, install_sh_text):
        body = _function_body(install_sh_text, "do_install")
        assert re.search(r'chown\s+-h\s+"\$FAUNA_USER":"\$FAUNA_GROUP"\s+"\$BLOB_DIR"', body), (
            "chown of $BLOB_DIR must be `chown -h`"
        )

    def test_nested_more_than_one_level_below_data_dir_is_refused(self, install_sh_text, data_dir):
        # root's mkdir/chown resolve intermediates by path, and the service user
        # can swap one after any check: only $DATA_DIR/<name> is allowed.
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/not-yet/made").returncode != 0
        assert self._run_guard(install_sh_text, data_dir, f"{data_dir}/plain/sub").returncode != 0

    def test_do_install_guards_before_and_after_mkdir(self, install_sh_text):
        body = _function_body(install_sh_text, "do_install")
        lines = body.splitlines()
        start = next(i for i, l in enumerate(lines) if l.strip() == 'if [ -n "$BLOB_DIR" ]; then')
        end = _find_matching_fi(lines, start)
        block = "\n".join(lines[start:end + 1])
        script = (
            "BLOB_DIR=/b FAUNA_USER=u FAUNA_GROUP=g\n"
            "refuse_linked_blob_dir() { echo guard; }\n"
            "mkdir() { echo mkdir; }\n"
            "chown() { echo chown; }\n" + block + "\n"
        )
        out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=20).stdout.split()
        assert out == ["guard", "mkdir", "guard", "chown"], out

    def test_blob_dir_is_normalized_once_at_parse_so_root_acts_on_the_judged_spelling(
        self, install_sh_text, tmp_path
    ):
        # The guard judges `realpath -sm`, so mkdir/chown must receive that
        # same string, not a raw `name/` or `x/../name` the kernel resolves afresh.
        arm = re.search(r'^\s*BLOB_DIR=\$\(realpath -sm -- "\$2"\)$', install_sh_text, re.M)
        assert arm, "--blob-dir must normalize BLOB_DIR with `realpath -sm` at parse"
        lines = _function_body(install_sh_text, "do_install").splitlines()
        start = next(i for i, l in enumerate(lines) if l.strip() == 'if [ -n "$BLOB_DIR" ]; then')
        block = "\n".join(lines[start:_find_matching_fi(lines, start) + 1])
        data = tmp_path / "data"
        for spelling in (f"{data}/name/", f"{data}/x/../name"):
            script = (
                f"DATA_DIR={shlex.quote(str(data))} FAUNA_USER=u FAUNA_GROUP=g\n"
                f"set -- {shlex.quote(spelling)} {shlex.quote(spelling)}\n"
                + arm.group(0).strip() + "\n"
                "refuse_linked_blob_dir() { return 0; }\n"
                'mkdir() { echo "mkdir ${@: -1}"; }\n'
                'chown() { echo "chown ${@: -1}"; }\n' + block + "\n"
            )
            out = subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=20).stdout.splitlines()
            assert out == [f"mkdir {data}/name", f"chown {data}/name"], (spelling, out)
