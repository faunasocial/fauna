"""The harness's SIGNED file writer for a test with no app: a directory plus one
seat pass per call (``fauna_ffi.harness_sync_pass``).

Why it exists: the nest refuses every unsigned record, so the headless daemon (removed 2026-10-02) could no
longer be the writer a test's reader waits on. Where a test has an app, the
app's own bound folder is the writer (``conftest._bind_live_location``); where
it has none, this is — the apps' own in-process engine host, built for one pass
with the identity seed, which signs every record directly with the identity key.

The writer is a directory the test writes plain files into, then ``sync()``: new
and changed files go up sealed and recorded, and a file removed since the last
``sync()`` is recorded deleted — the engine's own reconcile, over the state DB
this writer keeps between passes. Nothing runs between calls, so there is no
watcher, cadence or background process to wait out or reap: a returned
``sync()`` means the records are on the nest, and a test's reader can wait on
them as causally as it waits on any other write.

The set must be born custody-first (``common.auth.user_create_folder``) — the
engine signs nothing without the nonce custody holds.
"""

from __future__ import annotations

import secrets
from pathlib import Path

import fauna_ffi


class HarnessWriter:
    """A signing writer seat over ``root/files`` for the set ``folder_id``."""

    def __init__(self, nest_url: str, secret_key: str, folder_id: str, root: Path,
                 label: str = "e2e harness writer") -> None:
        self.nest_url = nest_url
        self._secret = bytes.fromhex(secret_key)
        self.folder_id = folder_id
        self.path = Path(root) / "files"
        self._state = Path(root) / "state"
        self.path.mkdir(parents=True, exist_ok=True)
        self._state.mkdir(parents=True, exist_ok=True)
        # One device per writer, stable across its passes, as a real seat's is.
        self.device_id = secrets.token_bytes(32)
        self.label = label

    def write(self, rel: str, body: bytes | str) -> Path:
        """Write ``body`` at ``rel`` under the writer's directory (no sync)."""
        target = self.path / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(body, str):
            body = body.encode()
        target.write_bytes(body)
        return target

    def sync(self, expect=()) -> list[str]:
        """Run one seat pass and return the rels it recorded.

        Fails loudly when the pass left work pending, or when a rel in
        ``expect`` was not recorded by it — so a writer that never wrote reads
        as the writer's failure, never as a reader that waited in vain.
        """
        recorded, pending = fauna_ffi.harness_sync_pass(
            self.nest_url, self._secret, self.folder_id, self.path, self._state,
            self.device_id, self.label,
        )
        missing = sorted(set(expect) - set(recorded))
        assert pending == 0 and not missing, (
            f"the harness writer's pass over {self.path} for {self.folder_id} "
            f"left {pending} file(s) pending and did not record {missing!r} "
            f"(recorded {sorted(recorded)!r}). The pass's own engine log is not "
            f"captured (the C ABI installs no subscriber); a refused record is "
            f"named in the nest's log."
        )
        return recorded
