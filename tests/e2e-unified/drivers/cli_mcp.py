"""The cli app's e2e driver: the device shell launched the way an LLM host
launches a local MCP server, spoken to over MCP's JSON-RPC on stdio — or, with
``transport="http"``, over the loopback streamable-HTTP listener
(``fauna-cli mcp --listen``, the URL read from its first stdout line) — with the
standard library only (``docs/goal/architecture/apps/cli.md`` § Test surface —
no MCP client dependency in the harness). The e2e client token is ``cli``.

The cli is not a ui.yaml app (§ Not a ui.yaml app), so this is not a
``PlatformDriver``: there are no elements to find, only tools to call. What a
test asserts is what a host would see — ``tools/list`` and each
``tools/call`` result, ``isError`` included.

**Launch isolation (convention 10).** Every session gets fresh
``XDG_CONFIG_HOME`` and ``FAUNA_E2E_CREDENTIAL_DIR`` directories, so nothing
the shell persists reaches the box's real credential store or another run.
The nest is named by ``FAUNA_E2E_CLI_NEST_URL`` — the debug/e2e build's stand
in for resolving a handle no DNS knows (convention 15).

**Pipes (convention 13).** stdout is the protocol channel and is read for the
child's whole life by a reader thread; stderr is drained alongside, kept in a
bounded deque for the failure message.

The same binary's **``argv`` shell** (§ One catalog, many front-ends → *The
``argv`` shell*) is driven by :class:`CliArgv` — one process per command line,
under the same isolated directories, so a ``connect`` it stores is the
connection the next command (and a ``CliMcpSession`` on the same ``home``)
resumes.
"""

from __future__ import annotations

import collections
import http.client
import json
import os
import queue
import subprocess
import threading
import time
from pathlib import Path

from drivers.port_util import (
    popen_group_kwargs,
    reap_descendants_of,
    terminate_tree,
    track_process,
    untrack_process,
)
from helpers.budgets import APP_EXIT_S

#: The protocol revision the driver asks for — one the shell speaks.
PROTOCOL_VERSION = "2025-06-18"


class McpError(AssertionError):
    """A JSON-RPC error answer, or no answer in budget."""


def _shell_env(
    nest_url: str, home: Path, runtime_dir: str | None = None, store: str = "e2e"
) -> dict:
    """The isolated environment every shell process under ``home`` shares.
    ``runtime_dir`` is the custodian app's private ``XDG_RUNTIME_DIR``: the
    conversations oracle's door sits beside that launch's agent socket, which a
    host on the same session would find in its own runtime dir — the harness
    isolates each app launch, so the shell is pointed at the custodian's.
    ``store="unattended"`` launches the shell as on a box with no usable OS
    secret store: no ``FAUNA_E2E_CREDENTIAL_DIR`` (which would short-circuit
    the resolution) and ``FAUNA_E2E_FORCE_HEADLESS_STORE``, so the store
    lands on the unattended file under ``XDG_CONFIG_HOME/fauna-cli``
    without probing the box's keyring (``apps/cli.md`` § The device shell →
    *Credentials*)."""
    assert store in ("e2e", "unattended"), store
    config = home / "xdg-config"
    creds = home / "credentials"
    config.mkdir(parents=True, exist_ok=True)
    creds.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update(
        {
            "XDG_CONFIG_HOME": str(config),
            "FAUNA_E2E_CREDENTIAL_DIR": str(creds),
            "FAUNA_E2E_CLI_NEST_URL": nest_url,
            "RUST_LOG": env.get("RUST_LOG", "info"),
        }
    )
    if store == "unattended":
        del env["FAUNA_E2E_CREDENTIAL_DIR"]
        env.pop("FAUNA_KEYRING_APP", None)
        env["FAUNA_E2E_FORCE_HEADLESS_STORE"] = "1"
    if runtime_dir is not None:
        env["XDG_RUNTIME_DIR"] = runtime_dir
    return env


class CliMcpSession:
    """One ``fauna-cli mcp`` process and its JSON-RPC conversation."""

    def __init__(
        self,
        binary: str,
        nest_url: str,
        home: Path,
        transport: str = "stdio",
        runtime_dir: str | None = None,
        store: str = "e2e",
    ):
        assert transport in ("stdio", "http"), transport
        self.transport = transport
        self.http_port: int | None = None
        self._proc = subprocess.Popen(
            [binary, "mcp", *(["--listen"] if transport == "http" else [])],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=_shell_env(nest_url, home, runtime_dir, store),
            **popen_group_kwargs(),
        )
        reap_descendants_of(self._proc.pid)
        track_process(self._proc)
        self._answers: "queue.Queue[dict]" = queue.Queue()
        self._stray: list[str] = []
        self.stderr: collections.deque[str] = collections.deque(maxlen=400)
        self._next_id = 0
        threading.Thread(target=self._read_stderr, daemon=True).start()
        if transport == "http":
            # The listener prints its URL once, then stdout is silent; the
            # port is the only thing the host needs from it.
            url = self._proc.stdout.readline().decode(errors="replace").strip()
            if not url.startswith("http://127.0.0.1:") or not url.endswith("/mcp"):
                raise McpError(f"no loopback URL on stdout: {url!r}\n{self.diagnostics()}")
            self.http_port = int(url.split(":")[2].split("/")[0])
            threading.Thread(target=self._drain_stdout, daemon=True).start()
        else:
            threading.Thread(target=self._read_stdout, daemon=True).start()

    # ── pipes ──────────────────────────────────────────────────────────
    def _read_stdout(self) -> None:
        for raw in self._proc.stdout:
            line = raw.decode(errors="replace").strip()
            if not line:
                continue
            try:
                self._answers.put(json.loads(line))
            except json.JSONDecodeError:
                # stdout must carry nothing but MCP messages: keep the line
                # for the failure message rather than dropping it.
                self._stray.append(line)

    def _drain_stdout(self) -> None:
        for raw in self._proc.stdout:
            self._stray.append(raw.decode(errors="replace").strip())

    def http_exchange(
        self, body: str, host: str | None = "", origin: str | None = None, method: str = "POST",
        path: str = "/mcp",
    ) -> tuple[int, str]:
        """One raw HTTP exchange with the listener: ``(status, body)``. ``host``
        ``""`` is the listener's own loopback name, ``None`` omits the header."""
        conn = http.client.HTTPConnection("127.0.0.1", self.http_port, timeout=30)
        try:
            conn.putrequest(method, path, skip_host=True)
            if host is not None:
                conn.putheader("Host", host or f"127.0.0.1:{self.http_port}")
            if origin is not None:
                conn.putheader("Origin", origin)
            conn.putheader("Content-Type", "application/json")
            conn.putheader("Accept", "application/json, text/event-stream")
            conn.putheader("Content-Length", str(len(body.encode())))
            conn.endheaders(body.encode())
            response = conn.getresponse()
            return response.status, response.read().decode(errors="replace")
        finally:
            conn.close()

    def _read_stderr(self) -> None:
        for raw in self._proc.stderr:
            self.stderr.append(raw.decode(errors="replace").rstrip())

    def diagnostics(self) -> str:
        stray = f"\nnon-protocol stdout: {self._stray!r}" if self._stray else ""
        return "fauna-cli stderr (tail):\n" + "\n".join(list(self.stderr)[-40:]) + stray

    # ── JSON-RPC ───────────────────────────────────────────────────────
    def _send(self, message: dict) -> None:
        if self.transport == "http":
            # Streamable HTTP: the answer comes back on the POST itself; a
            # notification is accepted with 202 and no body.
            status, body = self.http_exchange(json.dumps(message))
            if "id" in message:
                assert status == 200, f"POST /mcp answered {status}: {body!r}\n{self.diagnostics()}"
                self._answers.put(json.loads(body))
            else:
                assert status == 202, f"a notification must be accepted, got {status}: {body!r}"
            return
        self._proc.stdin.write((json.dumps(message) + "\n").encode())
        self._proc.stdin.flush()

    def request(self, method: str, params: dict | None = None, timeout: float = 30.0) -> dict:
        """One request; its ``result``. A JSON-RPC error raises."""
        self._next_id += 1
        rid = self._next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise McpError(f"no answer to {method} in {timeout}s\n{self.diagnostics()}")
            try:
                answer = self._answers.get(timeout=remaining)
            except queue.Empty:
                continue
            if answer.get("id") != rid:
                continue
            if "error" in answer:
                raise McpError(f"{method} answered an error: {answer['error']!r}")
            return answer["result"]

    def request_error(self, method: str, params: dict | None = None) -> dict:
        """A request the caller expects to be REFUSED; the JSON-RPC error."""
        self._next_id += 1
        rid = self._next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        while True:
            answer = self._answers.get(timeout=30.0)
            if answer.get("id") == rid:
                assert "error" in answer, f"{method} was expected to be refused: {answer!r}"
                return answer["error"]

    def initialize(self) -> dict:
        result = self.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "fauna-e2e", "version": "0"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return result

    def tool_names(self) -> list[str]:
        return [t["name"] for t in self.request("tools/list")["tools"]]

    def call(self, name: str, arguments: dict | None = None, timeout: float = 30.0) -> dict:
        """``tools/call`` — the result, ``isError`` and all."""
        return self.request(
            "tools/call", {"name": name, "arguments": arguments or {}}, timeout=timeout
        )

    @staticmethod
    def text(result: dict) -> str:
        return " ".join(c.get("text", "") for c in result.get("content", []))

    def close(self) -> None:
        if self.transport == "stdio":
            try:
                self._proc.stdin.close()
            except OSError:
                pass
        else:
            # The listener serves until told to stop; there is no pipe to close.
            terminate_tree(self._proc)
        try:
            self._proc.wait(timeout=APP_EXIT_S)
        except subprocess.TimeoutExpired:
            terminate_tree(self._proc)
        untrack_process(self._proc)


class ArgvRun:
    """One ``fauna-cli <command …>`` process: both pipes drained for its whole
    life (convention 13), stdin closed — never a terminal, so the shell prints
    JSON and a destructive command refuses without ``--yes``."""

    def __init__(self, binary: str, env: dict, args: list[str]):
        self.args = args
        self._proc = subprocess.Popen(
            [binary, *args],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            **popen_group_kwargs(),
        )
        reap_descendants_of(self._proc.pid)
        track_process(self._proc)
        self._lines: "queue.Queue[str]" = queue.Queue()
        self._stdout: list[str] = []
        self._stderr: list[str] = []
        self._readers = [
            threading.Thread(target=self._read, args=(self._proc.stdout, self._stdout, True), daemon=True),
            threading.Thread(target=self._read, args=(self._proc.stderr, self._stderr, False), daemon=True),
        ]
        for reader in self._readers:
            reader.start()

    def _read(self, pipe, into: list[str], lines: bool) -> None:
        for raw in pipe:
            text = raw.decode(errors="replace")
            into.append(text)
            if lines:
                self._lines.put(text)

    def first_line(self, timeout: float) -> str:
        """The first stdout line — ``connect``'s answer, printed before it waits."""
        try:
            return self._lines.get(timeout=timeout)
        except queue.Empty:
            raise McpError(f"{self._named()} printed nothing in {timeout}s\n{self.diagnostics()}")

    def wait(self, timeout: float) -> int:
        try:
            code = self._proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            terminate_tree(self._proc)
            untrack_process(self._proc)
            raise McpError(f"{self._named()} did not exit in {timeout}s\n{self.diagnostics()}")
        for reader in self._readers:
            reader.join(timeout=APP_EXIT_S)
        untrack_process(self._proc)
        return code

    def _named(self) -> str:
        return "`fauna-cli " + " ".join(self.args) + "`"

    @property
    def stdout(self) -> str:
        return "".join(self._stdout)

    @property
    def stderr(self) -> str:
        return "".join(self._stderr)

    def json(self) -> dict:
        """stdout as the tool result's JSON."""
        return json.loads(self.stdout)

    def diagnostics(self) -> str:
        return f"{self._named()}\nstdout: {self.stdout[-2000:]!r}\nstderr: {self.stderr[-2000:]!r}"


class CliArgv:
    """The ``argv`` shell under one isolated ``home`` — share the ``home`` with a
    :class:`CliMcpSession` and both front-ends are one principal."""

    def __init__(self, binary: str, nest_url: str, home: Path):
        self.binary = binary
        self.env = _shell_env(nest_url, home)

    def start(self, *args: str) -> ArgvRun:
        return ArgvRun(self.binary, self.env, list(args))

    def run(self, *args: str, timeout: float = 30.0) -> tuple[int, ArgvRun]:
        """Run one command to its exit: ``(exit code, the run)``."""
        run = self.start(*args)
        return run.wait(timeout), run
