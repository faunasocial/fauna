"""A harness-served OCI registry for the update channel's witnesses.

`nest/update-channel.md` § Tests: the nest's ``channel_head_version`` (tier_3)
and the Fauna updater (tier_4) both read a channel's head from the registry —
`fauna_provisioning::update_channel::registry`, three reads of the OCI
distribution API (``HEAD`` the tag's manifest for its digest, ``GET`` it and
step from an index to this platform's manifest, ``GET`` the config blob for its
``org.opencontainers.image.version`` label). A witness cannot read the real
registry: "what is Test's head today" is not a fixture, and a test nest must
never reach ghcr.io. So the harness serves the API itself on loopback and hands
the binary its origin through the compile-gated ``FAUNA_E2E_UPDATE_REGISTRY_ORIGIN``
seam (the nest: ``bins/fauna-nest/src/update_channel.rs::registry``; the
updater: ``bins/fauna-updater/src/main.rs``) — the ``FAUNA_E2E_RELEASE_FEED_URL``
precedent (`helpers/release_feed_stub.py`). Only the ORIGIN moves: the paths,
the media types, the digest arithmetic and the decision are the production ones.

It is plain Python, deliberately NOT a third-party registry image (which would
be an image add to vet). It answers anonymously (``GET /v2/`` → 200, the shape
the shared client treats as "no token needed") and serves exactly what a pull
needs: manifests by tag or digest (``HEAD`` and ``GET``) and blobs by digest.
The Docker daemon pulls from it over plain HTTP because it binds
``127.0.0.0/8``, which the daemon treats as an insecure registry by default.

Two ways to fill it:

* :meth:`OciRegistry.synthetic` — an index → this platform's manifest → a
  config blob carrying a version label, and no layers: enough for the head read
  (tier_3), never for a pull.
* :meth:`OciRegistry.from_image` — a REAL image exported with ``docker save``
  (an OCI layout since Docker 25), re-published under a chosen version label.
  The label is rewritten in the config blob, so each variant is a distinct image
  (its config digest is its image id) sharing every layer with the original —
  "a newer build" without building anything (tier_4).

``requests`` records ``"<METHOD> <path>"`` once each answer is written.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import platform
import subprocess
import tarfile
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

#: The one repository the nest and the updater follow
#: (`fauna_provisioning::update_channel::NEST_IMAGE_REPOSITORY`).
REPOSITORY = "faunasocial/nest"

#: The label the decision reads (`update_channel::VERSION_LABEL`).
VERSION_LABEL = "org.opencontainers.image.version"

OCI_INDEX = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
OCI_CONFIG = "application/vnd.oci.image.config.v1+json"

_CHUNK = 1 << 20


def platform_arch() -> str:
    """This machine's architecture as OCI spells it — the platform the shared
    client asks the index for (`registry::platform_arch`)."""
    machine = platform.machine().lower()
    return {"aarch64": "arm64", "arm64": "arm64", "x86_64": "amd64", "amd64": "amd64"}.get(
        machine, machine
    )


def digest_of(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _canonical(obj: dict) -> bytes:
    return json.dumps(obj, separators=(",", ":"), sort_keys=True).encode()


@dataclass(frozen=True)
class Published:
    """What one publish produced: the digests a head read names it by."""

    index: str
    manifest: str
    config: str


class OciRegistry:
    """Serves ``/v2/<REPOSITORY>/{manifests,blobs}/…`` on a loopback port."""

    def __init__(self):
        self._lock = threading.Lock()
        self._manifests: dict[str, tuple[bytes, str]] = {}
        self._blobs: dict[str, bytes | Path] = {}
        self._tags: dict[str, str] = {}
        self.requests: list[str] = []
        registry = self
        prefix = f"/v2/{REPOSITORY}/"

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):  # silence the default stderr log
                pass

            def _answer(self, head_only: bool):
                path = self.path.split("?", 1)[0]
                try:
                    if path in ("/v2", "/v2/"):
                        self._send(200, b"{}", "application/json", None, head_only)
                    elif path.startswith(prefix + "manifests/"):
                        found = registry._manifest(path[len(prefix + "manifests/"):])
                        if found is None:
                            self._send(404, b"", "text/plain", None, head_only)
                        else:
                            digest, body, media_type = found
                            self._send(200, body, media_type, digest, head_only)
                    elif path.startswith(prefix + "blobs/"):
                        digest = path[len(prefix + "blobs/"):]
                        blob = registry._blob(digest)
                        if blob is None:
                            self._send(404, b"", "text/plain", None, head_only)
                        else:
                            self._send(200, blob, "application/octet-stream", digest, head_only)
                    else:
                        self._send(404, b"", "text/plain", None, head_only)
                finally:
                    with registry._lock:
                        registry.requests.append(f"{self.command} {path}")

            def _send(self, status, body, media_type, digest, head_only):
                size = body.stat().st_size if isinstance(body, Path) else len(body)
                self.send_response(status)
                self.send_header("Content-Type", media_type)
                self.send_header("Content-Length", str(size))
                self.send_header("Docker-Distribution-API-Version", "registry/2.0")
                # One request per connection: a pooled keep-alive connection
                # would outlive `stop()` (shutdown only ends the accept loop),
                # and a "registry gone" witness would still be answered.
                self.send_header("Connection", "close")
                self.close_connection = True
                if digest:
                    self.send_header("Docker-Content-Digest", digest)
                self.end_headers()
                if head_only:
                    return
                if isinstance(body, Path):
                    with body.open("rb") as f:
                        while chunk := f.read(_CHUNK):
                            self.wfile.write(chunk)
                else:
                    self.wfile.write(body)
                self.wfile.flush()

            def do_GET(self):  # noqa: N802 (BaseHTTPRequestHandler's spelling)
                self._answer(head_only=False)

            def do_HEAD(self):  # noqa: N802
                self._answer(head_only=True)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self._thread: threading.Thread | None = None

    # ── lifecycle ────────────────────────────────────────────────────────────

    def start(self) -> "OciRegistry":
        self._thread = threading.Thread(
            target=self._server.serve_forever, name="oci-registry", daemon=True
        )
        self._thread.start()
        return self

    def stop(self) -> None:
        """Stop answering: the port refuses connections from here on."""
        self._server.shutdown()
        self._server.server_close()

    def __enter__(self) -> "OciRegistry":
        return self.start()

    def __exit__(self, *_exc) -> None:
        with contextlib.suppress(Exception):
            self.stop()

    @property
    def host(self) -> str:
        """``127.0.0.1:<port>`` — the registry part of an image reference."""
        host, port = self._server.server_address[:2]
        return f"{host}:{port}"

    @property
    def origin(self) -> str:
        """What a binary is handed as its registry origin (no trailing slash)."""
        return f"http://{self.host}"

    @property
    def repository(self) -> str:
        """The repository as an image reference spells it here."""
        return f"{self.host}/{REPOSITORY}"

    # ── contents ─────────────────────────────────────────────────────────────

    def _manifest(self, reference: str):
        with self._lock:
            digest = self._tags.get(reference, reference)
            found = self._manifests.get(digest)
        return None if found is None else (digest, *found)

    def _blob(self, digest: str):
        with self._lock:
            return self._blobs.get(digest)

    def put_blob(self, data: bytes) -> str:
        digest = digest_of(data)
        with self._lock:
            self._blobs[digest] = data
        return digest

    def put_blob_file(self, digest: str, path: Path) -> None:
        with self._lock:
            self._blobs[digest] = path

    def put_manifest(self, obj: dict, media_type: str) -> str:
        body = _canonical(obj)
        digest = digest_of(body)
        with self._lock:
            self._manifests[digest] = (body, media_type)
        return digest

    def tag(self, name: str, digest: str) -> None:
        """Point ``name`` at a manifest already published."""
        with self._lock:
            assert digest in self._manifests, f"no manifest {digest} to tag {name!r}"
            self._tags[name] = digest

    def untag(self, name: str) -> None:
        with self._lock:
            self._tags.pop(name, None)

    def _publish(self, config: dict, layers: list[dict], tag: str | None) -> Published:
        config_body = _canonical(config)
        config_digest = self.put_blob(config_body)
        manifest_digest = self.put_manifest(
            {
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST,
                "config": {
                    "mediaType": OCI_CONFIG,
                    "digest": config_digest,
                    "size": len(config_body),
                },
                "layers": layers,
            },
            OCI_MANIFEST,
        )
        manifest_size = len(self._manifests[manifest_digest][0])
        index_digest = self.put_manifest(
            {
                "schemaVersion": 2,
                "mediaType": OCI_INDEX,
                "manifests": [
                    {
                        "mediaType": OCI_MANIFEST,
                        "digest": manifest_digest,
                        "size": manifest_size,
                        "platform": {
                            "os": config.get("os", "linux"),
                            "architecture": config.get("architecture", platform_arch()),
                        },
                    }
                ],
            },
            OCI_INDEX,
        )
        if tag is not None:
            self.tag(tag, index_digest)
        return Published(index=index_digest, manifest=manifest_digest, config=config_digest)

    def synthetic(self, tag: str, version: str | None, arch: str | None = None) -> Published:
        """Publish a layerless image under ``tag`` whose config carries
        ``version`` as its version label (``None``: no label) — a head to READ,
        never to pull."""
        labels = {} if version is None else {VERSION_LABEL: version}
        config = {
            "architecture": arch or platform_arch(),
            "os": "linux",
            "config": {"Labels": labels},
            "rootfs": {"type": "layers", "diff_ids": []},
        }
        return self._publish(config, [], tag)

    def from_image(self, layout: "SavedImage", version: str, tag: str | None = None) -> Published:
        """Publish the saved image again with ``version`` as its version label —
        a distinct image (new config digest, so a new image id) on the very same
        layers."""
        config = json.loads(layout.config_bytes)
        config.setdefault("config", {})
        labels = dict(config["config"].get("Labels") or {})
        labels[VERSION_LABEL] = version
        config["config"]["Labels"] = labels
        for layer in layout.layers:
            self.put_blob_file(layer["digest"], layout.blob_path(layer["digest"]))
        return self._publish(config, layout.layers, tag)


class SavedImage:
    """A local image exported with ``docker save`` into ``directory`` and
    unpacked: its config and its layer descriptors, the layers as files."""

    def __init__(self, image: str, directory: Path):
        directory.mkdir(parents=True, exist_ok=True)
        archive = directory / "image.tar"
        subprocess.run(
            ["docker", "image", "save", "-o", str(archive), image],
            check=True, capture_output=True, timeout=900,
        )
        self.root = directory / "layout"
        with tarfile.open(archive) as tar:
            tar.extractall(self.root, filter="data")
        archive.unlink()
        index = json.loads((self.root / "index.json").read_text())
        descriptor = index["manifests"][0]
        node = json.loads(self.blob_path(descriptor["digest"]).read_bytes())
        # Docker nests the image's own index one level down; step to a manifest.
        while "manifests" in node:
            descriptor = next(
                m for m in node["manifests"]
                if m.get("platform", {}).get("architecture", platform_arch()) == platform_arch()
            )
            node = json.loads(self.blob_path(descriptor["digest"]).read_bytes())
        self.layers: list[dict] = [
            {k: layer[k] for k in ("mediaType", "digest", "size")} for layer in node["layers"]
        ]
        self.config_bytes = self.blob_path(node["config"]["digest"]).read_bytes()

    def blob_path(self, digest: str) -> Path:
        algorithm, hexdigest = digest.split(":", 1)
        return self.root / "blobs" / algorithm / hexdigest
