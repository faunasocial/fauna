//! Wire request/error types for the `FolderNestApi` trait.
//!
//! Shapes mirror `bins/fauna-nest/src/user_folder_routes.rs`
//! (`CreateFolderRequest`, `AddMemberRequest`) so the reqwest impl can copy
//! the bytes verbatim, and the web app's `createFolderFull` /
//! `addFolderMember` (`apps/fauna-web/src/lib/api.ts`).

use serde::Serialize;

use crate::state::RetentionPolicy;

/// One folder row as `fauna.folders.list` reports it — the two fields the
/// headless photo-library resolver keys on: the nest's `folders.id` primary key
/// (this nest's `FolderRef::Local` identity, the binding's only key) and the
/// name (a label two sets can share).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRow {
    pub id: i64,
    pub name: String,
}

/// Body of `POST /api/v1/file-sets`. Optional fields are omitted when absent
/// (a plain wizard create sends no `retention_policy` — only a preset does).
/// There is no path list: a set's include/exclude paths seal under the row id
/// the create mints, so they arrive on the first keyed update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateFolderRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<RetentionPolicy>,
    /// Create-time conflict policy (`"auto"` | `"latest_wins_always"`) — the
    /// user's global default (the `fauna.state.sync-prefs` default conflict policy),
    /// injected by the client glue via
    /// `FolderWizardMachine::set_default_conflict_policy`. `None` = the nest
    /// column default (`auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_policy: Option<String>,
}

/// One device place to set — the payload of `fauna.folders.places.set`.
///
/// **Flags, not a role noun** (folders re-model § Places): every one of the
/// eight points the wizard's three checkboxes span is expressible, and the
/// machine cannot round one point to a neighbouring one on the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetPlaceRequest {
    /// Hex-encoded device id.
    pub device_id: String,
    pub flags: fauna_protocol::folders::PlaceFlags,
}

fauna_core::declare_api_error!(
    /// Failure of a folder nest call. `detail` carries the nest's response
    /// body (e.g. `"folder with that name already exists"`).
    FolderApiError {
        /// 409 — name taken.
        Conflict,
        /// 400 — invalid request (e.g. device-id hex).
        BadRequest,
        /// 404 — folder or device not found / not owned.
        NotFound,
        /// 5xx / network — retryable.
        Transient,
    }
);

impl FolderApiError {
    /// Map an HTTP status + body to the right variant.
    pub fn from_status(code: u16, body: String) -> Self {
        match code {
            409 => FolderApiError::Conflict { detail: body },
            400 => FolderApiError::BadRequest { detail: body },
            404 => FolderApiError::NotFound { detail: body },
            _ => FolderApiError::Transient {
                detail: format!("status {code}: {body}"),
            },
        }
    }
}
