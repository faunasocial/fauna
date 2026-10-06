//! What counts as a **render input** — the one predicate, shared by everything
//! that has to agree on it.
//!
//! Two places decide whether a synced file feeds the render pipeline, and
//! before this module they were two separate spellings that quietly disagreed:
//! the render's own listing asks SQLite for `path LIKE '%.html.hbs'`, which
//! folds ASCII case because the nest never sets `case_sensitive_like`, while
//! the sync door's revoke test was a case-sensitive `ends_with`. So
//! `about.HTML.hbs` rendered a page on every render, and its delete or edit was
//! routed as an ordinary static file — no owed-render mark, no re-render, and
//! the page it had rendered kept serving after the author withdrew it
//! (`web-content-hosting.md` § Routing, render, serving → *A revoke is
//! durable*: a door whose commit can take content off the rendered site
//! marks).
//!
//! **The predicate here is authoritative and the SQL is a coarse pre-filter.**
//! `render_for_actor` runs every row the listing returns back through
//! [`is_render_input`], so the two can only agree: the listing may be sloppy in
//! the *widening* direction, and this function decides. The sync door calls the
//! same function. A pin holds the SQL to being a superset.
//!
//! **Case is folded, deliberately.** Matching the listing's existing behavior
//! rather than the `ends_with` spelling keeps every page that renders today
//! rendering: tightening to case-sensitive would have closed the same hole by
//! silently un-rendering an author's `about.HTML` at the next render, which is
//! a page removal nobody asked for (`principles.md` § The user always controls
//! their data — a withdrawal is the author's act, never a predicate's).

/// The template suffix a render input carries, matched ASCII-case-insensitively.
const TEMPLATE_SUFFIX: &str = ".html.hbs";

/// The site-metadata file, matched on the last path segment.
const SITE_METADATA: &str = "_site.json";

/// Whether a synced web file feeds the render pipeline: a Handlebars template
/// or the `_site.json` site-metadata file. Static assets do not.
///
/// `_site.json` is matched exactly, not case-folded: it is one reserved name
/// the render looks up by that exact path, so a `_SITE.JSON` is an ordinary
/// file on both sides and the two still agree.
pub fn is_render_input(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    is_template(path) || name == SITE_METADATA
}

/// Whether `path` is a Handlebars template — the half of [`is_render_input`]
/// the render's `.html.hbs` listing covers.
pub fn is_template(path: &str) -> bool {
    path.len() >= TEMPLATE_SUFFIX.len()
        && path[path.len() - TEMPLATE_SUFFIX.len()..].eq_ignore_ascii_case(TEMPLATE_SUFFIX)
}

/// Where a template renders to: its own path with the `.hbs` stripped, folding
/// case like the match that admitted it. Without the folding, `page.html.HBS`
/// rendered to `page.html.HBS` — a path the synced template itself already
/// occupies in `web_files`, which the serve order resolves first, so that
/// render was written and never served.
pub fn rendered_output_path(template_path: &str) -> String {
    let hbs = ".hbs";
    if template_path.len() >= hbs.len()
        && template_path[template_path.len() - hbs.len()..].eq_ignore_ascii_case(hbs)
    {
        return template_path[..template_path.len() - hbs.len()].to_string();
    }
    template_path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_and_site_metadata_are_render_inputs() {
        assert!(is_render_input("index.html.hbs"));
        assert!(is_render_input("blog/post.html.hbs"));
        assert!(is_render_input("_site.json"));
        assert!(is_render_input("nested/_site.json"));
        assert!(!is_render_input("style.css"));
        assert!(!is_render_input("index.html"));
        assert!(!is_render_input("img/logo.png"));
    }

    /// The divergence this module exists to close: the render's listing has
    /// always folded ASCII case, so these ARE render inputs, and the sync
    /// door's own test has to say so too or their deletes revoke nothing.
    #[test]
    fn a_case_folded_template_is_a_render_input_like_any_other() {
        assert!(is_render_input("about.HTML.hbs"));
        assert!(is_render_input("about.html.HBS"));
        assert!(is_render_input("README.HTML.HBS"));
        assert!(is_render_input("blog/Post.Html.Hbs"));
        // `_site.json` is the one reserved NAME, matched exactly on both sides.
        assert!(!is_render_input("_SITE.JSON"));
        // Still not a template: the suffix has to be the whole tail.
        assert!(!is_render_input("notes.html.hbsx"));
        assert!(!is_render_input("html.hbs.txt"));
    }

    #[test]
    fn the_output_path_strips_the_hbs_whatever_its_case() {
        assert_eq!(rendered_output_path("index.html.hbs"), "index.html");
        assert_eq!(rendered_output_path("about.HTML.hbs"), "about.HTML");
        assert_eq!(rendered_output_path("about.html.HBS"), "about.html");
        assert_eq!(rendered_output_path("blog/Post.Html.Hbs"), "blog/Post.Html");
        // Not a template — left alone rather than mangled.
        assert_eq!(rendered_output_path("style.css"), "style.css");
    }
}
