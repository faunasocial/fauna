//! `Index::query_all_of` + `query_tokens` — the AND-over-tokens query the IMAP
//! `SEARCH` body axis needs (`content-index.md` § Where the index is built;
//! rollout S5's query half).
//!
//! The property under test is **parity with the MDA's linear hint scan**, which
//! is what the swap replaces. That scan's rule, from
//! `bins/fauna-bridges/internal/mda/imap/search.go`'s `bodySearch`:
//!
//! > tokenize each term; a message matches iff **every** token of **every**
//! > term appears in its token set; terms tokenizing to nothing impose no
//! > constraint.
//!
//! Every test here is that sentence from one side or another. The reason it
//! needs its own entry point at all is that `Index::query` is OR-by-default
//! (`QueryParser` with no `set_conjunction_by_default`), so swapping the scan
//! onto it would have silently *widened* every user's search.

use fauna_index::{
    ContentId, ContentKind, FieldKind, Index, IndexedDoc, IndexedField, query_tokens,
};

fn body_doc(id: &[u8], kind: ContentKind, ts: i64, body: &str) -> IndexedDoc {
    IndexedDoc {
        kind,
        content_id: ContentId(id.to_vec()),
        timestamp_ns: ts,
        sender_actor_id: None,
        secondary_id: None,
        fields: vec![IndexedField {
            kind: FieldKind::Body,
            text: body.to_string(),
        }],
    }
}

fn terms(ts: &[&str]) -> Vec<String> {
    ts.iter().map(|s| s.to_string()).collect()
}

fn ids(hits: &[fauna_index::QueryHit]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = hits.iter().map(|h| h.content_id.0.clone()).collect();
    out.sort();
    out
}

/// A mail slice with three docs whose overlap makes AND and OR distinguishable.
fn slice() -> Index {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(
        b"both",
        ContentKind::Mail,
        1_000,
        "alpha and beta",
    ))
    .unwrap();
    idx.add_doc(body_doc(b"alpha", ContentKind::Mail, 2_000, "alpha only"))
        .unwrap();
    idx.add_doc(body_doc(b"beta", ContentKind::Mail, 3_000, "beta only"))
        .unwrap();
    idx.commit().unwrap();
    idx
}

/// The defect the whole entry point exists to prevent: two terms must NARROW.
#[test]
fn two_terms_require_both_tokens() {
    let idx = slice();
    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["alpha", "beta"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        ids(&hits),
        vec![b"both".to_vec()],
        "every token of every term must appear — this is the AND the scan does"
    );
}

/// Guards the reason `query_all_of` exists rather than a `+\"tok\"` string
/// through `Index::query`. If this ever fails, the swap can be deleted.
#[test]
fn the_parser_entry_point_is_still_or_by_default() {
    let idx = slice();
    let hits = idx
        .query("alpha beta", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(
        ids(&hits).len(),
        3,
        "Index::query is OR-by-default; query_all_of exists because SEARCH is not"
    );
}

/// A multi-token term (`SEARCH BODY \"alpha beta\"`) binds every one of its
/// tokens, exactly as the scan's inner loop does.
#[test]
fn a_multi_token_term_requires_each_of_its_tokens() {
    let idx = slice();
    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["alpha beta"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![b"both".to_vec()]);
}

/// The scan drops a term that tokenizes to nothing, so it imposes no
/// constraint; an unconstrained query must still not become match-all here.
#[test]
fn a_term_that_tokenizes_to_nothing_contributes_no_tokens() {
    assert!(query_tokens(&terms(&["-", "!", "x"])).is_empty());
    let idx = slice();
    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["!"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert!(
        hits.is_empty(),
        "no tokens is not match-all at this layer — the caller decides what an \
         unconstrained body search means (see FfiMailIndexSession::answer_body_search)"
    );
}

/// Tokens are deduplicated and order-independent: repeating a term cannot
/// change the answer, which is what makes the AND a set test.
#[test]
fn repeated_and_reordered_terms_answer_alike() {
    let idx = slice();
    let a = idx
        .query_all_of(
            &query_tokens(&terms(&["alpha", "beta", "alpha"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    let b = idx
        .query_all_of(
            &query_tokens(&terms(&["beta", "alpha"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert_eq!(ids(&a), ids(&b));
    assert_eq!(ids(&a), vec![b"both".to_vec()]);
}

/// A token carrying query-parser syntax must be matched literally. This is the
/// escaping trap that building the `BooleanQuery` directly removes — via
/// `Index::query` the same input would be lexed as an operator.
#[test]
fn a_token_carrying_parser_syntax_is_matched_literally() {
    let mut idx = Index::create_in_ram().unwrap();
    // `don't` survives tokenization as one word (apostrophes are word-internal
    // under UAX#29), and an apostrophe is exactly the sort of character a query
    // string would have to escape.
    idx.add_doc(body_doc(b"has", ContentKind::Mail, 1_000, "don't panic"))
        .unwrap();
    idx.add_doc(body_doc(b"lacks", ContentKind::Mail, 2_000, "panic later"))
        .unwrap();
    idx.commit().unwrap();

    let toks = query_tokens(&terms(&["don't"]));
    assert_eq!(toks, vec!["don't".to_string()], "one word, kept verbatim");
    let hits = idx
        .query_all_of(&toks, &[ContentKind::Mail], None, None)
        .unwrap();
    assert_eq!(ids(&hits), vec![b"has".to_vec()]);
}

/// Case and Unicode folding agree with the write side, so a user's `SEARCH`
/// finds mail regardless of how either was spelled.
#[test]
fn folding_agrees_with_the_write_side() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"m", ContentKind::Mail, 1_000, "OFFICE Ｍemo"))
        .unwrap();
    idx.commit().unwrap();

    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["office", "memo"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![b"m".to_vec()]);
}

/// **The silent-truncation trap.** `limit: None` must return every match, not a
/// top-N page: an IMAP `SEARCH` that drops matches is a wrong answer, and one
/// that only goes wrong on mailboxes large enough to exceed the cap.
#[test]
fn an_unbounded_limit_returns_every_match_not_a_page() {
    let mut idx = Index::create_in_ram().unwrap();
    for i in 0..250u32 {
        idx.add_doc(body_doc(
            format!("m{i}").as_bytes(),
            ContentKind::Mail,
            i as i64,
            "shared token here",
        ))
        .unwrap();
    }
    idx.commit().unwrap();

    let toks = query_tokens(&terms(&["shared"]));
    let all = idx
        .query_all_of(&toks, &[ContentKind::Mail], None, None)
        .unwrap();
    assert_eq!(all.len(), 250, "None must mean every match");

    let capped = idx
        .query_all_of(&toks, &[ContentKind::Mail], None, Some(10))
        .unwrap();
    assert_eq!(capped.len(), 10, "an explicit limit still pages");
}

/// An empty index answers `None` without asking tantivy for a zero-sized page
/// (`TopDocs::with_limit(0)` rejects), so a first-run session is a normal state.
#[test]
fn an_empty_index_answers_an_unbounded_query() {
    let idx = Index::create_in_ram().unwrap();
    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["anything"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert!(hits.is_empty());
}

/// The kind ceiling still applies — `query_all_of` must not become a way around
/// the per-kind key split (rule #7's blast radius).
#[test]
fn the_kind_filter_still_narrows() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"m", ContentKind::Mail, 1_000, "shared"))
        .unwrap();
    idx.add_doc(body_doc(b"p", ContentKind::Post, 2_000, "shared"))
        .unwrap();
    idx.commit().unwrap();

    let hits = idx
        .query_all_of(
            &query_tokens(&terms(&["shared"])),
            &[ContentKind::Mail],
            None,
            None,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![b"m".to_vec()]);
}
