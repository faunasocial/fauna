// The projection's OWN renderings must satisfy their Lexicon schemas
// (atproto-pds-full.md § F2 detail, *Lexicon-schema validation* — the
// "validates the CALLER'S record, which is not necessarily the committed one"
// sub-bullet).
//
// The write path's lexiconValidate (internal/atprotopds/repo_write.go) checks
// what an external app SUBMITTED. For a round-tripped collection the nest
// answers a record_override and the repo commits the projection's rendering
// instead, so that check says nothing about the bytes we actually publish. The
// guarantee cannot live on the request path either — a refusal there would
// arrive after the Fauna post already exists, with nowhere to send it — so it
// is a test-side guarantee, and this file is it.
//
// Two properties make it worth the cgo tier rather than a cheaper fake:
//
//   - The post/profile bytes come from the PRODUCTION builders the 7 apps
//     compose with (faunaFfi.BuildPost* / BuildEditedProfile*), not from
//     fixture literals written here. The rule: pin against what
//     production emits, never against each side's own idea of it.
//   - The rendering comes from ffiTranslator{} — the real shared-Rust
//     translator over the real FFI boundary — and is encoded with the same
//     atprotorepo.JSONRecordToDagCBOR the projector uses (projector.go:192,
//     :440), so what gets validated is byte-for-byte what the MST commit and
//     the firehose frame carry. Validating the projection_test.go fake's
//     output would prove nothing at all.
//
// The last leg (DagCBORRecordToJSONValue → Catalog.Validate) is the one thing
// production deliberately omits.
package main

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotolex"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// projSecret is a fixed 32-byte actor secret. Any 32 bytes are a valid ed25519
// secret, and the renderings under test do not depend on which — a fixed one
// just keeps failures reproducible.
var projSecret = func() []byte {
	s := make([]byte, 32)
	for i := range s {
		s[i] = byte(i + 1)
	}
	return s
}()

// A real CIDv1/raw/sha2-256, reused from internal/atprotorepo/blobs_test.go.
// It has to parse: JSONRecordToDagCBOR turns a blob ref's {"$link": …} into an
// actual CID link, so a placeholder string would fail at encode time and never
// reach the schema check we are here to run.
const projBlobCID = "bafkreibm6jg3ux5qumhcn2b3flc3tyu6dmlb4xa7u5bf44yegnrjhc4yeq"

// familyEmoji is one grapheme cluster of 25 UTF-8 bytes (7 code points joined
// by ZWJ). The translator's caps are counted in GRAPHEMES (300 for post text,
// 64/256 for displayName/description) while the lexicon's maxLength is counted
// in BYTES, so this is the input that separates the two — see the
// grapheme-stress arms below.
const familyEmoji = "\U0001F468‍\U0001F469‍\U0001F467‍\U0001F466"

// validateRendering runs one projected record through the exact encode step the
// projector uses and then through the vendored Lexicon catalog.
//
// KnownRecord is asserted first and separately: Catalog.Validate answers an
// error for an NSID it holds no schema for, so a catalog that had silently lost
// app.bsky.feed.post would otherwise present as an ordinary validation failure
// — or, if the check were ever inverted to skip-on-unknown, as a pass.
func validateRendering(t *testing.T, nsid, recordJSON string) {
	t.Helper()
	cat, err := atprotolex.Shared()
	if err != nil {
		t.Fatalf("load the vendored lexicon catalog: %v", err)
	}
	if !cat.KnownRecord(nsid) {
		t.Fatalf("the catalog holds no record schema for %s, so this test can prove nothing "+
			"about the projection's renderings (catalog holds %d record schemas)", nsid, cat.Len())
	}
	recordCBOR, err := atprotorepo.JSONRecordToDagCBOR(recordJSON)
	if err != nil {
		t.Fatalf("the projection's rendering is not dag-cbor-encodable, so it could never "+
			"reach a commit: %v\nrecord: %s", err, recordJSON)
	}
	value, err := atprotorepo.DagCBORRecordToJSONValue(recordCBOR)
	if err != nil {
		t.Fatalf("decode the record we just encoded: %v\nrecord: %s", err, recordJSON)
	}
	if err := cat.Validate(nsid, value); err != nil {
		t.Errorf("the projection's rendering does not satisfy the %s Lexicon schema: %v\n"+
			"record: %s\n"+
			"This is a LIVE PRODUCTION BUG, not a test-fixture problem: these bytes are what "+
			"the MST commit and the firehose frame carry, and no code on the projection path "+
			"checks them.", nsid, err, recordJSON)
	}
}

// projImage is one already-fetched, already-stored attachment, as the
// projection's blob walk hands it to the translator.
func projImage(alt string, withAspect bool) atprotorepo.ResolvedImage {
	img := atprotorepo.ResolvedImage{
		BlobCID:   projBlobCID,
		MIME:      "image/jpeg",
		SizeBytes: 12345,
		Alt:       alt,
	}
	if withAspect {
		w, h := uint32(1600), uint32(900)
		img.Width, img.Height = &w, &h
	}
	return img
}

func strptr(s string) *string { return &s }

// TestProjectedPostRecordsSatisfyTheirLexicon renders a spread of real Fauna
// posts through the real translator and asserts each satisfies
// app.bsky.feed.post.
//
// The arms are chosen for the shapes where a rendering can go wrong
// independently: the embed union has four distinct forms (none / images /
// video / recordWithMedia), reply and quote compose with all of them, facets
// carry their own byte-range constraints, and the two length arms probe the
// grapheme-vs-byte gap described at familyEmoji.
func TestProjectedPostRecordsSatisfyTheirLexicon(t *testing.T) {
	video := &atprotorepo.ResolvedVideo{
		BlobCID:      projBlobCID,
		MIME:         "video/mp4",
		SizeBytes:    987654,
		AspectWidth:  1920,
		AspectHeight: 1080,
	}
	reply := strptr(`{"parent_uri":"at://did:plc:parent/app.bsky.feed.post/3ktabc","parent_cid":"` +
		projBlobCID + `","root_uri":"at://did:plc:root/app.bsky.feed.post/3ktroot","root_cid":"` +
		projBlobCID + `"}`)
	quote := strptr(`{"uri":"at://did:plc:quoted/app.bsky.feed.post/3ktquot","cid":"` + projBlobCID + `"}`)

	cases := []struct {
		name       string
		build      func(t *testing.T) []byte
		reply      *string
		quote      *string
		images     []atprotorepo.ResolvedImage
		video      *atprotorepo.ResolvedVideo
		selfLabels []string
	}{
		{
			name:  "plain text",
			build: func(t *testing.T) []byte { return buildPost(t, "a perfectly ordinary post") },
		},
		{
			name: "text with tag facets",
			build: func(t *testing.T) []byte {
				return buildPostTagged(t, "tagged #one #two", []string{"one", "two"})
			},
		},
		{
			name: "unicode text with combining marks and RTL",
			build: func(t *testing.T) []byte {
				return buildPost(t, "café ☕ مرحبا שלום 🇳🇴 é done")
			},
		},
		{
			name:  "reply",
			build: func(t *testing.T) []byte { return buildPost(t, "replying") },
			reply: reply,
		},
		{
			name:  "quote",
			build: func(t *testing.T) []byte { return buildPost(t, "quoting") },
			quote: quote,
		},
		{
			// embed = app.bsky.embed.recordWithMedia, the nested form.
			name:   "quote with images",
			build:  func(t *testing.T) []byte { return buildPost(t, "quoting with pictures") },
			quote:  quote,
			images: []atprotorepo.ResolvedImage{projImage("a picture", true)},
		},
		{
			name:   "reply and quote together",
			build:  func(t *testing.T) []byte { return buildPost(t, "both at once") },
			reply:  reply,
			quote:  quote,
			images: []atprotorepo.ResolvedImage{projImage("alt", false)},
		},
		{
			name:   "one image, no aspect ratio",
			build:  func(t *testing.T) []byte { return buildPost(t, "one picture") },
			images: []atprotorepo.ResolvedImage{projImage("", false)},
		},
		{
			// The lexicon caps app.bsky.embed.images at 4; the translator takes
			// MAX_EMBED_IMAGES. Handing it more is what proves the cap is
			// applied on the way out rather than assumed by the caller.
			name:  "more images than the embed cap",
			build: func(t *testing.T) []byte { return buildPost(t, "many pictures") },
			images: []atprotorepo.ResolvedImage{
				projImage("one", true), projImage("two", true), projImage("three", true),
				projImage("four", true), projImage("five", true), projImage("six", true),
			},
		},
		{
			name:  "video",
			build: func(t *testing.T) []byte { return buildPost(t, "a moving picture") },
			video: video,
		},
		{
			name:  "video with a quote",
			build: func(t *testing.T) []byte { return buildPost(t, "quoting, with video") },
			quote: quote,
			video: video,
		},
		{
			name:       "self labels",
			build:      func(t *testing.T) []byte { return buildPost(t, "labelled") },
			selfLabels: []string{"!no-unauthenticated"},
		},
		{
			name: "text far over the grapheme cap",
			build: func(t *testing.T) []byte {
				return buildPost(t, strings.Repeat("long ", 400))
			},
		},
		{
			// 300 graphemes of 25 bytes each = 7500 bytes of text — inside the
			// translator's 300-grapheme cap and far outside the lexicon's
			// maxLength, which is counted in bytes.
			name: "text at the grapheme cap in multi-byte clusters",
			build: func(t *testing.T) []byte {
				return buildPost(t, strings.Repeat(familyEmoji, 300))
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			record, ok, err := ffiTranslator{}.PostRecord(
				tc.build(t), tc.reply, tc.quote, tc.images, tc.video, tc.selfLabels)
			if err != nil {
				t.Fatalf("the real translator refused a post the projection would publish: %v", err)
			}
			if !ok {
				t.Fatalf("the translator declined to project this post; every arm here carries " +
					"text, so a skip means the arm no longer tests what it names")
			}
			validateRendering(t, atprotorepo.NsidFeedPost, record)
		})
	}
}

// TestProjectedProfileRecordsSatisfyTheirLexicon is the profile half: the
// record is smaller, but its two text fields carry the same grapheme-vs-byte
// gap as post text, and its two blob refs are rendered by the same
// build_blob_ref.
func TestProjectedProfileRecordsSatisfyTheirLexicon(t *testing.T) {
	avatar := projImage("", false)
	banner := projImage("", true)

	cases := []struct {
		name        string
		displayName *string
		bio         *string
		links       []faunaFfi.FfiProfileLink
		avatar      *atprotorepo.ResolvedImage
		banner      *atprotorepo.ResolvedImage
	}{
		{
			// The whole record is optional fields, so the empty profile is a
			// real shape the projection publishes on a fresh account.
			name: "empty profile",
		},
		{
			name:        "display name and bio",
			displayName: strptr("Ada Lovelace"),
			bio:         strptr("first programmer; enjoys analytical engines"),
		},
		{
			// links have no app.bsky.actor.profile field at all — this arm
			// exists to prove they are dropped rather than rendered into
			// something the schema rejects.
			name:        "links present",
			displayName: strptr("Linked"),
			links: []faunaFfi.FfiProfileLink{
				{Label: "site", Uri: "https://example.invalid/"},
				{Label: "other", Uri: "https://example.invalid/other"},
			},
		},
		{
			name:        "avatar and banner",
			displayName: strptr("Pictured"),
			bio:         strptr("with pictures"),
			avatar:      &avatar,
			banner:      &banner,
		},
		{
			name:        "avatar only",
			displayName: strptr("Half pictured"),
			avatar:      &avatar,
		},
		{
			name:        "display name and bio far over the grapheme caps",
			displayName: strptr(strings.Repeat("name ", 100)),
			bio:         strptr(strings.Repeat("bio ", 500)),
		},
		{
			// 64 graphemes × 25 bytes = 1600 bytes of displayName, 256 × 25 =
			// 6400 of description — both inside the translator's grapheme caps.
			name:        "display name and bio at the grapheme caps in multi-byte clusters",
			displayName: strptr(strings.Repeat(familyEmoji, 64)),
			bio:         strptr(strings.Repeat(familyEmoji, 256)),
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			body, err := faunaFfi.BuildEditedProfile(projSecret, nil, nil, tc.displayName, tc.bio, tc.links)
			if err != nil {
				t.Fatalf("build the profile bytes the apps would publish: %v", err)
			}
			record, err := ffiTranslator{}.ProfileRecord(body, tc.avatar, tc.banner)
			if err != nil {
				t.Fatalf("the real translator refused a profile the projection would publish: %v", err)
			}
			validateRendering(t, atprotorepo.NsidProfile, record)
		})
	}
}

// TestProjectedRecordsAreCappedInBytesNotJustGraphemes is the regression pin
// for the bug these files were written to find (2026-08-03): the translator
// capped `text` / `displayName` / `description` by GRAPHEME count only, while
// the lexicon caps each by grapheme count AND UTF-8 byte length. For ASCII the
// two coincide, which is what hid it; one family-emoji cluster is 1 grapheme of
// 25 bytes, so 300 of them are 7500 bytes inside a 300-grapheme cap.
//
// It also pins the byte ceilings themselves, which the fix hard-codes in
// libs/fauna-bridge-atproto/src/outbound.rs. They were derived empirically from
// this same catalog rather than transcribed, and this is what keeps them honest:
// a refresh that moved a ceiling DOWN would turn these arms red (the rendering
// would no longer validate) instead of silently invalidating every record the
// projection publishes. A ceiling moving UP only costs us some text.
func TestProjectedRecordsAreCappedInBytesNotJustGraphemes(t *testing.T) {
	t.Run("post text", func(t *testing.T) {
		// Exactly at the grapheme cap; 7500 bytes before truncation.
		record, ok, err := ffiTranslator{}.PostRecord(
			buildPost(t, strings.Repeat(familyEmoji, 300)), nil, nil, nil, nil, nil)
		if err != nil || !ok {
			t.Fatalf("translate: err=%v projected=%v", err, ok)
		}
		assertFieldWithinBytes(t, record, "text", 3000)
		validateRendering(t, atprotorepo.NsidFeedPost, record)
	})

	t.Run("profile display name and description", func(t *testing.T) {
		body, err := faunaFfi.BuildEditedProfile(projSecret, nil, nil,
			strptr(strings.Repeat(familyEmoji, 64)), strptr(strings.Repeat(familyEmoji, 256)), nil)
		if err != nil {
			t.Fatalf("build profile: %v", err)
		}
		record, err := ffiTranslator{}.ProfileRecord(body, nil, nil)
		if err != nil {
			t.Fatalf("translate: %v", err)
		}
		assertFieldWithinBytes(t, record, "displayName", 640)
		assertFieldWithinBytes(t, record, "description", 2560)
		validateRendering(t, atprotorepo.NsidProfile, record)
	})
}

// assertFieldWithinBytes checks the rendered field against the byte ceiling
// directly, so a failure says WHICH field overran and by how much — the schema
// error alone reports only a length, and a record with two capped fields would
// leave the reader guessing which one it meant.
func assertFieldWithinBytes(t *testing.T, recordJSON, field string, maxBytes int) {
	t.Helper()
	var m map[string]any
	if err := json.Unmarshal([]byte(recordJSON), &m); err != nil {
		t.Fatalf("the rendering is not JSON: %v", err)
	}
	s, ok := m[field].(string)
	if !ok {
		t.Fatalf("the rendering has no %q string field, so this arm asserts nothing: %s",
			field, recordJSON)
	}
	if len(s) > maxBytes {
		t.Errorf("%s rendered %d UTF-8 bytes, over the lexicon's %d — truncation is counting "+
			"graphemes only", field, len(s), maxBytes)
	}
}

func buildPost(t *testing.T, body string) []byte {
	t.Helper()
	b, err := faunaFfi.BuildPost(projSecret, body)
	if err != nil {
		t.Fatalf("build the post bytes the apps would publish: %v", err)
	}
	return b
}

func buildPostTagged(t *testing.T, body string, tags []string) []byte {
	t.Helper()
	b, err := faunaFfi.BuildPostTagged(projSecret, body, tags)
	if err != nil {
		t.Fatalf("build the tagged post bytes the apps would publish: %v", err)
	}
	return b
}
