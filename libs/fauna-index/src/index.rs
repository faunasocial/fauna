//! `Index` — owns one Tantivy `Index` and its `IndexWriter`.
//!
//! This is the heart of the crate: an in-memory Tantivy index built against
//! a fixed schema (field names are part of the wire format) and our
//! `FaunaTokenizer` for every searchable text field. `add_doc` / `commit`
//! / `query` give a complete in-memory round-trip plus kind and time-range
//! filtering. Segment serialization and on-disk loading land in later tasks.

use crate::tokenizer_adapter::{FAUNA_TOKENIZER_NAME, FaunaTokenizer};
use crate::types::{
    ContentId, ContentKind, FieldKind, IndexError, IndexedDoc, QueryHit, TimeRange,
};
use std::ops::Bound;
use std::path::{Path, PathBuf};

use tantivy::collector::TopDocs;
use tantivy::directory::RamDirectory;
use tantivy::merge_policy::NoMergePolicy;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{
    BytesOptions, FAST, Field, INDEXED, IndexRecordOption, STORED, Schema, SchemaBuilder,
    TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Directory, DocAddress, Index as TantivyIndex, IndexWriter, TantivyDocument, Term};

/// On-disk format version for sealed segment bytes. Bumps require an index
/// rebuild (see Plan 10).
const FORMAT_VERSION: u32 = 1;

/// Tantivy's atomic-write meta files. They sit outside the managed-files set
/// (which only tracks segment artifacts), so seal/open enumerate them
/// explicitly.
const META_JSON: &str = "meta.json";
const MANAGED_JSON: &str = ".managed.json";

/// Field handles for the canonical schema. Field *names* are part of the wire
/// format (`build_schema` pins them); the `Field` handles inside this struct
/// are runtime tokens that map name → column.
struct Fields {
    kind: Field,
    content_id: Field,
    timestamp_ns: Field,
    sender_actor_id: Field,
    secondary_id: Field,
    title: Field,
    body: Field,
    tags: Field,
}

/// Tantivy heap budget for the writer. 50 MiB matches the documented Tantivy
/// recommendation for small in-memory indexes; larger budgets only help when
/// indexing in big batches.
const WRITER_HEAP_BYTES: usize = 50_000_000;

fn build_schema() -> (Schema, Fields) {
    let mut sb = SchemaBuilder::new();

    let text_indexing = TextFieldIndexing::default()
        .set_tokenizer(FAUNA_TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let text_opts = TextOptions::default().set_indexing_options(text_indexing);

    // `kind` is also `STORED` so we can recover it on hits via the doc store
    // — `searcher.doc()` reads from the store, not the fast field column.
    let kind = sb.add_u64_field("kind", INDEXED | STORED | FAST);
    let content_id = sb.add_bytes_field(
        "content_id",
        BytesOptions::default().set_stored().set_indexed(),
    );
    let timestamp_ns = sb.add_i64_field("timestamp_ns", INDEXED | STORED | FAST);
    let sender_actor_id =
        sb.add_bytes_field("sender_actor_id", BytesOptions::default().set_stored());
    let title = sb.add_text_field("title", text_opts.clone());
    let body = sb.add_text_field("body", text_opts.clone());
    let tags = sb.add_text_field("tags", text_opts);
    // Stored, never indexed: a lookup carrier read back by `doc_identities`,
    // not a search surface (`IndexedDoc::secondary_id`). Added last in build
    // order deliberately — field ids are positional, so every pre-v3 field
    // keeps the id it had, and a v2-aware reader resolving by name never
    // notices the addition (what keeps the v3 bump additive).
    let secondary_id = sb.add_bytes_field("secondary_id", BytesOptions::default().set_stored());

    let schema = sb.build();
    (
        schema,
        Fields {
            kind,
            content_id,
            timestamp_ns,
            sender_actor_id,
            secondary_id,
            title,
            body,
            tags,
        },
    )
}

/// The canonical `IndexWriter` for fauna indexes.
///
/// Two non-default choices, both in service of determinism (spec D2 — two
/// indexes built from byte-identical inputs must seal to byte-identical bytes):
///
/// * **Single indexing thread.** A multi-threaded writer round-robins
///   documents across worker threads and each worker flushes its own segment,
///   so the document → segment partition (and therefore every segment's byte
///   content) varies run-to-run with thread scheduling. One thread means all
///   docs land in one segment in insertion order, deterministically.
/// * **`NoMergePolicy`.** Automatic merges run on background threads after
///   `commit()`, so whether `seal_to_bytes` observes the pre- or post-merge
///   segment set is a timing race. Disabling automatic merges makes the
///   segment set a pure function of the explicit operations. `merge_segments`'
///   explicit `IndexWriter::merge` call is unaffected — the policy only
///   governs *automatic* candidate selection.
///
/// Segment ids are still random UUIDs at this layer; [`canonicalize_segment_ids`]
/// rewrites them on the way into the sealed byte stream.
fn build_writer(inner: &TantivyIndex) -> Result<IndexWriter<TantivyDocument>, IndexError> {
    let writer: IndexWriter<TantivyDocument> =
        inner.writer_with_num_threads(1, WRITER_HEAP_BYTES)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    Ok(writer)
}

pub struct Index {
    inner: TantivyIndex,
    fields: Fields,
    writer: IndexWriter<TantivyDocument>,
    /// A clone of the underlying `RamDirectory` that backs `inner`.
    ///
    /// Tantivy's `Index::directory()` returns a `&ManagedDirectory` and there
    /// is no `Any`-based downcast on the trait, so we cannot recover the
    /// underlying RAM store after the fact. Holding our own clone is cheap
    /// (`RamDirectory` is `Arc<RwLock<...>>`) and lets `seal_to_bytes` read
    /// raw bytes via `atomic_read`.
    ram_dir: RamDirectory,
}

impl Index {
    /// Build an in-memory index. `kind` retrieval on hits relies on the
    /// stored field, so the schema builds it as `INDEXED | STORED | FAST`.
    pub fn create_in_ram() -> Result<Self, IndexError> {
        let (schema, fields) = build_schema();
        let ram_dir = RamDirectory::create();
        let inner = TantivyIndex::builder()
            .schema(schema)
            .open_or_create(ram_dir.clone())?;
        inner
            .tokenizers()
            .register(FAUNA_TOKENIZER_NAME, FaunaTokenizer::new());
        let writer = build_writer(&inner)?;
        Ok(Self {
            inner,
            fields,
            writer,
            ram_dir,
        })
    }

    /// Add (or replace) a document keyed by `content_id`. Calling `add_doc`
    /// twice with the same `content_id` replaces the prior document — a
    /// `delete_term(content_id)` is issued on the writer before the new
    /// `add_document`. This is what makes ingest replay-safe (a nest crash +
    /// re-deliver re-runs `add_doc` with the same id; the index converges).
    ///
    /// The replacement isn't visible until the next `commit()`.
    pub fn add_doc(&mut self, indexed: IndexedDoc) -> Result<(), IndexError> {
        // Delete any prior doc with this content_id first. `delete_term` is a
        // no-op if no matching doc exists, so this is safe on first insert.
        let id_term = Term::from_field_bytes(self.fields.content_id, &indexed.content_id.0);
        self.writer.delete_term(id_term);

        let mut td = TantivyDocument::new();
        td.add_u64(self.fields.kind, indexed.kind as u64);
        td.add_bytes(self.fields.content_id, indexed.content_id.0.as_slice());
        td.add_i64(self.fields.timestamp_ns, indexed.timestamp_ns);
        if let Some(actor_id) = &indexed.sender_actor_id {
            td.add_bytes(self.fields.sender_actor_id, actor_id.as_slice());
        }
        if let Some(secondary) = &indexed.secondary_id {
            td.add_bytes(self.fields.secondary_id, secondary.as_slice());
        }
        for f in &indexed.fields {
            let target = match f.kind {
                FieldKind::Title => self.fields.title,
                FieldKind::Body => self.fields.body,
                FieldKind::PreTokenizedTags => self.fields.tags,
            };
            td.add_text(target, &f.text);
        }
        self.writer.add_document(td)?;
        Ok(())
    }

    /// Commit the writer so subsequent `query()` calls see new docs.
    pub fn commit(&mut self) -> Result<(), IndexError> {
        self.writer.commit()?;
        Ok(())
    }

    /// Run a query against the title / body / tags fields, optionally filtered
    /// by content kind and timestamp range. An empty (or whitespace-only)
    /// query string returns no hits — we don't run match-all by default.
    pub fn query(
        &self,
        query_str: &str,
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: usize,
    ) -> Result<Vec<QueryHit>, IndexError> {
        if query_str.trim().is_empty() {
            return Ok(Vec::new());
        }
        let reader = self.inner.reader()?;
        let searcher = reader.searcher();

        let qp = QueryParser::for_index(
            &self.inner,
            vec![self.fields.title, self.fields.body, self.fields.tags],
        );
        let user_q = qp
            .parse_query(query_str)
            .map_err(|e| IndexError::QueryParse(e.to_string()))?;

        let mut combined: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        combined.push((Occur::Must, user_q));
        combined.extend(self.filter_clauses(kinds, range));

        let final_q = BooleanQuery::new(combined);

        let top = searcher.search(&final_q, &TopDocs::with_limit(limit).order_by_score())?;
        self.collect_hits(&searcher, top)
    }

    /// Run a query requiring **every** token in `tokens` to appear in a doc's
    /// title / body / tags, optionally filtered by content kind and timestamp
    /// range. An empty `tokens` slice returns no hits — like [`Self::query`],
    /// this never runs match-all by default.
    ///
    /// # Why this exists beside [`Self::query`]
    ///
    /// [`Self::query`] hands the caller's string to `QueryParser`, which is
    /// **OR-by-default** (no `set_conjunction_by_default`) — right for a Search
    /// page, where more words should mean more relevant rows rather than fewer.
    /// It is wrong for IMAP `SEARCH`: `SEARCH BODY a BODY b` is an AND over
    /// every token of every term, and a widened answer there is a protocol
    /// violation the user sees as noise.
    ///
    /// Serving that from `query` would mean assembling a `+"tok"` query string,
    /// which buys a second problem — the tokens would round-trip through the
    /// parser's own lexer, so a token carrying parser syntax could change the
    /// query's shape. Building the `BooleanQuery` directly closes both at once:
    /// no parser, so nothing to escape and nothing to default. Pair it with
    /// [`crate::query_tokens`], which produces tokens under the same pipeline
    /// the index was written with.
    ///
    /// `limit: None` means **every** match. That is not a convenience: a
    /// top-N answer to an IMAP `SEARCH` is a wrong answer, and one that fails
    /// silently and only on mailboxes big enough to exceed the cap.
    pub fn query_all_of(
        &self,
        tokens: &[String],
        kinds: &[ContentKind],
        range: Option<TimeRange>,
        limit: Option<usize>,
    ) -> Result<Vec<QueryHit>, IndexError> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let reader = self.inner.reader()?;
        let searcher = reader.searcher();

        let mut combined: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for token in tokens {
            // One required clause per token, satisfied by any of the three
            // searchable fields — the same "appears anywhere in the doc" test
            // the parser's default-field list gives `query`, minus the parse.
            let field_clauses: Vec<(Occur, Box<dyn Query>)> =
                [self.fields.title, self.fields.body, self.fields.tags]
                    .into_iter()
                    .map(|field| {
                        let term = Term::from_field_text(field, token);
                        let q: Box<dyn Query> =
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                        (Occur::Should, q)
                    })
                    .collect();
            combined.push((Occur::Must, Box::new(BooleanQuery::new(field_clauses))));
        }
        combined.extend(self.filter_clauses(kinds, range));

        let limit = match limit {
            Some(n) => n,
            // `TopDocs::with_limit` rejects 0, and an empty index legitimately
            // has nothing to rank, so answer it without asking tantivy.
            None => match searcher.num_docs() {
                0 => return Ok(Vec::new()),
                n => n as usize,
            },
        };
        if limit == 0 {
            return Ok(Vec::new());
        }

        let final_q = BooleanQuery::new(combined);
        let top = searcher.search(&final_q, &TopDocs::with_limit(limit).order_by_score())?;
        self.collect_hits(&searcher, top)
    }

    /// The `kind` and `timestamp` narrowing clauses shared by every query entry
    /// point, so a new one cannot accidentally drop a filter.
    fn filter_clauses(
        &self,
        kinds: &[ContentKind],
        range: Option<TimeRange>,
    ) -> Vec<(Occur, Box<dyn Query>)> {
        let mut out: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if !kinds.is_empty() {
            let kind_clauses: Vec<(Occur, Box<dyn Query>)> = kinds
                .iter()
                .map(|k| {
                    let term = Term::from_field_u64(self.fields.kind, *k as u64);
                    let q: Box<dyn Query> =
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                    (Occur::Should, q)
                })
                .collect();
            out.push((Occur::Must, Box::new(BooleanQuery::new(kind_clauses))));
        }
        if let Some(r) = range {
            // Half-open [start, end), as the 0.22 `new_i64(field, start..end)` read it.
            let range_q = RangeQuery::new(
                Bound::Included(Term::from_field_i64(self.fields.timestamp_ns, r.start_ns)),
                Bound::Excluded(Term::from_field_i64(self.fields.timestamp_ns, r.end_ns)),
            );
            out.push((Occur::Must, Box::new(range_q)));
        }
        out
    }

    /// Rebuild [`QueryHit`]s from a scored `TopDocs` result.
    fn collect_hits(
        &self,
        searcher: &tantivy::Searcher,
        top: Vec<(f32, DocAddress)>,
    ) -> Result<Vec<QueryHit>, IndexError> {
        let mut hits = Vec::with_capacity(top.len());
        for (score, doc_addr) in top {
            let td: TantivyDocument = searcher.doc(doc_addr)?;
            let kind_u64 = td
                .get_first(self.fields.kind)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| IndexError::SchemaMismatch("kind missing".into()))?;
            let kind = decode_kind(kind_u64)?;
            let content_id = td
                .get_first(self.fields.content_id)
                .and_then(|v| v.as_bytes())
                .ok_or_else(|| IndexError::SchemaMismatch("content_id missing".into()))?
                .to_vec();
            let timestamp_ns = td
                .get_first(self.fields.timestamp_ns)
                .and_then(|v| v.as_i64())
                .ok_or_else(|| IndexError::SchemaMismatch("timestamp_ns missing".into()))?;
            let sender_actor_id = td
                .get_first(self.fields.sender_actor_id)
                .and_then(|v| v.as_bytes())
                .map(|b| b.to_vec());
            hits.push(QueryHit {
                kind,
                content_id: ContentId(content_id),
                timestamp_ns,
                sender_actor_id,
                score,
            });
        }
        Ok(hits)
    }

    /// Every distinct `content_id` currently live in this index.
    ///
    /// The membership half of [`Self::add_doc`]'s upsert key, exposed so a
    /// *resumed* builder can answer "have I already indexed this?" without
    /// re-sealing anything. That question is what keeps a client whose receive
    /// path re-walks its whole corpus on every launch from re-publishing the
    /// entire index each time (`content-index.md` § Ingest triggers, v1 — the
    /// 2026-08-03 correction); the alternative, an advisory UID cursor, cannot
    /// express two independent id spaces under one `ContentKind`.
    ///
    /// Deleted docs are skipped, so an id that was upserted in a later segment
    /// is reported once per segment that still holds a live copy — callers
    /// collect into a set. O(live docs), paid once at resume rather than per
    /// message, which is what keeps the check off the non-blocking receive path.
    pub fn content_ids(&self) -> Result<Vec<ContentId>, IndexError> {
        let reader = self.inner.reader()?;
        let searcher = reader.searcher();
        let mut out = Vec::new();
        for (seg_ord, seg) in searcher.segment_readers().iter().enumerate() {
            let alive = seg.alive_bitset();
            for doc_id in 0..seg.max_doc() {
                if alive.is_some_and(|a| !a.is_alive(doc_id)) {
                    continue;
                }
                let td: TantivyDocument = searcher.doc(DocAddress::new(seg_ord as u32, doc_id))?;
                let bytes = td
                    .get_first(self.fields.content_id)
                    .and_then(|v| v.as_bytes())
                    .ok_or_else(|| IndexError::SchemaMismatch("content_id missing".into()))?;
                out.push(ContentId(bytes.to_vec()));
            }
        }
        Ok(out)
    }

    /// Every live doc's identity pair — [`Self::content_ids`] plus the stored
    /// secondary spelling ([`crate::types::DocIdentity`]).
    ///
    /// The coverage walk for a caller whose candidates are spelled in the
    /// *secondary* identity (the MDA's `SEARCH` path holds only nest message
    /// ids), and the reverse map that translates its matched content ids back.
    /// Same O(live docs) doc-store walk as `content_ids`, paid once per call.
    pub fn doc_identities(&self) -> Result<Vec<crate::types::DocIdentity>, IndexError> {
        let reader = self.inner.reader()?;
        let searcher = reader.searcher();
        let mut out = Vec::new();
        for (seg_ord, seg) in searcher.segment_readers().iter().enumerate() {
            let alive = seg.alive_bitset();
            for doc_id in 0..seg.max_doc() {
                if alive.is_some_and(|a| !a.is_alive(doc_id)) {
                    continue;
                }
                let td: TantivyDocument = searcher.doc(DocAddress::new(seg_ord as u32, doc_id))?;
                let content = td
                    .get_first(self.fields.content_id)
                    .and_then(|v| v.as_bytes())
                    .ok_or_else(|| IndexError::SchemaMismatch("content_id missing".into()))?;
                let secondary = td
                    .get_first(self.fields.secondary_id)
                    .and_then(|v| v.as_bytes())
                    .map(|b| b.to_vec());
                out.push(crate::types::DocIdentity {
                    content_id: ContentId(content.to_vec()),
                    secondary_id: secondary,
                });
            }
        }
        Ok(out)
    }
}

impl Index {
    /// Snapshot the in-memory Tantivy directory into a self-describing byte
    /// stream. Format:
    ///
    /// ```text
    /// u32 BE FORMAT_VERSION
    /// u32 BE file_count
    /// repeated file_count times:
    ///     u32 BE name_len
    ///     name_len bytes UTF-8 file name (forward-slash separated)
    ///     u64 BE body_len
    ///     body_len raw file bytes
    /// ```
    ///
    /// Plan 2 encrypts this byte stream as the on-disk segment file.
    ///
    /// **Determinism (spec D2).** Two indexes built from byte-identical inputs
    /// seal to byte-identical bytes: [`build_writer`]'s single-threaded,
    /// no-auto-merge configuration makes the segment *set* and each segment's
    /// byte *content* a pure function of the operations, [`canonicalize_segment_ids`]
    /// strips Tantivy's random per-segment UUIDs (which would otherwise leak
    /// into file names and `meta.json`), and entries are emitted sorted by name.
    ///
    /// Commits the writer before snapshotting, so callers should NOT call
    /// `commit()` separately before this — doing so is harmless but wasteful.
    pub fn seal_to_bytes(&mut self) -> Result<Vec<u8>, IndexError> {
        self.writer.commit()?;
        let files = canonicalize_segment_ids(snapshot_ram_directory(
            &self.ram_dir,
            self.inner.directory(),
        )?)?;

        let mut out = Vec::with_capacity(
            8 + files
                .iter()
                .map(|(n, b)| n.len() + b.len() + 12)
                .sum::<usize>(),
        );
        out.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        out.extend_from_slice(&(files.len() as u32).to_be_bytes());
        for (name, bytes) in &files {
            let name_bytes = name.as_bytes();
            out.extend_from_slice(&(name_bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        Ok(out)
    }

    /// Inverse of `seal_to_bytes`. Reconstructs a fresh `RamDirectory` from
    /// the byte stream, opens a Tantivy `Index` against it, re-registers the
    /// fauna tokenizer, and rebuilds the schema field handles from the
    /// schema embedded in `meta.json`.
    pub fn open_from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let entries = parse_wire_format(bytes)?;
        let ram_dir = RamDirectory::create();
        for (name, body) in &entries {
            ram_dir
                .atomic_write(Path::new(name), body)
                .map_err(|e| IndexError::SchemaMismatch(format!("write to ram_dir: {e}")))?;
        }
        let inner = TantivyIndex::open(ram_dir.clone())?;
        inner
            .tokenizers()
            .register(FAUNA_TOKENIZER_NAME, FaunaTokenizer::new());
        let fields = build_schema_from_index(&inner)?;
        let writer = build_writer(&inner)?;
        Ok(Self {
            inner,
            fields,
            writer,
            ram_dir,
        })
    }

    /// Seal the index as an AEAD-encrypted segment blob under `master`.
    ///
    /// Wraps `seal_to_bytes` output in the segment-format envelope from
    /// `crate::seal`. Master-key rotation can then call
    /// [`crate::seal::rewrap_segment_master_key`] to rewrap the data key
    /// without re-encrypting the body.
    pub fn seal_encrypted(
        &mut self,
        master: &crate::IndexMasterKey,
    ) -> Result<Vec<u8>, IndexError> {
        let plaintext = self.seal_to_bytes()?;
        crate::seal::seal_segment_bytes(&plaintext, master)
    }

    /// Inverse of [`Self::seal_encrypted`]: AEAD-decrypt under `master`, then rebuild
    /// the in-memory Tantivy index from the plaintext bytes.
    pub fn open_encrypted(
        sealed: &[u8],
        master: &crate::IndexMasterKey,
    ) -> Result<Self, IndexError> {
        let plaintext = crate::seal::open_segment_bytes(sealed, master)?;
        Self::open_from_bytes(&plaintext)
    }

    /// [`Self::seal_encrypted`]'s mail/calendar twin: same envelope, data key
    /// wrapped under the MSEK-derived [`crate::IndexSegmentKey`] (the v2
    /// per-kind key split — `content-index.md` § Encryption posture).
    pub fn seal_encrypted_mailcal(
        &mut self,
        key: &crate::IndexSegmentKey,
    ) -> Result<Vec<u8>, IndexError> {
        let plaintext = self.seal_to_bytes()?;
        crate::seal::seal_segment_bytes_mailcal(&plaintext, key)
    }

    /// Inverse of [`Self::seal_encrypted_mailcal`].
    pub fn open_encrypted_mailcal(
        sealed: &[u8],
        key: &crate::IndexSegmentKey,
    ) -> Result<Self, IndexError> {
        let plaintext = crate::seal::open_segment_bytes_mailcal(sealed, key)?;
        Self::open_from_bytes(&plaintext)
    }

    /// Compact a collection of sealed-bytes segments into a single sealed
    /// segment by loading them via `open_multi_segment` and calling Tantivy's
    /// writer-side merge.
    ///
    /// This is the heart of D8 compaction: when a kind has too many small
    /// segments (>16) or too many tombstoned docs (>25 % of a segment), the
    /// builder calls this to produce one merged segment, then updates the
    /// manifest (live_segments shrinks, tombstoned_segments grows). Operates
    /// on plaintext segment bytes — the builder is a capability position that
    /// holds the key material to open its inputs (the Plan-5 nest-side caller
    /// was deleted 2026-07-13).
    ///
    /// The production caller since 2026-08-03 is
    /// `fauna_client_index::IndexBuilder::prepare_fold`, which folds at
    /// flush time: it unseals the fold inputs, merges them with the batch the
    /// flush had already sealed, and re-seals the result under the same key.
    ///
    /// Empty input is allowed (returns the bytes of an empty index, useful
    /// as a "deleted everything" output).
    pub fn merge_segments(segment_bytes_list: &[Vec<u8>]) -> Result<Vec<u8>, IndexError> {
        let mut combined = Self::open_multi_segment(segment_bytes_list)?;

        // Collect the segment ids Tantivy sees in the combined view. With no
        // segments (empty input) merge is a no-op — we skip to seal_to_bytes.
        // Sort them so the merged segment concatenates its sources in a
        // deterministic order (`open_multi_segment` already remaps the ids to a
        // 1-based counter in input order, so sorting == input order; the
        // explicit sort makes the determinism guarantee independent of however
        // `searchable_segment_ids` happens to order its result).
        let mut segment_ids: Vec<tantivy::index::SegmentId> =
            combined.inner.searchable_segment_ids()?;
        segment_ids.sort();

        if segment_ids.len() > 1 {
            // `IndexWriter::merge` returns `FutureResult` which has a `.wait()`
            // synchronous method that does not require an async executor.
            combined.writer.merge(&segment_ids).wait()?;
            combined.writer.commit()?;
        }

        combined.seal_to_bytes()
    }

    /// Fold `segment_bytes_list` into one segment **with `replacements` applied**
    /// — the one way a doc already sealed into a published segment stops
    /// matching.
    ///
    /// [`Self::merge_segments`] deliberately cannot do this:
    /// `open_multi_segment` copies each input in as-is and neither input
    /// carries a delete for the other, so a same-content-id doc survives the
    /// fold in both versions (pinned by `tests/cross_segment_supersession.rs`).
    /// The asymmetry is in *who issues the delete*: a writer that has already
    /// published segment N cannot reach into it, but a writer opened **over**
    /// segment N can. This constructor is that writer — each replacement goes
    /// through [`Self::add_doc`], whose own `delete_term` therefore lands on the
    /// copied-in segment before the merge physically drops it.
    ///
    /// So an **append** kind whose content id can have its content replaced —
    /// mail, where the Message-ID collision rule lets a `Sent` copy displace a
    /// squatting `INBOX` one (`../../docs/goal/behavior/content-index-ingest.md`
    /// § Ingest triggers, v1) — retires the stale copy by rewriting the segment
    /// holding it, rather than by the whole-corpus republish a snapshot kind
    /// does. The caller tombstones the inputs exactly as it does for a fold:
    /// retirement stays tombstone-only.
    ///
    /// Empty `replacements` is exactly [`Self::merge_segments`].
    pub fn rebuild_with(
        segment_bytes_list: &[Vec<u8>],
        replacements: Vec<IndexedDoc>,
    ) -> Result<Vec<u8>, IndexError> {
        let mut combined = Self::open_multi_segment(segment_bytes_list)?;

        for doc in replacements {
            combined.add_doc(doc)?;
        }
        combined.commit()?;

        // Same determinism argument as `merge_segments`: sort so the merged
        // segment concatenates its sources in input order. The added docs land
        // in a segment of their own at commit, so a single input plus one
        // replacement is already two segments and does merge.
        let mut segment_ids: Vec<tantivy::index::SegmentId> =
            combined.inner.searchable_segment_ids()?;
        segment_ids.sort();

        if segment_ids.len() > 1 {
            // Merging is what physically drops the deleted docs. Without it the
            // retirement would survive only as a `.del` bitset riding the
            // sealed bytes — true, but resting on file-level details of the
            // seal round-trip rather than on the segment simply not holding
            // the doc any more.
            combined.writer.merge(&segment_ids).wait()?;
            combined.writer.commit()?;
        }

        combined.seal_to_bytes()
    }

    /// Open a multi-segment index built from a collection of sealed-bytes
    /// segments. Each input becomes one Tantivy segment in the combined view,
    /// so queries naturally union hits across inputs. The combined view is
    /// read-only in spirit: a writer is constructed (Tantivy needs one to
    /// open an index) but callers shouldn't add documents to it.
    ///
    /// Used by:
    /// - The query path (Plan 6) to union all live segments for a kind.
    /// - `merge_segments` (D8 compaction, Task 5 of this plan).
    ///
    /// Inputs MUST share the same schema (which they will if they all came
    /// from `Index::seal_to_bytes` produced by this crate at the same
    /// `FORMAT_VERSION`). Schema equality is checked via `tantivy::schema::Schema`'s
    /// derived `PartialEq`, which compares structural field definitions rather
    /// than a JSON serialization that could be sensitive to map iteration order.
    pub fn open_multi_segment(segment_bytes_list: &[Vec<u8>]) -> Result<Self, IndexError> {
        if segment_bytes_list.is_empty() {
            return Self::create_in_ram();
        }

        // Combined directory all input segments are copied into. Independently
        // sealed segments now share canonical ids (`seal_to_bytes` rewrites
        // every segment's id to a 1-based index — so two single-segment seals
        // both end up `…0001`), which *would* collide on copy; we remap each
        // input's ids to a process-of-elimination 1-based counter shared across
        // all inputs, renaming the input's files to match.
        let combined_dir = RamDirectory::create();

        // Combined IndexMeta: schema from the first input (must match across
        // all), opstamp = max(input opstamps) + 1, segments concatenated.
        let mut combined_segments_json: Vec<serde_json::Value> = Vec::new();
        // Track the deserialized Schema for structural PartialEq comparison
        // and its JSON form separately (needed for the combined meta.json write).
        let mut combined_schema: Option<Schema> = None;
        let mut combined_schema_json: Option<serde_json::Value> = None;
        let mut max_opstamp: u64 = 0;
        // Track filenames copied into combined_dir so we can build .managed.json
        // from the actual copied set rather than probing hardcoded extensions.
        let mut copied_filenames: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        // 1-based segment-id counter, shared across all inputs, so the combined
        // directory has globally unique segment ids in input-concatenation order.
        let mut next_segment_id: usize = 0;

        for seg_bytes in segment_bytes_list {
            let entries = parse_wire_format(seg_bytes)?;

            // Pull out meta.json — it carries the schema and the segment list.
            let meta_bytes = entries
                .iter()
                .find(|(n, _)| n == META_JSON)
                .map(|(_, b)| b.as_slice())
                .ok_or_else(|| {
                    IndexError::SchemaMismatch("input segment missing meta.json".into())
                })?;
            let meta_json: serde_json::Value = serde_json::from_slice(meta_bytes)
                .map_err(|e| IndexError::SchemaMismatch(format!("parse meta.json: {e}")))?;

            // Schema must match across all inputs. Deserialize the JSON schema
            // into a `tantivy::schema::Schema` (which derives PartialEq) so
            // the comparison is structural rather than JSON-byte-order dependent.
            let schema_val = meta_json
                .get("schema")
                .ok_or_else(|| IndexError::SchemaMismatch("meta.json missing schema".into()))?
                .clone();
            let schema: Schema = serde_json::from_value(schema_val.clone()).map_err(|e| {
                IndexError::SchemaMismatch(format!("parse schema from meta.json: {e}"))
            })?;
            match &combined_schema {
                Some(prev) if prev != &schema => {
                    return Err(IndexError::SchemaMismatch(
                        "inputs to open_multi_segment have different schemas".into(),
                    ));
                }
                Some(_) => {}
                None => {
                    combined_schema = Some(schema);
                    combined_schema_json = Some(schema_val);
                }
            }

            // Collect this input's segments, remapping each id to the shared
            // counter; build a per-input old→new map so the matching files get
            // renamed on copy.
            let mut id_remap: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if let Some(segs) = meta_json.get("segments").and_then(|s| s.as_array()) {
                for seg in segs {
                    let mut seg = seg.clone();
                    let old_dashless = {
                        let old =
                            seg.get("segment_id")
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    IndexError::SchemaMismatch(
                                        "input segment missing segment_id".into(),
                                    )
                                })?;
                        old.replace('-', "")
                    };
                    next_segment_id += 1;
                    let new_id = format!("{next_segment_id:032x}");
                    seg.as_object_mut()
                        .ok_or_else(|| {
                            IndexError::SchemaMismatch("meta.json segment is not an object".into())
                        })?
                        .insert(
                            "segment_id".to_string(),
                            serde_json::Value::String(new_id.clone()),
                        );
                    id_remap.insert(old_dashless, new_id);
                    combined_segments_json.push(seg);
                }
            }
            if let Some(op) = meta_json.get("opstamp").and_then(|o| o.as_u64()) {
                max_opstamp = max_opstamp.max(op);
            }

            // Copy every other file into the combined directory, swapping the
            // segment-id prefix per `id_remap`. meta.json and .managed.json are
            // rebuilt below. Track each copied filename so we can build
            // .managed.json from the real set.
            for (name, bytes) in &entries {
                if name == META_JSON || name == MANAGED_JSON {
                    continue;
                }
                let new_name = rename_with_id_map(name, &id_remap);
                combined_dir
                    .atomic_write(Path::new(&new_name), bytes)
                    .map_err(|e| IndexError::SchemaMismatch(format!("combined-dir write: {e}")))?;
                copied_filenames.insert(new_name);
            }
        }

        // Write the combined meta.json.
        let combined_meta = serde_json::json!({
            "segments": combined_segments_json,
            "schema": combined_schema_json
                .ok_or_else(|| IndexError::SchemaMismatch("no schema collected".into()))?,
            "opstamp": max_opstamp + 1,
            "payload": serde_json::Value::Null,
        });
        let combined_meta_bytes = serde_json::to_vec(&combined_meta)
            .map_err(|e| IndexError::SchemaMismatch(format!("encode combined meta: {e}")))?;
        combined_dir
            .atomic_write(Path::new(META_JSON), &combined_meta_bytes)
            .map_err(|e| IndexError::SchemaMismatch(format!("write combined meta: {e}")))?;

        // Write a fresh .managed.json listing every file that was actually
        // copied into the combined directory. Building from `copied_filenames`
        // avoids the need to probe for hardcoded Tantivy extension patterns
        // (which would miss delete files, whose real pattern is
        // `{uuid}.{opstamp}.del`, not `{uuid}.del`).
        let mut managed: Vec<String> = copied_filenames.into_iter().collect();
        managed.sort();
        let managed_bytes = serde_json::to_vec(&managed)
            .map_err(|e| IndexError::SchemaMismatch(format!("encode managed.json: {e}")))?;
        combined_dir
            .atomic_write(Path::new(MANAGED_JSON), &managed_bytes)
            .map_err(|e| IndexError::SchemaMismatch(format!("write managed.json: {e}")))?;

        let inner = TantivyIndex::open(combined_dir.clone())?;
        inner
            .tokenizers()
            .register(FAUNA_TOKENIZER_NAME, FaunaTokenizer::new());
        let fields = build_schema_from_index(&inner)?;
        let writer = build_writer(&inner)?;
        Ok(Self {
            inner,
            fields,
            writer,
            ram_dir: combined_dir,
        })
    }
}

/// Snapshot every file in `ram_dir` as `(name, bytes)`, sorted by name for
/// determinism.
///
/// Tantivy 0.22.1 has no `as_any()` downcast on the `Directory` trait, so we
/// cannot enumerate `RamDirectory` files directly through `Index::directory()`
/// (which returns `&ManagedDirectory`). Instead we own a clone of the
/// `RamDirectory` and combine two enumeration sources:
///
/// 1. `ManagedDirectory::list_managed_files()` — every segment artifact
///    written via `open_write` (`.idx`, `.term`, `.pos`, `.fieldnorm`,
///    `.store`, `.fast`, …).
/// 2. The two well-known atomic-write files (`meta.json`, `.managed.json`),
///    which are not tracked in the managed-paths set.
///
/// Lock files are not in the managed-files set, so they're naturally excluded
/// — the implementation doesn't actively filter them; they simply never appear
/// in either of the two enumeration sources above.
fn snapshot_ram_directory(
    ram_dir: &RamDirectory,
    managed: &tantivy::directory::ManagedDirectory,
) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    let mut paths: Vec<PathBuf> = managed.list_managed_files().into_iter().collect();
    for extra in [META_JSON, MANAGED_JSON] {
        let p = PathBuf::from(extra);
        let exists = ram_dir
            .exists(&p)
            .map_err(|e| IndexError::SchemaMismatch(format!("snapshot exists check: {e}")))?;
        if exists && !paths.contains(&p) {
            paths.push(p);
        }
    }
    paths.sort();

    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        // `RamDirectory::atomic_read` returns the raw bytes — for managed
        // segment files this includes the CRC footer that Tantivy added on
        // write, so re-opening sees identical bytes on every file.
        let bytes = ram_dir
            .atomic_read(&path)
            .map_err(|e| IndexError::SchemaMismatch(format!("snapshot read failed: {e}")))?;
        let name = path.to_string_lossy().into_owned();
        entries.push((name, bytes));
    }
    Ok(entries)
}

/// Rewrite Tantivy's random per-segment UUIDs — which leak into both file
/// names (`<uuid>.idx`, `<uuid>.term`, `<uuid>.<opstamp>.del`, …) and
/// `meta.json`'s `segment_id` fields — to deterministic ids derived from each
/// segment's 1-based position in `meta.json`'s ordered `segments` list.
///
/// That list order is itself deterministic: [`build_writer`]'s single-threaded,
/// no-auto-merge configuration means a fresh build's docs land in one segment
/// in insertion order, and [`Index::open_multi_segment`] concatenates inputs in
/// argument order — so position is a stable key, and two indexes built from
/// byte-identical inputs seal to byte-identical bytes (spec D2).
///
/// Operates on the `(name, bytes)` entries from [`snapshot_ram_directory`]:
/// replaces `meta.json`'s bytes with the re-encoded JSON, swaps the leading
/// id token on every segment file, and renames `.managed.json`'s entries the
/// same way. The new ids are written in the dashless 32-hex form Tantivy uses
/// for file names; its `meta.json` deserializer accepts that form too.
/// Re-sorts entries by name before returning (renames change ordering).
fn canonicalize_segment_ids(
    entries: Vec<(String, Vec<u8>)>,
) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    let meta_bytes = entries
        .iter()
        .find(|(n, _)| n == META_JSON)
        .map(|(_, b)| b.clone())
        .ok_or_else(|| IndexError::SchemaMismatch("snapshot missing meta.json".into()))?;
    let mut meta: serde_json::Value = serde_json::from_slice(&meta_bytes).map_err(|e| {
        IndexError::SchemaMismatch(format!("parse meta.json for canonicalization: {e}"))
    })?;

    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let segs = meta
        .get_mut("segments")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| IndexError::SchemaMismatch("meta.json missing segments array".into()))?;
    for (i, seg) in segs.iter_mut().enumerate() {
        let old_dashless = {
            let old = seg
                .get("segment_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    IndexError::SchemaMismatch("meta.json segment missing segment_id".into())
                })?;
            old.replace('-', "")
        };
        let canon = format!("{:032x}", i + 1);
        seg.as_object_mut()
            .ok_or_else(|| IndexError::SchemaMismatch("meta.json segment is not an object".into()))?
            .insert(
                "segment_id".to_string(),
                serde_json::Value::String(canon.clone()),
            );
        id_map.insert(old_dashless, canon);
    }
    let new_meta_bytes = serde_json::to_vec(&meta).map_err(|e| {
        IndexError::SchemaMismatch(format!("re-encode canonicalized meta.json: {e}"))
    })?;

    let mut out: Vec<(String, Vec<u8>)> = Vec::with_capacity(entries.len());
    for (name, bytes) in entries {
        if name == META_JSON {
            out.push((name, new_meta_bytes.clone()));
        } else if name == MANAGED_JSON {
            let list: Vec<String> = serde_json::from_slice(&bytes)
                .map_err(|e| IndexError::SchemaMismatch(format!("parse .managed.json: {e}")))?;
            let mut renamed: Vec<String> = list
                .iter()
                .map(|p| rename_with_id_map(p, &id_map))
                .collect();
            renamed.sort();
            let new_bytes = serde_json::to_vec(&renamed)
                .map_err(|e| IndexError::SchemaMismatch(format!("re-encode .managed.json: {e}")))?;
            out.push((name, new_bytes));
        } else {
            let new_name = rename_with_id_map(&name, &id_map);
            out.push((new_name, bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// If `name` has the shape `<segment-id>.<rest>` and `<segment-id>` (the
/// dashless 32-hex form Tantivy uses in file names) is a key in `id_map`,
/// return `<mapped-id>.<rest>`; otherwise return `name` unchanged. Touches
/// only the leading id token, so it handles plain segment components
/// (`<id>.idx`, `<id>.term`, …) and delete files (`<id>.<opstamp>.del`)
/// uniformly, and leaves non-segment names (`meta.json`, `.managed.json`) be.
fn rename_with_id_map(name: &str, id_map: &std::collections::HashMap<String, String>) -> String {
    match name.split_once('.') {
        Some((head, rest)) => match id_map.get(head) {
            Some(mapped) => format!("{mapped}.{rest}"),
            None => name.to_string(),
        },
        None => name.to_string(),
    }
}

/// Recover field handles from a Tantivy index whose schema is loaded from
/// `meta.json`. The names here MUST match `build_schema()` — they are the wire
/// format.
fn build_schema_from_index(idx: &TantivyIndex) -> Result<Fields, IndexError> {
    let schema = idx.schema();
    let get = |name: &str| {
        schema.get_field(name).map_err(|_| {
            IndexError::SchemaMismatch(format!("expected field `{name}` in opened index"))
        })
    };
    Ok(Fields {
        kind: get("kind")?,
        content_id: get("content_id")?,
        timestamp_ns: get("timestamp_ns")?,
        sender_actor_id: get("sender_actor_id")?,
        // Strict on purpose: a miss here is a real wrong-blob diagnosis,
        // not a compat case to paper over.
        secondary_id: get("secondary_id")?,
        title: get("title")?,
        body: get("body")?,
        tags: get("tags")?,
    })
}

fn decode_kind(discriminant: u64) -> Result<ContentKind, IndexError> {
    match discriminant {
        0 => Ok(ContentKind::Mail),
        1 => Ok(ContentKind::Calendar),
        2 => Ok(ContentKind::Conversation),
        3 => Ok(ContentKind::Post),
        4 => Ok(ContentKind::File),
        5 => Ok(ContentKind::Contact),
        6 => Ok(ContentKind::Draft),
        7 => Ok(ContentKind::Media),
        other => Err(IndexError::SchemaMismatch(format!(
            "unknown kind discriminant {other}"
        ))),
    }
}

/// Parse the `seal_to_bytes` wire format into `(name, bytes)` pairs.
///
/// Used by both `Index::open_from_bytes` (which writes the entries into a
/// fresh `RamDirectory`) and `Index::open_multi_segment` (which combines them
/// across inputs). Keeping a single parser eliminates ~60 lines of duplication
/// and ensures both callers validate the wire format identically.
///
/// `checked_sub` guards on remaining-byte counts are wasm32-safe: a naive
/// `cursor + N > bytes.len()` comparison could overflow on 32-bit `usize` when
/// a tampered payload carries `body_len` near `u32::MAX`.
fn parse_wire_format(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    if bytes.len() < 8 {
        return Err(IndexError::SchemaMismatch("sealed bytes too short".into()));
    }
    let mut cursor = 0usize;
    let version = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4 bytes available"),
    );
    cursor += 4;
    if version != FORMAT_VERSION {
        return Err(IndexError::SchemaMismatch(format!(
            "unsupported format version {version}, expected {FORMAT_VERSION}"
        )));
    }
    let count = u32::from_be_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .expect("4 bytes available"),
    ) as usize;
    cursor += 4;

    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if bytes.len().checked_sub(cursor).is_none_or(|rem| rem < 4) {
            return Err(IndexError::SchemaMismatch("truncated (name length)".into()));
        }
        let name_len = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .expect("4 bytes available"),
        ) as usize;
        cursor += 4;
        if bytes
            .len()
            .checked_sub(cursor)
            .is_none_or(|rem| rem < name_len)
        {
            return Err(IndexError::SchemaMismatch("truncated (name)".into()));
        }
        let name = std::str::from_utf8(&bytes[cursor..cursor + name_len])
            .map_err(|_| IndexError::SchemaMismatch("non-utf8 file name".into()))?
            .to_string();
        cursor += name_len;
        if bytes.len().checked_sub(cursor).is_none_or(|rem| rem < 8) {
            return Err(IndexError::SchemaMismatch("truncated (body length)".into()));
        }
        let body_len = u64::from_be_bytes(
            bytes[cursor..cursor + 8]
                .try_into()
                .expect("8 bytes available"),
        ) as usize;
        cursor += 8;
        if bytes
            .len()
            .checked_sub(cursor)
            .is_none_or(|rem| rem < body_len)
        {
            return Err(IndexError::SchemaMismatch("truncated (body)".into()));
        }
        let body = bytes[cursor..cursor + body_len].to_vec();
        cursor += body_len;
        entries.push((name, body));
    }
    if cursor != bytes.len() {
        return Err(IndexError::SchemaMismatch("trailing bytes".into()));
    }
    Ok(entries)
}
