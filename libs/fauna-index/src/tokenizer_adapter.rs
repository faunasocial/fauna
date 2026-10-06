//! Tantivy `Tokenizer` impl that delegates to `fauna_mail::tokenize_positional`.
//!
//! Registered against Tantivy's TokenizerManager under the name
//! [`FAUNA_TOKENIZER_NAME`] and used by every searchable text field in the
//! schema. Emitting tokens through this adapter is the only way Tantivy
//! sees text in the index — guaranteeing all platforms produce
//! byte-identical posting lists for byte-identical input.
//!
//! Token byte offsets index into the **raw** field text — exactly what Tantivy
//! expects when it consumes them (snippet/highlight rendering via
//! `SnippetGenerator`). `fauna_mail::tokenize_positional` emits raw-input
//! offsets via its normalization-segment offset map (the S1 fix, 2026-08-02 —
//! the former NFKC-offset "Known limitation" here is resolved), and this
//! adapter forwards them verbatim.

use fauna_mail::tokenize_positional;
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

pub const FAUNA_TOKENIZER_NAME: &str = "fauna_canonical";

/// Version of the tokenizing pipeline whose output the segments hold — stamped
/// into [`crate::IndexManifest::tokenizer_version`] by every builder and
/// compared on merge. **Raise it whenever a change makes this adapter emit
/// different tokens or offsets for the same input**, which is what marks the
/// existing segments for a versioned re-index (spec D9).
///
/// It lives here, next to the adapter, rather than in any one builder: the
/// client builder (`libs/fauna-client-index`) and the MDA bridge write into
/// the *same* per-kind slices, so a version they don't agree on would let two
/// tokenizations share a segment chain with nothing recording the split.
///
/// `1` is the first real writer's value — the S1 raw-offset tokenizer
/// (2026-08-02). No byte written by an earlier pipeline exists at rest (the
/// unsealed nest-side writer's residue was purged; `crate::version`).
pub const TOKENIZER_PIPELINE_VERSION: u32 = 1;

/// The tokens a query over `terms` requires a document to contain — the query
/// side of the pipeline [`FaunaTokenizer`] applies on the write side.
///
/// Deduplicated and order-independent, because the question each token asks is
/// membership ("does this doc contain it?"), so a repeated token adds nothing
/// and position is not part of the test. Terms that tokenize to nothing (a
/// single character, pure punctuation) contribute no tokens and therefore no
/// constraint — the same thing they do to the MDA's linear hint scan, which is
/// what makes the two paths answer alike.
///
/// **Agreement with the write side is the whole point, and it is not a
/// coincidence to be re-derived at the call site.** `fauna_mail::tokenize` and
/// the `tokenize_positional` behind [`FaunaTokenizer`] run the *same* filters —
/// NFKC, `unicode_words`, lowercase, drop tokens under 2 chars, drop tokens
/// with no alphanumeric — and differ only in that one deduplicates and the
/// other carries positions. So a token this returns is exactly a distinct term
/// in the index, and a caller pairing this with
/// [`Index::query_all_of`](crate::Index::query_all_of) cannot silently ask for
/// a term no writer could have produced.
pub fn query_tokens(terms: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in terms {
        for token in fauna_mail::tokenize(term).tokens {
            if !out.contains(&token) {
                out.push(token);
            }
        }
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct FaunaTokenizer;

impl FaunaTokenizer {
    pub fn new() -> Self {
        FaunaTokenizer
    }
}

pub struct FaunaTokenStream {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Tokenizer for FaunaTokenizer {
    type TokenStream<'a> = FaunaTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let positional = tokenize_positional(text);
        let tokens = positional
            .into_iter()
            .map(|p| Token {
                offset_from: p.byte_offset_from,
                offset_to: p.byte_offset_to,
                position: p.position,
                text: p.text,
                position_length: 1,
            })
            .collect();
        FaunaTokenStream { tokens, cursor: 0 }
    }
}

impl TokenStream for FaunaTokenStream {
    fn advance(&mut self) -> bool {
        if self.cursor < self.tokens.len() {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.cursor - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.cursor - 1]
    }
}

#[cfg(test)]
mod tests {
    // `Tokenizer` and `TokenStream` traits are brought into scope via
    // `super::*` (the parent module imports them from `tantivy::tokenizer`),
    // so we do not need a separate `use` here.
    use super::*;

    #[test]
    fn produces_one_token_per_positional_token_in_order() {
        let mut tok = FaunaTokenizer::new();
        let mut stream = tok.token_stream("Hello WORLD hello");
        let mut collected = Vec::new();
        while stream.advance() {
            let t = stream.token();
            collected.push((t.text.clone(), t.position, t.offset_from, t.offset_to));
        }
        assert_eq!(
            collected,
            vec![
                ("hello".to_string(), 0, 0, 5),
                ("world".to_string(), 1, 6, 11),
                ("hello".to_string(), 2, 12, 17),
            ]
        );
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        let mut tok = FaunaTokenizer::new();
        let mut stream = tok.token_stream("");
        assert!(!stream.advance());
    }

    #[test]
    fn cloneable_for_tantivy_registration() {
        // Tantivy requires Tokenizer: Clone. This test is a compile-time check.
        let tok = FaunaTokenizer::new();
        let _clone = tok.clone();
    }
}
