"""Shared test infrastructure for Fauna E2E tests.

This package contains reusable functions imported by tests/e2e-unified/ conftest
files and test modules via `from common import ...`.
"""

from common.nest import (
    CLAIM_CODE,
    build_node,
    build_sync_service_win,
    build_app,
    build_macos_app,
    nest_id_from_data_dir,
    start_nest,
    stop_nest,
    wait_for_node,
    get_repo_root,
    _cargo_cmd,
)

from common.auth import (
    make_keypair,
    mark_tls_nest,
    port_base_url,
    ws_sslopt,
    claim_admin,
    register_user,
    create_folder,
    add_folder_member,
    PLACE_SYNC,
    PLACE_BACKUP,
    PLACE_ORIGINATES_ONLY,
    user_create_folder,
    set_folder_residency,
    user_folders_list,
    sync_register,
    sealed_path,
    sync_changes_list,
    sync_devices_list,
    media_list,
    sync_status,
    sync_conflicts_list,
    create_folder_snapshot,
    create_actor_and_register,
    admin_session,
)

from common.accounts import (
    actor_id_hex,
    build_registry_seed,
    single_account_seed,
)

from common.helpers import (
    remote_request,
    sign_as_nest,
)

from common.envelope import (
    FEDERATION_HELLO_V1,
    canonical_dagcbor_bytes,
    cid_of_dag_cbor,
    sign_over_cid,
    sign_dagcbor_envelope,
    verify_over_cid,
    verify_dagcbor_envelope,
    encode_post_dagcbor,
    sign_post_envelope,
    forwarded_post_id,
    wrap_embed_as_bytes,
    encode_structured_post_dagcbor,
    encode_contact_request_dagcbor,
    build_email_inbox_payload,
)

__all__ = [
    # nest.py
    "CLAIM_CODE",
    "build_node",
    "build_sync_service_win",
    "build_app",
    "build_macos_app",
    "nest_id_from_data_dir",
    "start_nest",
    "stop_nest",
    "wait_for_node",
    "get_repo_root",
    "_cargo_cmd",
    # auth.py
    "make_keypair",
    "mark_tls_nest",
    "port_base_url",
    "ws_sslopt",
    "claim_admin",
    "register_user",
    "create_folder",
    "add_folder_member",
    "PLACE_SYNC",
    "PLACE_BACKUP",
    "PLACE_ORIGINATES_ONLY",
    "user_create_folder",
    "set_folder_residency",
    "user_folders_list",
    "sync_register",
    "sealed_path",
    "sync_changes_list",
    "sync_devices_list",
    "media_list",
    "sync_status",
    "sync_conflicts_list",
    "create_folder_snapshot",
    "create_actor_and_register",
    "admin_session",
    # accounts.py
    "actor_id_hex",
    "build_registry_seed",
    "single_account_seed",
    # helpers.py
    "remote_request",
    "sign_as_nest",
    # envelope.py
    "FEDERATION_HELLO_V1",
    "canonical_dagcbor_bytes",
    "cid_of_dag_cbor",
    "sign_over_cid",
    "sign_dagcbor_envelope",
    "verify_over_cid",
    "verify_dagcbor_envelope",
    "encode_post_dagcbor",
    "sign_post_envelope",
    "forwarded_post_id",
    "wrap_embed_as_bytes",
    "encode_structured_post_dagcbor",
    "encode_contact_request_dagcbor",
    "build_email_inbox_payload",
]
