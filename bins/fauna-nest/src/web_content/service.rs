//! WebContentService — orchestrates the render pipeline and blob store.
//!
//! Reads `.html.hbs` template files from the blob store, renders them using
//! the functions from `render.rs`, and stores the output in `web_rendered`.

use std::sync::Arc;

use crate::acme_http01::{RetryState, next_attempt_delay, now_unix};
use anyhow::Result;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::subscription::crypto::{
    decrypt_content, derive_post_key, derive_web_render_key, encrypt_content,
};
use zeroize::Zeroizing;

use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;
use crate::db::web::{RenderClaim, RenderedPage, RenderedSealedPage};

use super::holder::WebServeHolder;
use super::render::{
    self, PaywallContext, PostContext, SiteContext, build_site_context, generate_rss,
    render_single_template,
};

/// Max published posts fed into a single site render. Bounds the render context
/// and the top-level `{{#each posts}}` iteration count (the "≤1000
/// iterations" figure in `web-content-hosting.md` § Routing/render's Safety
/// limits). The client-facing publish *listing* stays uncapped — only the render
/// path reads through [`CacheDb::list_web_published_servable_capped`](crate::db::CacheDb::list_web_published_servable_capped).
pub const MAX_RENDERED_POSTS: u32 = 1000;

/// Max `.html.hbs` templates rendered for one actor in a pass. A normal site has
/// a handful; this bounds the N-templates × M-posts output amplification.
const MAX_TEMPLATES: usize = 100;

// Wall-clock budget for a single template render — the one constant, owned by
// the render layer, which stops the render inside its thread at this bound.
use super::render::RENDER_TIMEOUT;

/// How long a render answering an **owed revoke** may keep the old site — the
/// withdrawn content included — serving before it must clear it
/// (`web-content-hosting.md` § Routing, render, serving → *A reader sees a whole
/// site or none*). A render replaces the site only in its last statement, and
/// how long it runs is the author's to stretch (the safety limits above), so
/// without this bound an author's slow templates could hold a moderator's legal
/// takedown on the public internet for hours. Measured from the render's claim.
/// A Rust constant: no user or admin would ever want to choose it.
const WITHDRAWAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Run a synchronous template render on a blocking thread under a wall-clock
/// timeout. The CPU-bound Handlebars render is moved off the async workers (so it
/// can't degrade the runtime for other clients) and bounded in time. The timeout
/// here only bounds the *await*: a `spawn_blocking` task cannot be cancelled, so
/// the thread itself is stopped one layer down, where
/// [`render::render_data`](super::render::render_data) checks the same
/// [`RENDER_TIMEOUT`] inside the render, alongside the output-byte and
/// template-size caps and the nesting bounds — a render also runs on a thread of
/// its own with a fixed stack, since a stack overflow on this blocking thread
/// would abort the whole nest. Together with the post/template caps this is the
/// render-DoS closure.
async fn render_bounded<F>(render: F) -> Result<String>
where
    F: FnOnce() -> Result<String> + Send + 'static,
{
    match tokio::time::timeout(RENDER_TIMEOUT, tokio::task::spawn_blocking(render)).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(anyhow::anyhow!("template render task failed: {join_err}")),
        Err(_elapsed) => Err(anyhow::anyhow!(
            "template render exceeded {RENDER_TIMEOUT:?} budget"
        )),
    }
}

/// One paywalled post whose FULL rendered page goes into the sealed store
/// (`web_rendered_sealed` — web paywall, Pillar 2): the full-content template
/// context plus the seal key and the serve-time key-derivation inputs.
pub struct SealedPageRender {
    /// The full-content variant of the post's context (title/content derived
    /// from the decrypted body). Must only ever be rendered into the sealed
    /// store — never into `web_rendered` or a list context.
    pub post: PostContext,
    pub tier: String,
    pub post_id: [u8; 32],
    /// `derive_web_render_key(tier period_key, post_id)` — wiped on drop.
    pub seal_key: Zeroizing<[u8; 32]>,
}

/// What a render did to the actor's rendered stores
/// (`web-content-hosting.md` § Routing, render, serving → *One render writes at
/// a time*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum RenderOutcome {
    /// This render's listing was still the newest, so the site is its output.
    Wrote,
    /// A newer render's listing — or a fail-closed clear — took the site over
    /// while this one was running, so it abandoned and wrote nothing. Not a
    /// failure: whoever superseded it has the fresher listing, and every debt
    /// this render read stays with the render that actually settles it.
    Superseded,
}

/// What [`WebContentService::drain_owed_renders`] hands back to the boot
/// wiring: how much of the drain failed, and the restore-retry task the drain
/// started, for the caller to adopt into its serving generation.
#[must_use]
pub struct BootDrain {
    /// Sites that failed to render during the drain (each cleared instead, so
    /// none of them serves stale pages). `0` on a box that stopped cleanly.
    pub failed: usize,
    /// The restore-retry loop, spawned once per service — `None` when it was
    /// already running. **Adopt it** with `AppState::scope_handle`: the loop
    /// outlives every request, and a deployment-seed rotation tears the
    /// serving generation down and re-enters `start_server` without dropping
    /// the process, so a detached loop would keep rendering under superseded
    /// key material (`box-recovery.md` § Deployment-seed rotation → *Adoption
    /// by the running process*).
    pub restore_retry: Option<tokio::task::JoinHandle<()>>,
}

/// Orchestrates the Handlebars render pipeline for a single actor's web site.
pub struct WebContentService {
    db: Arc<CacheDb>,
    blob_store: Arc<dyn BlobStoreBackend>,
    /// Post-body source for `render_published_posts` after the segment-store
    /// cutover — `None` in unit tests (which seed inline `content.payload`
    /// posts, served via the inline read path), set in production via
    /// [`WebContentService::with_post_body_source`].
    post_segments: Option<Arc<fauna_segment_store::SegmentManager>>,
    /// The web-serve capability holder (web paywall, Pillar 2). `None` = no
    /// holder (paywalled posts render teaser-only, never the sealed full page).
    holder: Option<Arc<WebServeHolder>>,
    /// The deployment's at-rest blob key, needed to strip `backup::encode_blob`
    /// framing off **synced** blobs (manifests + chunks written by the
    /// chunk-upload route). `None` = no at-rest encryption configured, or a unit
    /// test that seeds blobs directly. Rendered output is stored verbatim and
    /// does not pass through this.
    at_rest_key: Option<BackupKey>,
    /// The region registry the nest-as-publisher fold verifies the situs
    /// chain's content policies against. `None` = the region tier's own
    /// compiled-in registry (production); a test sets a fixture enrolling a
    /// synthetic authority — the same seam `region_tier::ingest_relay_artifact`
    /// takes, so the render and the relay verify against one registry.
    region_registry: Option<fauna_core::region_authority::RegionRegistry>,
    /// Bundled-scorer answers carried between renders of one site, so a render
    /// scores only the posts and scorers it has not seen.
    region_scores: super::region::ScoreCache,
    /// Bumped once per [`Self::render_published_posts`] call, any actor.
    /// Test-observability only — a pin needs to assert a batch (e.g. an
    /// account-deletion retraction pass, `pending_actions::retract_actor_posts`)
    /// rendered a site exactly once rather than once per post, and the final
    /// `web_rendered` rows alone can't show that: `render_for_actor` replaces
    /// them whole every call, so two renders and one look identical at
    /// rest (convention 14, `e2e-latency-independent-assertions.md` — no
    /// wall-clock proxy either).
    render_invocations: std::sync::atomic::AtomicU64,
    /// Wakes the restore retry when a fail-closed clear has just left a site
    /// owed its restore ([`Self::rerender_fail_closed`]).
    restore_wake: Arc<tokio::sync::Notify>,
    /// Set once the restore retry runs, so a second boot drain on one instance
    /// (tests drain more than once) starts no second task.
    restore_retry_started: std::sync::atomic::AtomicBool,
    /// [`WITHDRAWAL_DEADLINE`] in production. A field only so a pin can state
    /// which side of the deadline it is on instead of racing a clock
    /// ([`Self::with_withdrawal_deadline`]).
    withdrawal_deadline: std::time::Duration,
}

impl Drop for WebContentService {
    /// Wake the restore retry so it sees the service gone and ends.
    fn drop(&mut self) {
        self.restore_wake.notify_one();
    }
}

impl WebContentService {
    /// Create a new `WebContentService`.
    pub fn new(db: Arc<CacheDb>, blob_store: Arc<dyn BlobStoreBackend>) -> Self {
        Self {
            db,
            blob_store,
            post_segments: None,
            holder: None,
            at_rest_key: None,
            region_registry: None,
            region_scores: Default::default(),
            render_invocations: std::sync::atomic::AtomicU64::new(0),
            restore_wake: Arc::new(tokio::sync::Notify::new()),
            restore_retry_started: std::sync::atomic::AtomicBool::new(false),
            withdrawal_deadline: WITHDRAWAL_DEADLINE,
        }
    }

    /// Render under `deadline` instead of [`WITHDRAWAL_DEADLINE`] — the test
    /// seam (convention 14): zero puts a pin past the deadline before the
    /// render's first page, a day puts it inside, and neither races a clock.
    pub fn with_withdrawal_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.withdrawal_deadline = deadline;
        self
    }

    /// Test-observability: how many times [`Self::render_published_posts`]
    /// has run on this instance, any actor. See the field's doc comment.
    pub fn render_call_count(&self) -> u64 {
        self.render_invocations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Verify the nest-as-publisher fold against `registry` instead of the
    /// compiled-in one — the test seam (see the field's note).
    pub fn with_region_registry(
        mut self,
        registry: fauna_core::region_authority::RegionRegistry,
    ) -> Self {
        self.region_registry = Some(registry);
        self
    }

    /// Wire the at-rest blob key so the synced-file read path can strip the
    /// nest's `encode_blob` framing (production construction site, `lib.rs`).
    pub fn with_at_rest_key(mut self, key: Option<BackupKey>) -> Self {
        self.at_rest_key = key;
        self
    }

    /// The at-rest blob key (see [`Self::with_at_rest_key`]).
    pub fn at_rest_key(&self) -> Option<&BackupKey> {
        self.at_rest_key.as_ref()
    }

    /// Wire the web-serve capability holder so paywalled posts render their
    /// sealed full pages under a live creator grant (production construction
    /// site + holder-wired tests).
    pub fn with_web_serve_holder(mut self, holder: Arc<WebServeHolder>) -> Self {
        self.holder = Some(holder);
        self
    }

    /// Wire the post-body source (the `__post` segment store) so
    /// `render_published_posts` reads bodies segment-first with the inline
    /// `content.payload` fallback. Called at
    /// the single production construction site (`lib.rs`).
    pub fn with_post_body_source(
        mut self,
        post_segments: Arc<fauna_segment_store::SegmentManager>,
    ) -> Self {
        self.post_segments = Some(post_segments);
        self
    }

    /// Return a reference to the blob store backend.
    pub fn blob_store(&self) -> &Arc<dyn BlobStoreBackend> {
        &self.blob_store
    }

    /// Render all `.html.hbs` templates for the given actor and post list,
    /// storing the results in `web_rendered` (and, for paywalled posts with a
    /// live grant, the sealed full pages in `web_rendered_sealed`).
    ///
    /// `posts` carries the PUBLIC context for every post — for a gated post
    /// that is the preview plus its `paywall` box, which is what index/RSS/tag
    /// templates and the teaser page see. `sealed` carries the full-content
    /// contexts (computed by [`Self::render_published_posts`] under the
    /// holder's grant) that render into the sealed store only.
    ///
    /// **The one write goes through `claim`** (§ *One render writes at a time*):
    /// if a newer render's listing — or a fail-closed clear — takes the site
    /// over while this one is running, the first page to notice abandons the
    /// whole render and answers [`RenderOutcome::Superseded`], and the
    /// replacement refuses it regardless. An abandoned render has touched no
    /// row, and the caller settles no debt on its behalf.
    ///
    /// **It writes once, at its end** (§ *A reader sees a whole site or none*):
    /// rows accumulate while bodies stream to the blob store, and step 8
    /// replaces both stores in one transaction. Whether the old site is
    /// withdrawn *sooner* is not this function's business — the withdrawal
    /// deadline races it from [`Self::render_published_posts`].
    ///
    /// Steps:
    /// 1. Open the two row accumulators (public + sealed) — nothing is cleared.
    /// 2. Load `_site.json` → `SiteContext` (fallback to empty).
    /// 3. Find all `.html.hbs` files. Skip `_post.html.hbs` / `_paywall.html.hbs` in the main loop.
    /// 4. Render each template with the full post list; output path = input with `.hbs` stripped.
    /// 5. Per-post pages at `post/{slug}.html`: ungated posts via `_post.html.hbs`
    ///    (or the built-in default); gated posts get the paywall/teaser page via
    ///    `_paywall.html.hbs` (or the built-in default) — the canonical URL always
    ///    serves the teaser without a token.
    /// 6. Each `sealed` entry renders the FULL post page (same `_post`/default
    ///    template), is sealed under its `seal_key`, and lands in `web_rendered_sealed`
    ///    at the same `post/{slug}.html` path.
    /// 7. If posts exist, generate RSS feed and store at `feed.xml`.
    /// 8. Replace both rendered stores with the accumulated rows, in one
    ///    transaction under the claim.
    async fn render_for_actor(
        &self,
        claim: &RenderClaim,
        posts: &[PostContext],
        sealed: &[SealedPageRender],
    ) -> Result<RenderOutcome> {
        let actor_id = claim.actor_id();
        // 1. Nothing is cleared and no row is written until step 8: the rows of
        // BOTH stores accumulate here while their bodies stream to the blob
        // store, so a reader mid-render sees the last render's site whole.
        let mut pages: Vec<RenderedPage> = Vec::new();
        let mut sealed_pages: Vec<RenderedSealedPage> = Vec::new();

        // 2. Load site context.
        let site = self.load_site_context(actor_id).await?;

        // 3. Find all .html.hbs files (capped — a bad template is skipped, not
        // fatal, and the total count is bounded to limit output amplification).
        // The listing is actor-wide, so each row is gated on its folder's
        // CURRENT serving state — before
        // the cap, so a dead folder's templates cannot crowd live ones out.
        //
        // The SQL is a COARSE pre-filter and `render_input::is_template` is what
        // decides: SQLite's `LIKE` folds ASCII case and the sync door's revoke
        // test is Rust, so the two agree only if one of them is authoritative
        // (§ *A revoke is durable* — a template whose delete the door does not
        // count as a revoke leaves its page serving).
        let all_hbs = self.db.list_web_files_by_ext(actor_id, ".html.hbs").await?;
        let mut hbs_files = Vec::with_capacity(all_hbs.len());
        for file in all_hbs {
            if super::render_input::is_template(&file.path) && self.is_render_input_row(&file).await
            {
                hbs_files.push(file);
            }
        }
        if hbs_files.len() > MAX_TEMPLATES {
            tracing::warn!(
                "web render: actor has {} .html.hbs templates; capping render to {MAX_TEMPLATES}",
                hbs_files.len()
            );
            hbs_files.truncate(MAX_TEMPLATES);
        }

        // Track whether _post.html.hbs / _paywall.html.hbs exist.
        let mut post_template_bytes: Option<Vec<u8>> = None;
        let mut has_post_template = false;
        let mut paywall_template_bytes: Option<Vec<u8>> = None;

        // 4. Render each template (skip the per-post specials — handled below).
        for file in &hbs_files {
            let filename = file.path.rsplit('/').next().unwrap_or(file.path.as_str());

            if filename == "_post.html.hbs" {
                // Load the bytes for later per-post rendering.
                if let Some(bytes) = self.load_template_bytes(actor_id, &file.path).await? {
                    post_template_bytes = Some(bytes);
                    has_post_template = true;
                }
                continue;
            }
            if filename == "_paywall.html.hbs" {
                // The user's teaser-page override for gated posts.
                if let Some(bytes) = self.load_template_bytes(actor_id, &file.path).await? {
                    paywall_template_bytes = Some(bytes);
                }
                continue;
            }

            // Load template bytes.
            let Some(template_bytes) = self.load_template_bytes(actor_id, &file.path).await? else {
                continue;
            };

            // Render with the full post list and no single-post context, bounded
            // in time + output. A pathological template is skipped, not fatal.
            let rendered = {
                let site = site.clone();
                let posts = posts.to_vec();
                match render_bounded(move || {
                    render_single_template(&template_bytes, site, posts, None)
                })
                .await
                {
                    Ok(html) => html,
                    Err(e) => {
                        tracing::warn!("web render: skipping template {}: {e}", file.path);
                        continue;
                    }
                }
            };

            // Output path = input path with `.hbs` stripped, folding case like
            // the match that admitted it (`render_input::rendered_output_path`).
            let output_path = super::render_input::rendered_output_path(&file.path);

            if !self
                .store_rendered(claim, &mut pages, &output_path, rendered.as_bytes())
                .await?
            {
                return Ok(RenderOutcome::Superseded);
            }
        }

        // 5. Per-post rendering with _post.html.hbs — UNGATED posts only; a
        // gated post's canonical page is its teaser (rendered below), and its
        // full page renders into the sealed store.
        if let Some(template_bytes) = &post_template_bytes {
            for post in posts.iter().filter(|p| p.paywall.is_none()) {
                let slug = post.slug.clone();
                let rendered = {
                    let site = site.clone();
                    let posts = posts.to_vec();
                    let template_bytes = template_bytes.clone();
                    let post = post.clone();
                    match render_bounded(move || {
                        render_single_template(&template_bytes, site, posts, Some(post))
                    })
                    .await
                    {
                        Ok(html) => html,
                        Err(e) => {
                            tracing::warn!("web render: skipping per-post page {slug}: {e}");
                            continue;
                        }
                    }
                };
                let output_path = format!("post/{slug}.html");
                if !self
                    .store_rendered(claim, &mut pages, &output_path, rendered.as_bytes())
                    .await?
                {
                    return Ok(RenderOutcome::Superseded);
                }
            }
        }

        // 5b. Paywall/teaser pages for gated posts — the canonical
        // `post/{slug}.html` always serves the teaser without a token
        // (`monetization.md` § Pillar 2 "No token → teaser"): the public
        // preview + the paywall box, via the user's `_paywall.html.hbs` or the
        // built-in default. Rendered from the PUBLIC context only.
        for post in posts.iter().filter(|p| p.paywall.is_some()) {
            let slug = post.slug.clone();
            let rendered = {
                let site = site.clone();
                let posts_vec = posts.to_vec();
                let post = post.clone();
                let template = paywall_template_bytes.clone();
                match render_bounded(move || match &template {
                    Some(bytes) => render_single_template(bytes, site, posts_vec, Some(post)),
                    None => render::render_default_paywall(&site, &posts_vec, &post),
                })
                .await
                {
                    Ok(html) => html,
                    Err(e) => {
                        tracing::warn!("web render: skipping paywall page {slug}: {e}");
                        continue;
                    }
                }
            };
            let output_path = format!("post/{slug}.html");
            if !self
                .store_rendered(claim, &mut pages, &output_path, rendered.as_bytes())
                .await?
            {
                return Ok(RenderOutcome::Superseded);
            }
        }

        // 5c. Sealed FULL pages for paywalled posts under a live grant: the
        // normal post template over the full-content context, sealed under the
        // scope-derived key, stored in `web_rendered_sealed` at the same path.
        for sr in sealed {
            // Sealing a full page is a render plus an AEAD plus a blob write —
            // worth the one extra read to notice a supersession before paying
            // for it, rather than at the staging that would refuse it anyway.
            if self.db.web_render_superseded(claim).await? {
                return Ok(RenderOutcome::Superseded);
            }
            let slug = sr.post.slug.clone();
            let rendered = {
                let site = site.clone();
                let posts_vec = posts.to_vec();
                let post = sr.post.clone();
                let template = post_template_bytes.clone();
                match render_bounded(move || match &template {
                    Some(bytes) => render_single_template(bytes, site, posts_vec, Some(post)),
                    None => render::render_default_post(&site, &posts_vec, &post),
                })
                .await
                {
                    Ok(html) => html,
                    Err(e) => {
                        tracing::warn!("web render: skipping sealed full page {slug}: {e}");
                        continue;
                    }
                }
            };
            let sealed_bytes = encrypt_content(&sr.seal_key, rendered.as_bytes());
            let Some(hash_bytes) = self
                .store_render_body(claim, &sealed_bytes, "application/octet-stream")
                .await?
            else {
                return Ok(RenderOutcome::Superseded);
            };
            sealed_pages.push(RenderedSealedPage {
                path: format!("post/{slug}.html"),
                blob_hash: hash_bytes,
                content_type: "text/html".into(),
                tier: sr.tier.clone(),
                post_id: sr.post_id,
            });
        }

        // 6. Built-in defaults when no user templates are present.
        // If no .html.hbs files at all and there are posts, render a default index page.
        if hbs_files.is_empty() && !posts.is_empty() {
            let site = site.clone();
            let posts_vec = posts.to_vec();
            match render_bounded(move || render::render_default_index(&site, &posts_vec)).await {
                Ok(index_html) => {
                    if !self
                        .store_rendered(claim, &mut pages, "index.html", index_html.as_bytes())
                        .await?
                    {
                        return Ok(RenderOutcome::Superseded);
                    }
                }
                Err(e) => tracing::warn!("web render: default index render failed: {e}"),
            }
        }

        // If _post.html.hbs doesn't exist but there are posts, render default
        // post pages (ungated only — gated posts got their teaser in 5b).
        if !has_post_template && !posts.is_empty() {
            for post in posts.iter().filter(|p| p.paywall.is_none()) {
                let slug = post.slug.clone();
                let site = site.clone();
                let posts_vec = posts.to_vec();
                let post = post.clone();
                match render_bounded(move || render::render_default_post(&site, &posts_vec, &post))
                    .await
                {
                    Ok(rendered) => {
                        let output_path = format!("post/{slug}.html");
                        if !self
                            .store_rendered(claim, &mut pages, &output_path, rendered.as_bytes())
                            .await?
                        {
                            return Ok(RenderOutcome::Superseded);
                        }
                    }
                    Err(e) => tracing::warn!("web render: default post page {slug} failed: {e}"),
                }
            }
        }

        // 7. RSS feed when posts exist.
        if !posts.is_empty() {
            let rss = generate_rss(&site, posts, "");
            if !self
                .store_rendered(claim, &mut pages, "feed.xml", rss.as_bytes())
                .await?
            {
                return Ok(RenderOutcome::Superseded);
            }
        }

        // 8. The render's ONE write: both stores replaced in one transaction
        // under the claim — the old site's rows (a formerly-gated or
        // grant-revoked post's stale sealed page among them) go as this one's
        // land, so no reader ever sees a cleared-but-unwritten site.
        if !self
            .db
            .replace_web_rendered(claim, &pages, &sealed_pages)
            .await?
        {
            return Ok(RenderOutcome::Superseded);
        }
        Ok(RenderOutcome::Wrote)
    }

    /// Build the actor's published-post render context and (re-)render their
    /// site. The **production trigger** for serving web content from posts
    /// (Slice 1a, web-content-hosting.md § Routing/render): the
    /// `fauna.web.publish.{set,unset}` handlers call this after the
    /// `web_published` row changes, so a published post renders a default
    /// `index.html` + per-post pages with no `.html.hbs` template needed (the
    /// quickest apex content source). Posts are ordered newest-first and linked
    /// via `next`/`prev` slugs. Idempotent — re-renders the whole set each call
    /// (`render_for_actor` replaces the whole site), so an unpublish that
    /// drops the last post re-renders an empty site.
    ///
    /// **The moderation gate is the enumeration.** Bodies are read below through
    /// the deliberately flag-blind `load_post_body`, so a post a moderation flag
    /// withholds must never be *listed*: `list_web_published_servable_capped`
    /// drops it, and the whole-site replacement means its page, its index entry and
    /// its RSS item are gone after the next render (`moderation.md` § Legal
    /// takedown → *Posts*). The takedown handler triggers that render itself —
    /// [`crate::moderation_handlers`] — so "taken_down" means the site is shut.
    ///
    /// **The region fold rides the same render** (`region-blocking.md` § The
    /// content plane → *The nest-as-publisher leg*): with a content policy in
    /// force for the declared situs, each post's labels are folded through the
    /// shared engine and a blocked or collapsed post renders the reasoned
    /// placeholder on every page that carries it — its own page, the index,
    /// the feed, any template listing it, and a gated post's sealed full page (withheld
    /// outright under a block). The pages are static, so a change of what is in
    /// force re-renders every publishing site through
    /// [`Self::rerender_after_region_policy_change`].
    ///
    /// **A render that completes discharges the actor's owed-render marker**
    /// (`web_render_owed`, [`Self::render_owed`]) — whichever door asked for
    /// it, since the site it wrote reflects every state change committed
    /// before its listing. The marker's nonce is read BEFORE that listing and
    /// only that nonce is discharged, so a revoke committed mid-render stays
    /// owed.
    ///
    /// **A render that is SUPERSEDED discharges nothing** (§ *One render writes
    /// at a time*): it wrote no page, so it settled no debt. Both debts stay
    /// with whoever does write — the newer render, which read the newer nonce,
    /// or the fail-closed clear that took the site dark and re-owed its
    /// restore. To its caller a superseded render is still a success: the site
    /// it would have written has already been replaced by a fresher one.
    ///
    /// **A render answering an owed revoke races the withdrawal deadline**
    /// (§ *A reader sees a whole site or none*): the site is replaced only in
    /// the render's last statement, so the old site — withdrawn content
    /// included — serves until then, and how long that is the author controls.
    /// If the marker stood when this render began and it has not committed
    /// within [`WITHDRAWAL_DEADLINE`] of its claim, the old site is cleared
    /// under that claim (owing its restore in the same transaction) and the
    /// render carries on, still committing whole. A timer racing the render,
    /// not a check between pages: a render stuck inside one await is cleared
    /// like any other.
    pub async fn render_published_posts(&self, actor_id: &[u8; 32]) -> Result<()> {
        let owed = self.db.web_render_owed_nonce(actor_id).await?;
        // Open this render's claim BEFORE its listing (§ *One render writes at
        // a time*) — the first statement of `render_published_posts_now`. The
        // listing is what the whole output is a function of, so the render that
        // lists LAST is the one whose pages are current — which is why the
        // claim is minted here and not at the render's one write, where a
        // render that listed before a revoke would still outrank one that
        // listed after it.
        let claim = self.db.begin_web_render(actor_id).await?;
        let deadline = tokio::time::Instant::now() + self.withdrawal_deadline;
        // A render that completes is also the restore of a site a clear took
        // dark, whichever door asked for it. Read AFTER the mint, unlike the
        // marker: a clear that landed before the mint is answered by this
        // render's replacement, and one that lands after it supersedes this
        // render, which then discharges nothing — so no ordering leaves a
        // healthy site carrying a stale restore row. An unreadable row
        // discharges nothing, and the restore is simply tried again.
        let mut restore = self
            .db
            .web_restore_owed_nonce(actor_id)
            .await
            .unwrap_or(None);
        let mut cleared_at_deadline = false;
        let outcome = {
            let render = self.render_published_posts_now(&claim);
            tokio::pin!(render);
            if owed.is_some() {
                // A deadline that has ALREADY passed answers on its first
                // poll; a timer armed for a past instant does not (the wheel
                // rounds up to its next tick, and a reader gets in first).
                let past_deadline = async {
                    if tokio::time::Instant::now() < deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                };
                tokio::select! {
                    // Deadline first, so one already passed withdraws the old
                    // site before the render's first poll, deterministically.
                    biased;
                    () = past_deadline => {
                        // Superseded (`None`) clears nothing — the site is
                        // already somebody else's, and the render finds out at
                        // its next page.
                        if let Some(nonce) = self.db.clear_web_rendered(&claim).await? {
                            tracing::warn!(
                                actor = %hex::encode(&actor_id[..4]),
                                "web render answering an owed revoke outlasted {:?}; the old \
                                 site was withdrawn until it commits",
                                self.withdrawal_deadline
                            );
                            restore = Some(nonce);
                            cleared_at_deadline = true;
                        }
                        render.as_mut().await
                    }
                    outcome = &mut render => outcome,
                }
            } else {
                render.await
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                // The deadline clear left the site dark and owed its restore;
                // a best-effort caller clears nothing more, so wake the retry
                // here rather than leave the row for the next boot.
                if cleared_at_deadline {
                    self.restore_wake.notify_one();
                }
                return Err(e);
            }
        };
        if outcome == RenderOutcome::Superseded {
            return Ok(());
        }
        self.discharge(actor_id, owed).await;
        if let Some(nonce) = restore
            && let Err(e) = self.db.discharge_web_restore_owed(actor_id, nonce).await
        {
            tracing::warn!(
                actor = %hex::encode(&actor_id[..4]),
                "blanked site restored but its owed restore was not discharged: {e:#}"
            );
        }
        Ok(())
    }

    /// Discharge the marker read before a render that has now completed (or
    /// before a fail-closed clear that has). A discharge that fails is only
    /// logged: the revoke itself landed, and the surviving marker costs one
    /// redundant render at the next boot.
    async fn discharge(&self, actor_id: &[u8; 32], owed: Option<i64>) {
        if let Some(nonce) = owed
            && let Err(e) = self.db.discharge_web_render_owed(actor_id, nonce).await
        {
            tracing::warn!(
                actor = %hex::encode(&actor_id[..4]),
                "owed web render landed but its marker was not discharged: {e:#}"
            );
        }
    }

    /// The render itself, under a claim its one caller minted immediately
    /// before calling — the listing below is this function's first read.
    async fn render_published_posts_now(&self, claim: &RenderClaim) -> Result<RenderOutcome> {
        let actor_id = claim.actor_id();
        self.render_invocations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let published = self
            .db
            .list_web_published_servable_capped(actor_id, MAX_RENDERED_POSTS)
            .await?;
        // The web-serve holder's live grant snapshot — read once per render.
        let grant_set = self.holder.as_ref().map(|h| h.registry.current());
        let now = crate::db::now_epoch_secs() as u64;
        // The nest-as-publisher leg (`region-blocking.md` § The content plane):
        // the declared situs chain's content policies, read once per render.
        let registry = self
            .region_registry
            .clone()
            .unwrap_or_else(crate::region_tier::relay_registry);
        let policies = super::region::situs_policies(&self.db, &registry).await?;
        let mut scores = self.region_scores.begin(actor_id);
        // A public context per post, plus — for a gated post whose tier the
        // holder wields — the decrypted full text destined for the sealed page,
        // plus the region verdict its pages carry.
        struct RenderPost {
            ctx: PostContext,
            full: Option<(String, String, [u8; 32], Zeroizing<[u8; 32]>)>,
            region: Option<super::region::RegionPlacement>,
        }
        let mut items: Vec<RenderPost> = Vec::with_capacity(published.len());
        for (post_id_bytes, slug) in published {
            let Ok(post_id) = <[u8; 32]>::try_from(post_id_bytes.as_slice()) else {
                continue;
            };
            // Segment-first in production; tests without a wired segment source
            // fall back to the inline `content.payload` read (the live shape for a
            // post with no decodable author).
            let body = match &self.post_segments {
                Some(seg) => crate::segments::post::load_post_body(seg, &self.db, &post_id).await,
                None => self
                    .db
                    .get_post(&post_id)
                    .await
                    .map(|found| found.map(|(payload, _)| payload)),
            };
            // One post whose body cannot be READ is skipped like one that is
            // no longer stored, never allowed to fail the whole render: every
            // revoking door fails closed, so a single persistently unreadable
            // post would otherwise take the author's entire site dark at each
            // of them. Leaving a post off the site is the safe side.
            let body = match body {
                Ok(Some(body)) => body,
                Ok(None) => continue, // a published row pointing at a post no longer stored
                Err(e) => {
                    tracing::warn!(
                        post = %hex::encode(&post_id[..4]),
                        "web render: post body unreadable, left off the site: {e:#}"
                    );
                    continue;
                }
            };
            let Some(post) = crate::db::posts::decode_stored_post(&body) else {
                continue;
            };
            // For a gated post, `post.body_text()` is the public preview by
            // design — the teaser's raw material. Attach the paywall box.
            let mut ctx = post_to_context(&post, slug);
            let region = if policies.is_inert() {
                None
            } else {
                let labels =
                    super::region::post_labels(&self.db, &policies, &mut scores, &post_id, &post)
                        .await?;
                super::region::placement(&policies, &labels)
            };
            let mut full = None;
            // A blocked post's full body is never opened: its sealed page is
            // withheld with its public one.
            if let Some(gated) = &post.gated
                && !region.as_ref().is_some_and(|r| r.withholds_body())
            {
                let tier_row = self
                    .db
                    .get_subscription_tier(actor_id, &gated.tier)
                    .await
                    .unwrap_or(None);
                ctx.paywall = Some(PaywallContext {
                    tier: gated.tier.clone(),
                    price_hint: tier_row.as_ref().and_then(|t| t.price_hint.clone()),
                    payment_url: tier_row.as_ref().and_then(|t| t.payment_url.clone()),
                });
                full = self
                    .decrypt_gated_full_text(actor_id, &post_id, gated, grant_set.as_deref(), now)
                    .await
                    .map(|(text, seal_key)| (text, gated.tier.clone(), post_id, seal_key));
            }
            items.push(RenderPost { ctx, full, region });
        }
        self.region_scores.finish(actor_id, scores);
        // Newest first, then link chronological neighbors by slug.
        items.sort_by_key(|p| std::cmp::Reverse(p.ctx.created_at));
        let slugs: Vec<String> = items.iter().map(|p| p.ctx.slug.clone()).collect();
        for (i, item) in items.iter_mut().enumerate() {
            item.ctx.next = i.checked_sub(1).map(|j| slugs[j].clone());
            item.ctx.prev = slugs.get(i + 1).cloned();
        }
        // Split into the public contexts and the sealed full-page renders. The
        // full context inherits the linked public one, swapping in the full
        // title/content.
        let mut contexts = Vec::with_capacity(items.len());
        let mut sealed = Vec::new();
        for item in items {
            if let Some((full_text, tier, post_id, seal_key)) = item.full {
                let mut full_ctx = item.ctx.clone();
                full_ctx.title = derive_title(&full_text, &full_ctx.slug);
                full_ctx.content = render::markdown_to_html(&full_text);
                // A collapsed post's full page collapses the same way.
                if let Some(region) = &item.region {
                    region.apply(&mut full_ctx);
                }
                sealed.push(SealedPageRender {
                    post: full_ctx,
                    tier,
                    post_id,
                    seal_key,
                });
            }
            let mut ctx = item.ctx;
            if let Some(region) = &item.region {
                region.apply(&mut ctx);
            }
            contexts.push(ctx);
        }
        self.render_for_actor(claim, &contexts, &sealed).await
    }

    /// [`Self::render_for_actor`] with a claim of its own — the shape a unit
    /// test wants, where there is no listing to mint one before.
    #[cfg(test)]
    async fn render_for_actor_now(
        &self,
        actor_id: &[u8; 32],
        posts: &[PostContext],
        sealed: &[SealedPageRender],
    ) -> Result<RenderOutcome> {
        let claim = self.db.begin_web_render(actor_id).await?;
        self.render_for_actor(&claim, posts, sealed).await
    }

    /// Re-render `actor_id`'s site because a moderation flag just moved on one
    /// of their published posts — and **fail closed** if the render cannot run.
    ///
    /// A plain [`Self::render_published_posts`] failure is logged and shrugged
    /// off by `publish.set`, which is right there: that door only ADDS, so the
    /// old site is still the author's own consented output. After a legal
    /// takedown — as after any door that removes content
    /// ([`Self::rerender_after_revoke`]) — it is not:
    /// the stale `post/{slug}.html`, index and `feed.xml` still carry the
    /// withheld body, and no periodic re-render would ever replace them. So if
    /// the render errors, the actor's rendered output (public and sealed) is
    /// cleared instead: the site goes dark until the next successful render
    /// rather than keep serving compelled bytes. That costs nothing
    /// irrecoverable — every `web_rendered` row is derived, and rebuilt whole by
    /// any render — and it is the failure direction the takedown rulings take
    /// everywhere else (`moderation.md` § Legal takedown: under-publishing is the
    /// safe side). Returns the render error (with the clear's outcome attached)
    /// so the caller can log it; the takedown itself has already landed.
    pub async fn rerender_after_moderation_change(&self, actor_id: &[u8; 32]) -> Result<()> {
        self.rerender_fail_closed(actor_id, "a moderation change")
            .await
    }

    /// Re-render `actor_id`'s site because a door just **removed** content from
    /// it — and fail closed if the render cannot run. `door` names the door,
    /// for the error.
    ///
    /// What decides fail-closed is add-versus-revoke, not whose act it was. A
    /// failed render after `publish.set` leaves the old site, which is still
    /// output the author consented to; a failed render after a delete, an
    /// unpublish, a folder leaving the site or a template change leaves
    /// exactly what the author just withdrew, served to anyone, with nothing
    /// periodic to replace it (`web-content-hosting.md` § Routing, render,
    /// serving → *A revoke is durable*). Under-publishing is the safe side and
    /// costs nothing irrecoverable: every rendered row is derived.
    pub async fn rerender_after_revoke(&self, actor_id: &[u8; 32], door: &str) -> Result<()> {
        self.rerender_fail_closed(actor_id, door).await
    }

    /// [`Self::rerender_after_revoke`], but only when a render is owed to
    /// `actor_id` — the shape of every door whose state change decides *inside
    /// its own transaction* whether it revoked anything (a post delete marks
    /// only a still-published post; a folder update only a serving
    /// transition). Keyed on the marker rather than on what THIS call changed,
    /// which is what lets a client's retry heal a torn first attempt: the
    /// retry changes nothing, and still finds the render owed. Returns whether
    /// a render ran.
    pub async fn render_owed(&self, actor_id: &[u8; 32], door: &str) -> Result<bool> {
        if self.db.web_render_owed_nonce(actor_id).await?.is_none() {
            return Ok(false);
        }
        self.rerender_fail_closed(actor_id, door).await?;
        Ok(true)
    }

    /// The **boot drain**: render every site a render is still owed to — the
    /// reconcile half of the revoke's durability. A nest that stopped between
    /// a revoking door's commit and its render restarts with the marker that
    /// commit carried, and this pass honours it before the site serves much
    /// longer. Each site fails closed like the door would have; a failure is
    /// logged and never stops the walk. Returns the count of failed sites.
    ///
    /// Then the **owed restores** — every site an earlier fail-closed clear
    /// took dark gets one render attempt per boot — and the drain leaves the
    /// restore retry running for the time between boots
    /// (`web-content-hosting.md` § Routing, render, serving → *A blanked site
    /// is owed its restore*). The retry is **started** HERE rather than beside
    /// the call in `start_server` so that the whole-boot pin on this drain
    /// covers it: there is no second piece of boot wiring to drop.
    ///
    /// What the caller still owes it is one line, and the compiler asks for it:
    /// the task's `JoinHandle` comes back in [`BootDrain::restore_retry`]
    /// rather than being detached here, because the loop outlives every
    /// request and must die with the serving generation. See that field's own
    /// note — starting the loop and scoping it are deliberately separate, since
    /// only the caller knows which generation it is booting.
    pub async fn drain_owed_renders(self: &Arc<Self>) -> Result<BootDrain> {
        let mut failed = 0;
        let revoked = self.db.list_web_render_owed().await?;
        for actor in &revoked {
            if let Err(e) = self.rerender_fail_closed(actor, "a restart").await {
                failed += 1;
                tracing::warn!(
                    actor = %hex::encode(&actor[..4]),
                    "{e:#}"
                );
            }
        }
        // A site the walk above just tried is not tried twice in one boot.
        match self.restore_owed_sites(&revoked).await {
            Ok(still_dark) => failed += still_dark,
            Err(e) => {
                failed += 1;
                tracing::warn!("owed web restores: cannot list the blanked sites: {e:#}");
            }
        }
        // A boot that left a site dark has spent the retry's first attempt.
        let mut retry = RetryState::default();
        if failed > 0 {
            retry.record_failure(now_unix());
        }
        Ok(BootDrain {
            failed,
            restore_retry: self.start_restore_retry(retry),
        })
    }

    /// One render attempt for every site still owed its restore, skipping
    /// `already_tried`. Fail-closed like every other caller: a restore that
    /// errors after its own clear must not leave half a site. Returns how many
    /// sites are still dark.
    async fn restore_owed_sites(&self, already_tried: &[[u8; 32]]) -> Result<usize> {
        let mut still_dark = 0;
        for actor in self.db.list_web_restore_owed().await? {
            if already_tried.contains(&actor) {
                continue;
            }
            if let Err(e) = self.rerender_fail_closed(&actor, "a restore attempt").await {
                still_dark += 1;
                tracing::debug!(
                    actor = %hex::encode(&actor[..4]),
                    "{e:#}"
                );
            }
        }
        Ok(still_dark)
    }

    /// The **restore retry**: between boots, re-render the sites a fail-closed
    /// clear took dark. Idle until such a clear wakes it; then paced by the
    /// nest's one backoff (`RetryState` / `next_attempt_delay`: the first
    /// attempt at once, then evenly spread, never more than its hourly budget)
    /// for as long as any site stays dark — a whole pass per attempt, so the
    /// bound is nest-wide, not per site. The log is held in memory: a restart
    /// makes its own attempt in the boot drain, which seeds `retry`.
    ///
    /// Holds the service weakly, and is woken by its `Drop`, so an embedded
    /// nest that shuts down is not kept alive by this task.
    ///
    /// **Returns the task's handle for the caller to scope** — `None` when a
    /// retry is already running, since the `restore_retry_started` flip makes
    /// this once-per-service. Weak-holding is not a teardown mechanism: a
    /// deployment-seed rotation tears the serving generation down and
    /// re-enters `start_server` *without* dropping the process, so a detached
    /// loop would keep waking, keep upgrading its `Weak`, and keep rendering
    /// under the superseded generation (`state.rs`'s
    /// `boot_worker_spawns_are_generation_scoped_or_marked`, DRAIN-REACH).
    /// The handle rides out through [`BootDrain::restore_retry`] to the one
    /// boot caller, which adopts it with `AppState::scope_handle`.
    fn start_restore_retry(
        self: &Arc<Self>,
        mut retry: RetryState,
    ) -> Option<tokio::task::JoinHandle<()>> {
        use std::sync::atomic::Ordering;
        if self.restore_retry_started.swap(true, Ordering::SeqCst) {
            return None;
        }
        let service = Arc::downgrade(self);
        let wake = self.restore_wake.clone();
        // spawn-ok(returns-handle-for-scope): the handle is returned, and the boot caller adopts it via `AppState::scope_handle`
        Some(tokio::spawn(async move {
            loop {
                let Some(this) = service.upgrade() else {
                    return;
                };
                let nothing_owed =
                    matches!(this.db.list_web_restore_owed().await, Ok(owed) if owed.is_empty());
                drop(this);
                if nothing_owed {
                    retry.reset();
                    wake.notified().await;
                    continue;
                }
                tokio::time::sleep(next_attempt_delay(&retry, now_unix())).await;
                let Some(this) = service.upgrade() else {
                    return;
                };
                match this.restore_owed_sites(&[]).await {
                    Ok(0) => retry.reset(),
                    Ok(still_dark) => {
                        retry.record_failure(now_unix());
                        tracing::warn!(
                            still_dark,
                            "owed web restores: some blanked sites still fail to render"
                        );
                    }
                    Err(e) => {
                        retry.record_failure(now_unix());
                        tracing::warn!("owed web restores: cannot list the blanked sites: {e:#}");
                    }
                }
            }
        }))
    }

    /// Re-render every publishing site because the region content policy in
    /// force for the declared situs moved — a region declared, re-declared or
    /// withdrawn, a newer document accepted, a de-listed one retired
    /// (`region-blocking.md` § The content plane → *The nest-as-publisher
    /// leg*). The pages are static, so without this a page rendered under the
    /// old policy would keep serving until some unrelated publish re-rendered
    /// it.
    ///
    /// Each site takes [`Self::rerender_after_moderation_change`]'s
    /// fail-closed shape, for the same reason: a site whose render fails is
    /// cleared rather than left serving pages the policy now in force may
    /// withhold. A failure is logged and never stops the walk; the count of
    /// failed sites is returned.
    ///
    /// The walk is O(sites × posts), so it is the render most likely to be cut
    /// short by a restart. **It marks nothing itself**: every publishing site
    /// was already marked owed by the policy-moving write that got here — the
    /// `nest_region` declaration write, `put_relay_artifact` or
    /// `retire_relay_artifact`, each carrying the mark in its own transaction
    /// (`db/region_tier.rs`) — so the boot drain finishes a walk the nest did
    /// not, *and* covers a nest that stopped before the walk began. A mark of
    /// its own here would only re-roll nonces the policy write already wrote.
    pub async fn rerender_after_region_policy_change(&self) -> Result<usize> {
        let mut failed = 0;
        for actor in self.db.list_web_publishing_actors().await? {
            if let Err(e) = self
                .rerender_fail_closed(&actor, "a region policy change")
                .await
            {
                failed += 1;
                tracing::warn!(
                    actor = %hex::encode(&actor[..4]),
                    "{e:#}"
                );
            }
        }
        Ok(failed)
    }

    /// Render `actor_id`'s published posts, clearing the rendered site instead
    /// if the render fails (see [`Self::rerender_after_moderation_change`]).
    /// `cause` names what moved, for the error.
    ///
    /// **A clear that lands is a completed revoke, so it discharges the
    /// owed-render marker too.** Nothing the state change removed still
    /// serves, which is all the marker promises. What the clear leaves
    /// behind instead, in its own transaction, is the **owed restore** — a
    /// separate state because the two are owed differently: a revoke is a
    /// safety debt, paid at once and never paced; a restore is an
    /// availability debt, and a render that keeps failing has to be paced.
    /// The dark site is rendered again by the restore retry, by the next
    /// boot, or by the next render any door triggers, whichever comes first.
    async fn rerender_fail_closed(&self, actor_id: &[u8; 32], cause: &str) -> Result<()> {
        // Read ahead of the render, like the render's own read: an unreadable
        // marker discharges nothing, and the boot drain tries again.
        let owed = self
            .db
            .web_render_owed_nonce(actor_id)
            .await
            .unwrap_or(None);
        let render_err = match self.render_published_posts(actor_id).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        match self.db.clear_web_rendered_owing_restore(actor_id).await {
            Ok(()) => {
                self.discharge(actor_id, owed).await;
                self.restore_wake.notify_one();
                Err(render_err.context(format!(
                    "re-render after {cause} failed; the rendered site was cleared instead"
                )))
            }
            Err(clear_err) => Err(render_err.context(format!(
                "re-render after {cause} failed AND clearing the rendered site failed \
                 ({clear_err:#}); stale pages may still serve until the next render"
            ))),
        }
    }

    /// Decrypt a gated post's full body text under the holder's live grant,
    /// returning it with the `derive_web_render_key` seal key for the post's
    /// sealed full page. `None` (never an error — the teaser is the fallback,
    /// not a failure) when: no holder / no grant for the tier / the blob is
    /// missing / AEAD open fails / the plaintext isn't a `PostBody`.
    async fn decrypt_gated_full_text(
        &self,
        owner: &[u8; 32],
        post_id: &[u8; 32],
        gated: &fauna_core::subscription::types::GatedInfo,
        grants: Option<&fauna_capability_holder::GrantSet>,
        now: u64,
    ) -> Option<(String, Zeroizing<[u8; 32]>)> {
        let seal_id = &gated.seal_id;
        let period_key: [u8; 32] = grants?
            .key_for(owner, "content.read", "post", Some(&gated.tier), now)?
            .try_into()
            .ok()?;
        let seal_key = Zeroizing::new(derive_web_render_key(
            &period_key,
            &ContentHash::from_digest_raw(*post_id),
        ));
        let blob = self.load_blob_bytes(&gated.encrypted_ref.digest()).await?;
        let key = Zeroizing::new(derive_post_key(&period_key, seal_id));
        let plain = match decrypt_content(&key, &blob) {
            Ok(p) => Zeroizing::new(p),
            Err(e) => {
                tracing::warn!(
                    target: "web_paywall",
                    slug_post = %hex::encode(&post_id[..4]),
                    "gated body AEAD open failed ({e}); rendering teaser only"
                );
                return None;
            }
        };
        // The body blob's plaintext is the canonical dag-cbor `PostBody`
        // (`ui/feed.md` § Encryption at rest).
        let body: fauna_core::data::PostBody = match fauna_core::encoding::canonical_decode(&plain)
        {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    target: "web_paywall",
                    "gated body decode failed ({e}); rendering teaser only"
                );
                return None;
            }
        };
        use fauna_core::data::PostBody;
        let text = match &body {
            PostBody::Text { content, .. } | PostBody::TextWithMedia { content, .. } => {
                content.clone()
            }
            PostBody::Structured { content, .. } => content.clone().unwrap_or_default(),
            PostBody::Media { alt_text, .. } => alt_text.clone().unwrap_or_default(),
            PostBody::Video { .. } => String::new(),
        };
        Some((text, seal_key))
    }

    // ---- Private helpers ----

    /// the render pipeline is a *producer of bytes from `web_files`
    /// rows* — templates and `_site.json` — so every input read asks the same
    /// folder gate as the serve doors (`web-content-hosting.md` § Static site
    /// files: any producer of bytes from a `web_files` row asks the same
    /// core). A sealed row is additionally never a render input (the v1 bound
    /// the sync-side trigger already enforces — repeated here at consumption,
    /// so a paywalled set's template cannot ride in via an unrelated
    /// trigger). Gating at consumption is what makes a folder's serving
    /// transitions retroactive: a disabled or re-classified folder's
    /// templates stop feeding EVERY later render, not just the next sync
    /// change.
    async fn is_render_input_row(&self, row: &crate::db::web::WebFileRow) -> bool {
        row.content_key_version.is_none()
            && super::serve::gate_web_file_folder(&self.db, row)
                .await
                .is_some()
    }

    /// Load and parse `_site.json` from web_files; fall back to an empty
    /// `SiteContext` if not present, not a live render input, or unparseable.
    async fn load_site_context(&self, actor_id: &[u8; 32]) -> Result<SiteContext> {
        if let Some(row) = self.db.get_web_file(actor_id, "_site.json").await?
            && self.is_render_input_row(&row).await
            && let Some(bytes) = self.load_web_file_bytes(&row.blob_hash).await
        {
            let json_str = String::from_utf8_lossy(&bytes);
            return Ok(build_site_context(Some(&json_str), "", ""));
        }
        Ok(build_site_context(None, "", ""))
    }

    /// Read template bytes from the blob store for a given actor+path.
    /// `None` for a row that is not a live render input — the fetch-by-path
    /// twin of the listing filter, so a stale path can never re-enter here.
    async fn load_template_bytes(
        &self,
        actor_id: &[u8; 32],
        path: &str,
    ) -> Result<Option<Vec<u8>>> {
        let Some(row) = self.db.get_web_file(actor_id, path).await? else {
            return Ok(None);
        };
        if !self.is_render_input_row(&row).await {
            return Ok(None);
        }
        Ok(self.load_web_file_bytes(&row.blob_hash).await)
    }

    /// BLAKE3-hash the data, store it in the blob store, and add its
    /// `web_rendered` row to `pages` — the render writes every row at once, in
    /// its last statement ([`CacheDb::replace_web_rendered`]). Content-type is
    /// inferred from the path extension. `Ok(false)` = superseded, nothing
    /// stored.
    async fn store_rendered(
        &self,
        claim: &RenderClaim,
        pages: &mut Vec<RenderedPage>,
        path: &str,
        data: &[u8],
    ) -> Result<bool> {
        let content_type = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream");
        let Some(hash_bytes) = self.store_render_body(claim, data, content_type).await? else {
            return Ok(false);
        };
        pages.push(RenderedPage {
            path: path.to_string(),
            blob_hash: hash_bytes,
            content_type: content_type.to_string(),
        });
        Ok(true)
    }

    /// Store one rendered body — a public page or a sealed paywalled one — as
    /// a blob the sweep can see, returning its content address; `Ok(None)` =
    /// superseded, nothing stored (`backup-restore.md` § 9 step 2h and the
    /// sweep's premise below it).
    ///
    /// Three steps, in this order, and the order is the point:
    /// 1. **Stage** the body under the render's claim — the reference that keeps
    ///    it until the replacing transaction hands it to the rendered row. First,
    ///    so no sweep that can see the body's metadata row can miss its reference.
    ///    It is also the supersession check: a render that has already lost the
    ///    site stops spending the box's IO on pages nobody will serve, and since
    ///    the caller abandons on `None`, the most wasted work a supersession can
    ///    cost is the one page render already in hand.
    /// 2. **Store** the bytes.
    /// 3. **Record** the `blob_metadata` row, in the same breath as the store:
    ///    that table is the sweep's world, so without it the body would be
    ///    neither collected once its page is replaced nor counted, for good.
    ///
    /// ⚠ Never record the row without step 1 — or without step 2h teaching the
    /// sweep the rendered stores: a row nothing references makes the body a
    /// deletion candidate, and the page 404s with its rendered row pointing at
    /// nothing.
    async fn store_render_body(
        &self,
        claim: &RenderClaim,
        data: &[u8],
        content_type: &str,
    ) -> Result<Option<[u8; 32]>> {
        let hash_bytes = *blake3::hash(data).as_bytes();
        if !self.db.stage_web_render_body(claim, &hash_bytes).await? {
            return Ok(None);
        }
        self.blob_store
            .put(&ContentHash::from_digest_raw(hash_bytes), data)
            .await?;
        self.db
            .put_blob_metadata(&hash_bytes, data.len() as i64, content_type, None, None)
            .await?;
        Ok(Some(hash_bytes))
    }

    /// Fetch bytes stored **verbatim** in the blob store (the blob route's
    /// shape: `POST /api/v1/blob` puts the body as-is). A post's sealed
    /// `encrypted_ref` is such a blob. NOT for `web_files` rows — those are
    /// manifests, see [`Self::load_web_file_bytes`].
    async fn load_blob_bytes(&self, hash_slice: &[u8]) -> Option<Vec<u8>> {
        let arr: [u8; 32] = hash_slice.try_into().ok()?;
        self.blob_store
            .get(&ContentHash::from_digest_raw(arr))
            .await
            .ok()
            .flatten()
    }

    /// Load a **synced** render input (`.html.hbs` template, `_site.json`) given
    /// its `web_files` row hash — which is a *manifest* hash, so this walks
    /// manifest → chunks → plaintext (see [`super::file_bytes`]), stripping the
    /// nest's at-rest framing on the way. Returns `None` when the blob is
    /// missing, the walk fails, or the file is content-key-sealed.
    ///
    /// A sealed render input is deliberately unreadable here: the render
    /// pipeline holds no content key (the paywall grant is a *serve*-time
    /// capability), and monetization.md § Pillar 2's v1 bound says templates
    /// live in a public web set — a paywalled set carries static assets, which
    /// are served, never rendered.
    async fn load_web_file_bytes(&self, hash_slice: &[u8]) -> Option<Vec<u8>> {
        let arr: [u8; 32] = hash_slice.try_into().ok()?;
        match super::file_bytes::read_file_by_manifest(
            &self.db,
            &self.blob_store,
            self.at_rest_key.as_ref(),
            &arr,
            &[],
        )
        .await
        {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                tracing::warn!(error = %e, hash = %hex::encode(arr), "web render input unreadable");
                None
            }
        }
    }
}

// ==================== Post → PostContext projection ====================

/// Project a decoded [`Post`](fauna_core::data::Post) into the [`PostContext`]
/// the render pipeline consumes: the body text rendered markdown→HTML, a derived
/// title (Fauna posts carry no title field — see [`derive_title`]), `created_at`
/// in **seconds** (the wire `Timestamp` is microseconds), and `#tag` facet
/// names. `next`/`prev` are filled by the caller after sorting.
fn post_to_context(post: &fauna_core::data::Post, slug: String) -> PostContext {
    let text = post.body_text();
    let title = derive_title(&text, &slug);
    PostContext {
        title,
        slug,
        content: render::markdown_to_html(&text),
        created_at: (post.created_at.0 / 1_000_000) as i64,
        // `indexed_tags()`, not `tags()` — this is the by-id deep-link door
        // `Post::indexed_tags`'s own doc comment names as the one that must
        // use it, or the same post's hashtags read differently depending on
        // which door opened it.
        tags: post.indexed_tags(),
        next: None,
        prev: None,
        paywall: None,
    }
}

/// Derive a page title for a (titleless) Fauna post: the first non-empty line of
/// the body, stripped of a leading markdown heading marker and truncated to a
/// sensible length; falls back to the slug for a body with no text (e.g. a
/// media-only post).
fn derive_title(text: &str, slug: &str) -> String {
    const MAX_TITLE_CHARS: usize = 80;
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let stripped = first.trim_start_matches('#').trim();
    if stripped.is_empty() {
        return slug.to_string();
    }
    let truncated: String = stripped.chars().take(MAX_TITLE_CHARS).collect();
    if stripped.chars().count() > MAX_TITLE_CHARS {
        format!("{truncated}…")
    } else {
        truncated
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use fauna_core::data::ContentHash;

    use super::*;
    use crate::blob_store::BlobStoreBackend;
    use crate::db::CacheDb;

    // ---- In-memory blob store ----

    struct MemBlobStore {
        data: std::sync::Mutex<HashMap<[u8; 32], Vec<u8>>>,
    }

    impl MemBlobStore {
        fn new() -> Self {
            Self {
                data: std::sync::Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl BlobStoreBackend for MemBlobStore {
        async fn put(&self, hash: &ContentHash, data: &[u8]) -> anyhow::Result<()> {
            self.data
                .lock()
                .unwrap()
                .insert(hash.digest(), data.to_vec());
            Ok(())
        }

        async fn get(&self, hash: &ContentHash) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.data.lock().unwrap().get(&hash.digest()).cloned())
        }

        async fn exists(&self, hash: &ContentHash) -> anyhow::Result<bool> {
            Ok(self.data.lock().unwrap().contains_key(&hash.digest()))
        }

        async fn exists_batch(&self, hashes: &[ContentHash]) -> anyhow::Result<Vec<bool>> {
            let data = self.data.lock().unwrap();
            Ok(hashes
                .iter()
                .map(|h| data.contains_key(&h.digest()))
                .collect())
        }

        async fn delete(&self, hash: &ContentHash) -> anyhow::Result<()> {
            self.data.lock().unwrap().remove(&hash.digest());
            Ok(())
        }

        async fn usage_bytes(&self) -> anyhow::Result<u64> {
            Ok(self
                .data
                .lock()
                .unwrap()
                .values()
                .map(|v| v.len() as u64)
                .sum())
        }
    }

    // ---- Test helpers ----

    fn actor() -> [u8; 32] {
        [1u8; 32]
    }

    fn make_db() -> Arc<CacheDb> {
        Arc::new(CacheDb::open_in_memory().unwrap())
    }

    fn make_store() -> Arc<dyn BlobStoreBackend> {
        Arc::new(MemBlobStore::new())
    }

    /// The actor's website folder, created on first use: website-enabled +
    /// `public` audience — the shape a really-serving site has (mirrors
    /// serve.rs's `website_folder`). the render pipeline asks the
    /// folder gate on every input row, so a template keyed to a dangling
    /// `folder_id` is a template the renderer rightly refuses — the old
    /// hardcoded `Some(1)` seeded exactly that shape.
    async fn site_folder(db: &Arc<CacheDb>, actor: &[u8; 32]) -> i64 {
        if let Some(fs) = db.get_folder_for_actor("site", actor).await.unwrap() {
            return fs.id;
        }
        let id = db
            .create_folder_with_options("site", actor, Default::default())
            .await
            .unwrap();
        db.update_folder_for_user(
            "site",
            actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                audience: Some(Some("public")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        id
    }

    /// Seed a synced web file the way the sync ingest really does — a chunked,
    /// at-rest-framed manifest, NOT a raw blob (the old helper wrote raw bytes,
    /// which is what let the render pipeline's "reads garbage" bug hide) —
    /// keyed to a real live website folder (see [`site_folder`]).
    async fn store_web_file(
        db: &Arc<CacheDb>,
        blob_store: &Arc<dyn BlobStoreBackend>,
        actor: &[u8; 32],
        path: &str,
        content: &[u8],
        content_type: &str,
    ) {
        let folder_id = site_folder(db, actor).await;
        let manifest_hash =
            crate::web_content::file_bytes::seed_synced_file(blob_store, None, content, None).await;
        db.upsert_web_file(
            actor,
            path,
            &manifest_hash,
            content_type,
            Some(folder_id),
            None,
        )
        .await
        .unwrap();
    }

    fn make_post(slug: &str, title: &str) -> PostContext {
        PostContext {
            title: title.to_string(),
            slug: slug.to_string(),
            content: format!("<p>Content for {slug}</p>"),
            created_at: 1_700_000_000,
            tags: vec![],
            next: None,
            prev: None,
            paywall: None,
        }
    }

    // ---- Tests ----

    #[tokio::test]
    async fn render_and_store_hbs_files() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        // Store a site context.
        let site_json = r#"{"title":"Test Site","description":"A test"}"#;
        store_web_file(
            &db,
            &store,
            &actor,
            "_site.json",
            site_json.as_bytes(),
            "application/json",
        )
        .await;

        // Store a simple index template.
        let template = b"<html><title>{{site.title}}</title><body>{{#each posts}}<p>{{this.title}}</p>{{/each}}</body></html>";
        store_web_file(
            &db,
            &store,
            &actor,
            "index.html.hbs",
            template,
            "text/plain",
        )
        .await;

        let svc = WebContentService::new(db.clone(), store.clone());
        let posts = vec![make_post("hello", "Hello World")];
        let _ = svc.render_for_actor_now(&actor, &posts, &[]).await.unwrap();

        // Verify `index.html` exists in web_rendered.
        let row = db
            .get_web_rendered(&actor, "index.html")
            .await
            .unwrap()
            .expect("index.html should be rendered");

        // Fetch the blob and verify content.
        let mut hash_arr = [0u8; 32];
        hash_arr.copy_from_slice(&row.blob_hash);
        let bytes = store
            .get(&ContentHash::from_digest_raw(hash_arr))
            .await
            .unwrap()
            .expect("blob should exist");
        let html = String::from_utf8(bytes).unwrap();

        assert!(
            html.contains("Test Site"),
            "site title should appear in output"
        );
        assert!(html.contains("Hello World"), "post title should appear");
        assert_eq!(row.content_type, "text/html");
    }

    #[tokio::test]
    async fn render_per_post_pages() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        // Store a per-post template.
        let template = b"<article><h1>{{post.title}}</h1>{{{post.content}}}</article>";
        store_web_file(
            &db,
            &store,
            &actor,
            "_post.html.hbs",
            template,
            "text/plain",
        )
        .await;

        let svc = WebContentService::new(db.clone(), store.clone());
        let posts = vec![
            make_post("slug1", "Post One"),
            make_post("slug2", "Post Two"),
        ];
        let _ = svc.render_for_actor_now(&actor, &posts, &[]).await.unwrap();

        // Both per-post pages should exist.
        let row1 = db
            .get_web_rendered(&actor, "post/slug1.html")
            .await
            .unwrap()
            .expect("post/slug1.html should exist");
        let row2 = db
            .get_web_rendered(&actor, "post/slug2.html")
            .await
            .unwrap()
            .expect("post/slug2.html should exist");

        // Verify the content of the first post's page.
        let mut hash_arr = [0u8; 32];
        hash_arr.copy_from_slice(&row1.blob_hash);
        let bytes = store
            .get(&ContentHash::from_digest_raw(hash_arr))
            .await
            .unwrap()
            .unwrap();
        let html1 = String::from_utf8(bytes).unwrap();
        assert!(html1.contains("Post One"), "first post title should appear");
        assert!(
            !html1.contains("Post Two"),
            "second post title should NOT appear in first page"
        );

        // Verify the second post's page.
        let mut hash_arr2 = [0u8; 32];
        hash_arr2.copy_from_slice(&row2.blob_hash);
        let bytes2 = store
            .get(&ContentHash::from_digest_raw(hash_arr2))
            .await
            .unwrap()
            .unwrap();
        let html2 = String::from_utf8(bytes2).unwrap();
        assert!(
            html2.contains("Post Two"),
            "second post title should appear"
        );

        assert_eq!(row1.content_type, "text/html");
        assert_eq!(row2.content_type, "text/html");
    }

    #[tokio::test]
    async fn render_generates_rss() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        let svc = WebContentService::new(db.clone(), store.clone());
        let posts = vec![
            make_post("first", "First Post"),
            make_post("second", "Second Post"),
        ];
        let _ = svc.render_for_actor_now(&actor, &posts, &[]).await.unwrap();

        // feed.xml should have been created.
        let row = db
            .get_web_rendered(&actor, "feed.xml")
            .await
            .unwrap()
            .expect("feed.xml should exist");

        let mut hash_arr = [0u8; 32];
        hash_arr.copy_from_slice(&row.blob_hash);
        let bytes = store
            .get(&ContentHash::from_digest_raw(hash_arr))
            .await
            .unwrap()
            .expect("feed.xml blob should exist");
        let xml = String::from_utf8(bytes).unwrap();

        assert!(xml.starts_with("<?xml"), "should be valid XML");
        assert!(xml.contains("<rss"), "should be RSS");
        assert!(xml.contains("first"), "first post slug should appear");
        assert!(xml.contains("second"), "second post slug should appear");
        assert!(xml.contains("First Post"), "first post title should appear");
    }

    #[tokio::test]
    async fn no_rss_when_no_posts() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        let svc = WebContentService::new(db.clone(), store.clone());
        let _ = svc.render_for_actor_now(&actor, &[], &[]).await.unwrap();

        // feed.xml should NOT have been created.
        let row = db.get_web_rendered(&actor, "feed.xml").await.unwrap();
        assert!(
            row.is_none(),
            "feed.xml should not exist when there are no posts"
        );
    }

    #[tokio::test]
    async fn default_templates_used_when_no_hbs() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        // No .hbs files stored — only posts provided.
        let svc = WebContentService::new(db.clone(), store.clone());
        let posts = vec![make_post("my-first-post", "My First Post")];
        let _ = svc.render_for_actor_now(&actor, &posts, &[]).await.unwrap();

        // index.html should exist (from default index template).
        let index_row = db
            .get_web_rendered(&actor, "index.html")
            .await
            .unwrap()
            .expect("index.html should be rendered using default template");

        let mut hash_arr = [0u8; 32];
        hash_arr.copy_from_slice(&index_row.blob_hash);
        let index_bytes = store
            .get(&ContentHash::from_digest_raw(hash_arr))
            .await
            .unwrap()
            .expect("index.html blob should exist");
        let index_html = String::from_utf8(index_bytes).unwrap();
        assert!(
            index_html.contains("My First Post"),
            "index.html should contain the post title"
        );
        assert!(
            index_html.contains("/post/my-first-post"),
            "index.html should link to /post/{{slug}}"
        );

        // post/my-first-post.html should exist (from default post template).
        let post_row = db
            .get_web_rendered(&actor, "post/my-first-post.html")
            .await
            .unwrap()
            .expect("post/my-first-post.html should be rendered using default template");

        let mut hash_arr2 = [0u8; 32];
        hash_arr2.copy_from_slice(&post_row.blob_hash);
        let post_bytes = store
            .get(&ContentHash::from_digest_raw(hash_arr2))
            .await
            .unwrap()
            .expect("post page blob should exist");
        let post_html = String::from_utf8(post_bytes).unwrap();
        assert!(
            post_html.contains("My First Post"),
            "post page should contain post title"
        );
        assert!(
            post_html.contains("href=\"/\""),
            "post page should have a link back to home"
        );
    }

    #[tokio::test]
    async fn clear_before_render() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        // Pre-populate a stale rendered entry, as an earlier render left it.
        let stale_hash = [0xdeu8; 32];
        let earlier = db.begin_web_render(&actor).await.unwrap();
        db.upsert_web_rendered(&earlier, "stale.html", &stale_hash, "text/html")
            .await
            .unwrap();

        let template = b"<p>hello</p>";
        store_web_file(&db, &store, &actor, "page.html.hbs", template, "text/plain").await;

        let svc = WebContentService::new(db.clone(), store.clone());
        let _ = svc.render_for_actor_now(&actor, &[], &[]).await.unwrap();

        // Stale entry should be gone.
        let stale = db.get_web_rendered(&actor, "stale.html").await.unwrap();
        assert!(
            stale.is_none(),
            "stale rendered entry should have been cleared"
        );

        // New entry should exist.
        assert!(
            db.get_web_rendered(&actor, "page.html")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn render_published_posts_renders_default_index() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;

        let db = make_db();
        let store = make_store();
        let actor = actor();

        // Store a published text post (markdown body: a title line + bold + a tag).
        let post = Post {
            author: ActorId(actor),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "My Apex Home\n\nWelcome to my **site**. #hello".into(),
                facets: vec![Facet {
                    byte_start: 0,
                    byte_end: 0,
                    feature: FacetFeature::Tag {
                        name: "Hello".into(),
                    },
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        db.publish_web_post(&actor, &post_id, "apex-home")
            .await
            .unwrap();

        let svc = WebContentService::new(db.clone(), store.clone());
        svc.render_published_posts(&actor).await.unwrap();

        // A default index.html renders (no `.html.hbs` needed) with the derived
        // title and a link to the post by slug.
        let index = read_rendered(&db, &store, &actor, "index.html").await;
        assert!(
            index.contains("My Apex Home"),
            "derived title should appear: {index}"
        );
        assert!(
            index.contains("/post/apex-home"),
            "index should link to the post by slug: {index}"
        );

        // The per-post page renders the markdown body to HTML.
        let page = read_rendered(&db, &store, &actor, "post/apex-home.html").await;
        assert!(
            page.contains("<strong>site</strong>"),
            "markdown bold should render to HTML: {page}"
        );

        // Unpublishing the last post re-renders an empty site (whole-site replacement).
        db.unpublish_web_post(&actor, &post_id).await.unwrap();
        svc.render_published_posts(&actor).await.unwrap();
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "unpublishing the last post should clear the default index"
        );
    }

    /// **A post a moderation flag withholds never reaches the web site**
    /// (`moderation.md` § Legal takedown → *Posts*). The render reads bodies
    /// through the flag-blind `load_post_body`, so its enumeration is the gate;
    /// until 2026-09-10 it had none, and a taken-down, quarantined or
    /// suppressed post that was still published rendered to a public page, the
    /// index and `feed.xml` like any other.
    ///
    /// One published post per arm plus a clean control, each asserted on its
    /// own page and on the index/feed, so reverting any one arm reddens exactly
    /// its own lines.
    #[tokio::test]
    async fn a_flagged_published_post_never_renders() {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;

        let db = make_db();
        let store = make_store();
        let actor = actor();

        let mut ids = Vec::new();
        for (i, slug) in ["clean", "taken", "quarantined", "suppressed"]
            .iter()
            .enumerate()
        {
            let post = Post {
                author: ActorId(actor),
                created_at: Timestamp(1_700_000_000_000_000 + i as u64),
                body: PostBody::Text {
                    content: format!("body-of-the-{slug}-post"),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin: None,
            };
            let data = canonical_encode(&post).unwrap();
            let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
            db.put_post(&post_id, &data, None).await.unwrap();
            db.publish_web_post(&actor, &post_id, slug).await.unwrap();
            ids.push(post_id);
        }
        db.set_post_legal_takedown(&ids[1], Some("EU-DSA-2024/911"))
            .await
            .unwrap();
        db.set_post_quarantined(&ids[2], true).await.unwrap();
        db.set_post_suppressed(&ids[3], true).await.unwrap();

        let svc = WebContentService::new(db.clone(), store.clone());
        svc.render_published_posts(&actor).await.unwrap();

        assert!(
            db.get_web_rendered(&actor, "post/clean.html")
                .await
                .unwrap()
                .is_some(),
            "control: the unflagged post renders"
        );
        let index = read_rendered(&db, &store, &actor, "index.html").await;
        let feed = read_rendered(&db, &store, &actor, "feed.xml").await;
        for (slug, arm) in [
            ("taken", "a TAKEN-DOWN"),
            ("quarantined", "a QUARANTINED"),
            ("suppressed", "a SUPPRESSED"),
        ] {
            assert!(
                db.get_web_rendered(&actor, &format!("post/{slug}.html"))
                    .await
                    .unwrap()
                    .is_none(),
                "{arm} post must not render a public page"
            );
            let body = format!("body-of-the-{slug}-post");
            assert!(
                !index.contains(&body) && !index.contains(&format!("/post/{slug}")),
                "{arm} post must not appear on the index: {index}"
            );
            assert!(
                !feed.contains(&body),
                "{arm} post must not appear in feed.xml: {feed}"
            );
        }
    }

    /// a folder's templates stop feeding renders the
    /// moment the folder stops being a live website source — the toggle
    /// leaving, or the audience leaving `public` — because the render
    /// pipeline is a producer of bytes from `web_files` rows and asks the
    /// same folder gate as the serve doors (`web-content-hosting.md`
    /// § Static site files). Without the gate, a re-render RE-CONSUMES the
    /// disabled folder's templates and rebuilds the very pages the
    /// transition was meant to drop.
    #[tokio::test]
    async fn disabled_folder_templates_stop_feeding_renders() {
        let db = make_db();
        let store = make_store();
        let actor = actor();

        store_web_file(
            &db,
            &store,
            &actor,
            "index.html.hbs",
            b"<html><body>canary-template-marker</body></html>",
            "text/plain",
        )
        .await;

        let svc = WebContentService::new(db.clone(), store.clone());
        let _ = svc.render_for_actor_now(&actor, &[], &[]).await.unwrap();
        let index = read_rendered(&db, &store, &actor, "index.html").await;
        assert!(
            index.contains("canary-template-marker"),
            "baseline: the template renders while its folder is live: {index}"
        );

        // The owner switches the website off; the next render must not
        // re-consume the disabled folder's template.
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let _ = svc.render_for_actor_now(&actor, &[], &[]).await.unwrap();
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "TOGGLE-OFF: the disabled folder's template kept feeding the render"
        );

        // Toggle back on but no longer public: a plaintext template whose
        // folder left the public audience is not a render input either —
        // on the render plane.
        db.update_folder_for_user(
            "site",
            &actor,
            crate::db::FolderUpdate {
                website_enabled: Some(true),
                audience: Some(Some("private")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let _ = svc.render_for_actor_now(&actor, &[], &[]).await.unwrap();
        assert!(
            db.get_web_rendered(&actor, "index.html")
                .await
                .unwrap()
                .is_none(),
            "AUDIENCE-FLIP: a no-longer-public folder's template kept feeding the render"
        );
    }

    /// Build + store + publish a gated post (preview body public, full body
    /// sealed under `derive_post_key(period_key, seal_id)` in the blob store)
    /// plus its tier row. Returns the post_id.
    async fn seed_gated_published_post(
        db: &Arc<CacheDb>,
        store: &Arc<dyn BlobStoreBackend>,
        actor: &[u8; 32],
        slug: &str,
        tier: &str,
        period_key: &[u8; 32],
    ) -> [u8; 32] {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        use fauna_core::subscription::types::KeyAccess;

        db.create_subscription_tier(
            actor,
            tier,
            2,
            None,
            Some("5 €/mo"),
            Some("https://pay.example/gold"),
            false,
            None,
            None,
            false,
        )
        .await
        .unwrap();

        // Full body, sealed to the tier period key under a random seal_id.
        let full_body = PostBody::Text {
            content: "Premium Post\n\nThe **full** premium content.".into(),
            facets: vec![],
        };
        let plain = canonical_encode(&full_body).unwrap();
        let seal_id = ContentHash::from_digest_raw([0xabu8; 32]);
        let key = derive_post_key(period_key, &seal_id);
        let sealed_blob = encrypt_content(&key, &plain);
        let encrypted_hash = *blake3::hash(&sealed_blob).as_bytes();
        store
            .put(&ContentHash::from_digest_raw(encrypted_hash), &sealed_blob)
            .await
            .unwrap();

        let post = Post {
            author: ActorId(*actor),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: "Premium Post\n\nA public teaser…".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(fauna_core::subscription::types::GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw(encrypted_hash),
                key_access: KeyAccess::Broadcast {
                    key_blob_ref: ContentHash::from_digest_raw([1u8; 32]),
                },
                tier: tier.to_string(),
                tier_rank: 2,
                seal_id,
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        db.publish_web_post(actor, &post_id, slug).await.unwrap();
        post_id
    }

    #[tokio::test]
    async fn gated_post_renders_teaser_without_grant_and_sealed_full_page_with_grant() {
        use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob};

        let db = make_db();
        let store = make_store();
        let actor = actor();
        let period_key = [42u8; 32];
        let dir = tempfile::tempdir().unwrap();
        let holder = crate::web_content::holder::WebServeHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .expect("holder inits");

        let post_id =
            seed_gated_published_post(&db, &store, &actor, "premium", "gold", &period_key).await;

        let svc =
            WebContentService::new(db.clone(), store.clone()).with_web_serve_holder(holder.clone());

        // ── No grant: the canonical page is the teaser; nothing sealed. ──
        svc.render_published_posts(&actor).await.unwrap();
        let teaser = read_rendered(&db, &store, &actor, "post/premium.html").await;
        assert!(
            teaser.contains("A public teaser"),
            "teaser preview: {teaser}"
        );
        assert!(teaser.contains("gold"), "paywall tier name: {teaser}");
        assert!(teaser.contains("5 €/mo"), "paywall price hint: {teaser}");
        assert!(
            teaser.contains("https://pay.example/gold"),
            "payment link: {teaser}"
        );
        assert!(
            !teaser.contains("full</strong> premium content"),
            "full content must NOT leak into the teaser: {teaser}"
        );
        assert!(
            db.get_web_rendered_sealed(&actor, "post/premium.html")
                .await
                .unwrap()
                .is_none(),
            "no grant → no sealed full page"
        );

        // ── Mint the web-serve holder a content.read{post:gold} grant → the
        // sealed full page materializes; the public page stays the teaser. ──
        let blob = build_grant_blob(
            &actor,
            &[7u8; 16],
            &holder.x25519_pubkey,
            Some(&holder.mlkem_ek),
            GrantWindow(0, u64::MAX),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_POST.to_string()),
                    tier: Some("gold".to_string()),
                    set: None,
                    factor: None,
                },
                Some(period_key.to_vec()),
            )],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
        db.put_capability_grant(&actor, &[7u8; 16], &holder.x25519_pubkey, i64::MAX, &blob)
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        svc.render_published_posts(&actor).await.unwrap();

        let teaser = read_rendered(&db, &store, &actor, "post/premium.html").await;
        assert!(
            !teaser.contains("full</strong> premium content"),
            "the canonical URL still serves the teaser under a grant"
        );
        let sealed_row = db
            .get_web_rendered_sealed(&actor, "post/premium.html")
            .await
            .unwrap()
            .expect("grant → sealed full page row");
        assert_eq!(sealed_row.tier, "gold");
        assert_eq!(sealed_row.post_id, post_id.to_vec());
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&sealed_row.blob_hash);
        let sealed_bytes = store
            .get(&ContentHash::from_digest_raw(hash))
            .await
            .unwrap()
            .expect("sealed blob exists");
        assert!(
            db.get_blob_metadata(&hash).await.unwrap().is_some(),
            "the sealed page's body carries its blob_metadata row — the sweep's world \
             (backup-restore.md § 9 step 2h)"
        );
        // The stored bytes are ciphertext; they open only under the
        // scope-derived key and contain the full content.
        assert!(
            !String::from_utf8_lossy(&sealed_bytes).contains("premium content"),
            "sealed page must not rest in plaintext"
        );
        let render_key = derive_web_render_key(&period_key, &ContentHash::from_digest_raw(post_id));
        let full_html =
            String::from_utf8(decrypt_content(&render_key, &sealed_bytes).unwrap()).unwrap();
        assert!(
            full_html.contains("<strong>full</strong> premium content"),
            "sealed page carries the full rendered body: {full_html}"
        );

        // ── Revoke (delete + refresh) → re-render clears the sealed page. ──
        db.delete_capability_grant(&actor, &[7u8; 16])
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        svc.render_published_posts(&actor).await.unwrap();
        assert!(
            db.get_web_rendered_sealed(&actor, "post/premium.html")
                .await
                .unwrap()
                .is_none(),
            "revoke → the sealed slice darkens on re-render"
        );
        let teaser = read_rendered(&db, &store, &actor, "post/premium.html").await;
        assert!(
            teaser.contains("A public teaser"),
            "ungated/teaser serving continues after revoke"
        );
    }

    // ---- The nest-as-publisher region fold (`region-blocking.md` § The
    // content plane → *The nest-as-publisher leg*) ----

    const REGION_AUTHORITY_SEED: u8 = 61;
    const REGION_REASON: &str = "Withheld under Fixture Act § 7 & <Schedule 2>.";

    fn region_no() -> fauna_core::region_authority::RegionCode {
        fauna_core::region_authority::RegionCode::parse("NO").unwrap()
    }

    /// A registry enrolling one synthetic authority for `NO` — the fixture the
    /// region conformance suite uses, never a real region.
    fn fixture_region_registry() -> fauna_core::region_authority::RegionRegistry {
        use fauna_core::region_authority::{AuthorityKey, RegionEntry, RegionRegistry};
        let key = ed25519_dalek::SigningKey::from_bytes(&[REGION_AUTHORITY_SEED; 32]);
        RegionRegistry {
            version: 1,
            regions: vec![RegionEntry {
                region: region_no(),
                authority_name: "Fixture Authority".into(),
                official_domain: "authority.example".into(),
                parent: None,
                keys: vec![AuthorityKey {
                    key_id: "k1".into(),
                    public_key: key.verifying_key().to_bytes().to_vec(),
                    enrolled_at: 0,
                    retired_at: None,
                }],
            }],
        }
    }

    fn region_rule(
        factor: &str,
        verdict: fauna_core::region_policy::ContentVerdict,
    ) -> fauna_core::region_policy::ContentRule {
        fauna_core::region_policy::ContentRule {
            factor: factor.into(),
            min_permille: 500,
            verdict,
            reason_code: "FX-7".into(),
            reason: std::collections::BTreeMap::from([(
                fauna_core::region_policy::REASON_DEFAULT_KEY.to_string(),
                REGION_REASON.to_string(),
            )]),
            extra: Default::default(),
        }
    }

    fn region_document(
        rules: Vec<fauna_core::region_policy::ContentRule>,
        scorers: Vec<fauna_core::region_policy::BundledScorer>,
    ) -> fauna_core::region_policy::ContentPolicyDocument {
        fauna_core::region_policy::ContentPolicyDocument {
            version: fauna_core::region_policy::GRAMMAR_VERSION,
            rules,
            scorers,
            extra: Default::default(),
        }
    }

    /// Declare `NO` the situs and store `document` as the relay's verified
    /// `(NO, content-policy)` cell — the state the region tier's worker leaves
    /// after accepting the authority's artifact.
    async fn put_situs_content_policy(
        db: &Arc<CacheDb>,
        sequence: u64,
        document: &fauna_core::region_policy::ContentPolicyDocument,
    ) {
        use fauna_core::region_authority::{
            PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, sign_artifact, verify_artifact,
        };
        let artifact = sign_artifact(
            PolicyArtifact {
                region: region_no(),
                key_id: "k1".into(),
                sequence,
                issued_at: 1_000,
                payload_kind: PAYLOAD_KIND_CONTENT_POLICY.into(),
                payload: fauna_core::encoding::canonical_encode(document).unwrap(),
                sig: Vec::new(),
            },
            &ed25519_dalek::SigningKey::from_bytes(&[REGION_AUTHORITY_SEED; 32]),
        )
        .unwrap();
        let verified = verify_artifact(
            artifact,
            &fixture_region_registry(),
            crate::db::now_epoch_secs() as u64,
            None,
        )
        .unwrap();
        db.put_relay_artifact(&verified, None, false).await.unwrap();
        db.set_declared_region(&region_no(), false).await.unwrap();
    }

    /// Store + publish one ungated text post; returns its id.
    async fn seed_text_post(
        db: &Arc<CacheDb>,
        actor: &[u8; 32],
        slug: &str,
        text: &str,
    ) -> [u8; 32] {
        use fauna_core::data::*;
        use fauna_core::encoding::canonical_encode;
        use fauna_core::identity::ActorId;
        let post = Post {
            author: ActorId(*actor),
            created_at: Timestamp(1_700_000_000_000_000),
            body: PostBody::Text {
                content: text.into(),
                facets: vec![Facet {
                    byte_start: 0,
                    byte_end: 0,
                    feature: FacetFeature::Tag {
                        name: format!("tag{slug}"),
                    },
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        db.publish_web_post(actor, &post_id, slug).await.unwrap();
        post_id
    }

    /// Label a post the way the nest holds labels: `content_id` is the post
    /// id's lowercase hex.
    ///
    /// `scanner_id` is a real 32-byte id, not the empty blob the fixture used to
    /// pass: the publisher fold reads only rows that NAME THEIR WRITER
    /// (`db::feeds::ATTRIBUTED_LABEL`), so an unattributed fixture row would
    /// exercise nothing. `label_post_unattributed` is the deliberate inverse.
    async fn label_post(db: &Arc<CacheDb>, post_id: &[u8; 32], category: &str, confidence: f64) {
        label_post_from(db, post_id, category, confidence, &[0x5Au8; 32]).await;
    }

    /// The pre-attribution shape — a row naming no writer, as the class-only
    /// `fauna.labels.attach` door wrote for any member about anyone's post. The
    /// fold must ignore it.
    async fn label_post_unattributed(
        db: &Arc<CacheDb>,
        post_id: &[u8; 32],
        category: &str,
        confidence: f64,
    ) {
        label_post_from(db, post_id, category, confidence, &[0u8; 32]).await;
    }

    async fn label_post_from(
        db: &Arc<CacheDb>,
        post_id: &[u8; 32],
        category: &str,
        confidence: f64,
        scanner_id: &[u8; 32],
    ) {
        db.upsert_content_label(
            "post",
            &hex::encode(post_id),
            category,
            confidence,
            0,
            b"fixture-classifier",
            1,
            0,
            None,
            None,
            0,
            scanner_id,
            b"",
        )
        .await
        .unwrap();
    }

    /// **The publisher arm of the label-provenance finding.** The fold folds
    /// "the labels the nest already holds" (`region-blocking.md` § The
    /// nest-as-publisher leg) — and `fauna.labels.attach` was gated on the
    /// caller's class alone, so the label any member could write about anyone's
    /// post was the label an authority's rule fired on. It would have replaced
    /// the victim's public page, index entry and feed entry with the authority's
    /// placeholder, under the authority's name and reason, on a surface with no
    /// viewer and no reveal — making a member the lever invariant 5 reserves to
    /// the authority. A row that names no writer is therefore not in the set the
    /// fold reads: the body stays, everywhere.
    ///
    /// Its inverse is the test above, where the same 0.9 `nsfw` label under the
    /// same blocking document DOES block — the pair is what says this is a
    /// provenance rule and not a blinded fold.
    #[tokio::test]
    async fn an_unattributed_label_never_reaches_the_publisher_fold() {
        use fauna_core::region_policy::ContentVerdict;
        let db = make_db();
        let store = make_store();
        let actor = actor();
        let post = seed_text_post(&db, &actor, "opinion", "an-unattributed-body-6b3f").await;
        label_post_unattributed(&db, &post, "nsfw", 0.9).await;
        put_situs_content_policy(
            &db,
            1,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Block)], vec![]),
        )
        .await;

        let svc = WebContentService::new(db.clone(), store.clone())
            .with_region_registry(fixture_region_registry());
        svc.render_published_posts(&actor).await.unwrap();

        let page = read_rendered(&db, &store, &actor, "post/opinion.html").await;
        assert!(
            page.contains("an-unattributed-body-6b3f"),
            "an unattributed label blocks nothing — the body stays: {page}"
        );
        assert!(
            !page.contains("data-region-verdict=\"block\""),
            "and no verdict frame is rendered: {page}"
        );
        let index = read_rendered(&db, &store, &actor, "index.html").await;
        assert!(
            index.contains("an-unattributed-body-6b3f") || index.contains("opinion"),
            "the index entry is untouched: {index}"
        );
    }

    /// **A post the situs's policy blocks renders the reasoned placeholder on
    /// every page that carried it, and never its body** — and the rest of the
    /// site is untouched, and withdrawing the declaration brings the body back.
    #[tokio::test]
    async fn a_region_block_renders_the_reasoned_placeholder_in_place_of_the_post() {
        use fauna_core::region_policy::ContentVerdict;
        let db = make_db();
        let store = make_store();
        let actor = actor();
        let blocked = seed_text_post(&db, &actor, "blocked", "a-region-blocked-body-51c2").await;
        seed_text_post(&db, &actor, "clean", "a-clean-body-77a0").await;
        label_post(&db, &blocked, "nsfw", 0.9).await;
        put_situs_content_policy(
            &db,
            1,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Block)], vec![]),
        )
        .await;

        let svc = WebContentService::new(db.clone(), store.clone())
            .with_region_registry(fixture_region_registry());
        svc.render_published_posts(&actor).await.unwrap();

        let page = read_rendered(&db, &store, &actor, "post/blocked.html").await;
        assert!(
            page.contains("Not shown in NO — blocked under the policy of Fixture Authority"),
            "the frame names the region and its authority: {page}"
        );
        assert!(
            page.contains("Withheld under Fixture Act § 7 &amp; &lt;Schedule 2&gt;."),
            "the authority's reason, verbatim and escaped: {page}"
        );
        assert!(page.contains("data-region-verdict=\"block\""), "{page}");
        assert!(
            !page.contains("a-region-blocked-body-51c2"),
            "a blocked body never reaches its page: {page}"
        );
        let index = read_rendered(&db, &store, &actor, "index.html").await;
        let feed = read_rendered(&db, &store, &actor, "feed.xml").await;
        for (name, listing) in [("index", &index), ("feed", &feed)] {
            assert!(
                !listing.contains("a-region-blocked-body-51c2"),
                "a blocked body never reaches the {name}: {listing}"
            );
            assert!(
                listing.contains("a-clean-body-77a0"),
                "an unlabelled post is untouched in the {name}: {listing}"
            );
        }
        let clean = read_rendered(&db, &store, &actor, "post/clean.html").await;
        assert!(clean.contains("a-clean-body-77a0"), "{clean}");
        assert!(!clean.contains("fauna-region-notice"), "{clean}");

        // Withdrawing the declaration: the next render publishes the body.
        db.clear_declared_region(false).await.unwrap();
        svc.render_published_posts(&actor).await.unwrap();
        let page = read_rendered(&db, &store, &actor, "post/blocked.html").await;
        assert!(page.contains("a-region-blocked-body-51c2"), "{page}");
        assert!(!page.contains("fauna-region-notice"), "{page}");
    }

    /// **A collapse keeps the post one reveal away**: the notice is the
    /// `<summary>`, the reason sits under it, the body behind it.
    #[tokio::test]
    async fn a_region_collapse_puts_the_post_behind_a_reveal() {
        use fauna_core::region_policy::ContentVerdict;
        let db = make_db();
        let store = make_store();
        let actor = actor();
        let post = seed_text_post(&db, &actor, "collapsed", "a-collapsed-body-9e13").await;
        label_post(&db, &post, "nsfw", 0.6).await;
        put_situs_content_policy(
            &db,
            1,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Collapse)], vec![]),
        )
        .await;
        let svc = WebContentService::new(db.clone(), store.clone())
            .with_region_registry(fixture_region_registry());
        svc.render_published_posts(&actor).await.unwrap();

        let page = read_rendered(&db, &store, &actor, "post/collapsed.html").await;
        let summary = page
            .find("<summary class=\"fauna-region-notice-frame\">Hidden in NO under the policy of Fixture Authority — select to show</summary>")
            .unwrap_or_else(|| panic!("the reveal's summary is the frame: {page}"));
        let body = page
            .find("a-collapsed-body-9e13")
            .unwrap_or_else(|| panic!("a collapsed body is still on its page: {page}"));
        assert!(summary < body, "the body sits behind the reveal: {page}");
        assert!(page.contains("data-region-verdict=\"collapse\""), "{page}");
    }

    /// **A policy the nest cannot verify binds nothing**: the cell verified
    /// when stored, but against a registry that no longer enrols its
    /// authority (de-listed since) the render applies nothing — the
    /// relay's retirement then removes it, but no page waits for that.
    #[tokio::test]
    async fn a_policy_that_no_longer_verifies_binds_no_page() {
        use fauna_core::region_policy::ContentVerdict;
        let db = make_db();
        let store = make_store();
        let actor = actor();
        let post = seed_text_post(&db, &actor, "p", "a-body-under-a-delisted-policy").await;
        label_post(&db, &post, "nsfw", 0.9).await;
        put_situs_content_policy(
            &db,
            1,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Block)], vec![]),
        )
        .await;
        let svc = WebContentService::new(db.clone(), store.clone())
            .with_region_registry(fauna_core::region_authority::RegionRegistry::default());
        svc.render_published_posts(&actor).await.unwrap();
        let page = read_rendered(&db, &store, &actor, "post/p.html").await;
        assert!(page.contains("a-body-under-a-delisted-policy"), "{page}");
    }

    /// **Bundled scorers run over the public plaintext at render**, and their
    /// factor folds like any held label: a `list` names a post by id, a `wasm`
    /// module reads its text (the committed cat-detector fixture).
    #[tokio::test]
    async fn bundled_scorers_produce_the_factor_a_region_rule_reads() {
        use fauna_core::region_policy::{BundledScorer, ContentVerdict, ScorerKind};
        let db = make_db();
        let store = make_store();
        let actor = actor();
        let listed = seed_text_post(&db, &actor, "listed", "a-listed-body-0f4b").await;
        seed_text_post(&db, &actor, "cat", "I love my cat, body-4410").await;
        seed_text_post(&db, &actor, "plain", "the weather, body-2c7e").await;
        let list = BundledScorer {
            name: "listed".into(),
            kind: ScorerKind::List,
            bytes: fauna_core::scoring::build_list_artifact(None, vec![(listed, 900)]).unwrap(),
            extra: Default::default(),
        };
        let wasm = BundledScorer {
            name: "cats".into(),
            kind: ScorerKind::Wasm,
            bytes: include_bytes!("../../../../tests/e2e-unified/fixtures/labeler/cat_labeler.wat")
                .to_vec(),
            extra: Default::default(),
        };
        put_situs_content_policy(
            &db,
            1,
            &region_document(
                vec![
                    region_rule("region:NO/listed", ContentVerdict::Block),
                    region_rule("region:NO/cats", ContentVerdict::Block),
                ],
                vec![list, wasm],
            ),
        )
        .await;
        let svc = WebContentService::new(db.clone(), store.clone())
            .with_region_registry(fixture_region_registry());
        // Twice: the second render answers from the per-(post, scorer) cache
        // and must render exactly the same site.
        for _ in 0..2 {
            svc.render_published_posts(&actor).await.unwrap();
            let listed_page = read_rendered(&db, &store, &actor, "post/listed.html").await;
            assert!(!listed_page.contains("a-listed-body-0f4b"), "{listed_page}");
            assert!(listed_page.contains("fauna-region-notice"), "{listed_page}");
            let cat_page = read_rendered(&db, &store, &actor, "post/cat.html").await;
            assert!(!cat_page.contains("body-4410"), "{cat_page}");
            assert!(cat_page.contains("fauna-region-notice"), "{cat_page}");
            let plain_page = read_rendered(&db, &store, &actor, "post/plain.html").await;
            assert!(plain_page.contains("body-2c7e"), "{plain_page}");
            assert!(!plain_page.contains("fauna-region-notice"), "{plain_page}");
        }
    }

    /// **A blocked gated post's sealed full page is withheld with its public
    /// one** — the nest never even opens the full body — while a collapsed
    /// one's sealed page collapses the same way its teaser does.
    #[tokio::test]
    async fn a_region_verdict_reaches_the_sealed_full_page_too() {
        use fauna_core::region_policy::ContentVerdict;
        use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob};

        let db = make_db();
        let store = make_store();
        let actor = actor();
        let period_key = [42u8; 32];
        let dir = tempfile::tempdir().unwrap();
        let holder = crate::web_content::holder::WebServeHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .expect("holder inits");
        let post_id =
            seed_gated_published_post(&db, &store, &actor, "premium", "gold", &period_key).await;
        let blob = build_grant_blob(
            &actor,
            &[7u8; 16],
            &holder.x25519_pubkey,
            Some(&holder.mlkem_ek),
            GrantWindow(0, u64::MAX),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_POST.to_string()),
                    tier: Some("gold".to_string()),
                    set: None,
                    factor: None,
                },
                Some(period_key.to_vec()),
            )],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
        db.put_capability_grant(&actor, &[7u8; 16], &holder.x25519_pubkey, i64::MAX, &blob)
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        label_post(&db, &post_id, "nsfw", 0.9).await;
        let svc = WebContentService::new(db.clone(), store.clone())
            .with_web_serve_holder(holder.clone())
            .with_region_registry(fixture_region_registry());

        // Block: no sealed page, and the teaser is the notice with no paywall.
        put_situs_content_policy(
            &db,
            1,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Block)], vec![]),
        )
        .await;
        svc.render_published_posts(&actor).await.unwrap();
        assert!(
            db.get_web_rendered_sealed(&actor, "post/premium.html")
                .await
                .unwrap()
                .is_none(),
            "a blocked gated post renders no sealed full page, grant or not"
        );
        let teaser = read_rendered(&db, &store, &actor, "post/premium.html").await;
        assert!(teaser.contains("fauna-region-notice"), "{teaser}");
        assert!(!teaser.contains("A public teaser"), "{teaser}");
        assert!(
            !teaser.contains("https://pay.example/gold"),
            "nothing on a blocked page to subscribe to: {teaser}"
        );

        // Collapse (a newer document): the sealed full page exists and carries
        // the full body behind the same reveal.
        put_situs_content_policy(
            &db,
            2,
            &region_document(vec![region_rule("nsfw", ContentVerdict::Collapse)], vec![]),
        )
        .await;
        svc.render_published_posts(&actor).await.unwrap();
        let sealed_row = db
            .get_web_rendered_sealed(&actor, "post/premium.html")
            .await
            .unwrap()
            .expect("a collapsed gated post keeps its sealed full page");
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&sealed_row.blob_hash);
        let sealed_bytes = store
            .get(&ContentHash::from_digest_raw(hash))
            .await
            .unwrap()
            .expect("sealed blob exists");
        let render_key = derive_web_render_key(&period_key, &ContentHash::from_digest_raw(post_id));
        let full_html =
            String::from_utf8(decrypt_content(&render_key, &sealed_bytes).unwrap()).unwrap();
        let summary = full_html
            .find("<summary")
            .unwrap_or_else(|| panic!("the sealed page collapses too: {full_html}"));
        let body = full_html
            .find("<strong>full</strong> premium content")
            .unwrap_or_else(|| panic!("the full body is behind the reveal: {full_html}"));
        assert!(summary < body, "{full_html}");
    }

    /// Fetch a rendered path's blob bytes as a UTF-8 string (test helper).
    async fn read_rendered(
        db: &Arc<CacheDb>,
        store: &Arc<dyn BlobStoreBackend>,
        actor: &[u8; 32],
        path: &str,
    ) -> String {
        let row = db
            .get_web_rendered(actor, path)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{path} should be rendered"));
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&row.blob_hash);
        let bytes = store
            .get(&ContentHash::from_digest_raw(hash))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{path} blob should exist"));
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn derive_title_first_line_heading_and_fallback() {
        // First non-empty line, heading marker stripped.
        assert_eq!(derive_title("# Hello World\n\nbody", "slug"), "Hello World");
        assert_eq!(
            derive_title("plain first line\nsecond", "slug"),
            "plain first line"
        );
        // Leading blank lines are skipped, surrounding whitespace trimmed.
        assert_eq!(derive_title("\n\n  spaced  \n", "slug"), "spaced");
        // Empty / whitespace-only body falls back to the slug.
        assert_eq!(derive_title("", "my-slug"), "my-slug");
        assert_eq!(derive_title("   \n  ", "my-slug"), "my-slug");
        // Past the cap → truncated with an ellipsis (80 chars + '…').
        let long = "x".repeat(100);
        let title = derive_title(&long, "slug");
        assert_eq!(title.chars().count(), 81);
        assert!(title.ends_with('…'));
    }
}
