"""Python ctypes wrapper around the native fauna_ffi library.

Provides the BARE-signed payload builders test helpers need to produce the
opaque payloads the nest's WS-RPC `fauna.posts.create` kind carries —
`build_post` and its siblings — matching what native apps build.

The cdylib is built on demand. linux/mac route every call through `just
e2e-ffi` (build-if-stale-gated — a ~50ms no-op on a warm tree, a real rebuild
only when `libs/` sources actually changed), which copies its
`--no-default-features --features labeler,e2e-harness` build into a harness-PRIVATE slot
(`target/<profile>/e2e-ffi/harness/`, the justfile's `e2e-ffi-slot`) rather
than the shared `target/<profile>/libfauna_ffi.*` every other host build of
fauna-ffi writes — reading that shared slot directly is how a `just
mail-bridge-ffi` build left behind was silently served stale after a
`libs/fauna-ffi/src/cabi.rs` change (measured 2026-09-25). Windows routes the same way through `just windows-ffi-test dev`
instead, loading the flavor-private slot `_windows-ffi-flavor` copies into —
the very build the `app[windows]` prebuild takes, so one build serves both.

Each builder wraps a C-ABI export in `libs/fauna-ffi/src/cabi.rs`. The whole
`cabi` module — the pure builders and the networked seed exports
(`harness_create_set`, `harness_record_change`, `harness_report_conflict`, and
the file writer `harness_sync_pass` — `src/cabi/harness.rs`) — exists only
under fauna-ffi's `e2e-harness` feature (test surface, e2e convention 15),
which the private slots above are built with. The matching
`argtypes` registrations live alongside the function below.
"""

from __future__ import annotations

import base64
import ctypes
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

import cbor2


class _FfiBuffer(ctypes.Structure):
    _fields_ = [("data", ctypes.POINTER(ctypes.c_uint8)),
                ("len", ctypes.c_uint32)]


def _find_repo_root() -> Path:
    here = Path(__file__).resolve()
    for p in [here, *here.parents]:
        if (p / "Cargo.toml").exists() and (p / "libs" / "fauna-ffi").exists():
            return p
    raise RuntimeError("fauna repo root not found from " + str(here))


# The profile + private-slot path `just e2e-ffi` copies its cdylib into
# (`e2e-ffi-slot` in the justfile — the single source of truth; spelled here
# to match it, the same way the win branch below hardcodes
# `_windows-ffi-flavor`'s slot segments rather than shelling out to ask for
# them on every call). linux/mac only — win keeps its own scan below.
_E2E_FFI_PROFILE = "release"
_E2E_FFI_SLOT = f"{_E2E_FFI_PROFILE}/e2e-ffi/harness"


def _e2e_ffi_target_root(root: Path) -> Path:
    env_target = os.environ.get("CARGO_TARGET_DIR")
    return Path(env_target) if env_target else root / "target"


def _build_via_just_e2e_ffi(root: Path, fname: str) -> Path:
    """Route every linux/mac call through `just e2e-ffi` — build-if-stale
    gated (a ~50ms no-op on a warm tree, a real rebuild only when `libs/`
    sources actually changed) — instead of building once-if-absent and
    otherwise trusting whatever sits in the shared `target/<profile>/`
    fauna-ffi slot, the way `static_dir` calls `just web` on every call
    rather than only when the SPA bundle happens to be missing
    (build-system.md § Test fixtures).

    The shared slot is the ONE `target/<profile>/libfauna_ffi.*` every host
    build of fauna-ffi in the workspace writes (`mail-bridge-ffi`,
    `apple-ffi-host`, a bare `cargo build -p fauna-ffi`/`--workspace`), so
    reading it directly means whichever build ran last owns the file this
    loader serves — measured stale 2026-09-25: a
    `just mail-bridge-ffi` build left behind predated a
    `libs/fauna-ffi/src/cabi.rs` change, and four folder e2e tests silently
    ran the OLD fixture code against the NEW nest. `just e2e-ffi` copies its
    build into its OWN private slot instead (`e2e-ffi-slot`), which nothing
    else ever writes.
    """
    rc, tail = _run_build_recipe(["just", "e2e-ffi", _E2E_FFI_PROFILE], root)
    lib = _e2e_ffi_target_root(root) / _E2E_FFI_SLOT / fname
    if rc != 0 or not lib.exists():
        raise RuntimeError(
            f"`just e2e-ffi {_E2E_FFI_PROFILE}` (exit {rc}) did "
            f"not produce {lib}. Run it by hand and re-run.{tail}"
        )
    return lib


_BUILD_TAIL_LINES = 25


def _run_build_recipe(cmd: list[str], root: Path) -> tuple[int, str]:
    """Run a cdylib build recipe, echo its output, and return (exit code, its
    last `_BUILD_TAIL_LINES` lines formatted as a RuntimeError suffix).

    The first build happens when this module is IMPORTED — at module top of a
    dozen test files, so inside pytest's collection capture, which discards
    the output of an import that succeeds. A failed build still imports (the
    module falls back to `_UnavailableLib`), so a streamed recipe's own
    diagnosis vanished and the run log kept only "exit 1 … did not produce":
    measured on a whole tier_2/3 linux run, where one silent prebuild failure
    turned ~46 fauna-ffi tests red with its cause lost. The tail rides the
    exception instead, its lines kept at column 0 so a `[build-slot] ERROR: …
    slot freed within` or disk-exhaustion line still matches the merge-gate
    check's line-anchored INFRA_RE — the nightly parity gate classifies such a
    run INFRA, not RED (`merge-gate-catalog.md`, the 29th gate).
    """
    proc = subprocess.run(
        cmd, cwd=str(root), check=False,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, errors="replace",
    )
    out = proc.stdout or ""
    if out:
        sys.stdout.write(out)
        sys.stdout.flush()
    lines = out.rstrip("\n").splitlines()[-_BUILD_TAIL_LINES:]
    tail = ("\nIts output ends:\n" + "\n".join(lines)) if lines else ""
    return proc.returncode, tail


# The windows leg's flavor + profile: the SAME test-helpers debug build
# `windows-debug` (the `app[windows]` prebuild) takes as its prerequisite, so
# one build serves both and the prebuild of a run finds the gate fresh. Spelled
# to match `_windows-ffi-flavor`'s slot `target/<profile>/windows-ffi/<rid>/<flavor>/`.
_WINDOWS_FFI_PROFILE = "dev"
_WINDOWS_FFI_PROFILE_DIR = "debug"
_WINDOWS_FFI_FLAVOR = "test-helpers"


def _build_via_just_windows_ffi_test(root: Path) -> Path:
    """The windows twin of `_build_via_just_e2e_ffi`: route every call through
    `just windows-ffi-test dev` (build-if-stale gated — ~3 s warm, taking no
    build slot) and load the flavor-private slot it copies into.

    The loader used to scan for whatever dll sat in the slot and load it
    without a word — never building, never checking staleness — and that
    load happens at COLLECTION, because a dozen test modules import this one
    at module top. Two consequences: a stale dll silently ran old fixture
    code against a new nest, and the run's own `app[windows]` prebuild then
    had to copy the rebuilt dll over the one this process had mapped —
    `Device or resource busy`, every windows test errored (measured). Building first makes the mapped dll the fresh one,
    so the prebuild after it copies nothing (`scripts/win-stage-dll.sh`
    covers any other holder).

    The RID is the HOST's: a dll only loads into a process of its own
    architecture, so an arm64 python gets the default (arm64) build and an
    emulated x64 one the recipe's `x86_64-pc-windows-msvc` leg.
    """
    if platform.machine().lower() in ("arm64", "aarch64"):
        rid, target = "win-arm64", ""
    else:
        rid, target = "win-x64", "x86_64-pc-windows-msvc"
    cmd = ["just", "windows-ffi-test", _WINDOWS_FFI_PROFILE]
    if target:
        cmd.append(target)
    rc, tail = _run_build_recipe(cmd, root)
    lib = (root / "target" / _WINDOWS_FFI_PROFILE_DIR / "windows-ffi" / rid
           / _WINDOWS_FFI_FLAVOR / "fauna_ffi.dll")
    if rc != 0 or not lib.exists():
        raise RuntimeError(
            f"`{' '.join(cmd)}` (exit {rc}) did not produce {lib}. "
            f"Run it by hand (from Git Bash — `just` needs `sh`) and re-run.{tail}"
        )
    return lib


# A caller that only IMPORTS this module — the publish gate's
# `suite_collect_check`, collecting a fresh scratch copy where every build is
# cold (935 s on Windows, past that gate's 900 s ceiling) — sets this to skip the
# import-time build. The library then loads as `_UnavailableLib`, exactly the
# absent-cdylib path `_load_lib` already tolerates; it is opt-IN, so every
# ordinary run keeps the build-at-import the stale-dll fixes rely on.
_NO_BUILD_ENV = "FAUNA_E2E_FFI_NO_BUILD"


def _find_cdylib() -> Path:
    if os.environ.get(_NO_BUILD_ENV) == "1":
        raise RuntimeError(
            f"the fauna_ffi cdylib build is skipped ({_NO_BUILD_ENV}=1, a "
            f"collect-only caller) — unset it and run `just e2e-ffi` (linux/mac) "
            f"or `just windows-ffi-test dev` (windows) to build the library"
        )
    root = _find_repo_root()
    if sys.platform.startswith("linux"):
        return _build_via_just_e2e_ffi(root, "libfauna_ffi.so")
    if sys.platform == "darwin":
        return _build_via_just_e2e_ffi(root, "libfauna_ffi.dylib")
    if sys.platform != "win32":
        raise RuntimeError(f"unsupported platform: {sys.platform}")
    return _build_via_just_windows_ffi_test(root)


class _UnavailableSymbol:
    """One export of a cdylib that could not be loaded.

    Tolerates the `argtypes`/`restype` registrations below — with no library
    behind them they are bookkeeping on a throwaway — and raises the loader's
    own error the moment a test calls through it.
    """

    def __init__(self, error: BaseException, name: str) -> None:
        self._error = error
        self._name = name

    def __call__(self, *args: object, **kwargs: object) -> int:
        raise RuntimeError(
            f"fauna_ffi.{self._name} needs the native fauna-ffi library, which "
            f"is not loaded: {self._error}"
        ) from self._error


class _UnavailableLib:
    """Stand-in for a cdylib that could not be found, built, or loaded.

    Every export resolves — so the `hasattr` probes and the registrations
    below run unchanged — and every one of them raises when called.
    """

    def __init__(self, error: BaseException) -> None:
        self._error = error

    def __getattr__(self, name: str) -> _UnavailableSymbol:
        return _UnavailableSymbol(self._error, name)


def _load_lib() -> "ctypes.CDLL | _UnavailableLib":
    """Load the cdylib, or defer the failure to first use.

    Importing this module must never be able to take a whole collection down.
    It is a module-level import in ten shipped test modules, so the bare
    `ctypes.CDLL(str(_find_cdylib()))` this replaces turned any tree without a
    built fauna-ffi into `Interrupted: 10 errors during collection` — exit 2,
    ZERO of ~3100 tests run. A fresh clone of the public repository is exactly
    such a tree, which is how `publish-heavy-checks`' `suite_collect_check`
    caught it — on windows, whose
    loader then never built.

    This is the per-symbol deferral below, one level up: that one turns a
    STALE library into one actionable error in the one test needing the
    missing symbol; this turns an ABSENT one into the same, in each test that
    actually calls a builder. The auto-build stays at import time, where a
    build and its machine-wide slot wait belong (testing.md § point 9's
    bound-inversion rule) — only its failure stops being fatal to collection.
    """
    try:
        return ctypes.CDLL(str(_find_cdylib()))
    except Exception as exc:  # absent, unbuildable, or unloadable
        return _UnavailableLib(exc)


_lib = _load_lib()

_lib.fauna_post_build.argtypes = [
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # secret, secret_len
    ctypes.c_char_p,                                   # body
    ctypes.c_char_p,                                   # tags_json (null or JSON array)
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # reply_to, reply_to_len
    ctypes.POINTER(_FfiBuffer),
]
_lib.fauna_post_build.restype = ctypes.c_int32

# Bound defensively rather than eagerly: this module is imported at the TOP of
# many tests, while the release `libfauna_ffi.so` it loads is (re)built later by
# whichever `just` recipe the test shells out to. Touching a missing symbol here
# would raise at IMPORT time and take down every unrelated test in the process;
# deferring turns a stale library into one actionable error, in the one test
# that actually needs the symbol.
_HAS_POST_BUILD_WITH_MEDIA = hasattr(_lib, "fauna_post_build_with_media")
if _HAS_POST_BUILD_WITH_MEDIA:
    _lib.fauna_post_build_with_media.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # secret, secret_len
        ctypes.c_char_p,                                   # body
        ctypes.c_char_p,                                   # media_json
        ctypes.POINTER(_FfiBuffer),
    ]
    _lib.fauna_post_build_with_media.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_POST_BUILD_VIDEO = hasattr(_lib, "fauna_post_build_video")
if _HAS_POST_BUILD_VIDEO:
    _lib.fauna_post_build_video.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # secret, secret_len
        ctypes.c_char_p,                                   # video_json
        ctypes.POINTER(_FfiBuffer),
    ]
    _lib.fauna_post_build_video.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_PROFILE_BUILD_WITH_PICTURES = hasattr(_lib, "fauna_profile_build_with_pictures")
if _HAS_PROFILE_BUILD_WITH_PICTURES:
    _lib.fauna_profile_build_with_pictures.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # secret, secret_len
        ctypes.c_char_p,                                   # display_name
        ctypes.c_char_p,                                   # bio
        ctypes.c_char_p,                                   # avatar_cid
        ctypes.c_char_p,                                   # banner_cid
        ctypes.POINTER(_FfiBuffer),
    ]
    _lib.fauna_profile_build_with_pictures.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_ATPROTO_DELEGATION_CERT = hasattr(_lib, "fauna_atproto_delegation_cert_build")
if _HAS_ATPROTO_DELEGATION_CERT:
    _lib.fauna_atproto_delegation_cert_build.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # identity secret, len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,  # k_pub, len
        ctypes.c_char_p,                                   # capabilities_json
        ctypes.c_uint64,                                   # created_at_micros
        ctypes.c_uint64,                                   # expires_at_micros (0 = never)
        ctypes.POINTER(_FfiBuffer),
    ]
    _lib.fauna_atproto_delegation_cert_build.restype = ctypes.c_int32

_lib.fauna_post_build_gated.argtypes = [
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret, secret_len
    ctypes.c_char_p,                                   # preview
    ctypes.c_char_p,                                   # full_body
    ctypes.c_char_p,                                   # tier
    ctypes.c_uint32,                                   # tier_rank
    ctypes.POINTER(ctypes.c_uint8),                    # key_blob_ref (32)
    ctypes.POINTER(ctypes.c_uint8),                    # period_key (32)
    ctypes.POINTER(_FfiBuffer),                        # out_post
    ctypes.POINTER(_FfiBuffer),                        # out_blob
]
_lib.fauna_post_build_gated.restype = ctypes.c_int32

_lib.fauna_capability_build_post_grant.argtypes = [
    ctypes.POINTER(ctypes.c_uint8),                    # owner (32)
    ctypes.POINTER(ctypes.c_uint8),                    # grant_id (16)
    ctypes.POINTER(ctypes.c_uint8),                    # holder_pubkey (32)
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # holder_mlkem_ek, len (null+0 = classical)
    ctypes.c_uint64,                                   # epoch_start
    ctypes.c_uint64,                                   # epoch_end
    ctypes.c_char_p,                                   # tier
    ctypes.POINTER(ctypes.c_uint8),                    # period_key (32)
    ctypes.POINTER(_FfiBuffer),                        # out
]
_lib.fauna_capability_build_post_grant.restype = ctypes.c_int32

_lib.fauna_folder_seal_file.argtypes = [
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # content, content_len
    ctypes.POINTER(ctypes.c_uint8),                    # content_key (null = plaintext, else 32)
    ctypes.POINTER(_FfiBuffer),                        # out
]
_lib.fauna_folder_seal_file.restype = ctypes.c_int32

# Bound defensively: a stale library without it fails only the test that seals
# an owner-only file.
if hasattr(_lib, "fauna_folder_seal_owner_file"):
    _lib.fauna_folder_seal_owner_file.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                    # secret (32-byte Ed25519 seed)
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # content, content_len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_folder_seal_owner_file.restype = ctypes.c_int32

# Bound defensively (the `fauna_device_label_seal` convention below): a stale
# `libfauna_ffi.so` without it must fail only the test that unframes.
if hasattr(_lib, "fauna_chunk_unframe"):
    _lib.fauna_chunk_unframe.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # body, body_len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_chunk_unframe.restype = ctypes.c_int32

_lib.fauna_capability_build_folder_grant.argtypes = [
    ctypes.POINTER(ctypes.c_uint8),                    # owner (32)
    ctypes.POINTER(ctypes.c_uint8),                    # grant_id (16)
    ctypes.POINTER(ctypes.c_uint8),                    # holder_pubkey (32)
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # holder_mlkem_ek, len (null+0 = classical)
    ctypes.c_uint64,                                   # epoch_start
    ctypes.c_uint64,                                   # epoch_end
    ctypes.c_char_p,                                   # set_name
    ctypes.c_uint64,                                   # version (content-key generation)
    ctypes.POINTER(ctypes.c_uint8),                    # content_key (32)
    ctypes.POINTER(_FfiBuffer),                        # out
]
_lib.fauna_capability_build_folder_grant.restype = ctypes.c_int32

_lib.fauna_capability_build_folder_scope_wrap.argtypes = [
    ctypes.POINTER(ctypes.c_uint8),                    # owner (32)
    ctypes.POINTER(ctypes.c_uint8),                    # holder_pubkey (32)
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # holder_mlkem_ek, len (null+0 = classical)
    ctypes.c_char_p,                                   # set_name
    ctypes.c_uint64,                                   # version (content-key generation)
    ctypes.POINTER(ctypes.c_uint8),                    # content_key (32)
    ctypes.POINTER(_FfiBuffer),                        # out
]
_lib.fauna_capability_build_folder_scope_wrap.restype = ctypes.c_int32

# The third-party `ext.*` pair (`third-party-kinds.md` § The record doors):
# the owner's consent-time grant and the principal's row sealer. Bound
# defensively, like `fauna_chunk_unframe` above.
if hasattr(_lib, "fauna_capability_build_ext_kinds_grant"):
    _lib.fauna_capability_build_ext_kinds_grant.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                # identity_seed (32)
        ctypes.POINTER(ctypes.c_uint8),                # grant_id (16)
        ctypes.POINTER(ctypes.c_uint8),                # holder_x25519 (32)
        ctypes.c_uint64,                               # epoch_start
        ctypes.c_uint64,                               # epoch_end
        ctypes.c_char_p,                               # kinds, newline-separated
        ctypes.POINTER(ctypes.c_uint8),                # writer (32) or null
        ctypes.POINTER(_FfiBuffer),                    # out
    ]
    _lib.fauna_capability_build_ext_kinds_grant.restype = ctypes.c_int32
# The third-party folder deposit grant (`file-sync.md` § Third-party deposit
# ingress) — keyless, one folder.
if hasattr(_lib, "fauna_capability_build_folder_deposit_grant"):
    _lib.fauna_capability_build_folder_deposit_grant.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                # identity_seed (32)
        ctypes.POINTER(ctypes.c_uint8),                # grant_id (16)
        ctypes.POINTER(ctypes.c_uint8),                # holder_x25519 (32)
        ctypes.c_uint64,                               # epoch_start
        ctypes.c_uint64,                               # epoch_end
        ctypes.c_int64,                                # folder_id
        ctypes.POINTER(_FfiBuffer),                    # out
    ]
    _lib.fauna_capability_build_folder_deposit_grant.restype = ctypes.c_int32
if hasattr(_lib, "fauna_ext_kind_seal_row"):
    _lib.fauna_ext_kind_seal_row.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # grant_blob, len
        ctypes.POINTER(ctypes.c_uint8),                    # holder_secret (32)
        ctypes.POINTER(ctypes.c_uint8),                    # writer_secret (32)
        ctypes.c_char_p,                                   # kind
        ctypes.c_char_p,                                   # key
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # value, len
        ctypes.c_uint64,                                   # writer_seq
        ctypes.c_int64,                                    # at_ms
        ctypes.c_uint8,                                    # tombstone
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_ext_kind_seal_row.restype = ctypes.c_int32
if hasattr(_lib, "fauna_bridged_seal_for_user"):
    _lib.fauna_bridged_seal_for_user.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                    # recipient_x25519 (32)
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # mlkem_ek, len (0 = classical)
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # plaintext, len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_bridged_seal_for_user.restype = ctypes.c_int32
    _lib.fauna_bridged_open_as_bridge.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                    # bridge_secret (32)
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # sealed, len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_bridged_open_as_bridge.restype = ctypes.c_int32

_lib.fauna_path_seal.argtypes = [
    ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
    ctypes.c_char_p,                                   # path
    ctypes.POINTER(_FfiBuffer),                        # out
]
_lib.fauna_path_seal.restype = ctypes.c_int32

# Bound DEFENSIVELY, per the convention documented at `fauna_post_build_with_media`
# below — and here it matters more than anywhere else in this module: the device
# seal rides `common.auth.sync_register`, which nearly every seeding fixture calls,
# so an eager bind against a stale `libfauna_ffi.so` would raise at IMPORT time and
# take down the whole run instead of one test.
_HAS_DEVICE_LABEL_SEAL = hasattr(_lib, "fauna_device_label_seal")
if _HAS_DEVICE_LABEL_SEAL:
    _lib.fauna_device_label_seal.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # device_id (raw 32 bytes, the salt), len
        ctypes.c_char_p,                                   # label
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_device_label_seal.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_DEVICE_LABEL_OPEN = hasattr(_lib, "fauna_device_label_open")
if _HAS_DEVICE_LABEL_OPEN:
    _lib.fauna_device_label_open.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # device_id (raw 32 bytes, the salt), len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # sealed envelope off the wire row, len
        ctypes.c_char_p,                                   # plaintext `label` column
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_device_label_open.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_DEVICE_GRANT_BUILD = hasattr(_lib, "fauna_device_grant_build")
if _HAS_DEVICE_GRANT_BUILD:
    _lib.fauna_device_grant_build.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # device_key (32-byte public key), len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_device_grant_build.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_KEYBLOB_OPEN = hasattr(_lib, "fauna_keyblob_open")
if _HAS_KEYBLOB_OPEN:
    _lib.fauna_keyblob_open.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # subscriber secret (32-byte seed), len
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # key_blob.get's blob_data, len
        ctypes.POINTER(_FfiBuffer),                        # out: 32-byte key || 32-byte author
    ]
    _lib.fauna_keyblob_open.restype = ctypes.c_int32

# Same deferred binding as above, for the same reason.
_HAS_BIRTH_UPLOAD = hasattr(_lib, "fauna_subscriptions_build_birth_upload")
if _HAS_BIRTH_UPLOAD:
    _lib.fauna_subscriptions_build_birth_upload.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # author secret (32-byte seed), len
        ctypes.c_char_p,                                   # tier name
        ctypes.POINTER(ctypes.c_uint8),                    # period key (32 bytes)
        ctypes.POINTER(_FfiBuffer),                        # out: dag-cbor EncryptedKeyBlobUpload
    ]
    _lib.fauna_subscriptions_build_birth_upload.restype = ctypes.c_int32

_lib.fauna_ffi_last_error.argtypes = []
_lib.fauna_ffi_last_error.restype = ctypes.c_char_p

_lib.fauna_ffi_free_buffer.argtypes = [_FfiBuffer]
_lib.fauna_ffi_free_buffer.restype = None


def _take_buffer(rc: int, out: _FfiBuffer, fn_name: str) -> bytes:
    """Copy the FFI buffer to Python bytes, free it, and raise on error."""
    if rc != 0:
        err = _lib.fauna_ffi_last_error()
        msg = err.decode("utf-8", "replace") if err else "unknown FFI error"
        raise RuntimeError(f"{fn_name}: {msg}")
    try:
        return bytes(ctypes.cast(out.data, ctypes.POINTER(ctypes.c_uint8 * out.len))[0])
    finally:
        _lib.fauna_ffi_free_buffer(out)


def seal_path(secret: bytes, path: str) -> bytes:
    """Seal a folder-relative ``path`` under the **owner root**, the way an
    unbound set's writer does — returns the canonical dag-cbor ``SealedLabel``
    envelope to put in a ``fauna.sync.changes.record``'s ``path_sealed``.

    **Use this, not a hand-rolled envelope, for any seed whose rows an app must
    render.** Since the S9 flip the nest rests no plaintext ``path``, so a reader
    that cannot open the seal has nothing to fall back on and the row degrades to
    ``Omit`` — which *drops the item*, not renders it nameless. A synthetic blob
    therefore seeds rows no app can ever see, and every UI assertion over them
    fails for reasons unrelated to what it tests.

    The bytes come from the real ``fauna_core::label_custody::seal_path`` funnel
    via ``fauna_path_seal`` (``libs/fauna-ffi/src/cabi.rs``), so a seed cannot
    drift from what a client writes — a drifted seal fails **silently**, as
    ``Omit``.

    ``secret``: the actor's 32-byte Ed25519 seed (``SigningKey.encode()``).

    ⚠ **Owner root, no generation** — correct for a set created and never bound
    to a folder (no M2 content keys), which is what the raw-RPC seeding fixtures
    make. A *bound* set seals under its content-key generation instead; an
    owner-rooted blob there opens for nobody.
    """
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    out = _FfiBuffer()
    rc = _lib.fauna_path_seal(
        sec_arr, 32,
        path.encode("utf-8"),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_path_seal")


def seal_device_label(secret: bytes, device_id: bytes, label: str) -> bytes | None:
    """Seal a user-chosen ``label`` under the **registering owner's root**, the
    way a client's sync daemon does — returns the canonical dag-cbor
    ``SealedLabel`` envelope to put in a ``fauna.sync.register``'s
    ``label_sealed``, or ``None`` for a machine-authored label that must not seal.

    **Use this for any device seed whose name an app must render.** The S9 flip
    scrubs the plaintext ``sync_devices.label`` for every user-chosen label
    (``register_sync_device``, ``bins/fauna-nest/src/db/sync_storage.rs``), so a
    *sealless* register rests no label at all and the row comes back nameless
    forever. Every device-name assertion over such a seed then fails for a reason
    unrelated to what it tests.

    ⚠ **This plane's degrade differs from the path plane's — do not carry a
    conclusion across.** ``DevicesMachine::render_devices`` maps ``Omit`` to an
    **empty label on a KEPT row** (a device stays actionable by ``device_id``, and
    hiding one the user may need to revoke is the worse outcome), where
    :func:`seal_path`'s plane *drops* the item. So a missing device seal breaks
    only *name* assertions; ``device-card`` counts stay correct.

    The bytes come from the real ``fauna_core::label_custody::seal_device_label``
    funnel via ``fauna_device_label_seal`` (``libs/fauna-ffi/src/cabi.rs``) — a
    reimplementation here would be a second writer of a one-funnel plane, and a
    drifted seal fails **silently**, as ``Omit``.

    ``secret``: the actor's 32-byte Ed25519 seed (``SigningKey.encode()``).
    ``device_id``: the **raw** 32-byte device id — the seal's salt, so it must be
    the same bytes whose hex the register payload carries.

    Returns ``None`` when ``label`` is one of the three machine-authored constants
    (``is_synthetic_device_label``: the WebDAV pseudo-device, the backup
    coordinator, the self-register placeholder), which rest plaintext by ratified
    design. Registering those sealless is correct, not a fallback.
    """
    if not _HAS_DEVICE_LABEL_SEAL:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_device_label_seal`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    if len(device_id) != 32:
        raise ValueError("device_id must be the raw 32 bytes, not its hex")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    dev_arr = (ctypes.c_uint8 * 32)(*device_id)
    out = _FfiBuffer()
    rc = _lib.fauna_device_label_seal(
        sec_arr, 32,
        dev_arr, 32,
        label.encode("utf-8"),
        ctypes.byref(out),
    )
    sealed = _take_buffer(rc, out, "fauna_device_label_seal")
    # An empty buffer is the `Ok(None)` arm (a synthetic label), not a failure —
    # see the cabi doc. Surfaced as `None` so a caller passes no `label_sealed`.
    return sealed or None


def build_device_grant(secret: bytes, device_key: bytes) -> dict:
    """Sign the root-signed, ``RenewBearer``-scoped ``DeviceAuthorization`` over
    ``device_key`` — a device's enrollment grant — and return it in the
    ``{envelope, bytes}`` embed-as-bytes shape a
    ``fauna.sync.device_grant.register``'s ``authorization`` field carries.

    The bytes come from the one real minter
    (``fauna_client_sync::build_principal_grant``) via ``fauna_device_grant_build``
    (``libs/fauna-ffi/src/cabi.rs``); a fixture must never re-derive a signed
    envelope. Registering it gives the row a granted ``principal`` — the key
    every principal-keyed Devices-page rule reads.
    """
    if not _HAS_DEVICE_GRANT_BUILD:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_device_grant_build`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    if len(device_key) != 32:
        raise ValueError("device_key must be the raw 32-byte public key")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    key_arr = (ctypes.c_uint8 * 32)(*device_key)
    out = _FfiBuffer()
    rc = _lib.fauna_device_grant_build(sec_arr, 32, key_arr, 32, ctypes.byref(out))
    return cbor2.loads(_take_buffer(rc, out, "fauna_device_grant_build"))


def open_device_label(secret: bytes, device_id: bytes,
                      sealed: bytes | None, plaintext: str = "") -> str | None:
    """Render one device label sealed-first, the way an app's device list does —
    returns the label, or ``None`` for the ``Omit`` degrade (this reader cannot
    render it).

    **This is the read-side seam for driver-less assertions on the device plane.**
    :func:`seal_path`'s plane is convergent, so a test asserts on it by re-sealing
    the path it expects; a device label uses a **random nonce** (it is mutable and
    the salt does not determine it), so its envelope is not reproducible and
    byte-comparison is unavailable. Opening is the only way to assert what a
    register actually stored without a client driver.

    Post-flip the wire row's plaintext ``label`` is ``''`` for any user-chosen
    label, so ``fauna.sync.devices.list``'s ``label`` field alone can no longer be
    asserted on — pass the row's ``label`` and ``label_sealed`` here instead::

        row = next(d for d in reply["devices"] if d["device_id"] == dev_hex)
        assert open_device_label(seed, bytes.fromhex(dev_hex),
                                 row.get("label_sealed"), row.get("label", "")) == "my-linux-box"

    Same ``fauna_core::label_custody::render_device_label`` seam under the same
    owner-only custody as ``DevicesMachine::render_devices``, so what this renders
    is by construction what the owner's app renders.
    """
    if not _HAS_DEVICE_LABEL_OPEN:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_device_label_open`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    if len(device_id) != 32:
        raise ValueError("device_id must be the raw 32 bytes, not its hex")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    dev_arr = (ctypes.c_uint8 * 32)(*device_id)
    blob = bytes(sealed or b"")
    blob_arr = (ctypes.c_uint8 * len(blob))(*blob) if blob else None
    out = _FfiBuffer()
    rc = _lib.fauna_device_label_open(
        sec_arr, 32,
        dev_arr, 32,
        blob_arr, len(blob),
        plaintext.encode("utf-8"),
        ctypes.byref(out),
    )
    rendered = _take_buffer(rc, out, "fauna_device_label_open")
    # Empty buffer == `SealedLabelRender::Omit`, the plane's ratified degrade
    # (the app keeps the row and shows no name) — surfaced as `None` so a caller
    # cannot mistake it for a successfully-rendered empty string.
    return rendered.decode("utf-8") if rendered else None


def build_tier_birth_upload(secret: bytes, tier: str, period_key: bytes) -> dict:
    """The birth ``KeyBlob`` envelope ``fauna.subscriptions.tiers.create``
    requires, as the decoded ``encrypted_upload`` map to drop into the request
    payload: an empty-roster blob for ``tier`` under ``period_key``,
    self-signed by the author whose 32-byte Ed25519 seed is ``secret``.

    Rides the same shared ``mint_self_delegated_upload`` every app's create
    rides, for :func:`build_authoring_delegation_cert`'s stated reason — a
    test-only reimplementation would agree with itself about an envelope the
    nest refuses.
    """
    if not _HAS_BIRTH_UPLOAD:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_subscriptions_build_birth_upload`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32 or len(period_key) != 32:
        raise ValueError("secret and period_key must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    pk_arr = (ctypes.c_uint8 * 32)(*period_key)
    out = _FfiBuffer()
    rc = _lib.fauna_subscriptions_build_birth_upload(
        sec_arr, 32,
        tier.encode("utf-8"),
        pk_arr,
        ctypes.byref(out),
    )
    return cbor2.loads(_take_buffer(rc, out, "fauna_subscriptions_build_birth_upload"))


def open_key_blob(secret: bytes, blob_data: bytes) -> tuple[bytes, bytes]:
    """Unwrap a tier's period key from a stored broadcast ``KeyBlob``, as the
    subscriber whose 32-byte Ed25519 seed is ``secret``.

    Returns ``(period_key, blob_author)`` — the 32-byte key this subscriber can
    actually open, and the 32-byte actor id of whoever minted the blob.

    ``blob_data`` is ``fauna.subscriptions.key_blob.get``'s ``blob_data`` field
    verbatim (see :meth:`ApiActor.subscription_key_blob`).

    **The read-side seam for asserting a tier really re-keyed.** After an
    identity succession the aftermath's tier leg mints a fresh period key and
    republishes the covering blob, so "what you publish afterwards cannot be read
    by whoever held the old key" is observable only by unwrapping before and
    after and comparing the KEYS — a changed blob, a bumped version or a new hash
    prove nothing about the key inside. The author comes back too because it is
    the era stamp the leg derives from: a blob published before the ceremony
    still names the predecessor even after the succession moved the row, so the
    flip is directly assertable::

        before, _ = open_key_blob(sub_seed, api.subscription_key_blob(a, t)["blob_data"])
        ...  # ceremony + the successor's first session
        after, author = open_key_blob(sub_seed, api.subscription_key_blob(heir, t)["blob_data"])
        assert after != before and author.hex() == successor_id

    Rides the same ``fauna_core::subscription::crypto`` unwrap every app rides,
    for :func:`build_authoring_delegation_cert`'s stated reason: the bug class
    here is a client and a nest disagreeing about the envelope, which a
    test-only reimplementation would agree with itself about.

    Raises if the blob does not decode, or carries no entry for this subscriber
    — the latter is a real finding (a rotation that dropped a paying member),
    never a quiet ``None``.
    """
    if not _HAS_KEYBLOB_OPEN:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_keyblob_open`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    blob = bytes(blob_data)
    if not blob:
        raise ValueError("blob_data is empty — key_blob.get returned no blob")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    blob_arr = (ctypes.c_uint8 * len(blob))(*blob)
    out = _FfiBuffer()
    rc = _lib.fauna_keyblob_open(
        sec_arr, 32,
        blob_arr, len(blob),
        ctypes.byref(out),
    )
    packed = _take_buffer(rc, out, "fauna_keyblob_open")
    if len(packed) != 64:
        raise RuntimeError(
            f"fauna_keyblob_open returned {len(packed)} bytes, expected 64 "
            f"(32-byte period key || 32-byte blob author)"
        )
    return packed[:32], packed[32:]


def cid_of_post_id(post_id_hex: str) -> bytes:
    """The 36-byte IPLD CID of a post, from the 32-byte content-row id hex that
    ``fauna.posts.create`` returns.

    A post's row id IS its record CID's digest (``post_id == record_cid.digest()``
    — ``bins/fauna-nest/src/segments/post.rs``), so the CID is just that digest
    behind the standard ``v1 + dag-cbor + blake3-256 + len 32`` prefix. Use it to
    turn a created post's id into the ``reply_to`` :func:`build_post` wants.
    """
    digest = bytes.fromhex(post_id_hex)
    if len(digest) != 32:
        raise ValueError(f"post id must be a 32-byte hex digest, got {len(digest)}")
    return bytes([0x01, 0x71, 0x1E, 0x20]) + digest


def build_authoring_delegation_cert(
    identity_secret: bytes,
    k_pub: bytes,
    capabilities: list[str] | None = None,
    created_at_micros: int = 0,
    expires_at_micros: int = 0,
) -> bytes:
    """Mint a D10 delegated-authoring cert (``atproto-pds-full.md`` § D10).

    The bytes go straight into
    ``fauna.bridges.atproto.provision_authoring_delegation``'s ``cert`` field.
    This wraps the SAME shared-Rust minter all 7 apps use
    (``fauna_client_bridges::atproto_delegation``), so the cert a test provisions
    is byte-identical to one a real client mints — a tier_3 write test proves
    nothing if the authorization on the path is a test-only reimplementation.

    identity_secret: the account's 32-byte Ed25519 identity seed (it signs the
        cert; its public half is the grantor).
    k_pub: the 32-byte authoring sub-key from
        ``fauna.bridges.atproto.fetch_authoring_key`` — fetch it FIRST, the nest
        checks the cert names that exact key.
    capabilities: subset of ``["Post", "UpdateProfile"]``; ``None`` ⇒ ``["Post"]``.
    created_at_micros / expires_at_micros: epoch MICROSECONDS —
        ``fauna_core::data::Timestamp``'s unit. Milliseconds here would mint a
        cert the ingest gate reads as decades expired (its step-5 check compares
        the cert against the signed post's own microsecond ``created_at``), so
        every post it authorizes would be rejected. ``expires_at_micros=0``
        mints a non-expiring delegation, still revocable from the client.
    """
    if not _HAS_ATPROTO_DELEGATION_CERT:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_atproto_delegation_cert_build`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(identity_secret) != 32:
        raise ValueError("identity_secret must be 32 bytes")
    if len(k_pub) != 32:
        raise ValueError("k_pub must be 32 bytes")

    sec_arr = (ctypes.c_uint8 * 32)(*identity_secret)
    k_arr = (ctypes.c_uint8 * 32)(*k_pub)
    caps_json = json.dumps(capabilities).encode("utf-8") if capabilities else None
    out = _FfiBuffer()
    rc = _lib.fauna_atproto_delegation_cert_build(
        sec_arr, 32,
        k_arr, 32,
        caps_json,
        ctypes.c_uint64(created_at_micros),
        ctypes.c_uint64(expires_at_micros),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_atproto_delegation_cert_build")


def build_post(
    secret: bytes,
    body: str,
    tags: list[str] | None = None,
    reply_to: bytes | None = None,
) -> bytes:
    """Build + sign a feed Post, return BARE bytes.

    The result is the raw signed-post body carried by the
    ``fauna.posts.create`` WS-RPC kind (formerly the body of the deleted
    ``POST /api/v1/feeds/{id}/posts`` / ``POST /api/v1/posts`` REST twins).
    The nest's ingest pipeline auto-indexes the post into matching feeds.

    secret: 32-byte Ed25519 seed.
    tags: optional list of tag strings (without the ``#`` prefix).
    reply_to: optional 36-byte post CID — makes the post a reply (adds a
        ``Reference::Reply``). Build it from a created post's id with
        :func:`cid_of_post_id`.
    """
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    if reply_to is not None and len(reply_to) != 36:
        raise ValueError("reply_to must be a 36-byte CID")

    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    tags_json = json.dumps(tags).encode("utf-8") if tags else None
    reply_arr = None
    reply_len = 0
    if reply_to is not None:
        reply_arr = (ctypes.c_uint8 * 36)(*reply_to)
        reply_len = 36
    out = _FfiBuffer()
    rc = _lib.fauna_post_build(
        sec_arr, 32,
        body.encode("utf-8"),
        tags_json,
        reply_arr, reply_len,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_post_build")


def content_cid(data: bytes) -> str:
    """Fauna content CID of ``data``, base32 — the ``{cid_b32}`` path segment of
    ``PUT|GET /api/v1/blob/{cid_b32}``.

    CIDv1 + raw codec (``0x55``) + blake3-256 multihash, the shape
    ``fauna_cbor::Cid::of_raw`` produces (the twin of :func:`cid_of_post_id`,
    which uses the dag-cbor codec ``0x71`` because a post id names an encoded
    value rather than opaque bytes).
    """
    import blake3

    raw = bytes([0x01, 0x55, 0x1E, 0x20]) + blake3.blake3(data).digest()
    return "b" + base64.b32encode(raw).decode("ascii").lower().rstrip("=")


def build_post_with_media(
    secret: bytes,
    body: str,
    media: list[dict],
) -> bytes:
    """Build + sign a feed Post with image attachments, return BARE bytes.

    ``media`` is a list of descriptors ``{"blob_cid", "mime", "size_bytes"}``
    plus optional ``"width"``/``"height"``. Each ``blob_cid`` must name bytes
    already uploaded to the nest (``PUT /api/v1/blob/{cid_b32}``): the post is a
    signed claim about those bytes, so building one for a blob the nest does not
    hold would be a lie the signature carries. Get the CID with
    :func:`content_cid`.
    """
    if not _HAS_POST_BUILD_WITH_MEDIA:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_post_build_with_media`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    out = _FfiBuffer()
    rc = _lib.fauna_post_build_with_media(
        sec_arr, 32,
        body.encode("utf-8"),
        json.dumps(media).encode("utf-8"),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_post_build_with_media")


def build_video_post(
    secret: bytes,
    manifest_cid: str,
    thumbnail_cid: str,
    segments: list[dict],
    duration_ms: int = 3000,
    aspect: tuple[int, int] = (16, 9),
) -> bytes:
    """Build + sign a video Post (``PostBody::Video``), return BARE bytes.

    ``segments`` is a list of ``{"blob_cid", "resolution", "byte_size"}`` plus
    optional ``"codec"``/``"bitrate"``, **in playback order within each
    resolution** — that order is what the ATProto projection concatenates. Every
    ``blob_cid`` must name bytes already uploaded to the nest
    (``PUT /api/v1/blob/{cid_b32}``): same signed-claim rule as
    :func:`build_post_with_media`. Get CIDs with :func:`content_cid`.

    A video post carries no text of its own — that is what makes the projection's
    "never commit a blank record" rule reachable.
    """
    if not _HAS_POST_BUILD_VIDEO:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_post_build_video`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    out = _FfiBuffer()
    payload = {
        "manifest_cid": manifest_cid,
        "thumbnail_cid": thumbnail_cid,
        "duration_ms": duration_ms,
        "aspect_width": aspect[0],
        "aspect_height": aspect[1],
        "segments": segments,
    }
    rc = _lib.fauna_post_build_video(
        sec_arr, 32,
        json.dumps(payload).encode("utf-8"),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_post_build_video")


def build_profile_with_pictures(
    secret: bytes,
    display_name: str | None = None,
    bio: str | None = None,
    avatar_cid: str | None = None,
    banner_cid: str | None = None,
) -> bytes:
    """Build + sign a ``Profile``, return the ``fauna.profile.set`` body.

    ``avatar_cid`` / ``banner_cid`` name bytes already uploaded to the nest
    (``PUT /api/v1/blob/{cid_b32}``) — same rule as
    :func:`build_post_with_media`: the profile is a signed claim about those
    bytes. Get the CID with :func:`content_cid`. Passing ``None`` for a picture
    is how a profile with no picture — or a *cleared* one — is expressed.
    """
    if not _HAS_PROFILE_BUILD_WITH_PICTURES:
        raise RuntimeError(
            "libfauna_ffi is stale: it has no `fauna_profile_build_with_pictures`. "
            "Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    out = _FfiBuffer()

    def _c(s: str | None) -> bytes | None:
        return None if s is None else s.encode("utf-8")

    rc = _lib.fauna_profile_build_with_pictures(
        sec_arr, 32,
        _c(display_name), _c(bio), _c(avatar_cid), _c(banner_cid),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_profile_build_with_pictures")


def build_gated_post(
    secret: bytes,
    preview: str,
    full_body: str,
    tier: str,
    tier_rank: int,
    key_blob_ref: bytes,
    period_key: bytes,
) -> tuple[bytes, bytes]:
    """Build + sign a tier-gated Post (web paywall / gated-content fixtures).

    Returns ``(post_bytes, encrypted_blob)``: the ``fauna.posts.create`` body
    (whose ``GatedInfo`` carries ``encrypted_ref`` = BLAKE3 of the blob, the
    tier, and the random ``seal_id`` derive input) plus the sealed full-body
    blob to upload to the nest blob store. Wraps the shared
    ``fauna_client_core::post::build_gated_post`` seal helper.
    """
    if len(secret) != 32 or len(key_blob_ref) != 32 or len(period_key) != 32:
        raise ValueError("secret, key_blob_ref, and period_key must be 32 bytes")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    kbr_arr = (ctypes.c_uint8 * 32)(*key_blob_ref)
    pk_arr = (ctypes.c_uint8 * 32)(*period_key)
    out_post = _FfiBuffer()
    out_blob = _FfiBuffer()
    rc = _lib.fauna_post_build_gated(
        sec_arr, 32,
        preview.encode("utf-8"),
        full_body.encode("utf-8"),
        tier.encode("utf-8"),
        tier_rank,
        kbr_arr,
        pk_arr,
        ctypes.byref(out_post),
        ctypes.byref(out_blob),
    )
    post_bytes = _take_buffer(rc, out_post, "fauna_post_build_gated")
    blob_bytes = _take_buffer(rc, out_blob, "fauna_post_build_gated")
    return post_bytes, blob_bytes


def build_post_grant(
    owner: bytes,
    grant_id: bytes,
    holder_pubkey: bytes,
    holder_mlkem_ek: bytes | None,
    epoch_start: int,
    epoch_end: int,
    tier: str,
    period_key: bytes,
) -> bytes:
    """Build the canonical-CBOR capability GrantBlob wrapping a tier period
    key to the web-serve holder — the ``fauna.capabilities.mint`` grant_blob
    for a ``content.read{post:tier}`` scope. ``holder_mlkem_ek`` selects the
    X-Wing hybrid wrap when the holder published one (None = classical)."""
    if len(owner) != 32 or len(grant_id) != 16 or len(holder_pubkey) != 32 or len(period_key) != 32:
        raise ValueError("owner/holder/period_key must be 32 bytes; grant_id 16")
    owner_arr = (ctypes.c_uint8 * 32)(*owner)
    grant_arr = (ctypes.c_uint8 * 16)(*grant_id)
    holder_arr = (ctypes.c_uint8 * 32)(*holder_pubkey)
    pk_arr = (ctypes.c_uint8 * 32)(*period_key)
    if holder_mlkem_ek:
        ek_arr = (ctypes.c_uint8 * len(holder_mlkem_ek))(*holder_mlkem_ek)
        ek_len = len(holder_mlkem_ek)
    else:
        ek_arr = None
        ek_len = 0
    out = _FfiBuffer()
    rc = _lib.fauna_capability_build_post_grant(
        owner_arr,
        grant_arr,
        holder_arr,
        ek_arr,
        ek_len,
        epoch_start,
        epoch_end,
        tier.encode("utf-8"),
        pk_arr,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_capability_build_post_grant")


def seal_folder_file(
    content: bytes, content_key: bytes | None
) -> tuple[bytes, list[tuple[bytes, bytes]]]:
    """Chunk (and, when ``content_key`` is given, content-key-seal) a
    ``web``-mode folder file, returning ``(manifest_bytes, [(store_key, body),
    …])`` — the folder analogue of ``build_gated_post``.

    Upload each ``body`` to ``POST /api/v1/chunks`` with ``X-Content-Hash:
    store_key`` (the upload reply's ``hash`` must echo it: a sealed body is
    keyed by its own ``blake3``, a plaintext body is FRAMED and keyed by the
    plaintext hash it unframes to); upload ``manifest_bytes`` to ``POST /api/v1/manifests`` and
    record the returned manifest hash via ``fauna.sync.changes.record``.

    ``content_key``: ``None`` = a plaintext (public) set — store key == plaintext
    chunk hash. 32 bytes = a paywalled sealed set — chunks are ``chunk_crypto``
    AEAD ciphertext addressed by ciphertext hash via ``manifest.stored_hashes``
    (the M2 shape). Wraps the shared ``fauna_core`` chunk+seal path (the same
    code the nest's ``seed_synced_file`` test helper runs), so the harness never
    reimplements the chunk KDF.
    """
    if content_key is not None and len(content_key) != 32:
        raise ValueError("content_key must be 32 bytes or None")
    content_arr = (ctypes.c_uint8 * len(content))(*content)
    key_arr = (ctypes.c_uint8 * 32)(*content_key) if content_key is not None else None
    out = _FfiBuffer()
    rc = _lib.fauna_folder_seal_file(
        content_arr,
        len(content),
        key_arr,
        ctypes.byref(out),
    )
    bundle = _take_buffer(rc, out, "fauna_folder_seal_file")
    manifest_bytes, chunks = cbor2.loads(bundle)
    return manifest_bytes, [(bytes(store_key), bytes(body)) for store_key, body in chunks]


def seal_owner_file(
    secret: bytes, content: bytes
) -> tuple[bytes, list[tuple[bytes, bytes]]]:
    """``seal_folder_file`` for a file in an **owner-only** set: every chunk
    sealed under the owner root the actor's seed ``secret`` derives — the root
    an unbound set's writer seals under, and the one a private share link opens
    the manifest's hashes with. Same return shape; the root never leaves Rust."""
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    if not hasattr(_lib, "fauna_folder_seal_owner_file"):
        raise RuntimeError(
            "libfauna_ffi has no `fauna_folder_seal_owner_file` — it is stale. "
            "Run `just e2e-ffi` to rebuild it."
        )
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    content_arr = (ctypes.c_uint8 * len(content))(*content)
    out = _FfiBuffer()
    rc = _lib.fauna_folder_seal_owner_file(
        sec_arr, content_arr, len(content), ctypes.byref(out)
    )
    bundle = _take_buffer(rc, out, "fauna_folder_seal_owner_file")
    manifest_bytes, chunks = cbor2.loads(bundle)
    return manifest_bytes, [(bytes(store_key), bytes(body)) for store_key, body in chunks]


def unframe_chunk(body: bytes) -> bytes:
    """Strip one stored chunk body's frame through the shared strict unframe
    (``fauna_core::compress::unframe_strict_bounded``) — the framed half of
    what every file-sync reader runs before reassembly. Raises on a body with
    no frame prefix."""
    if not hasattr(_lib, "fauna_chunk_unframe"):
        raise RuntimeError(
            "libfauna_ffi lacks fauna_chunk_unframe — run `just e2e-ffi` to "
            "rebuild it"
        )
    body_arr = (ctypes.c_uint8 * len(body))(*body)
    out = _FfiBuffer()
    rc = _lib.fauna_chunk_unframe(body_arr, len(body), ctypes.byref(out))
    return _take_buffer(rc, out, "fauna_chunk_unframe")


def _folder_set_qualifier(set_name: str) -> str:
    """The `content.read{folder}` scope's `set` qualifier: the lowercase hex of the
    set's `set_name_hash`, never the plaintext name
    (`ScopeTuple::folder_set_qualifier`). The C-ABI takes the qualifier verbatim."""
    from helpers.set_names import set_name_hash

    return set_name_hash(set_name).hex()


def build_folder_grant(
    owner: bytes,
    grant_id: bytes,
    holder_pubkey: bytes,
    holder_mlkem_ek: bytes | None,
    epoch_start: int,
    epoch_end: int,
    set_name: str,
    version: int,
    content_key: bytes,
) -> bytes:
    """Build the canonical-CBOR capability GrantBlob wrapping a ``web``-mode file
    set's content key (one generation, ``epoch = version``) to the web-serve
    holder — the ``fauna.capabilities.mint`` grant_blob for a
    ``content.read{folder:set}`` scope. The folder twin of ``build_post_grant``
    and the Python analogue of the production ``mint_folder_grant``.
    ``holder_mlkem_ek`` selects the X-Wing hybrid wrap when the holder published
    one (None = classical)."""
    if (
        len(owner) != 32
        or len(grant_id) != 16
        or len(holder_pubkey) != 32
        or len(content_key) != 32
    ):
        raise ValueError("owner/holder/content_key must be 32 bytes; grant_id 16")
    owner_arr = (ctypes.c_uint8 * 32)(*owner)
    grant_arr = (ctypes.c_uint8 * 16)(*grant_id)
    holder_arr = (ctypes.c_uint8 * 32)(*holder_pubkey)
    ck_arr = (ctypes.c_uint8 * 32)(*content_key)
    if holder_mlkem_ek:
        ek_arr = (ctypes.c_uint8 * len(holder_mlkem_ek))(*holder_mlkem_ek)
        ek_len = len(holder_mlkem_ek)
    else:
        ek_arr = None
        ek_len = 0
    out = _FfiBuffer()
    rc = _lib.fauna_capability_build_folder_grant(
        owner_arr,
        grant_arr,
        holder_arr,
        ek_arr,
        ek_len,
        epoch_start,
        epoch_end,
        _folder_set_qualifier(set_name).encode("utf-8"),
        version,
        ck_arr,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_capability_build_folder_grant")


def _u8(raw: bytes):
    return (ctypes.c_uint8 * len(raw))(*raw) if raw else None


def build_ext_kinds_grant(
    identity_seed: bytes,
    grant_id: bytes,
    holder_x25519: bytes,
    epoch_start: int,
    epoch_end: int,
    kinds: list[str],
    writer: bytes | None,
) -> bytes:
    """The owner's consent-time grant to a third-party principal over
    ``kinds`` (full ``ext.*`` strings) — the production
    ``mint_ext_kinds_grant`` over the delegable branch of ``identity_seed``
    (the owner's 32-byte Ed25519 seed). ``writer`` is the principal's attested
    Ed25519 key (a keyless ``content.write`` tuple per kind), None for a
    read-only grant. Returns the ``fauna.capabilities.mint`` grant_blob."""
    if len(identity_seed) != 32 or len(grant_id) != 16 or len(holder_x25519) != 32:
        raise ValueError("identity_seed/holder_x25519 must be 32 bytes; grant_id 16")
    if writer is not None and len(writer) != 32:
        raise ValueError("writer must be 32 bytes")
    out = _FfiBuffer()
    rc = _lib.fauna_capability_build_ext_kinds_grant(
        _u8(identity_seed),
        _u8(grant_id),
        _u8(holder_x25519),
        epoch_start,
        epoch_end,
        "\n".join(kinds).encode("utf-8"),
        _u8(writer) if writer is not None else None,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_capability_build_ext_kinds_grant")


def build_folder_deposit_grant(
    identity_seed: bytes,
    grant_id: bytes,
    holder_x25519: bytes,
    epoch_start: int,
    epoch_end: int,
    folder_id: int,
) -> bytes:
    """The owner's consent-time keyless ``deposit`` grant to a third-party
    principal over one folder — the production ``mint_folder_deposit_grant``,
    owned by the actor ``identity_seed`` (the owner's 32-byte Ed25519 seed)
    derives. Returns the ``fauna.capabilities.mint`` grant_blob."""
    if len(identity_seed) != 32 or len(grant_id) != 16 or len(holder_x25519) != 32:
        raise ValueError("identity_seed/holder_x25519 must be 32 bytes; grant_id 16")
    out = _FfiBuffer()
    rc = _lib.fauna_capability_build_folder_deposit_grant(
        _u8(identity_seed),
        _u8(grant_id),
        _u8(holder_x25519),
        epoch_start,
        epoch_end,
        folder_id,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_capability_build_folder_deposit_grant")


def ext_kind_seal_row(
    grant_blob: bytes,
    holder_secret: bytes,
    writer_secret: bytes,
    kind: str,
    key: str,
    value: bytes,
    writer_seq: int,
    at_ms: int,
    tombstone: bool = False,
) -> tuple[bytes, bytes]:
    """Seal one ``latest-wins`` row of ``kind`` as the principal: open the
    kind's pair from ``grant_blob`` with the holder's X25519 secret, seal under
    ``ext:<kind>`` signed by the writer's Ed25519 seed. Returns
    ``(item_key, envelope)`` — the two sealed fields of a
    ``fauna.account.state.put``. ``tombstone`` seals the deletion of ``key``
    instead (the payload's tombstone marker; ``value`` is then ignored)."""
    if len(holder_secret) != 32 or len(writer_secret) != 32:
        raise ValueError("holder_secret/writer_secret must be 32 bytes")
    out = _FfiBuffer()
    rc = _lib.fauna_ext_kind_seal_row(
        _u8(grant_blob),
        len(grant_blob),
        _u8(holder_secret),
        _u8(writer_secret),
        kind.encode("utf-8"),
        key.encode("utf-8"),
        _u8(value),
        len(value),
        writer_seq,
        at_ms,
        1 if tombstone else 0,
        ctypes.byref(out),
    )
    sealed = cbor2.loads(_take_buffer(rc, out, "fauna_ext_kind_seal_row"))
    return bytes(sealed["item_key"]), bytes(sealed["envelope"])


def bridged_seal_for_user(
    recipient_x25519: bytes, mlkem_ek: bytes | None, plaintext: bytes
) -> bytes:
    """Seal one bridged message as a bridge principal does — to the user's
    registered recipient key (``fauna.bridges.fetch_recipient_mls_pubkey``'s
    ``key``: its ``mls_pubkey`` and ``mlkem_ek``). The bytes a
    ``fauna.bridges.conversation.deposit`` carries as ``sealed_content``."""
    if len(recipient_x25519) != 32:
        raise ValueError("recipient_x25519 must be 32 bytes")
    ek = mlkem_ek or b""
    out = _FfiBuffer()
    rc = _lib.fauna_bridged_seal_for_user(
        _u8(recipient_x25519), _u8(ek), len(ek), _u8(plaintext), len(plaintext),
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_bridged_seal_for_user")


def bridged_open_as_bridge(bridge_secret: bytes, sealed: bytes) -> bytes:
    """Open one outbound bridged item with the bridge principal's X25519
    holder secret — what a bridge does with an item it drained through
    ``fauna.bridges.conversation.outbox.fetch``. Raises when the item does not
    open under that key."""
    if len(bridge_secret) != 32:
        raise ValueError("bridge_secret must be 32 bytes")
    out = _FfiBuffer()
    rc = _lib.fauna_bridged_open_as_bridge(
        _u8(bridge_secret), _u8(sealed), len(sealed), ctypes.byref(out)
    )
    return _take_buffer(rc, out, "fauna_bridged_open_as_bridge")


def build_folder_scope_wrap(
    owner: bytes,
    holder_pubkey: bytes,
    holder_mlkem_ek: bytes | None,
    set_name: str,
    version: int,
    content_key: bytes,
) -> bytes:
    """Build the canonical-CBOR ``WrappedScopeKey`` for ONE content-key generation
    of a ``content.read{folder:set}`` grant — the wire form each ``appended_keys``
    entry a ``fauna.capabilities.renew`` carries. Used to append a NEW generation's
    key to a standing grant (the rotation leg): the Python twin of the per-
    generation wrap the production ``rotate_paywall_grant`` re-provisions. The AAD
    binds ``(owner, content.read, folder, set, version)`` but not the grant id, so
    the wrap is valid appended to the live grant."""
    if len(owner) != 32 or len(holder_pubkey) != 32 or len(content_key) != 32:
        raise ValueError("owner/holder/content_key must be 32 bytes")
    owner_arr = (ctypes.c_uint8 * 32)(*owner)
    holder_arr = (ctypes.c_uint8 * 32)(*holder_pubkey)
    ck_arr = (ctypes.c_uint8 * 32)(*content_key)
    if holder_mlkem_ek:
        ek_arr = (ctypes.c_uint8 * len(holder_mlkem_ek))(*holder_mlkem_ek)
        ek_len = len(holder_mlkem_ek)
    else:
        ek_arr = None
        ek_len = 0
    out = _FfiBuffer()
    rc = _lib.fauna_capability_build_folder_scope_wrap(
        owner_arr,
        holder_arr,
        ek_arr,
        ek_len,
        _folder_set_qualifier(set_name).encode("utf-8"),
        version,
        ck_arr,
        ctypes.byref(out),
    )
    return _take_buffer(rc, out, "fauna_capability_build_folder_scope_wrap")


# ── The harness's networked seed exports (`libs/fauna-ffi/src/cabi/harness.rs`) ──
#
# Since writer-signed change records (`mls-group-key-material.md` § M2 →
# *Multi-writer* → *Writer-signed change records*, ruling (4)) the nest refuses
# an unsigned change record `signature_required`, so a fixture that seeds
# `sync_changes` rows over RPC sends them through these: the set born
# custody-first by the shared `create_set`, each record signed by the shared
# `RecordSigning` under the nonce the recorder's own custody holds. Python
# builds only the unsigned request; the statement and the signature are never
# re-derived here. Gated on fauna-ffi's `e2e-harness` feature (test surface,
# e2e convention 15) — bound defensively, like the seals above, so a library
# without them fails the one test that needs them.

_HARNESS_EXPORTS = (
    "fauna_harness_create_set",
    "fauna_harness_record_change",
    "fauna_harness_report_conflict",
    "fauna_harness_create_private_link",
)
_HAS_HARNESS = all(hasattr(_lib, name) for name in _HARNESS_EXPORTS)
if _HAS_HARNESS:
    for _name in _HARNESS_EXPORTS:
        _fn = getattr(_lib, _name)
        _fn.argtypes = [
            ctypes.c_char_p,                                   # nest base URL
            ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
            ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # dag-cbor request, len
            ctypes.POINTER(_FfiBuffer),                        # out: {"ok": reply} | {"err": RpcError}
        ]
        _fn.restype = ctypes.c_int32


def _harness_call(fn_name: str, nest_url: str, secret: bytes, request: dict):
    """Run one harness export and return the nest's decoded reply, or raise the
    nest's refusal as the same ``RpcCallError`` the harness's WS client raises
    (so a test asserts on the wire ``code`` whichever path sent the call)."""
    if not _HAS_HARNESS:
        raise RuntimeError(
            f"libfauna_ffi has no `{fn_name}` — it is stale, or not the "
            "`e2e-harness` flavor. Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32:
        raise ValueError("secret must be 32 bytes")
    body = cbor2.dumps(request, canonical=True)
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    req_arr = (ctypes.c_uint8 * len(body)).from_buffer_copy(body)
    out = _FfiBuffer()
    rc = getattr(_lib, fn_name)(
        nest_url.encode("utf-8"), sec_arr, 32, req_arr, len(body), ctypes.byref(out),
    )
    reply = cbor2.loads(_take_buffer(rc, out, fn_name))
    if "err" in reply:
        from clients._ws_rpc_core import _build_rpc_call_error

        raise _build_rpc_call_error(reply["err"])
    return reply["ok"]


def harness_create_set(nest_url: str, secret: bytes, request: dict) -> dict:
    """Create a set **custody-first** as the actor whose seed is ``secret``: the
    shared ``fauna_client_folders::create_set`` mints the set nonce into that
    actor's custody, then sends ``fauna.folders.create`` carrying it.
    ``request`` is the ``FolderCreateRequest`` map (no ``set_nonce`` — the
    helper mints it). Returns the ``FolderCreateReply``.

    A set born through a raw ``fauna.folders.create`` has no custody entry, so
    the owner app's launch reconcile mints a new nonce over the nest's copy and
    every row signed under the old one stops verifying — so every set a signed
    seed records into must be born here.

    Custody is the account plane's, so the call hosts the account store for its
    own length, as an app does, and fails unless the nonce reads back off the
    nest afterwards. **Side effect on the account:** each call enrolls one more
    fleet device, and the account gains one self-registered ``sync_devices``
    row (the same id every call). A test that counts the account's devices, or
    runs it at its device cap, creates its sets through the app instead."""
    return _harness_call("fauna_harness_create_set", nest_url, secret, request)


def harness_record_change(nest_url: str, secret: bytes, request: dict) -> dict:
    """Send ``fauna.sync.changes.record`` **signed**, as the actor whose seed is
    ``secret``: the set nonce resolved from that actor's roster + custody, the
    request signed by the shared ``RecordSigning`` with the identity key.
    ``request`` is the unsigned ``SyncChangeRecordRequest`` map. Returns the
    ``SyncChangeRecordReply`` (``{seq}``). Raises locally, before sending, when
    the recorder's custody holds no nonce for the set."""
    return _harness_call("fauna_harness_record_change", nest_url, secret, request)


def harness_create_private_link(
    nest_url: str, secret: bytes, manifest: bytes, filename: str, lifetime_secs: int
) -> tuple[dict, str]:
    """Make a **private (fragment-keyed) share link** to a file, as the actor
    whose seed is ``secret``, through the shared
    ``ShareClient::create_private_link`` (``share-links.md`` § The private-file
    extension): the token minted, the key envelope sealed under a fresh link
    key, ``fauna.share.create`` sent, and the URL — ``<nest>/share/<token>#<key>``
    — revealed only against the nest's reply. ``manifest`` is the sealed wire
    manifest ``seal_owner_file`` returned (already uploaded). Returns
    ``(ShareRecord, url)``; the key exists only in that url."""
    record, url = _harness_call(
        "fauna_harness_create_private_link",
        nest_url,
        secret,
        [manifest, filename, lifetime_secs],
    )
    return record, url


def harness_report_conflict(nest_url: str, secret: bytes, request: dict) -> dict:
    """Send ``fauna.sync.conflicts.report`` as the actor whose seed is
    ``secret``, a pre-resolved report's winner head row **signed** by the shared
    ``RecordSigning::sign_report`` (an unresolved report goes out as is).
    ``request`` is the ``ConflictReportRequest`` map. Returns the
    ``ConflictReportReply``."""
    return _harness_call("fauna_harness_report_conflict", nest_url, secret, request)


_HAS_SHARE_VIEWER_OPEN = hasattr(_lib, "fauna_harness_share_viewer_open")
if _HAS_SHARE_VIEWER_OPEN:
    _lib.fauna_harness_share_viewer_open.argtypes = [
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # dag-cbor request, len
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_harness_share_viewer_open.restype = ctypes.c_int32


def _share_viewer_open(url: str, manifest_reply: bytes, chunks):
    if not _HAS_SHARE_VIEWER_OPEN:
        raise RuntimeError(
            "libfauna_ffi has no `fauna_harness_share_viewer_open` — it is stale, "
            "or not the `e2e-harness` flavor. Run `just e2e-ffi` to rebuild it."
        )
    body = cbor2.dumps([url, manifest_reply, chunks], canonical=True)
    req_arr = (ctypes.c_uint8 * len(body)).from_buffer_copy(body)
    out = _FfiBuffer()
    rc = _lib.fauna_harness_share_viewer_open(req_arr, len(body), ctypes.byref(out))
    filename, chunk_count, plaintext = cbor2.loads(
        _take_buffer(rc, out, "fauna_harness_share_viewer_open")
    )
    return filename, chunk_count, plaintext


def share_viewer_open(url: str, fetch) -> tuple[str, bytes]:
    """Open a **private share link as its viewer page does**, with no browser:
    the shared ``fauna_client_share::viewer`` open (token signature, the
    manifest the token names, the envelope under the fragment's key) and
    assemble (each chunk against its plaintext hash, the whole against the
    file hash) — the functions the viewer's wasm runs. ``fetch(path_url)``
    performs each GET a stranger's browser would make (``<link>/manifest``,
    then ``<link>/chunk/<i>``) and returns the body; it never sees the
    fragment. Returns ``(filename, plaintext)``; raises on a link that does
    not open."""
    base = url.split("#", 1)[0]
    manifest_reply = fetch(f"{base}/manifest")
    _, count, _ = _share_viewer_open(url, manifest_reply, None)
    chunks = [fetch(f"{base}/chunk/{i}") for i in range(count)]
    filename, _, plaintext = _share_viewer_open(url, manifest_reply, chunks)
    return filename, bytes(plaintext)


_HAS_NOSTR_GIFT_WRAP = hasattr(_lib, "fauna_harness_nostr_gift_wrap_dm")
if _HAS_NOSTR_GIFT_WRAP:
    _lib.fauna_harness_nostr_gift_wrap_dm.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),                    # sender_secret (32, secp256k1)
        ctypes.c_char_p,                                   # recipient (npub1… or 64 hex)
        ctypes.c_char_p,                                   # content
        ctypes.POINTER(_FfiBuffer),                        # out
    ]
    _lib.fauna_harness_nostr_gift_wrap_dm.restype = ctypes.c_int32


def nostr_gift_wrap_dm(sender_secret: bytes, recipient: str, content: str) -> dict:
    """Build a NIP-17 gift-wrapped DM as a far Nostr user would — ``content``
    from the holder of ``sender_secret`` (a secp256k1 secret) to ``recipient``
    (an ``npub1…`` or 64 hex) — through the shared ``nip17::wrap_dm``. Returns the
    kind-1059 event as the NIP-01 dict a relay ``EVENT`` frame carries."""
    if not _HAS_NOSTR_GIFT_WRAP:
        raise RuntimeError(
            "libfauna_ffi has no `fauna_harness_nostr_gift_wrap_dm` — it is stale, or "
            "not built with the `e2e-harness` feature (`just e2e-ffi`)"
        )
    if len(sender_secret) != 32:
        raise ValueError("sender_secret must be 32 bytes")
    out = _FfiBuffer()
    rc = _lib.fauna_harness_nostr_gift_wrap_dm(
        _u8(sender_secret), recipient.encode(), content.encode(), ctypes.byref(out)
    )
    return json.loads(_take_buffer(rc, out, "fauna_harness_nostr_gift_wrap_dm"))


_HAS_SYNC_PASS = hasattr(_lib, "fauna_harness_sync_pass")
if _HAS_SYNC_PASS:
    _lib.fauna_harness_sync_pass.argtypes = [
        ctypes.c_char_p,                                   # nest base URL
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # secret (32-byte Ed25519 seed), len
        ctypes.c_char_p,                                   # folder_id (a FolderRef wire string)
        ctypes.c_char_p,                                   # watch_dir
        ctypes.c_char_p,                                   # state_dir (the engine's state DBs)
        ctypes.POINTER(ctypes.c_uint8), ctypes.c_uint32,   # device_id (32 bytes), len
        ctypes.c_char_p,                                   # device_label
        ctypes.POINTER(_FfiBuffer),                        # out: {"ok": [recorded, pending]}
    ]
    _lib.fauna_harness_sync_pass.restype = ctypes.c_int32


def harness_sync_pass(
    nest_url: str,
    secret: bytes,
    folder_id: str,
    watch_dir,
    state_dir,
    device_id: bytes,
    device_label: str = "e2e harness writer",
) -> tuple[list[str], int]:
    """One **seat pass** over ``watch_dir`` as the actor whose seed is
    ``secret`` — the harness's SIGNED file writer where a test has no app.

    The apps' own in-process engine host builds the shared engine for the set
    ``folder_id`` (a ``FolderRef`` wire string, ``common.auth.user_folder_ref``),
    runs one local converge — new and changed files go up sealed and recorded, a
    file gone since the last pass over the same ``state_dir`` is recorded
    deleted — and drops it. Every record signs with the identity key, so the
    set must be born custody-first (``harness_create_set``, which
    ``user_create_folder`` routes through). Never pulls.

    Returns ``(recorded, pending)``: the rels whose record reached the nest in
    this pass, and how many files it left pending. Raises when the pass could
    not run at all (the engine refused to build, no connection)."""
    if not _HAS_SYNC_PASS:
        raise RuntimeError(
            "libfauna_ffi has no `fauna_harness_sync_pass` — it is stale, or not "
            "the `e2e-harness` flavor. Run `just e2e-ffi` to rebuild it."
        )
    if len(secret) != 32 or len(device_id) != 32:
        raise ValueError("secret and device_id must be 32 bytes each")
    sec_arr = (ctypes.c_uint8 * 32)(*secret)
    dev_arr = (ctypes.c_uint8 * 32)(*device_id)
    out = _FfiBuffer()
    rc = _lib.fauna_harness_sync_pass(
        nest_url.encode("utf-8"), sec_arr, 32,
        folder_id.encode("utf-8"),
        os.fspath(watch_dir).encode("utf-8"),
        os.fspath(state_dir).encode("utf-8"),
        dev_arr, 32,
        device_label.encode("utf-8"),
        ctypes.byref(out),
    )
    recorded, pending = cbor2.loads(_take_buffer(rc, out, "fauna_harness_sync_pass"))["ok"]
    return list(recorded), int(pending)
