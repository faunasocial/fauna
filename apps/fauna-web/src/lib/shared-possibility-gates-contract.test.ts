// The reply bar, the rule editor's Save, the media share link and the Logs
// pages' copy/clear paint from the shared `Possibility` answer, never from a
// bool the SPA reads itself (`docs/goal/ui/README.md` § Offer only what is
// possible, rule 6). The answers are shared Rust, pinned there
// (`fauna_conversations::thread_controls`, `fauna_client_policy`'s rule editor,
// `fauna_media_machine::share_link_possibility`,
// `fauna_log::format::ring_action_possibility`); the runtime halves below run
// web's own paint over stand-ins of those answers, and the source halves pin
// that each page reads the answer (testing.md convention 17).
//
// Crude like its siblings: the source halves read the files as TEXT with
// comments stripped.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import { askPossibility, formGate, formRefusal } from './form-gate.ts';
import { t } from './i18n/strings.ts';
import { paintPossibility, type Possibility } from './possibility.ts';
import { stripComments } from './source-contract.ts';

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

/** Every opening tag carrying `data-testid={IDS.<id>}`. */
function tagsOf(code: string, id: string): string[] {
  const out: string[] = [];
  let at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  while (at >= 0) {
    const open = code.lastIndexOf('<', at);
    let close = code.indexOf('>', at);
    // Skip a `=>` inside an attribute expression.
    while (code[close - 1] === '=') close = code.indexOf('>', close + 1);
    out.push(code.slice(open, close + 1));
    at = code.indexOf(`data-testid={IDS.${id}}`, at + 1);
  }
  return out;
}

// ── reply bar: `ThreadCapabilities::send_possibility` ─────────────────────
const SEND_BLOCKED: Possibility = {
  state: 'blocked',
  reason: { key: 'conversations.unified.room_notice_bridge_disconnected' },
};
const sendAnswer = (canSend: boolean): Possibility => (canSend ? { state: 'possible' } : SEND_BLOCKED);

Deno.test('the reply bar paints a blocked send disabled with the room notice sentence', () => {
  const paint = paintPossibility(sendAnswer(false));
  assert(paint.shown && paint.disabled);
  assertEquals(paint.reason?.key, 'conversations.unified.room_notice_bridge_disconnected');
  const live = paintPossibility(sendAnswer(true));
  assert(live.shown && !live.disabled);
});

Deno.test('the conversations page reads threadSendPossibility, never can_send, for the reply bar', async () => {
  const page = await read('../routes/conversations/+page.svelte');
  assert(/threadSendPossibility\(caps\)/.test(page), 'the page never asks threadSendPossibility');
  assert(
    /sendBlocked = \$derived\(mode === 'new' \? ntc\.send\.disabled : \(replySend\?\.disabled/.test(page),
    'sendBlocked does not paint from the shared send answer',
  );
  assert(!/isGated\(caps\?\.can_send\)\}\s*\/?>/.test(page.slice(page.indexOf('<MarkdownEditor'))), 'dm-text-field reads can_send');
  for (const tag of tagsOf(page, 'DM_SEND_BUTTON')) {
    assert(/disabled=\{(ntc\.send\.disabled|sendBlocked)\}/.test(tag), `Send paints off something else: ${tag}`);
  }
  const send = page.slice(page.indexOf('function send()'));
  assert(send.slice(0, 600).includes('replySendAnswer()'), 'send does not ask the reply answer before it acts');
});

// ── rule editor: `RuleEditor::save_possibility` ──────────────────────────
Deno.test('the rule editor Save paints its answer: blocked with the reason, possible live', () => {
  const blocked: Possibility = { state: 'blocked', reason: { key: 'common.still_loading' } };
  assertEquals(formGate(askPossibility(() => blocked)), {
    shown: true,
    disabled: true,
    reason: t.common.still_loading,
  });
  assertEquals(formRefusal(askPossibility(() => blocked)), t.common.still_loading);
  assertEquals(formGate(askPossibility(() => ({ state: 'possible' }))).disabled, false);
});

Deno.test('RuleEditor reads savePossibility, never view.can_save', async () => {
  const src = await read('./components/RuleEditor.svelte');
  assert(src.includes('editor.savePossibility()'), 'the editor never asks savePossibility');
  assert(!src.includes('can_save'), 'the editor still reads view.can_save');
  for (const tag of tagsOf(src, 'RULE_EDITOR_SAVE_BUTTON')) {
    assert(/saveGate\.disabled/.test(tag) && /onclick=\{save\}/.test(tag), `Save paints off something else: ${tag}`);
  }
});

// ── share link: `share_link_possibility` (runtime half: media-possibility.test.ts)
Deno.test('the media page offers share-link-button from the shared answer', async () => {
  const page = await read('../routes/media/+page.svelte');
  assert(/shareLinkPaint\(shareLinkPossibility,/.test(page), 'the page never asks shareLinkPossibility');
  assert(/\{#if shareLink\.shown && !snap\?\.share_create\}/.test(page), 'share-link-button is not guarded by its answer');
});

// ── Logs pages and the invite code (runtime half: ring-actions.test.ts) ──
Deno.test('LogsView paints copy and clear from logRingActionPossibility and confirms the copy', async () => {
  const view = await read('./components/LogsView.svelte');
  assert(view.includes('logRingActionPossibility(filtered.length, emptyKey)'), 'LogsView never asks the ring answer');
  for (const id of ['LOG_COPY_BUTTON', 'LOG_CLEAR_BUTTON']) {
    for (const tag of tagsOf(view, id)) {
      assert(/disabled=\{ringGate\.disabled\}/.test(tag), `${id} paints off something else: ${tag}`);
    }
  }
  assert(tagsOf(view, 'LOG_COPY_BUTTON').every((tag) => tag.includes('data-copied={copied')), 'copy has no copied marker');
  const admin = await read('../routes/admin/logs/+page.svelte');
  assert(/<LogsView \{entries\} emptyKey=\{ADMIN_LOGS_EMPTY_KEY\} bind:error \/>/.test(admin), 'admin Logs does not wire its empty key and error');
  const settings = await read('../routes/settings/[[subpage]]/+page.svelte');
  assert(/<LogsView entries=\{logEntries\} showClear onClear=\{clearLogs\} bind:error \/>/.test(settings), 'device Logs does not wire its error');
});

Deno.test('the invite code copy confirms and says a failure on admin-users-action-error', async () => {
  const page = await read('../routes/admin/users/+page.svelte');
  const [tag] = tagsOf(page, 'ADMIN_USERS_INVITE_CODE_COPY_BTN');
  assert(tag.includes('onclick={copyMintedCode}') && tag.includes('data-copied='), `invite copy: ${tag}`);
  const body = page.slice(page.indexOf('async function copyMintedCode()'));
  assert(/if \(outcome\.failed !== null\) actionError = outcome\.failed;/.test(body.slice(0, 400)), 'a failed copy is not said');
});
