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

Tests zip a tree in memory with `tests/common/mod.rs::fixture_zip`.
