# Archive fixtures — generated, never real

Hand-written miniature export archives in the exact directory layout and JSON
shape the platforms produce (`docs/goal/behavior/archive-import.md` § Parser
contract rule 7). Every name is fictional (`Test Owner`, `Friend One`,
`Friend Two`); no real export ever enters the repo. The `.jpg` files are a few
bytes of text — the parser hashes bytes and maps MIME from the extension, it
never decodes images.

- `facebook-json/` — the JSON-format Facebook export, one file per category,
  including the byte-wise `\u00XX` mojibake Facebook writes, one deliberately
  broken post (no `timestamp`) and one file the parser must ignore.
  A `privacy` field is present on posts to exercise audience parsing; real
  exports may omit it, in which case the audience is `Unknown`.
- `facebook-html/` — the HTML-format export skeleton; detected, then refused.
- `facebook-minimal/` — profile + one post only; every other category is
  "zero", never a refusal.
- `instagram-json/` — the JSON-format Instagram export: profile, feed posts
  (a one-photo post, a multi-photo post, one naming a photo the export left
  out, one broken record with no timestamp), an archived post, the owner's
  comments and likes (two in the same second), followers and following, one
  direct-message thread, and two files the parser must ignore (stories, ads).
  Instagram escapes text byte-wise like Facebook does.
- `instagram-html/` — the HTML-format export skeleton; detected, then refused.

Tests zip a tree in memory with `tests/common/mod.rs::fixture_zip`.
