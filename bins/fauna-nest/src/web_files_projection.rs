//! The `web_files` projection's two maintenance doors: the incremental route a
//! recorded change takes into a website folder's projection
//! ([`route_web_file_change`]), and the serving-transition backfill that
//! rebuilds one folder's projection from its live heads
//! ([`reconcile_web_files_projection`]) — `web-content-hosting.md` § Content
//! model.

use std::sync::Arc;

/// Route one file change for a `web`-mode folder into `web_files` and, when
/// the changed file is a render input, re-render the actor's site.
///
/// (web-content-hosting.md § Routing/render, Slice 1b.) Server-side-executable
/// extensions are rejected at sync time; a `delete` drops the row, any other
/// change upserts it (the stored hash is the blob the serve/render layers read
/// directly). A `.html.hbs` template or `_site.json` change additionally fires
/// the render pipeline via [`WebContentService::render_published_posts`] — the
/// same entry point the publish handlers use (Slice 1a), so templates and
/// web-published posts always re-render together. Static assets (css/js/images)
/// are served straight from `web_files` and need no render.
pub(crate) async fn route_web_file_change(
    db: &crate::db::CacheDb,
    web_content_service: Option<&Arc<crate::web_content::service::WebContentService>>,
    actor_id: &[u8; 32],
    path: &str,
    change_type: &str,
    manifest_hash: Option<[u8; 32]>,
    folder_id: i64,
    content_key_version: Option<i64>,
) {
    // Redacted: this branch `return`s BEFORE
    // `upsert_web_file`, so the rejected file never becomes a public URL — the
    // "Website-folder paths" carve-out (public URLs) does not cover it, and the
    // filename is user-chosen content an admin would otherwise never see. The
    // matched extension is a fixed public constant and stays literal so the
    // line still says why the file was refused.
    if let Some(ext) = crate::web_content::serve::rejected_extension(path) {
        tracing::warn!(
            path = %fauna_core::log_redact::log_path(path),
            extension = ext,
            "rejected web file with server-side extension"
        );
        return;
    }
    // A change at a render-input PATH — a template or `_site.json` — is a
    // revoking door: a delete drops a page, and an edit can withdraw content
    // from one, which the nest cannot tell from an edit that adds. So the row
    // write and the owed-render marker are one transaction, and the render
    // below is keyed on that marker and fails closed (`web-content-hosting.md`
    // § Routing, render, serving → *A revoke is durable*). Keyed on the path
    // alone, sealed or not: a sealed file is never a render INPUT (the render's
    // own listing refuses it — `is_render_input_row`; v1 bound,
    // monetization.md § Pillar 2), but a template re-recorded sealed has just
    // stopped being one, and the page it used to render is what the render
    // must now drop.
    let render_input = is_render_input(path);
    let wrote = if change_type == "delete" {
        if render_input {
            db.delete_web_render_input(actor_id, path).await.map(drop)
        } else {
            db.delete_web_file(actor_id, path).await.map(drop)
        }
    } else if let Some(mh) = manifest_hash {
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        if render_input {
            db.upsert_web_render_input(
                actor_id,
                path,
                &mh,
                &content_type,
                Some(folder_id),
                content_key_version,
            )
            .await
        } else {
            db.upsert_web_file(
                actor_id,
                path,
                &mh,
                &content_type,
                Some(folder_id),
                content_key_version,
            )
            .await
        }
    } else {
        // A non-delete change with no manifest hash carries no content — nothing
        // to ingest, and no render input changed.
        return;
    };
    if let Err(e) = wrote {
        tracing::warn!(error = %e, "web_files write after sync change failed");
    }

    // Re-render whatever the site is owed; a re-render reads the actor's
    // current templates + `_site.json` + web-published posts and rewrites
    // `web_rendered` wholesale (idempotent), so a template *delete* re-renders
    // an accordingly-smaller site. Static assets are served straight from
    // `web_files` and need no render.
    if render_input
        && let Some(svc) = web_content_service
        && let Err(e) = svc.render_owed(actor_id, "a template change").await
    {
        tracing::warn!(error = %e, "web render after sync change for {path}");
    }
}

/// (Re)build one folder's `web_files` projection from its live plaintext sync
/// heads — the serving-transition backfill (`web-content-hosting.md` § Content
/// model: `web_files` is a *projection* of the folder's live heads).
///
/// [`route_web_file_change`] maintains the projection incrementally while the
/// website toggle is on at arrival; this reconcile is what makes the toggle
/// mean the same thing for content synced BEFORE it was enabled: upsert every
/// live plaintext head (same server-side-extension refusal), then drop every
/// row of this folder whose path has no live head (a delete recorded while the
/// toggle was off never fired `delete_web_file`, and re-serving it at
/// re-enable is resurrection). Deliberately fires NO renders: the one
/// transition re-render runs after it and reads the reconciled rows. Cost is
/// O(folder heads) under the owner's own authenticated update — the
/// snapshot-create class.
///
/// ⚠ **The drop pass judges only the class the input can describe.** Sealed
/// heads rest no plaintext path (S9), so they are structurally absent from
/// `live_plaintext_heads_for_folder` — and a **sealed** `web_files` row's
/// absence from the live set is therefore *ignorance, not evidence*. Dropping on
/// it deleted the folder's entire sealed projection on any serving transition
/// landing enabled: a bind flipping the audience `private` → `shared` while the
/// site kept serving took a paywalled site dark, and the client's own
/// `SyncEngine::converge_corpus_to_website` would not re-drive it (its
/// `corpus_website` marker still reads `served` — nothing about the toggle or
/// the sealed-ness changed). So the pass skips sealed rows entirely; they are
/// maintained by the client re-records that are their only route in
/// (`web-content-hosting.md` § Content model). Consequence, stated honestly: a
/// sealed path deleted while the toggle was off keeps a stale row, because
/// neither side can see that head — the additive client pass will not remove it
/// either. That is the price of the nest holding no names.
pub(crate) async fn reconcile_web_files_projection(
    db: &crate::db::CacheDb,
    actor_id: &[u8; 32],
    folder_id: i64,
) -> anyhow::Result<()> {
    let heads = db.live_plaintext_heads_for_folder(folder_id).await?;
    let mut live: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (path, manifest, ckv) in &heads {
        // A refused extension is deliberately NOT marked live: an existing row
        // for one (a historic ingest bug) is healed by the delete pass below.
        if crate::web_content::serve::rejected_extension(path).is_some() {
            continue;
        }
        live.insert(path.as_str());
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        db.upsert_web_file(
            actor_id,
            path,
            manifest,
            &content_type,
            Some(folder_id),
            *ckv,
        )
        .await?;
    }
    for row in db.list_web_files(actor_id).await? {
        // `is_sealed()` ⇒ this row's head carries no plaintext path, so the
        // live set above could never have named it. See the ⚠ note above.
        if row.folder_id == Some(folder_id) && !row.is_sealed() && !live.contains(row.path.as_str())
        {
            db.delete_web_file(actor_id, &row.path).await?;
        }
    }
    Ok(())
}

/// Whether a synced web file feeds the render pipeline: a Handlebars template or
/// the `_site.json` site-metadata file. Static assets do not.
///
/// The predicate itself lives in [`crate::web_content::render_input`], because
/// the render's own listing has to agree with it exactly — this door's job is
/// to ask, not to spell it out a second time.
fn is_render_input(path: &str) -> bool {
    crate::web_content::render_input::is_render_input(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::{BlobStoreBackend, DiskBlobStore};
    use crate::db::CacheDb;
    use crate::web_content::service::WebContentService;

    #[test]
    fn is_render_input_matches_templates_and_site_json() {
        assert!(is_render_input("index.html.hbs"));
        assert!(is_render_input("blog/post.html.hbs"));
        assert!(is_render_input("_site.json"));
        assert!(is_render_input("nested/_site.json"));
        assert!(!is_render_input("style.css"));
        assert!(!is_render_input("index.html"));
        assert!(!is_render_input("img/logo.png"));
        // The render's listing folds ASCII case, so the door must too — else a
        // mixed-case template renders a page whose delete revokes nothing
        // (`web_content::render_input`, which owns the predicate).
        assert!(is_render_input("about.HTML.hbs"));
        assert!(is_render_input("about.html.HBS"));
    }

    /// Seed a file the way a real sync does — chunked + at-rest-framed — and
    /// return its **manifest** hash, which is what a `FileChanged` carries and
    /// what `web_files.blob_hash` stores. (Seeding a raw blob here would let the
    /// render pipeline appear to work while reading framed CBOR.)
    async fn seed_blob(store: &Arc<dyn BlobStoreBackend>, content: &[u8]) -> [u8; 32] {
        crate::web_content::file_bytes::seed_synced_file(store, None, content, None).await
    }

    #[tokio::test]
    async fn web_file_change_ingests_template_and_fires_render() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let svc = Arc::new(WebContentService::new(db.clone(), store.clone()));
        let actor = [7u8; 32];

        // A real live website folder — the render pipeline gates every input
        // row on its folder's current serving state, so a row keyed
        // to a dangling folder_id is a template the renderer rightly refuses.
        let folder_id = db
            .create_folder_with_options("site", &actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // A `.html.hbs` template synced into a web folder is ingested AND
        // fires the render pipeline → `web_rendered` gets the stripped output.
        let template = b"<h1>{{site.title}}</h1>";
        let t_hash = seed_blob(&store, template).await;
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "index.html.hbs",
            "create",
            Some(t_hash),
            folder_id,
            None,
        )
        .await;

        assert!(
            db.get_web_file(&actor, "index.html.hbs")
                .await
                .unwrap()
                .is_some(),
            "template must land in web_files"
        );
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_some(),
            "a .html.hbs sync change must trigger render → web_rendered"
        );

        // A server-side-executable extension is rejected (never ingested).
        let php_hash = seed_blob(&store, b"<?php echo 'x'; ?>").await;
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "evil.php",
            "create",
            Some(php_hash),
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_file(&actor, "evil.php").await.unwrap().is_none(),
            "executable extension must be rejected at sync time"
        );

        // A static asset is ingested but does not need a render.
        let css_hash = seed_blob(&store, b"body{}").await;
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "style.css",
            "create",
            Some(css_hash),
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_file(&actor, "style.css")
                .await
                .unwrap()
                .is_some(),
            "static asset must be ingested into web_files"
        );

        // Deleting the only template re-renders an emptier site: the stripped
        // output is dropped (render_for_actor replaces the whole site).
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "index.html.hbs",
            "delete",
            None,
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_file(&actor, "index.html.hbs")
                .await
                .unwrap()
                .is_none(),
            "delete must drop the web_files row"
        );
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "deleting the template must re-render → stale index.html cleared"
        );
    }

    /// A render-input sync change is a revoking door
    /// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
    /// durable*): the row write and the owed-render marker are one
    /// transaction, and a render that errors before its clear takes the site
    /// dark rather than keep serving the deleted template's page.
    #[tokio::test]
    async fn a_template_delete_is_owed_durably_and_fails_closed() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let svc = Arc::new(WebContentService::new(db.clone(), store.clone()));
        let actor = [8u8; 32];
        let folder_id = db
            .create_folder_with_options("site", &actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t_hash = seed_blob(&store, b"<h1>{{site.title}}</h1>").await;
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "index.html.hbs",
            "create",
            Some(t_hash),
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.list_web_render_owed().await.unwrap().is_empty(),
            "the render that answered the create discharged its marker"
        );

        // The torn half: the door with no render service is the row write
        // alone — what a nest that stopped before rendering leaves behind.
        route_web_file_change(
            &db,
            None,
            &actor,
            "index.html.hbs",
            "delete",
            None,
            folder_id,
            None,
        )
        .await;
        assert_eq!(
            db.list_web_render_owed().await.unwrap(),
            vec![actor],
            "the template delete's own transaction records the owed render"
        );
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_some(),
            "the torn state: the template is gone and its page still serves"
        );

        // A static asset owes nothing.
        let css_hash = seed_blob(&store, b"body{}").await;
        db.discharge_web_render_owed(
            &actor,
            db.web_render_owed_nonce(&actor).await.unwrap().unwrap(),
        )
        .await
        .unwrap();
        route_web_file_change(
            &db,
            None,
            &actor,
            "style.css",
            "create",
            Some(css_hash),
            folder_id,
            None,
        )
        .await;
        assert!(db.list_web_render_owed().await.unwrap().is_empty());

        // The error arm: re-create + delete with every render failing before
        // its clear. The page must go dark, and the clear discharges.
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "index.html.hbs",
            "create",
            Some(t_hash),
            folder_id,
            None,
        )
        .await;
        db.conn()
            .await
            .execute_batch("ALTER TABLE nest_region RENAME TO nest_region_broken")
            .unwrap();
        assert!(svc.render_published_posts(&actor).await.is_err());
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "index.html.hbs",
            "delete",
            None,
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "a template delete whose render errors must clear the site"
        );
        assert!(db.list_web_render_owed().await.unwrap().is_empty());
    }

    /// **A mixed-case template is a render input on BOTH sides**
    /// (`web-content-hosting.md` § Routing, render, serving → *A revoke is
    /// durable*). The render's
    /// listing asks SQLite for `path LIKE '%.html.hbs'`, which folds ASCII
    /// case, so `about.HTML.hbs` renders a page like any other template. The
    /// door's revoke test used to be a case-sensitive `ends_with`, so the same
    /// file's delete was routed as a static asset: no owed-render mark, no
    /// re-render, and `about.HTML` kept serving after its author withdrew it.
    /// Both sides now ask `web_content::render_input::is_render_input`.
    #[tokio::test]
    async fn a_mixed_case_template_renders_and_its_delete_is_owed_like_any_other() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let svc = Arc::new(WebContentService::new(db.clone(), store.clone()));
        let actor = [9u8; 32];
        let folder_id = db
            .create_folder_with_options("site", &actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let t_hash = seed_blob(&store, b"<h1>{{site.title}}</h1>").await;
        route_web_file_change(
            &db,
            Some(&svc),
            &actor,
            "about.HTML.hbs",
            "create",
            Some(t_hash),
            folder_id,
            None,
        )
        .await;
        assert!(
            db.get_web_rendered(&actor, "about.HTML")
                .await
                .unwrap()
                .is_some(),
            "precondition: the listing folds case, so this template renders a page"
        );
        assert!(
            db.list_web_render_owed().await.unwrap().is_empty(),
            "the render that answered the create discharged its marker"
        );

        // The torn half, as the lowercase pin does it: the door with no render
        // service is the row write alone — what a nest that stopped before
        // rendering leaves behind. The MARK is what must survive it.
        route_web_file_change(
            &db,
            None,
            &actor,
            "about.HTML.hbs",
            "delete",
            None,
            folder_id,
            None,
        )
        .await;
        assert_eq!(
            db.list_web_render_owed().await.unwrap(),
            vec![actor],
            "a mixed-case template's delete owes the render its lowercase twin owes — \
             without it the withdrawn page serves until some unrelated render runs"
        );

        // And the owed render drops the page, through the same door a client
        // retry would use.
        assert!(
            svc.render_owed(&actor, "a template change").await.unwrap(),
            "the door renders what is owed"
        );
        assert!(
            db.get_web_rendered(&actor, "about.HTML")
                .await
                .unwrap()
                .is_none(),
            "the withdrawn template's page is gone"
        );
    }

    /// The SQL listing is a coarse pre-filter and the Rust predicate decides
    /// (`web_content::render_input`), so the listing must never be NARROWER
    /// than the predicate — a template the predicate admits but the listing
    /// drops would never render at all, and one the listing admits but the
    /// predicate rejects is simply filtered out by `render_for_actor`.
    #[tokio::test]
    async fn the_render_listing_is_a_superset_of_the_render_input_predicate() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        let actor = [10u8; 32];
        let folder_id = db
            .create_folder_with_options("site", &actor, Default::default())
            .await
            .unwrap();
        let hash = seed_blob(&store, b"x").await;
        let paths = [
            "index.html.hbs",
            "about.HTML.hbs",
            "about.html.HBS",
            "blog/Post.Html.Hbs",
            "style.css",
            "index.html",
            "notes.html.hbsx",
        ];
        for path in paths {
            db.upsert_web_file(&actor, path, &hash, "text/plain", Some(folder_id), None)
                .await
                .unwrap();
        }
        let listed: std::collections::HashSet<String> = db
            .list_web_files_by_ext(&actor, ".html.hbs")
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.path)
            .collect();
        for path in paths {
            if crate::web_content::render_input::is_template(path) {
                assert!(
                    listed.contains(path),
                    "{path} is a template by the predicate, so the listing must offer it"
                );
            }
        }
    }
}
