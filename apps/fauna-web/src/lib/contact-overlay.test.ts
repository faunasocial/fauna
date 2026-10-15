// Deno tests for the private contact overlay's SPA glue (`./contact-overlay.ts`;
// `docs/goal/ui/contacts.md` § The private overlay). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/contact-overlay.test.ts
//
// The names, the staging and the Save's answer are shared Rust's (pinned beside
// `fauna_account_seams::contact_overlays::save_staged` and the wasm face's
// round trip in `libs/fauna-wasm/src/account_runtime.rs`). What can still go
// wrong on this side: a tab with no manager reading the manager's face (or
// none), a face minted per read, and a refused Save painting nothing — the
// absent-runtime bound says a tab that hosts no runtime refuses the Save
// VISIBLY on `error-message`, never silently
// (`account-client-lifecycle.md` § The client-side lifecycle → *Bounds, stated*).

import {
  overlayFaces,
  savePrivateSection,
  type OverlayEditorFace,
  type OverlayFace,
  type SaveAnswer,
} from './contact-overlay.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

/** A face in the runtime-less shape: every name the public label, and a Save
 *  answered as the shared `save_staged` answers a seat with no manager. */
function fakeFace(tag: string, answer: SaveAnswer): OverlayFace {
  let staged = false;
  const editor: OverlayEditorFace = {
    form: () => ({ nickname: staged ? 'Mum' : '', notes: '', labels: [] }),
    isStaged: () => staged,
    setNickname: () => {
      staged = true;
    },
    setNotes: () => {
      staged = true;
    },
    addLabel: () => undefined,
    removeLabel: () => {},
    reset: () => {
      staged = false;
    },
    save: () => {
      if (answer.kind === 'saved') staged = false;
      return Promise.resolve(answer);
    },
  };
  return {
    revision: () => 0,
    peerLabel: (_d, handle, id) => ({ primary: `${tag}:${handle ?? id.slice(0, 12)}`, public: null }),
    labelsLine: () => undefined,
    matchesFilter: () => true,
    subscriptionAuthorLabel: () => tag,
    editor: () => editor,
  };
}

const ELSEWHERE: SaveAnswer = {
  kind: 'notReady',
  text: { key: 'profile.private_save_elsewhere' },
};

Deno.test('a tab with no manager reads the runtime-less face, and one face per manager', () => {
  let built = 0;
  let homeless = 0;
  const faceFor = overlayFaces<{ id: string }>(
    (m) => {
      built += 1;
      return fakeFace(`m${m.id}`, { kind: 'saved', text: null });
    },
    () => {
      homeless += 1;
      return fakeFace('none', ELSEWHERE);
    },
  );

  const none = faceFor(null);
  assert(none.peerLabel(null, 'alice', 'ab'.repeat(32)).primary === 'none:alice', 'no manager: the runtime-less face');
  assert(faceFor(null) === none && homeless === 1, 'the runtime-less face is reused while the tab has no manager');

  const a = { id: 'a' };
  const fa = faceFor(a);
  assert(fa !== none && built === 1, 'the manager arriving swaps the face');
  assert(faceFor(a) === fa && built === 1, 'and its face is reused across reads');

  const b = { id: 'b' };
  assert(faceFor(b) !== fa && built === 2, 'an account switch builds the new manager its face');
  assert(faceFor(null) !== none && homeless === 2, 'a reset falls back to a fresh runtime-less face');
});

Deno.test("a tab that hosts no runtime refuses the Save visibly and keeps the staged edits", async () => {
  const editor = fakeFace('none', ELSEWHERE).editor('ab'.repeat(32));
  editor.setNickname('Mum');

  const shown = await savePrivateSection(editor);

  assert(shown !== null && shown !== '', `the refusal is painted on error-message, got ${JSON.stringify(shown)}`);
  assert(
    shown === 'Your private notes can only be saved in the other open instance of this app',
    `the refusal is the shared answer's own text, got ${JSON.stringify(shown)}`,
  );
  assert(editor.isStaged() && editor.form().nickname === 'Mum', 'the staged edits stay on screen');
});

Deno.test('a landed Save clears the error and drops the staging', async () => {
  const editor = fakeFace('m', { kind: 'saved', text: null }).editor('ab'.repeat(32));
  editor.setNickname('Mum');
  assert((await savePrivateSection(editor)) === null, 'nothing to paint once it saved');
  assert(!editor.isStaged(), 'the staging is spent');
});

Deno.test('a Save that answers no text, or throws, still paints a refusal', async () => {
  const silent = fakeFace('m', { kind: 'failed', text: null }).editor('x');
  const shown = await savePrivateSection(silent);
  assert(!!shown && shown.includes('failed'), `a textless refusal is never blank, got ${JSON.stringify(shown)}`);

  const throwing: OverlayEditorFace = {
    ...fakeFace('m', ELSEWHERE).editor('x'),
    save: () => Promise.reject(new Error('the face is gone')),
  };
  const thrown = await savePrivateSection(throwing);
  assert(!!thrown && thrown.includes('the face is gone'), `a thrown Save names its cause, got ${JSON.stringify(thrown)}`);
});
