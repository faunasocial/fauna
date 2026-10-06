// Behavioural cover for `clearSent` — the rule a successful post submit clears
// the composer by (feed.md § User actions, the `post-submit-button` row): it
// clears exactly what it SENT, and nothing the user typed or picked after the
// click.
//
// The case that matters is the second one below. On web the window between the
// click and the submit resolving is the whole submit — provenance read,
// thumbnail, two blob uploads, `fauna.posts.create`, then the post-submit
// reload — and a reload the page's own load started later can put the new post
// on screen while that submit is still waiting on its own now-superseded fetch.
// A user who sees their post land and starts the next one then had it erased
// when the first submit resolved: body, tags and picked file, silently. That
// is how `test_feed.py`'s second compose in a journey sat with
// `post-submit-button` disabled for the full 90 s, `compose-file-ready` gone
// and `error-message` empty.

import { assert, assertEquals } from "jsr:@std/assert";
import { clearSent } from "./compose-sent.ts";

type Fields = {
  body: string;
  tags: string;
  file: File | null;
  fileData: Uint8Array | null;
  sellSubscribersFree: boolean;
};

const EMPTY: Fields = {
  body: "",
  tags: "",
  file: null,
  fileData: null,
  sellSubscribersFree: true,
};

function picked(name: string): { file: File; fileData: Uint8Array } {
  const fileData = new Uint8Array([1, 2, 3]);
  return { file: new File([fileData], name, { type: "image/png" }), fileData };
}

Deno.test("an untouched composer clears to empty after a successful submit", () => {
  const { file, fileData } = picked("a.png");
  const sent: Fields = { body: "first post", tags: "rust", file, fileData, sellSubscribersFree: false };
  // The live composer is exactly what was sent — nobody touched it.
  const next = clearSent({ ...sent }, sent, EMPTY);
  assertEquals(next, EMPTY);
});

Deno.test("what the user typed and picked AFTER the click survives the first submit's clear", () => {
  const first = picked("plain.png");
  const sent: Fields = { body: "first post", tags: "", ...first, sellSubscribersFree: true };
  // While the first submit was still in flight the user wrote the next post and
  // picked its photo; the tags field they left alone.
  const second = picked("signed.png");
  const live: Fields = { body: "second post", tags: "", ...second, sellSubscribersFree: true };

  const next = clearSent(live, sent, EMPTY);

  assertEquals(next.body, "second post", "the next post's text must not be erased");
  assert(next.file === second.file, "the newly picked file must not be dropped");
  assert(next.fileData === second.fileData, "the newly picked file's bytes must not be dropped");
});

Deno.test("a file is compared by identity, not by name", () => {
  // Re-picking a file with the same name is a new pick: the user chose it after
  // the click, so it is new input even though it looks the same.
  const first = picked("photo.png");
  const again = picked("photo.png");
  const sent: Fields = { ...EMPTY, body: "x", ...first };
  const next = clearSent({ ...sent, ...again }, sent, EMPTY);
  assert(next.file === again.file, "a re-pick after the click must survive");
  assertEquals(next.body, "", "the unchanged body still clears");
});

Deno.test("a partial edit keeps only the edited field", () => {
  const sent: Fields = { ...EMPTY, body: "posted", tags: "a, b" };
  // The user replaced the text but left the tags: the tags went out with the
  // post, so they clear; the text is the next post's, so it stays.
  const next = clearSent({ ...sent, body: "next one" }, sent, EMPTY);
  assertEquals(next, { ...EMPTY, body: "next one" });
});

Deno.test("clearSent returns a fresh object and never mutates its inputs", () => {
  const sent: Fields = { ...EMPTY, body: "posted" };
  const live: Fields = { ...sent };
  const next = clearSent(live, sent, EMPTY);
  assert(next !== live && next !== sent && next !== EMPTY);
  assertEquals(live.body, "posted");
  assertEquals(sent.body, "posted");
  assertEquals(EMPTY.body, "");
});

// The sticky audience group (owner ruling,
// mirrored from `FeedComposeState::clear_sent` in
// `libs/fauna-feed/src/compose.rs`): a gate tier/room/sale must not silently
// widen to Public just because the user kept typing the next post.

type AudienceFields = Fields & { gateTier: string; gatePreview: string };

const EMPTY_AUDIENCE: AudienceFields = { ...EMPTY, gateTier: "", gatePreview: "" };

const STICKY = {
  contentKeys: ["body", "tags", "file", "fileData"] as (keyof AudienceFields)[],
  stickyKeys: ["gateTier", "gatePreview"] as (keyof AudienceFields)[],
};

Deno.test("a sticky audience survives when other content changed since the click", () => {
  const sent: AudienceFields = { ...EMPTY_AUDIENCE, body: "first post", gateTier: "supporters", gatePreview: "teaser" };
  // The user kept typing the next post without re-touching the picker.
  const live: AudienceFields = { ...sent, body: "second post" };
  const next = clearSent(live, sent, EMPTY_AUDIENCE, STICKY);
  assertEquals(next.gateTier, "supporters", "the audience must not clear to Public while the user keeps typing");
  assertEquals(next.gatePreview, "teaser", "the teaser stays with its sticky tier");
});

Deno.test("a sticky audience still clears when the composer is otherwise untouched", () => {
  const sent: AudienceFields = { ...EMPTY_AUDIENCE, body: "first post", gateTier: "supporters", gatePreview: "teaser" };
  const next = clearSent({ ...sent }, sent, EMPTY_AUDIENCE, STICKY);
  assertEquals(next, EMPTY_AUDIENCE);
});
