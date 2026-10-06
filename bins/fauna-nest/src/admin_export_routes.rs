//! GET /api/v1/admin/export/all — admin bulk export as tar.zst.
//!
//! Streams a tar.zst archive containing:
//! - tables/{table}.ndjson  — logical dump of every known table
//! - blobs/{blake3_hex}     — every blob from the local blob store

use std::io::Write;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fauna_core::data::ContentHash;

use crate::auth::AdminBearerAuth;
use crate::routes::AppState;

/// GET /api/v1/admin/export/all
///
/// Admin-only. Validates the Bearer token and checks admin status.
/// Streams a tar.zst archive of the full nest data (logical dump + all blobs).
///
/// **Carries legally withheld blobs, deliberately** (`moderation.md` § Legal
/// takedown → *The blob-serve door* → *What the withhold binds on owner- and
/// admin-scoped routes*, path 2). A takedown removes a record's availability
/// *through this nest*, never the box's custody of the bytes — tombstone-not-
/// delete keeps them on disk so `restore=true` can re-serve them — and this
/// archive is that custody moving with the box: its reader is the operator
/// the order was served on, who already holds every byte under their own
/// root, and `content_meta` (the flag) and `audit_log` (the order) ride in
/// `tables/` beside `blobs/`. An archive that dropped the withheld bytes would
/// turn the tombstone into a deletion at the next restore, so an overturn
/// could re-serve nothing.
pub async fn handle_admin_export_all(
    State(state): State<Arc<AppState>>,
    _auth: AdminBearerAuth,
) -> Response {
    // Fetch blob hashes async before handing off to spawn_blocking.
    let blob_hashes: Option<Vec<ContentHash>> = if let Some(svc) = &state.backup_service {
        let store = svc.local_blob_store();
        match store.list_all_hashes().await {
            Ok(hashes) => Some(hashes),
            Err(e) => {
                tracing::warn!("admin export: list_all_hashes failed ({e:#}), skipping blobs");
                None
            }
        }
    } else {
        None
    };

    // Pre-fetch blob data async so the blocking writer doesn't need to call await.
    let blob_data: Vec<(ContentHash, Vec<u8>)> = if let Some(hashes) = blob_hashes {
        if let Some(svc) = &state.backup_service {
            let store = svc.local_blob_store();
            let mut out = Vec::with_capacity(hashes.len());
            for hash in hashes {
                match store.get(&hash).await {
                    Ok(Some(data)) => out.push((hash, data)),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(
                        "admin export: get blob {} failed: {e:#}",
                        hex::encode(hash.digest())
                    ),
                }
            }
            out
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let (tx, body) = crate::streaming::streaming_body(64);
    let db = state.db.clone();

    tokio::task::spawn_blocking(move || {
        let writer = crate::streaming::ChannelWriter::new(tx);
        if let Err(e) = write_admin_export(&db, blob_data, writer) {
            tracing::error!("admin export failed: {e:#}");
        }
    });

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(header::CONTENT_TYPE, "application/zstd".parse().unwrap());
    resp_headers.insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"fauna-nest-export.tar.zst\""
            .parse()
            .unwrap(),
    );
    (StatusCode::OK, resp_headers, body).into_response()
}

/// Write the full tar.zst export to `writer`.
///
/// - `blob_data`: pre-fetched blob contents (collected async before calling this).
/// - `writer`: any `std::io::Write` — tar is sequential so no Seek is needed.
pub fn write_admin_export(
    db: &crate::db::CacheDb,
    blob_data: Vec<(ContentHash, Vec<u8>)>,
    writer: impl Write,
) -> anyhow::Result<()> {
    let encoder = zstd::stream::Encoder::new(writer, 3)?;
    let mut tar = tar::Builder::new(encoder);

    // 1. Write logical dump tables as NDJSON.
    //
    // Shares `append_dump_tables` with the daily logical dump rather than
    // carrying its own copy of the loop: `DUMP_TABLES`'s narrowness is a
    // ratified confidentiality boundary (see its doc comment), and one
    // enforcement point is what keeps the two consumers from drifting apart.
    let conn = db.conn_blocking();
    crate::export::logical::append_dump_tables(&conn, &mut tar, "tables/")?;
    drop(conn);

    // 2. Write pre-fetched blobs.
    for (hash, data) in &blob_data {
        let filename = format!("blobs/{}", hex::encode(hash.digest()));
        crate::export::archive::append_entry(&mut tar, &filename, data)?;
    }

    crate::export::archive::finish(tar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    #[test]
    fn admin_export_produces_tar_zst() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let mut output = Cursor::new(Vec::new());
        write_admin_export(&db, Vec::new(), &mut output).unwrap();

        let output = output.into_inner();
        assert!(!output.is_empty());

        let decoder = zstd::stream::Decoder::new(output.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let entries: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(
            entries.iter().any(|e| e.ends_with(".ndjson")),
            "expected at least one .ndjson entry, got: {entries:?}"
        );

        // The `tables/` prefix is this export's only difference from the daily
        // logical dump, and since 2026-08-15 both go through one shared
        // `append_dump_tables(.., prefix)`. Asserting only "some .ndjson entry"
        // (as this test did before) would stay green if the prefix were dropped
        // or misspelled, so pin it: every table entry lives under `tables/`.
        let table_entries: Vec<&String> =
            entries.iter().filter(|e| e.ends_with(".ndjson")).collect();
        assert!(
            table_entries.iter().all(|e| e.starts_with("tables/")),
            "every dumped table must land under `tables/`, got: {table_entries:?}"
        );
    }

    #[test]
    fn admin_export_includes_blobs() {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let hash = ContentHash::of_raw(b"test blob");
        let blob_data = vec![(hash, b"test blob".to_vec())];

        let mut output = Cursor::new(Vec::new());
        write_admin_export(&db, blob_data, &mut output).unwrap();

        let output = output.into_inner();
        let decoder = zstd::stream::Decoder::new(output.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let entries: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        let blob_hex = hex::encode(hash.digest());
        let expected_path = format!("blobs/{blob_hex}");
        assert!(
            entries.iter().any(|e| e == &expected_path),
            "expected blob entry {expected_path} in tar, got: {entries:?}"
        );
    }
}
