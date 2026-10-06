//! The terminal `RenderDocument` walker — tui's half of the shared render model
//! (`apps/tui.md` § Rendering; the model itself: `architecture/render-model.md`).
//!
//! The document is a *semantic* model, never a widget tree (render-model.md § The
//! boundary): it carries structure, role, content and state, and "the shell's whole job
//! is `match block { … }` → construct the native widget". Here the native widget is
//! ratatui text, so this module is the whole of tui's body-render glue. It parses
//! nothing, fetches nothing, and holds no render state — every decision (what is bold,
//! which remote image is revealed, which quote resolved) was already made in shared Rust.
//!
//! ## What the walker paints, and what it deliberately does NOT
//!
//! **Rule: a block that ui.yaml gives its own element ID is painted by the *page*, as
//! that element; everything else is body text painted here.** So [`Image`], [`Attachment`],
//! [`LinkPreview`], [`QuotedPost`] and [`QuotedMessage`] are **no-ops** in this walker —
//! the page extracts them (`RenderDocument::first_image_hash` / `quoted_post` /
//! `resolved_link_previews`) and registers each as its own automatable element
//! (`post-image`, `quoted-post`, `link-preview-card`, …). Painting them here *as well*
//! would double-render them — the same trap android and web avoid by keeping those walker
//! arms inert.
//!
//! `RemoteImage` **used to be** the exception — it had no element ID, so it painted here
//! as an inline text placeholder in both states. It now has one: ui.yaml's
//! `doc-remote-image` (indexed, user-approved 2026-07-31), minted precisely so the
//! blocked → reveal → *painted* transition is observable headlessly. So the rule above
//! applies to it unchanged: the arm here is inert, and the page registers each block
//! through [`remote_image_elements`] — placeholder while blocked or still loading, real
//! half-block art once its bytes arrive. The paint being the element's own characters is
//! what makes it assertable at all (`tui.md` § Rendering: the art *is* the e2e paint
//! observable); a paint inside this walker's `Line` output would have none.
//!
//! The image therefore lands just under the body rather than at its exact place in the
//! flow. That is not a tui invention: `RenderBlock::Image` sits inline in the model too,
//! and every one of the 7 apps paints it as a sibling `post-image` element.
//!
//! ## Exhaustive on purpose
//!
//! The `match`es below carry **no `_` arm**. A new `RenderBlock`/`Inline` variant must
//! fail tui's build rather than silently paint an empty body — the same discipline the
//! wizard's `OnboardingStep` matches use, and the fleet-wide expectation that a new
//! variant lands with every app's walker.

use fauna_core::render::{Inline, RenderBlock, RenderDocument};
use fauna_ui_ids as ids;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Inline `` `code` `` and fenced code blocks.
const CODE: Color = Color::Yellow;
/// Link labels (the href is not painted — a terminal cannot click it; the label is what
/// `to_plaintext` carries too, so paint and the automation read agree).
const LINK: Color = Color::Cyan;

/// Paint a body into ratatui lines. Pure — the only entry point.
///
/// The automation read of the same body is `document.to_plaintext()` (the shared
/// projection every app's `feed-post-text` / `dm-message-text` reads —
/// render-model.md § Implementation status), **not** a re-flattening of these lines.
pub fn render_document(doc: &RenderDocument) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    blocks_into(&doc.blocks, "", &mut out);
    out
}

/// Marks a remote image the reader has not consented to load. Its url is not
/// requested and nothing about the post has reached the image's host.
const BLOCKED: &str = "🚫";
/// Marks a **revealed** remote image whose bytes have not arrived (or could not
/// be decoded). The reveal already happened, so this is a load state, not a gate.
const PENDING: &str = "🖼";

/// The `doc-remote-image` elements of one body — one per [`RenderBlock::RemoteImage`],
/// in body order, whatever its state.
///
/// The element registers in **every** state, under the same id, exactly as
/// `post-image` does: a blocked image is as addressable as a painted one, so a test
/// can prove the block exists before asserting that it has not painted (an absence
/// assertion against an element that never registered is vacuous). What changes with
/// state is only the body:
///
/// - **blocked** → a placeholder label. `images` is not even consulted; the url has
///   not been fetched, because [`revealed_remote_image_urls`] never offered it.
/// - **revealed, bytes not (yet) here** → a placeholder label, same as a failed
///   `post-image` fetch. One unreachable image in someone else's post degrades to a
///   glyph, never to a page banner.
/// - **revealed, art cached** → the rasterized half-block picture, which *is* the
///   element's text and so is what the e2e counts (`tui.md` § Rendering).
///
/// The alt text rides along as the label body. It is remote-authored, and it is
/// **not** sanitized here on purpose: every line reaching a widget is stripped at
/// `crate::ui`'s paint funnel, which is the one gate (`tui.md` § Rendering — a
/// per-source strip is the shape to refuse).
pub fn remote_image_elements(
    doc: &RenderDocument,
    images: &crate::image_cache::ImageCache,
) -> Vec<crate::element::Element> {
    use crate::element::Element;
    use crate::image_cache::ImageState;

    doc.remote_images()
        .into_iter()
        .map(|image| {
            let art = image
                .revealed
                .then(|| match images.get(image.url) {
                    Some(ImageState::Ready(art)) => Some(art.clone()),
                    Some(ImageState::Loading) | Some(ImageState::Failed) | None => None,
                })
                .flatten();
            match art {
                Some(art) => Element::thumbnail(ids::DOC_REMOTE_IMAGE, art),
                None => {
                    let marker = if image.revealed { PENDING } else { BLOCKED };
                    let body = if image.alt.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", image.alt)
                    };
                    Element::label(ids::DOC_REMOTE_IMAGE, format!("{marker}{body}"))
                }
            }
        })
        .collect()
}

/// The urls of a body's **revealed** remote images — the only urls this client
/// may request, and the only list `crate::remote_image` is ever handed.
///
/// This is the single place the D3 reveal gate turns into network behaviour, which
/// is why the fetch module holds no flag of its own: an unrevealed image simply has
/// no url here, so no call site can fetch one by forgetting to check.
pub fn revealed_remote_image_urls(doc: &RenderDocument) -> Vec<String> {
    doc.remote_images()
        .into_iter()
        .filter(|image| image.revealed)
        .map(|image| image.url.to_string())
        .collect()
}

fn blocks_into(blocks: &[RenderBlock], indent: &str, out: &mut Vec<Line<'static>>) {
    for block in blocks {
        block_into(block, indent, "", out);
    }
}

/// Paint one block. `prefix` decorates only the block's **first** line (a list bullet, a
/// task checkbox); its continuation lines align underneath it.
fn block_into(block: &RenderBlock, indent: &str, prefix: &str, out: &mut Vec<Line<'static>>) {
    let pad = " ".repeat(prefix.chars().count());
    match block {
        RenderBlock::Paragraph { inlines } => {
            out.push(line(
                indent,
                prefix,
                inlines_spans(inlines, Style::default()),
            ));
        }
        RenderBlock::Heading { level, inlines } => {
            out.push(line(
                indent,
                prefix,
                inlines_spans(inlines, heading(*level)),
            ));
        }
        RenderBlock::ListBlock { ordered, items } => {
            for (i, item) in items.iter().enumerate() {
                // An ordered list renumbers from 1 at render — the model carries no item
                // numbers (render.rs § ListBlock).
                let marker = if *ordered {
                    format!("{}. ", i + 1)
                } else {
                    "• ".to_string()
                };
                item_into(&item.blocks, indent, &marker, out);
            }
        }
        RenderBlock::TaskList { items } => {
            for item in items {
                // A *static* checked/unchecked box: the interactive checkbox belongs to
                // the Notes editor, not this display render (render.rs § TaskList).
                let marker = if item.checked { "☑ " } else { "☐ " };
                item_into(&item.blocks, indent, marker, out);
            }
        }
        RenderBlock::CodeBlock { text, .. } => {
            for (i, src) in text.lines().enumerate() {
                let lead = if i == 0 { prefix } else { pad.as_str() };
                out.push(line(
                    indent,
                    lead,
                    vec![Span::styled(src.to_string(), Style::default().fg(CODE))],
                ));
            }
        }
        RenderBlock::BlockQuote { blocks } => {
            let inner = format!("{indent}{pad}│ ");
            for (i, block) in blocks.iter().enumerate() {
                // The caller's bullet, if any, still leads the quote's first line.
                let lead = if i == 0 { prefix } else { "" };
                block_into(block, &inner, lead, out);
            }
        }
        // ── Embeds the PAGE paints as their own ui.yaml elements (see the module doc).
        // Painting them here too would double-render them. `RemoteImage` joined this arm
        // when `doc-remote-image` was minted — see [`remote_image_elements`]. A
        // `ProxiedImage` (render-model.md § D6c) paints in the page's `post-image` slot, a
        // `ProxiedVideo` in its `video-thumbnail` slot.
        RenderBlock::Image { .. }
        | RenderBlock::Video { .. }
        | RenderBlock::ProxiedImage { .. }
        | RenderBlock::ProxiedVideo { .. }
        | RenderBlock::RemoteImage { .. }
        | RenderBlock::Attachment { .. }
        | RenderBlock::LinkPreview { .. }
        | RenderBlock::QuotedPost { .. }
        | RenderBlock::QuotedMessage { .. } => {}
    }
}

/// Paint one list/task item: the marker leads its first block, and every later block
/// aligns under the marker — so a nested list inside an item indents correctly.
fn item_into(blocks: &[RenderBlock], indent: &str, marker: &str, out: &mut Vec<Line<'static>>) {
    let pad = " ".repeat(marker.chars().count());
    for (i, block) in blocks.iter().enumerate() {
        if i == 0 {
            block_into(block, indent, marker, out);
        } else {
            block_into(block, &format!("{indent}{pad}"), "", out);
        }
    }
}

fn line(indent: &str, prefix: &str, spans: Vec<Span<'static>>) -> Line<'static> {
    let lead = format!("{indent}{prefix}");
    if lead.is_empty() {
        return Line::from(spans);
    }
    let mut all = Vec::with_capacity(spans.len() + 1);
    all.push(Span::raw(lead));
    all.extend(spans);
    Line::from(all)
}

fn heading(level: u8) -> Style {
    let style = Style::default().add_modifier(Modifier::BOLD);
    if level <= 1 {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
}

fn inlines_spans(inlines: &[Inline], base: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for inline in inlines {
        inline_into(inline, base, &mut out);
    }
    out
}

/// `Inline` is a *tree* (`Bold`/`Italic`/`Link` nest), so emphasis composes by inheriting
/// the enclosing style rather than by flattening.
///
/// There is deliberately **no `Mention` arm**: the shipped enum has no such variant
/// (`fauna_core::render` — "waits for a producer; markdown has no mention syntax today"),
/// and render-model.md § The model leaves the field set to the implementing slice.
fn inline_into(inline: &Inline, base: Style, out: &mut Vec<Span<'static>>) {
    match inline {
        Inline::Text { text } => out.push(Span::styled(text.clone(), base)),
        Inline::Bold { inlines } => {
            for i in inlines {
                inline_into(i, base.add_modifier(Modifier::BOLD), out);
            }
        }
        Inline::Italic { inlines } => {
            for i in inlines {
                inline_into(i, base.add_modifier(Modifier::ITALIC), out);
            }
        }
        Inline::Code { text } => out.push(Span::styled(text.clone(), base.fg(CODE))),
        Inline::Link { inlines, .. } => {
            for i in inlines {
                inline_into(i, base.fg(LINK).add_modifier(Modifier::UNDERLINED), out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::art;
    use fauna_core::render::{
        AuthoringOriginStatus, PreviewState, VerificationStatus, markdown_to_document,
    };

    /// The painted text of each line, marker/indent included.
    fn painted(doc: &RenderDocument) -> Vec<String> {
        render_document(doc)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The style of the span carrying `needle`, across all lines.
    fn style_of(doc: &RenderDocument, needle: &str) -> Style {
        render_document(doc)
            .iter()
            .flat_map(|l| l.spans.clone())
            .find(|s| s.content == needle)
            .unwrap_or_else(|| panic!("no span painted {needle:?}"))
            .style
    }

    #[test]
    fn emphasis_composes_through_the_inline_tree() {
        let doc = markdown_to_document("plain **bold** *italic* `code` [label](https://x.example)");
        assert_eq!(
            painted(&doc),
            vec!["plain bold italic code label"],
            "the href is not painted — the label is, matching to_plaintext"
        );
        assert!(style_of(&doc, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(
            style_of(&doc, "italic")
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert_eq!(style_of(&doc, "code").fg, Some(CODE));
        let link = style_of(&doc, "label");
        assert_eq!(link.fg, Some(LINK));
        assert!(link.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn headings_are_bold_and_the_top_level_underlines() {
        let doc = markdown_to_document("# One\n\n## Two");
        assert_eq!(painted(&doc), vec!["One", "Two"]);
        assert!(style_of(&doc, "One").add_modifier.contains(Modifier::BOLD));
        assert!(
            style_of(&doc, "One")
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert!(
            !style_of(&doc, "Two")
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn lists_bullet_number_and_nest() {
        let doc = markdown_to_document("- one\n- two\n  - deep");
        assert_eq!(painted(&doc), vec!["• one", "• two", "  • deep"]);

        let ordered = markdown_to_document("1. first\n2. second");
        assert_eq!(painted(&ordered), vec!["1. first", "2. second"]);
    }

    #[test]
    fn task_items_paint_a_static_checkbox() {
        let doc = markdown_to_document("- [x] done\n- [ ] todo");
        assert_eq!(painted(&doc), vec!["☑ done", "☐ todo"]);
    }

    #[test]
    fn code_blocks_keep_one_line_per_source_line() {
        let doc = markdown_to_document("```\nlet a = 1;\nlet b = 2;\n```");
        assert_eq!(painted(&doc), vec!["let a = 1;", "let b = 2;"]);
        assert_eq!(style_of(&doc, "let a = 1;").fg, Some(CODE));
    }

    #[test]
    fn block_quotes_gutter_their_content() {
        let doc = markdown_to_document("> quoted");
        assert_eq!(painted(&doc), vec!["│ quoted"]);
    }

    /// The three states of `doc-remote-image`, and the property the whole promote
    /// exists for: the element registers in **every** one of them under the same
    /// id, so "did it paint?" is a question about its *text*, answerable
    /// headlessly, rather than about whether an element showed up at all.
    #[test]
    fn a_remote_image_registers_in_every_state_and_paints_only_when_revealed() {
        let mut doc = markdown_to_document("![a cat](https://x.example/c.png)");
        let mut cache = crate::image_cache::ImageCache::new();
        let elements = |doc: &RenderDocument, cache: &crate::image_cache::ImageCache| {
            remote_image_elements(doc, cache)
                .into_iter()
                .map(|e| (e.id, e.text))
                .collect::<Vec<_>>()
        };

        // Blocked: a placeholder, and the reveal button's predicate agrees.
        assert_eq!(
            elements(&doc, &cache),
            vec![("doc-remote-image".to_string(), "🚫 a cat".to_string())]
        );
        assert!(doc.has_blocked_remote_images());
        // Even art already in the cache stays unpainted while blocked — the gate
        // is the block's own flag, never "do we happen to have the bytes".
        cache.set("https://x.example/c.png".to_string(), Some(art()));
        assert_eq!(
            elements(&doc, &cache),
            vec![("doc-remote-image".to_string(), "🚫 a cat".to_string())],
            "a blocked image must not paint bytes another post's reveal fetched"
        );

        // The reveal is the MANAGER's projection, not a flag this client keeps.
        doc.set_remote_images_revealed(true);
        assert!(!doc.has_blocked_remote_images());
        assert!(
            elements(&doc, &cache)[0].1.contains('▀'),
            "revealed + cached art paints the picture as the element's own text"
        );

        // Revealed but not yet fetched, and revealed-then-failed, both degrade to
        // the pending placeholder — never a blank row and never a page banner.
        let empty = crate::image_cache::ImageCache::new();
        assert_eq!(
            elements(&doc, &empty),
            vec![("doc-remote-image".to_string(), "🖼 a cat".to_string())]
        );
        let mut failed = crate::image_cache::ImageCache::new();
        failed.set("https://x.example/c.png".to_string(), None);
        assert_eq!(
            elements(&doc, &failed),
            vec![("doc-remote-image".to_string(), "🖼 a cat".to_string())]
        );
    }

    /// Only a revealed image's url is ever offered to the fetcher — the single
    /// place D3's "when does untrusted content phone home" gate becomes network
    /// behaviour.
    #[test]
    fn only_revealed_urls_are_offered_for_fetching() {
        let mut doc =
            markdown_to_document("![a](https://x.example/a.png)\n\n![b](https://y.example/b.png)");
        assert!(
            revealed_remote_image_urls(&doc).is_empty(),
            "blocked-by-default: nothing to fetch before the reader consents"
        );
        doc.set_remote_images_revealed(true);
        assert_eq!(
            revealed_remote_image_urls(&doc),
            vec!["https://x.example/a.png", "https://y.example/b.png"],
            "one button reveals all of a post's remote content together"
        );
    }

    /// The load-bearing no-double-render rule: the page paints these as their own ui.yaml
    /// elements (`post-image`, `doc-remote-image`, `quoted-post`, `link-preview-card`), so
    /// the walker must not.
    #[test]
    fn embeds_with_their_own_element_id_are_not_painted_by_the_walker() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Paragraph {
                    inlines: vec![Inline::Text {
                        text: "body".into(),
                    }],
                },
                RenderBlock::Image {
                    hash: "abc".into(),
                    alt: "alt".into(),
                },
                RenderBlock::RemoteImage {
                    url: "https://x.example/c.png".into(),
                    alt: "a cat".into(),
                    revealed: true,
                },
                RenderBlock::QuotedPost {
                    post_id: "p".into(),
                    author: "a".into(),
                    body: "quoted body".into(),
                    verification: VerificationStatus::Verified,
                    authoring_origin: AuthoringOriginStatus::Unknown,
                    legal_takedown_ref: None,
                    not_found: false,
                },
                RenderBlock::LinkPreview {
                    url: "https://x.example".into(),
                    state: PreviewState::Resolving,
                },
            ],
        };
        assert_eq!(
            painted(&doc),
            vec!["body"],
            "only the body text — the four embeds are the page's elements"
        );
        // …and the page can still reach every one of them through the shared projections.
        assert_eq!(doc.first_image_hash(), Some("abc"));
        assert_eq!(doc.quoted_post().map(|q| q.body), Some("quoted body"));
        assert_eq!(doc.resolving_link_preview_urls(), vec!["https://x.example"]);
        assert_eq!(
            doc.remote_images()
                .iter()
                .map(|i| i.url)
                .collect::<Vec<_>>(),
            vec!["https://x.example/c.png"]
        );
    }

    #[test]
    fn an_empty_document_paints_nothing() {
        assert!(render_document(&RenderDocument::default()).is_empty());
    }
}
