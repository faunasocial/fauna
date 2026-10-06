use std::time::{Duration, Instant};

use anyhow::Result;
use handlebars::template::{Template, TemplateElement, TemplateMapping};
use handlebars::{
    Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext, RenderErrorReason,
};
use pulldown_cmark::{Parser, html};
use serde::{Deserialize, Serialize};

/// Wall-clock budget for one template render (the "5 s per template" figure in
/// `web-content-hosting.md` § Routing/render's Safety limits), enforced INSIDE
/// the render thread by [`RenderBudget`] ([`inject_budget_checks`]). The caller
/// (`service.rs`) also runs the render under `spawn_blocking` + a timeout of the
/// same length, but that timeout only abandons the thread — a `spawn_blocking`
/// task cannot be cancelled — so without the in-thread stop a runaway template
/// would keep a blocking-pool thread spinning long after the timeout fired.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard cap on the bytes a single template render may emit.
///
/// User templates are attacker-controlled (`web-content-hosting.md` § Same-origin
/// security model), so a nested `{{#each posts}}{{#each posts}}…{{/each}}{{/each}}`
/// amplifies a bounded context
/// (≤[`MAX_RENDERED_POSTS`](super::service::MAX_RENDERED_POSTS) posts) into
/// unbounded output. Rendering through [`CappedWriter`] aborts the render the
/// instant output passes this cap, bounding storage. It is NOT the iteration
/// bound — a loop that prints nothing never trips it; [`RENDER_TIMEOUT`] is.
/// 4 MiB is generous for a static HTML page (a 1000-post index is ~hundreds of
/// KB) while killing output amplification near-instantly.
pub const MAX_RENDER_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Hard cap on a template's source size. It bounds parse time and memory, and
/// the work between two [`RENDER_TIMEOUT`] checks (one pass over one block
/// body — [`inject_budget_checks`]). A hand-written HTML template is tens of KB.
pub const MAX_TEMPLATE_BYTES: usize = 1024 * 1024;

/// Hard cap on the block tags (`{{#…}}`, `{{^…}}`, `{{else…}}`, raw-block
/// openers) in one template — a bound on its block NESTING that no quoting can
/// hide, since every opener counts wherever it appears. The Handlebars parser
/// recurses once per nested block, and a Rust stack overflow is not a panic: it
/// aborts the whole nest (`web-content-hosting.md` § Routing, render, serving →
/// Safety limits). [`MAX_TEMPLATE_BYTES`] alone admits ~100k nested blocks,
/// which overflows any thread; this cap keeps the parse far inside
/// [`RENDER_STACK_BYTES`]. A hand-written page uses tens.
pub const MAX_TEMPLATE_BLOCK_TAGS: usize = 1024;

/// Hard cap on the `(` / `[` / `{` inside one `{{…}}` expression — the bound on
/// the parser's recursion through nested subexpressions and literals (it
/// overflowed a 2 MiB stack at 300 nested subexpressions). Counted by
/// [`check_template_nesting`], which errs only toward counting too much. A
/// real expression uses a handful.
pub const MAX_EXPRESSION_NESTING: usize = 128;

/// Stack size of the thread every template render runs on. The render runs on
/// a thread of its own so the stack it gets does not depend on its caller (a
/// tokio blocking thread has 2 MiB), which is what makes [`RENDER_STACK_BUDGET`]
/// a real bound. Reserved, not committed: pages are touched only as used.
const RENDER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How much of [`RENDER_STACK_BYTES`] a render may use before [`RenderBudget`]
/// stops it — the guard against recursion no static check can see (an inline
/// partial calling itself, directly, mutually or by a computed name). The other
/// half is headroom for the frames between two checks (one level of a block or
/// partial, plus a bounded expression).
const RENDER_STACK_BUDGET: usize = 8 * 1024 * 1024;

/// Refuse, before parsing, a template whose nesting could overflow the parser's
/// stack: more than [`MAX_TEMPLATE_BLOCK_TAGS`] block tags, or an expression
/// holding more than [`MAX_EXPRESSION_NESTING`] `(` / `[` / `{`.
///
/// The parser is dependency code the render cannot instrument, so this bound is
/// lexical, and built to err only one way — counting MORE than the parser
/// nests, never less. Block tags are counted everywhere. Inside an expression
/// every opener counts and nothing is subtracted; the only judgement is where an
/// expression ENDS, and a `}}` ends one only when no reading of the text so far
/// could put it inside a string or `[…]` literal. Anything ambiguous — a
/// backslash, a quote inside brackets, a quote glued to a word — makes every
/// later expression one long one for the rest of the template (it can only
/// over-count). Comments (`{{!…}}`, `{{!--…--}}`) are skipped exactly as the
/// parser skips them.
fn check_template_nesting(src: &str) -> Result<()> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut block_tags = 0usize;
    let mut in_expr = false;
    let mut nesting = 0usize;
    let mut quote: Option<u8> = None;
    let mut brackets = 0usize;
    // Once set, no expression ends again.
    let mut ambiguous = false;
    while i < b.len() {
        if b[i..].starts_with(b"{{") {
            if is_block_tag(&b[i + 2..]) {
                block_tags += 1;
                if block_tags > MAX_TEMPLATE_BLOCK_TAGS {
                    anyhow::bail!("template has more than {MAX_TEMPLATE_BLOCK_TAGS} block tags");
                }
            }
            if !in_expr {
                let (open, close): (&[u8], &[u8]) = if b[i + 2..].starts_with(b"!--") {
                    (b"{{!--", b"--}}")
                } else {
                    (b"{{!", b"}}")
                };
                if b[i..].starts_with(open) {
                    i = find_bytes(b, i + open.len(), close).map_or(b.len(), |at| at + close.len());
                    continue;
                }
                in_expr = true;
                nesting = 0;
                quote = None;
                brackets = 0;
            }
            i += 2;
            continue;
        }
        if in_expr {
            let c = b[i];
            if matches!(c, b'(' | b'[' | b'{') {
                nesting += 1;
                if nesting > MAX_EXPRESSION_NESTING {
                    anyhow::bail!(
                        "template nests more than {MAX_EXPRESSION_NESTING} levels in one expression"
                    );
                }
            }
            match (quote, c) {
                (_, b'\\') => ambiguous = true,
                (Some(q), _) if c == q => {
                    quote = None;
                    // A closing quote runs straight into a word: not a string end
                    // every reading agrees on.
                    if b.get(i + 1)
                        .is_some_and(|n| !(n.is_ascii_whitespace() || b")}~".contains(n)))
                    {
                        ambiguous = true;
                    }
                }
                (Some(_), _) => {}
                (None, b'"' | b'\'' | b'`') => {
                    // A string opens only where a parameter can start, and a
                    // quote inside a `[…]` literal is that literal's text.
                    let prev = b[i - 1];
                    if brackets > 0 || !(prev.is_ascii_whitespace() || b"(={~".contains(&prev)) {
                        ambiguous = true;
                    }
                    quote = Some(c);
                }
                (None, b'[') => brackets += 1,
                (None, b']') => brackets = brackets.saturating_sub(1),
                _ => {}
            }
            if !ambiguous && quote.is_none() && brackets == 0 && b[i..].starts_with(b"}}") {
                in_expr = false;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    Ok(())
}

/// Whether the text after a `{{` opens a block level: a `#` / `^` / `else` tag
/// (past any `~` and whitespace) or a raw block (`{{{{`).
fn is_block_tag(rest: &[u8]) -> bool {
    let braces = rest.iter().take_while(|&&c| c == b'{').count();
    let tag = rest[braces..]
        .iter()
        .position(|&c| !(c == b'~' || c.is_ascii_whitespace()))
        .map_or(&[][..], |at| &rest[braces + at..]);
    braces >= 2 || tag.starts_with(b"#") || tag.starts_with(b"^") || tag.starts_with(b"else")
}

/// The first index at or after `from` where `needle` starts in `haystack`.
fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}

/// An address inside the calling frame — the render's stack position, compared
/// against the one recorded when the render thread started.
#[inline(never)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// A `std::io::Write` sink that buffers up to `cap` bytes and then fails every
/// further write with an error, so `Handlebars::render_to_write` aborts a runaway
/// render instead of building an unbounded string. See [`MAX_RENDER_OUTPUT_BYTES`].
struct CappedWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl CappedWriter {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }
}

impl std::io::Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + data.len() > self.cap {
            return Err(std::io::Error::other(format!(
                "render output exceeded {}-byte limit",
                self.cap
            )));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The name of the helper [`inject_budget_checks`] calls — reserved: a user
/// template naming it just calls the (output-free) check.
const BUDGET_HELPER: &str = "__fauna_render_budget";

/// The in-thread [`RENDER_TIMEOUT`] and [`RENDER_STACK_BUDGET`] check: fails the
/// render once `deadline` has passed or the render has used more than its
/// stack budget since `stack_base`, and writes nothing otherwise. This is the
/// cooperative stop that ends a runaway render inside its own thread — the
/// caller's timeout cannot, since a `spawn_blocking` task runs to completion,
/// and no timeout can stop a recursion that exhausts the stack in milliseconds.
struct RenderBudget {
    deadline: Instant,
    /// [`stack_position`] at the render thread's start.
    stack_base: usize,
}

impl HelperDef for RenderBudget {
    fn call<'reg: 'rc, 'rc>(
        &self,
        _h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc Context,
        _rc: &mut RenderContext<'reg, 'rc>,
        _out: &mut dyn Output,
    ) -> HelperResult {
        if Instant::now() >= self.deadline {
            return Err(RenderErrorReason::Other(
                "template render exceeded its wall-clock budget".to_string(),
            )
            .into());
        }
        if stack_position().abs_diff(self.stack_base) > RENDER_STACK_BUDGET {
            return Err(RenderErrorReason::Other(
                "template render nests too deeply (a partial that renders itself?)".to_string(),
            )
            .into());
        }
        Ok(())
    }
}

/// A compiled `{{__fauna_render_budget}}` call and its source mapping — the
/// element [`inject_budget_checks`] plants.
fn budget_check_element() -> Result<(TemplateElement, Option<TemplateMapping>)> {
    let mut t = Template::compile(&format!("{{{{{BUDGET_HELPER}}}}}"))?;
    let element = t
        .elements
        .pop()
        .ok_or_else(|| anyhow::anyhow!("empty budget check"))?;
    Ok((element, t.mapping.pop()))
}

/// Plant a [`BUDGET_HELPER`] call at the head of every block body (and every
/// `{{else}}` branch) in `template`, recursively. Handlebars keeps no iteration
/// counter and its built-in helpers cannot be wrapped, but every loop — `each`,
/// at any nesting — re-renders a block body per iteration, so a check at the
/// head of each body stops any render within one pass over one body (itself
/// bounded by [`MAX_TEMPLATE_BYTES`]) of its deadline. The same check bounds
/// the render's stack use ([`RENDER_STACK_BUDGET`]): every level of recursion
/// renders a body. Planted after parsing, so standalone-whitespace handling and
/// the built-ins' semantics are untouched. The top level is planted too — it
/// renders once, except that `{{> t}}` recurses into it by its registered name.
/// This walk's own recursion is bounded by [`MAX_TEMPLATE_BLOCK_TAGS`].
fn inject_budget_checks(
    template: &mut Template,
    check: &TemplateElement,
    check_mapping: &Option<TemplateMapping>,
) {
    for element in &mut template.elements {
        match element {
            TemplateElement::HelperBlock(ht) => {
                for body in [&mut ht.template, &mut ht.inverse].into_iter().flatten() {
                    inject_budget_checks(body, check, check_mapping);
                }
            }
            // `{{#*inline}}` / `{{#> partial}}` bodies: a partial that renders
            // itself recurses, checked once per level.
            TemplateElement::DecoratorBlock(dt) | TemplateElement::PartialBlock(dt) => {
                if let Some(body) = &mut dt.template {
                    inject_budget_checks(body, check, check_mapping);
                }
            }
            _ => {}
        }
    }
    // Keep `mapping` parallel to `elements` (it locates render errors).
    if let Some(m) = check_mapping
        && template.mapping.len() == template.elements.len()
    {
        template.mapping.insert(0, m.clone());
    }
    template.elements.insert(0, check.clone());
}

/// Convert a Markdown string to HTML using pulldown-cmark.
pub fn markdown_to_html(markdown: &str) -> String {
    let parser = Parser::new(markdown);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

/// Site-level metadata, sourced from a JSON blob stored on the nest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteContext {
    pub title: String,
    pub description: String,
}

/// Per-post data passed to templates.
#[derive(Debug, Clone, Serialize)]
pub struct PostContext {
    pub title: String,
    pub slug: String,
    /// HTML content (already rendered from Markdown). For a paywalled post
    /// this is the public **preview** everywhere except the sealed full-page
    /// render (`monetization.md` § Pillar 2) — the full body must never reach
    /// an index/RSS/teaser template context.
    pub content: String,
    /// Unix timestamp (seconds).
    pub created_at: i64,
    pub tags: Vec<String>,
    /// Slug of the next post (chronologically), if any.
    pub next: Option<String>,
    /// Slug of the previous post (chronologically), if any.
    pub prev: Option<String>,
    /// Present iff the post is gated to a subscription tier — the teaser
    /// page's paywall box (tier + price + payment link). Templates read it as
    /// `{{post.paywall.tier}}` etc.; absent for ungated posts so existing
    /// templates see no change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paywall: Option<PaywallContext>,
}

/// The paywall metadata a teaser template renders (web paywall, Pillar 2):
/// the gating tier plus the creator's informational price/payment-link
/// strings from the tier definition (both optional there).
#[derive(Debug, Clone, Serialize)]
pub struct PaywallContext {
    pub tier: String,
    pub price_hint: Option<String>,
    pub payment_url: Option<String>,
}

/// Top-level data structure passed to every Handlebars template render.
#[derive(Debug, Serialize)]
pub struct TemplateData {
    pub site: SiteContext,
    /// All posts (used on index/tag pages).
    pub posts: Vec<PostContext>,
    /// The single post being rendered (used on post pages).
    pub post: Option<PostContext>,
}

/// Build a `SiteContext` from an optional JSON blob.
///
/// If `site_json` is `Some` and parses successfully, its values are used.
/// Otherwise the fallback name/bio are used.
pub fn build_site_context(
    site_json: Option<&str>,
    fallback_name: &str,
    fallback_bio: &str,
) -> SiteContext {
    if let Some(json) = site_json
        && let Ok(ctx) = serde_json::from_str::<SiteContext>(json)
    {
        return ctx;
    }
    SiteContext {
        title: fallback_name.to_string(),
        description: fallback_bio.to_string(),
    }
}

/// Render a Handlebars template string with the provided `TemplateData`.
///
/// Uses non-strict mode so that missing variables produce empty strings
/// rather than errors. Output is bounded at [`MAX_RENDER_OUTPUT_BYTES`] via
/// [`CappedWriter`]: a template that amplifies the bounded context into runaway
/// output (a nested `{{#each}}`) is aborted with an error rather than allowed to
/// exhaust memory. A loop that prints nothing is stopped instead by the
/// [`RENDER_TIMEOUT`] deadline, checked inside the render at every pass over a
/// block body ([`inject_budget_checks`]), and a template over
/// [`MAX_TEMPLATE_BYTES`] is refused
/// before parsing. The caller (`service.rs`) additionally runs the render under
/// `spawn_blocking` + a timeout so it can never pin a tokio worker.
pub fn render_template(template_str: &str, data: &TemplateData) -> Result<String> {
    render_data(template_str, data)
}

/// [`render_template`] over any serializable context — the same Handlebars
/// setup and output cap, for the contexts that are not a full page render (the
/// folder paywall teaser).
pub fn render_data<T: Serialize + Sync>(template_str: &str, data: &T) -> Result<String> {
    render_data_within(template_str, data, RENDER_TIMEOUT)
}

/// [`render_data`] with an explicit wall-clock `budget`, measured from the call
/// (tests pass a short one).
///
/// Parses and renders on a thread of its own with a [`RENDER_STACK_BYTES`]
/// stack, joined before returning: a template that nests or recurses too deep
/// must fail this call, and a stack overflow cannot — it aborts the process.
fn render_data_within<T: Serialize + Sync>(
    template_str: &str,
    data: &T,
    budget: Duration,
) -> Result<String> {
    let deadline = Instant::now() + budget;
    if template_str.len() > MAX_TEMPLATE_BYTES {
        anyhow::bail!("template exceeds {MAX_TEMPLATE_BYTES}-byte limit");
    }
    check_template_nesting(template_str)?;
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("web-render".to_string())
            .stack_size(RENDER_STACK_BYTES)
            .spawn_scoped(s, || render_on_this_thread(template_str, data, deadline))?
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("template render panicked")))
    })
}

/// The body of [`render_data_within`], run on its render thread.
fn render_on_this_thread<T: Serialize>(
    template_str: &str,
    data: &T,
    deadline: Instant,
) -> Result<String> {
    let stack_base = stack_position();
    let mut template = Template::compile(template_str)?;
    let (check, check_mapping) = budget_check_element()?;
    inject_budget_checks(&mut template, &check, &check_mapping);
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    hb.register_helper(
        BUDGET_HELPER,
        Box::new(RenderBudget {
            deadline,
            stack_base,
        }),
    );
    hb.register_template("t", template);
    let mut writer = CappedWriter::new(MAX_RENDER_OUTPUT_BYTES);
    hb.render_to_write("t", data, &mut writer)?;
    let rendered = String::from_utf8(writer.buf)?;
    Ok(rendered)
}

/// Decode template bytes as UTF-8, then render with `render_template`.
pub fn render_single_template(
    template_bytes: &[u8],
    site: SiteContext,
    posts: Vec<PostContext>,
    post: Option<PostContext>,
) -> Result<String> {
    let template_str = std::str::from_utf8(template_bytes)?;
    let data = TemplateData { site, posts, post };
    render_template(template_str, &data)
}

// ==================== Default built-in templates ====================

const DEFAULT_INDEX_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{{site.title}}</title>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 650px; margin: 2rem auto; padding: 0 1rem; color: #222; }
    h1 { margin-bottom: 0.25rem; }
    .description { color: #555; margin-top: 0; margin-bottom: 2rem; }
    .post-list { list-style: none; padding: 0; }
    .post-list li { margin-bottom: 1rem; }
    .post-list a { font-size: 1.1rem; text-decoration: none; color: #0066cc; }
    .post-list a:hover { text-decoration: underline; }
    .post-date { color: #888; font-size: 0.85rem; margin-left: 0.5rem; }
  </style>
</head>
<body>
  <h1>{{site.title}}</h1>
  {{#if site.description}}<p class="description">{{site.description}}</p>{{/if}}
  <ul class="post-list">
    {{#each posts}}
    <li>
      <a href="/post/{{this.slug}}">{{this.title}}</a>
    </li>
    {{/each}}
  </ul>
</body>
</html>"#;

const DEFAULT_POST_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{{post.title}} — {{site.title}}</title>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 650px; margin: 2rem auto; padding: 0 1rem; color: #222; }
    h1 { margin-bottom: 0.25rem; }
    .meta { color: #888; font-size: 0.85rem; margin-bottom: 2rem; }
    .content { line-height: 1.7; }
    .nav { margin-top: 2.5rem; display: flex; justify-content: space-between; font-size: 0.9rem; }
    .nav a { color: #0066cc; text-decoration: none; }
    .nav a:hover { text-decoration: underline; }
  </style>
</head>
<body>
  <h1>{{post.title}}</h1>
  <div class="meta">{{post.created_at}}</div>
  <div class="content">{{{post.content}}}</div>
  <div class="nav">
    {{#if post.prev}}<a href="/post/{{post.prev}}">&larr; Previous</a>{{else}}<span></span>{{/if}}
    <a href="/">Home</a>
    {{#if post.next}}<a href="/post/{{post.next}}">Next &rarr;</a>{{else}}<span></span>{{/if}}
  </div>
</body>
</html>"#;

/// The built-in paywall/teaser page for a gated post (web paywall, Pillar 2):
/// the public preview plus the paywall box — tier, optional price hint, and
/// the creator's payment link. Pre-rendered static output like every other
/// page; the full content is never in this template's context.
const DEFAULT_PAYWALL_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{{post.title}} — {{site.title}}</title>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 650px; margin: 2rem auto; padding: 0 1rem; color: #222; }
    h1 { margin-bottom: 0.25rem; }
    .meta { color: #888; font-size: 0.85rem; margin-bottom: 2rem; }
    .content { line-height: 1.7; }
    .paywall { margin-top: 2rem; padding: 1.25rem; border: 1px solid #ddd; border-radius: 8px; background: #fafafa; }
    .paywall h2 { margin-top: 0; font-size: 1.1rem; }
    .paywall .price { font-weight: 600; }
    .paywall a.subscribe { display: inline-block; margin-top: 0.75rem; padding: 0.5rem 1rem; background: #0066cc; color: #fff; text-decoration: none; border-radius: 6px; }
    .nav { margin-top: 2.5rem; font-size: 0.9rem; }
    .nav a { color: #0066cc; text-decoration: none; }
  </style>
</head>
<body>
  <h1>{{post.title}}</h1>
  <div class="meta">{{post.created_at}}</div>
  <div class="content">{{{post.content}}}</div>
  <div class="paywall">
    <h2>Subscribers only</h2>
    <p>The full post is available to <strong>{{post.paywall.tier}}</strong> subscribers.{{#if post.paywall.price_hint}} <span class="price">{{post.paywall.price_hint}}</span>{{/if}}</p>
    {{#if post.paywall.payment_url}}<a class="subscribe" href="{{post.paywall.payment_url}}">Subscribe</a>{{/if}}
  </div>
  <div class="nav"><a href="/">Home</a></div>
</body>
</html>"#;

/// Render the built-in paywall/teaser page for one gated post.
pub fn render_default_paywall(
    site: &SiteContext,
    posts: &[PostContext],
    post: &PostContext,
) -> Result<String> {
    let data = TemplateData {
        site: site.clone(),
        posts: posts.to_vec(),
        post: Some(post.clone()),
    };
    render_template(DEFAULT_PAYWALL_TEMPLATE, &data)
}

/// The teaser for a **paywalled `web` folder** (monetization.md § Pillar 2,
/// the folder half): what an unentitled visitor gets in place of a sealed
/// static file. Post-shaped teasers preview the content; a folder has no
/// preview to show (its bytes are the product), so this states the tier and
/// where to subscribe, and nothing else. Rendered at serve time — a set has no
/// pre-rendered per-path artifact the way a published post does.
const DEFAULT_FOLDER_PAYWALL_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Subscribers only — {{site.title}}</title>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 650px; margin: 2rem auto; padding: 0 1rem; color: #222; }
    h1 { margin-bottom: 0.25rem; font-size: 1.4rem; }
    .path { color: #888; font-size: 0.85rem; margin-bottom: 2rem; font-family: ui-monospace, monospace; }
    .paywall { margin-top: 2rem; padding: 1.25rem; border: 1px solid #ddd; border-radius: 8px; background: #fafafa; }
    .paywall h2 { margin-top: 0; font-size: 1.1rem; }
    .paywall .price { font-weight: 600; }
    .paywall a.subscribe { display: inline-block; margin-top: 0.75rem; padding: 0.5rem 1rem; background: #0066cc; color: #fff; text-decoration: none; border-radius: 6px; }
    .nav { margin-top: 2.5rem; font-size: 0.9rem; }
    .nav a { color: #0066cc; text-decoration: none; }
  </style>
</head>
<body>
  <h1>Subscribers only</h1>
  <div class="path">{{path}}</div>
  <div class="paywall">
    <h2>This file is for subscribers</h2>
    <p>It is available to <strong>{{paywall.tier}}</strong> subscribers.{{#if paywall.price_hint}} <span class="price">{{paywall.price_hint}}</span>{{/if}}</p>
    {{#if paywall.payment_url}}<a class="subscribe" href="{{paywall.payment_url}}">Subscribe</a>{{/if}}
  </div>
  <div class="nav"><a href="/">Home</a></div>
</body>
</html>"#;

/// The context the folder teaser renders (no post — see the template doc).
#[derive(Debug, Serialize)]
struct FolderPaywallData {
    site: SiteContext,
    paywall: PaywallContext,
    path: String,
}

/// Render the built-in teaser for a sealed file in a paywalled `web` folder.
pub fn render_folder_paywall(
    site: &SiteContext,
    paywall: &PaywallContext,
    path: &str,
) -> Result<String> {
    let data = FolderPaywallData {
        site: site.clone(),
        paywall: paywall.clone(),
        path: path.to_string(),
    };
    render_data(DEFAULT_FOLDER_PAYWALL_TEMPLATE, &data)
}

/// Render the built-in default index page (site title, description, post list).
///
/// Each post links to `/post/{slug}`.
pub fn render_default_index(site: &SiteContext, posts: &[PostContext]) -> Result<String> {
    let data = TemplateData {
        site: site.clone(),
        posts: posts.to_vec(),
        post: None,
    };
    render_template(DEFAULT_INDEX_TEMPLATE, &data)
}

/// Render the built-in default post page (title, date, content, prev/next nav).
pub fn render_default_post(
    site: &SiteContext,
    posts: &[PostContext],
    post: &PostContext,
) -> Result<String> {
    let data = TemplateData {
        site: site.clone(),
        posts: posts.to_vec(),
        post: Some(post.clone()),
    };
    render_template(DEFAULT_POST_TEMPLATE, &data)
}

/// Escape a string for safe inclusion in XML/RSS.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Generate a minimal RSS 2.0 feed for the given site and posts.
///
/// `base_url` should be the full origin, e.g. `https://alice.fauna.social`.
pub fn generate_rss(site: &SiteContext, posts: &[PostContext], base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');

    let mut items = String::new();
    for post in posts {
        let link = format!("{}/posts/{}", base_url, xml_escape(&post.slug));
        items.push_str(&format!(
            "    <item>\n\
                   <title>{}</title>\n\
                   <link>{}</link>\n\
                   <guid>{}</guid>\n\
                   <pubDate>{}</pubDate>\n\
                   <description>{}</description>\n\
                 </item>\n",
            xml_escape(&post.title),
            link,
            link,
            // RFC 822 date — simple epoch-based formatting
            rfc822_date(post.created_at),
            xml_escape(&post.content),
        ));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\">\n\
           <channel>\n\
             <title>{}</title>\n\
             <link>{}</link>\n\
             <description>{}</description>\n\
         {}\
           </channel>\n\
         </rss>",
        xml_escape(&site.title),
        xml_escape(base_url),
        xml_escape(&site.description),
        items,
    )
}

/// Format a Unix timestamp as an RSS `pubDate` — the RFC 822/2822 date-time,
/// which is the RFC 5322 grammar [`fauna_core::imf_date`] owns.
///
/// This used to be a local implementation, and it was the copy that proved the
/// grammar needed an owner: it cast the epoch-day count to `u64` before
/// `% 7`, so every pre-1970 timestamp printed an arbitrary weekday, and it
/// printed the year unpadded. Both are fixed by delegating.
fn rfc822_date(unix_secs: i64) -> String {
    fauna_core::imf_date::format_rfc5322_date(unix_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_to_html_basic() {
        let html = markdown_to_html("# Hello\n\nWorld");
        assert!(html.contains("<h1>"), "expected h1 tag, got: {html}");
        assert!(html.contains("Hello"), "expected 'Hello', got: {html}");
        assert!(html.contains("<p>"), "expected paragraph, got: {html}");
        assert!(html.contains("World"), "expected 'World', got: {html}");
    }

    #[test]
    fn build_site_context_from_json() {
        let json = r#"{"title":"My Blog","description":"A test blog"}"#;
        let ctx = build_site_context(Some(json), "fallback", "fallback bio");
        assert_eq!(ctx.title, "My Blog");
        assert_eq!(ctx.description, "A test blog");
    }

    #[test]
    fn build_site_context_fallback() {
        // None input
        let ctx = build_site_context(None, "Alice", "Alice's notes");
        assert_eq!(ctx.title, "Alice");
        assert_eq!(ctx.description, "Alice's notes");

        // Invalid JSON also falls back
        let ctx2 = build_site_context(Some("not json"), "Bob", "Bob's stuff");
        assert_eq!(ctx2.title, "Bob");
        assert_eq!(ctx2.description, "Bob's stuff");
    }

    fn make_post(slug: &str, title: &str) -> PostContext {
        PostContext {
            title: title.to_string(),
            slug: slug.to_string(),
            content: format!("<p>Content for {slug}</p>"),
            created_at: 1_700_000_000,
            tags: vec!["rust".to_string()],
            next: None,
            prev: None,
            paywall: None,
        }
    }

    #[test]
    fn render_template_with_posts() {
        let site = SiteContext {
            title: "My Site".to_string(),
            description: "A site".to_string(),
        };
        let posts = vec![
            make_post("hello-world", "Hello World"),
            make_post("second-post", "Second Post"),
        ];
        let template = r#"<html><head><title>{{site.title}}</title></head><body>{{#each posts}}<article>{{this.title}}</article>{{/each}}</body></html>"#;
        let data = TemplateData {
            site,
            posts,
            post: None,
        };
        let html = render_template(template, &data).expect("render should succeed");
        assert!(html.contains("My Site"), "site title missing");
        assert!(html.contains("Hello World"), "first post missing");
        assert!(html.contains("Second Post"), "second post missing");
    }

    #[test]
    fn render_template_handles_large_but_bounded_output() {
        // A flat loop over 1001 posts emits ~tens of KB — well under the cap —
        // so a legitimately large site renders fine.
        let site = SiteContext {
            title: "Stress".to_string(),
            description: "Stress test".to_string(),
        };
        let posts: Vec<PostContext> = (0..1001)
            .map(|i| make_post(&format!("post-{i}"), &format!("Post {i}")))
            .collect();
        let template = r#"{{#each posts}}<li>{{this.title}}</li>{{/each}}"#;
        let data = TemplateData {
            site,
            posts,
            post: None,
        };
        let html = render_template(template, &data).expect("should render 1001 posts");
        assert!(html.contains("Post 1000"), "last post should be present");
    }

    #[test]
    fn render_template_aborts_runaway_amplification() {
        // A nested `{{#each posts}}{{#each posts}}…` over 1001 posts is ~1M
        // iterations — an attacker-controlled template amplifying a bounded
        // context into unbounded output. The `CappedWriter` must abort it with
        // an error rather than build a multi-MB string (the DoS closes).
        let site = SiteContext {
            title: "Boom".to_string(),
            description: String::new(),
        };
        let posts: Vec<PostContext> = (0..1001)
            .map(|i| make_post(&format!("slug-padding-{i:06}"), &format!("Post {i}")))
            .collect();
        let template = r#"{{#each posts}}{{#each ../posts}}{{this.slug}}{{/each}}{{/each}}"#;
        let data = TemplateData {
            site,
            posts,
            post: None,
        };
        let result = render_template(template, &data);
        assert!(
            result.is_err(),
            "runaway nested-each output must be aborted by the byte cap"
        );
    }

    #[test]
    fn render_stops_a_no_output_runaway_inside_its_thread() {
        // A triple-nested `{{#each}}` that prints nothing never trips the
        // output cap: ~1e9 empty iterations over 1000 posts. The caller's
        // timeout only abandons the `spawn_blocking` thread, so the render
        // itself must stop at its budget — pinned by waiting on the render
        // thread, not on a timeout around it.
        let posts: Vec<PostContext> = (0..1000)
            .map(|i| make_post(&format!("p{i}"), &format!("Post {i}")))
            .collect();
        let data = TemplateData {
            site: SiteContext {
                title: "Spin".to_string(),
                description: String::new(),
            },
            posts,
            post: None,
        };
        let template =
            "{{#each posts}}{{#each ../posts}}{{#each ../../posts}}{{/each}}{{/each}}{{/each}}";
        let budget = std::time::Duration::from_millis(200);
        let (tx, rx) = std::sync::mpsc::channel();
        // spawn-ok(test): the render under test, on a thread the test joins by channel.
        std::thread::spawn(move || {
            let _ = tx.send(render_data_within(template, &data, budget));
        });
        let result = rx
            .recv_timeout(budget + std::time::Duration::from_secs(3))
            .expect("the render thread must end shortly after its budget");
        assert!(result.is_err(), "a render past its budget must fail");
    }

    #[test]
    fn render_stops_a_helper_free_loop_body_at_its_budget() {
        // The loop body calls no helper and prints nothing — only the
        // check planted at the head of the body stops it: 1000 posts × 2k
        // empty expressions, tens of seconds unchecked in a debug build.
        let posts: Vec<PostContext> = (0..1000)
            .map(|i| make_post(&format!("p{i}"), &format!("Post {i}")))
            .collect();
        let data = TemplateData {
            site: SiteContext {
                title: "Spin".to_string(),
                description: String::new(),
            },
            posts,
            post: None,
        };
        let template = format!("{{{{#each posts}}}}{}{{{{/each}}}}", "{{a}}".repeat(2_000));
        let budget = std::time::Duration::from_millis(200);
        let (tx, rx) = std::sync::mpsc::channel();
        // spawn-ok(test): the render under test, on a thread the test joins by channel.
        std::thread::spawn(move || {
            let _ = tx.send(render_data_within(&template, &data, budget));
        });
        let result = rx
            .recv_timeout(budget + std::time::Duration::from_secs(3))
            .expect("the render thread must end shortly after its budget");
        assert!(result.is_err(), "a render past its budget must fail");
    }

    #[test]
    fn budget_checks_leave_block_semantics_untouched() {
        // The planted checks write nothing and keep `@index`/`@last`, `../`,
        // `{{else}}`, `with` and inline partials rendering exactly as before.
        let data = TemplateData {
            site: SiteContext {
                title: "S".to_string(),
                description: String::new(),
            },
            posts: vec![make_post("a", "A"), make_post("b", "B")],
            post: None,
        };
        let template = "{{#*inline \"p\"}}[{{this}}]{{/inline}}\
            {{#each posts}}{{@index}}:{{this.slug}}/{{../site.title}}{{#if @last}}!{{/if}}{{/each}}\
            {{#each nothing}}x{{else}}none{{/each}}\
            {{#with site}}<{{title}}>{{/with}}{{> p site.title}}";
        assert_eq!(
            render_template(template, &data).unwrap(),
            "0:a/S1:b/S!none<S>[S]"
        );
    }

    #[test]
    fn render_refuses_an_oversized_template() {
        let data = TemplateData {
            site: SiteContext {
                title: String::new(),
                description: String::new(),
            },
            posts: vec![],
            post: None,
        };
        let template = "x".repeat(MAX_TEMPLATE_BYTES + 1);
        assert!(render_template(&template, &data).is_err());
        let template = "x".repeat(MAX_TEMPLATE_BYTES);
        assert!(render_template(&template, &data).is_ok());
    }

    #[test]
    fn default_index_template_renders_posts() {
        let site = SiteContext {
            title: "My Blog".to_string(),
            description: "Just a blog".to_string(),
        };
        let posts = vec![make_post("hello-world", "Hello World")];
        let html = render_default_index(&site, &posts).expect("default index should render");
        assert!(html.contains("My Blog"), "site title should appear");
        assert!(html.contains("Hello World"), "post title should appear");
        assert!(
            html.contains("/post/hello-world"),
            "post link should point to /post/{{slug}}"
        );
    }

    #[test]
    fn default_post_template_renders_single() {
        let site = SiteContext {
            title: "My Blog".to_string(),
            description: "Just a blog".to_string(),
        };
        let post = PostContext {
            title: "Hello World".to_string(),
            slug: "hello-world".to_string(),
            content: "<p>Great post content.</p>".to_string(),
            created_at: 1_700_000_000,
            tags: vec![],
            next: None,
            prev: None,
            paywall: None,
        };
        let posts = vec![post.clone()];
        let html = render_default_post(&site, &posts, &post).expect("default post should render");
        assert!(html.contains("Hello World"), "post title should appear");
        assert!(
            html.contains("Great post content."),
            "post content should appear"
        );
        assert!(
            html.contains("href=\"/\""),
            "back-to-home link should exist"
        );
    }

    #[test]
    fn generate_rss_feed() {
        let site = SiteContext {
            title: "Alice's Blog".to_string(),
            description: "Thoughts and code".to_string(),
        };
        let posts = vec![
            make_post("first-post", "First Post"),
            make_post("second-post", "Second Post"),
        ];
        let rss = generate_rss(&site, &posts, "https://alice.fauna.social");

        assert!(
            rss.starts_with("<?xml"),
            "should start with XML declaration"
        );
        assert!(
            rss.contains("<rss version=\"2.0\">"),
            "should have rss element"
        );
        assert!(
            rss.contains("Alice&#x27;s Blog")
                || rss.contains("Alice&apos;s Blog")
                || rss.contains("Alice's Blog"),
            "site title should be present"
        );
        assert!(rss.contains("first-post"), "first post slug should appear");
        assert!(
            rss.contains("second-post"),
            "second post slug should appear"
        );
        assert!(rss.contains("First Post"), "first post title should appear");
        assert!(
            rss.contains("https://alice.fauna.social/posts/first-post"),
            "post URL should be correct"
        );
    }

    /// Render `template` through the public [`render_data`] from a thread with
    /// the 2 MiB stack of a tokio blocking-pool thread — the thread
    /// `service.rs` renders on. A stack overflow is not a panic: it aborts the
    /// whole process, so a regression here kills the test binary rather than
    /// failing one test (run these pins in their own `cargo test`).
    fn render_on_blocking_sized_stack(template: String) -> Result<String> {
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn_scoped(s, move || {
                    let data = serde_json::json!({ "a": true, "n": "p" });
                    render_data(&template, &data)
                })
                .expect("spawn the render thread")
                .join()
                .expect("the render thread must not panic")
        })
    }

    /// `result` is an error naming `reason` — not merely an error, which a
    /// template that fails to parse would satisfy without exercising the bound.
    fn assert_refused(what: &str, result: Result<String>, reason: &str) {
        match result {
            Err(e) => assert!(
                format!("{e:#}").contains(reason),
                "{what}: expected an error naming {reason:?}, got {e:#}"
            ),
            Ok(out) => panic!("{what}: expected Err, rendered {} bytes", out.len()),
        }
    }

    #[test]
    fn a_recursive_partial_fails_the_render_instead_of_the_nest() {
        // Before the stack guard each of these overflowed the render thread's
        // stack, and a stack overflow aborts the whole nest — every user's
        // site, and (the owed render re-running at boot) in a crash loop.
        for template in [
            // Self-recursive inline partial.
            "{{#*inline \"p\"}}{{> p}}{{/inline}}{{> p}}",
            // Mutual recursion.
            "{{#*inline \"p\"}}{{> q}}{{/inline}}{{#*inline \"q\"}}{{> p}}{{/inline}}{{> p}}",
            // A dynamic partial name the AST cannot resolve statically.
            "{{#*inline \"p\"}}x{{> (lookup this \"n\")}}{{/inline}}{{> p}}",
            // The top-level template calling itself by its registered name.
            "x{{> t}}",
        ] {
            let result = render_on_blocking_sized_stack(template.to_string());
            assert_refused(template, result, "nests too deeply");
        }
    }

    #[test]
    fn deep_nesting_fails_the_render_instead_of_the_nest() {
        // Each shape overflowed a 2 MiB stack in the parser, the budget-check
        // walk or the render at ≤3000 levels; every one fits in 1 MiB of
        // source, so the source cap does not bound it.
        let n = 5_000;
        let blocks = "block tags";
        let expression = "levels in one expression";
        for (name, template, reason) in [
            (
                "blocks",
                format!("{}{}", "{{#if a}}".repeat(n), "{{/if}}".repeat(n)),
                blocks,
            ),
            (
                "partial blocks",
                format!("{}{}", "{{#> q}}".repeat(n), "{{/q}}".repeat(n)),
                blocks,
            ),
            (
                "else chain",
                format!("{{{{#if a}}}}{}{{{{/if}}}}", "{{else if a}}".repeat(n)),
                blocks,
            ),
            (
                "subexpressions",
                format!("{{{{a {}{}}}}}", "(a ".repeat(n), ")".repeat(n)),
                expression,
            ),
            (
                "brackets",
                format!("{{{{a {}{}}}}}", "[".repeat(100_000), "]".repeat(100_000)),
                expression,
            ),
            (
                "braces",
                format!("{{{{a {}{}}}}}", "{ ".repeat(100_000), " }".repeat(100_000)),
                expression,
            ),
        ] {
            let result = render_on_blocking_sized_stack(template);
            assert_refused(name, result, reason);
        }
    }

    #[test]
    fn nesting_hidden_behind_quotes_brackets_or_escapes_is_still_counted() {
        // The pre-parse scan must never believe an expression ended while the
        // parser is still inside it: each prefix below puts a `}}` where a
        // naive scanner would reset its count, followed by deep real nesting.
        let n = 5_000;
        let deep = format!("{}{}", "(a ".repeat(n), ")".repeat(n));
        for prefix in [
            "{{a \"}}\" ",
            "{{a '}}' ",
            "{{a `}}` ",
            "{{a [}}] ",
            "{{a [x\"] \"] }} ",
            "{{a \"x\\\" }} \" ",
        ] {
            let template = format!("{prefix}{deep}}}}}");
            let result = render_on_blocking_sized_stack(template);
            assert_refused(prefix, result, "levels in one expression");
        }
    }

    #[test]
    fn a_template_at_both_nesting_limits_fits_the_render_stack() {
        // The limits are sized against RENDER_STACK_BYTES: the deepest template
        // they admit — every block tag nested, the innermost expression at the
        // expression cap — parses and renders without tripping the stack
        // budget. (Debug frames are the larger; this runs in a debug build.)
        let blocks = MAX_TEMPLATE_BLOCK_TAGS - 1;
        let nots = MAX_EXPRESSION_NESTING - 1;
        let template = format!(
            "{}{{{{#if {}a{}}}}}deep{{{{/if}}}}{}",
            "{{#if a}}".repeat(blocks),
            "(not ".repeat(nots),
            ")".repeat(nots),
            "{{/if}}".repeat(blocks),
        );
        let result = render_on_blocking_sized_stack(template);
        assert_eq!(
            result.unwrap(),
            if nots.is_multiple_of(2) { "deep" } else { "" }
        );
    }

    #[test]
    fn nesting_limits_leave_ordinary_templates_rendering() {
        // Moderate real nesting, quoted helper arguments followed by
        // paren-heavy CSS, and commented prose all render as before.
        let nested = format!("{}ok{}", "{{#if a}}".repeat(50), "{{/if}}".repeat(50));
        assert_eq!(render_on_blocking_sized_stack(nested).unwrap(), "ok");
        let css = format!(
            "{{{{#if (lookup this \"a\")}}}}<style>{}</style>{{{{/if}}}}",
            "a{color:rgba(1,2,3,0.5)}".repeat(2_000)
        );
        let css = render_on_blocking_sized_stack(css);
        assert!(css.is_ok(), "{css:?}");
        let prose = format!("{{{{! don't [touch] }}}}{}", "(x)".repeat(2_000));
        assert!(render_on_blocking_sized_stack(prose).is_ok());
        let long_comment = "{{!-- }} {{#if a}} it's --}}fine".to_string();
        assert_eq!(
            render_on_blocking_sized_stack(long_comment).unwrap(),
            "fine"
        );
    }
}
