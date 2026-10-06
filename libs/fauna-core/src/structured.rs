//! Structured-post field projection — the single source of truth for the
//! `PostBody::Structured` schema + field-key contract.
//!
//! A handful of post types (Nostr long-form articles, community definitions,
//! classified listings, live activities) arrive as `PostBody::Structured` with
//! a `schema` discriminator and a `Vec<StructuredField>` of `{key, value}`
//! pairs. Two sides touch that wire shape:
//!
//! * the **writer** — `fauna_bridge_nostr::translate::nostr_event_to_fauna`
//!   builds the `Structured` body from an incoming Nostr event, and
//! * the **reader** — every app's feed renders the structured card by
//!   projecting those fields into a typed view.
//!
//! Before this module each side independently hard-coded the schema strings and
//! field keys (the web reader in `feed-utils.ts`, the bridge writer inline), so
//! a field-key change on one side silently broke the other. Centralizing the
//! contract here — schema constants, the typed [`StructuredView`], and the
//! projection in both directions — makes that drift impossible: the bridge
//! reuses the constants/helpers and the apps ride [`structured_view`] (web
//! over the `fauna_wasm::structuredView` export, native via UniFFI/direct call).
//!
//! WASM-safe (only `String`/`Vec`/serde) — compiled unconditionally so the web
//! `fauna-wasm` build and the native apps share one projection. See
//! `docs/goal/ui/feed.md` § Post content types + § Where logic lives.

use serde::{Deserialize, Serialize};

use crate::data::{PostBody, StructuredField};

/// Schema discriminator for a Nostr long-form article (NIP-23).
pub const SCHEMA_ARTICLE: &str = "nostr/article";
/// Schema discriminator for a Nostr community definition (NIP-72).
pub const SCHEMA_COMMUNITY: &str = "nostr/community";
/// Schema discriminator for a Nostr classified listing (NIP-99).
pub const SCHEMA_CLASSIFIED: &str = "nostr/classified";
/// Schema discriminator for a Nostr live activity (NIP-53).
pub const SCHEMA_LIVE_ACTIVITY: &str = "nostr/live-activity";

/// A structured post projected into the typed view its feed card renders, or
/// `None` for any body that isn't a recognized structured schema (a plain
/// post, or the `nostr/kind-N` unknown-kind fallback — both render as text).
///
/// Serialized internally-tagged on `kind` (kebab-case variant names) so the
/// JS/web face is a flat object, e.g. `{ kind: "article", title, summary,
/// image, content }` — the shape `PostCard.svelte` discriminates on. The
/// free-text `content` of the post maps to the field a card actually shows:
/// the article/classified body, the community `rules`, the live-activity
/// `summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StructuredView {
    Article {
        title: String,
        summary: String,
        image: String,
        content: String,
    },
    Community {
        name: String,
        description: String,
        identifier: String,
        /// The community rules — the post's free-text `content`.
        rules: String,
    },
    Classified {
        title: String,
        price: String,
        location: String,
        condition: String,
        content: String,
    },
    LiveActivity {
        title: String,
        status: String,
        streaming_url: String,
        participants: String,
        /// The activity summary — the post's free-text `content`.
        summary: String,
    },
}

/// Look up a structured field's value by key, returning an empty string when
/// absent — the shared projection primitive used wherever a
/// `&[StructuredField]` is read (reader projection here + the nostr bridge).
pub fn field(fields: &[StructuredField], key: &str) -> String {
    fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.clone())
        .unwrap_or_default()
}

/// Project a `(schema, fields, content)` triple into its [`StructuredView`], or
/// `None` if `schema` is not one of the recognized [`SCHEMA_ARTICLE`] …
/// [`SCHEMA_LIVE_ACTIVITY`] discriminators. The field-key contract here is the
/// exact twin of what the nostr bridge writes. Split out from
/// [`structured_view`] so the wasm boundary can project a loosely-decoded body
/// without reconstructing the full [`PostBody`] enum.
pub fn structured_view_from_parts(
    schema: &str,
    fields: &[StructuredField],
    content: Option<&str>,
) -> Option<StructuredView> {
    let body = content.unwrap_or_default().to_string();
    match schema {
        SCHEMA_ARTICLE => Some(StructuredView::Article {
            title: field(fields, "title"),
            summary: field(fields, "summary"),
            image: field(fields, "image"),
            content: body,
        }),
        SCHEMA_COMMUNITY => Some(StructuredView::Community {
            name: field(fields, "name"),
            description: field(fields, "description"),
            identifier: field(fields, "identifier"),
            rules: body,
        }),
        SCHEMA_CLASSIFIED => Some(StructuredView::Classified {
            title: field(fields, "title"),
            price: field(fields, "price"),
            location: field(fields, "location"),
            condition: field(fields, "condition"),
            content: body,
        }),
        SCHEMA_LIVE_ACTIVITY => Some(StructuredView::LiveActivity {
            title: field(fields, "title"),
            status: field(fields, "status"),
            streaming_url: field(fields, "streaming_url"),
            participants: field(fields, "participants"),
            summary: body,
        }),
        _ => None,
    }
}

/// Project a post body into its typed structured view, or `None` if it isn't a
/// recognized structured schema (plain posts, media, video, and the
/// unknown-kind `nostr/kind-N` fallback all yield `None` and render as text).
pub fn structured_view(body: &PostBody) -> Option<StructuredView> {
    let PostBody::Structured {
        schema,
        fields,
        content,
        ..
    } = body
    else {
        return None;
    };
    structured_view_from_parts(schema, fields, content.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(key: &str, value: &str) -> StructuredField {
        StructuredField {
            key: key.into(),
            value: value.into(),
        }
    }

    fn structured(schema: &str, fields: Vec<StructuredField>, content: Option<&str>) -> PostBody {
        PostBody::Structured {
            schema: schema.into(),
            fields,
            content: content.map(str::to_string),
            facets: vec![],
            items: vec![],
        }
    }

    #[test]
    fn projects_article() {
        let body = structured(
            SCHEMA_ARTICLE,
            vec![
                sf("title", "My Article"),
                sf("summary", "A summary"),
                sf("image", "https://img"),
            ],
            Some("# Body markdown"),
        );
        assert_eq!(
            structured_view(&body),
            Some(StructuredView::Article {
                title: "My Article".into(),
                summary: "A summary".into(),
                image: "https://img".into(),
                content: "# Body markdown".into(),
            })
        );
    }

    #[test]
    fn projects_community_with_content_as_rules() {
        let body = structured(
            SCHEMA_COMMUNITY,
            vec![
                sf("name", "Rustaceans"),
                sf("description", "We like Rust"),
                sf("identifier", "rustaceans"),
            ],
            Some("Be kind."),
        );
        assert_eq!(
            structured_view(&body),
            Some(StructuredView::Community {
                name: "Rustaceans".into(),
                description: "We like Rust".into(),
                identifier: "rustaceans".into(),
                rules: "Be kind.".into(),
            })
        );
    }

    #[test]
    fn projects_classified() {
        let body = structured(
            SCHEMA_CLASSIFIED,
            vec![
                sf("title", "Bike"),
                sf("price", "$100"),
                sf("location", "Oslo"),
                sf("condition", "used"),
            ],
            Some("A nice bike."),
        );
        assert_eq!(
            structured_view(&body),
            Some(StructuredView::Classified {
                title: "Bike".into(),
                price: "$100".into(),
                location: "Oslo".into(),
                condition: "used".into(),
                content: "A nice bike.".into(),
            })
        );
    }

    #[test]
    fn projects_live_activity_with_content_as_summary() {
        let body = structured(
            SCHEMA_LIVE_ACTIVITY,
            vec![
                sf("title", "Stream"),
                sf("status", "live"),
                sf("streaming_url", "https://watch"),
                sf("participants", "42"),
            ],
            Some("Come watch."),
        );
        assert_eq!(
            structured_view(&body),
            Some(StructuredView::LiveActivity {
                title: "Stream".into(),
                status: "live".into(),
                streaming_url: "https://watch".into(),
                participants: "42".into(),
                summary: "Come watch.".into(),
            })
        );
    }

    #[test]
    fn missing_field_projects_empty_string() {
        let body = structured(SCHEMA_ARTICLE, vec![sf("title", "Only title")], None);
        assert_eq!(
            structured_view(&body),
            Some(StructuredView::Article {
                title: "Only title".into(),
                summary: String::new(),
                image: String::new(),
                content: String::new(),
            })
        );
    }

    #[test]
    fn unknown_schema_is_none() {
        let body = structured("nostr/kind-9999", vec![sf("content", "x")], Some("x"));
        assert_eq!(structured_view(&body), None);
    }

    #[test]
    fn non_structured_body_is_none() {
        let body = PostBody::Text {
            content: "hello".into(),
            facets: vec![],
        };
        assert_eq!(structured_view(&body), None);
    }

    #[test]
    fn serializes_to_flat_kind_tagged_object() {
        let view = StructuredView::LiveActivity {
            title: "S".into(),
            status: "live".into(),
            streaming_url: "u".into(),
            participants: "1".into(),
            summary: "sum".into(),
        };
        let json = serde_json::to_value(&view).unwrap();
        // The web face discriminates on a flat `kind` tag (kebab-case variant)
        // with snake_case field names at top level.
        assert_eq!(json["kind"], "live-activity");
        assert_eq!(json["streaming_url"], "u");
        assert_eq!(json["summary"], "sum");
    }
}
