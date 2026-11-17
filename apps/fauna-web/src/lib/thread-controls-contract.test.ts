// The conversation room and thread controls a role or the rail decides
// (`docs/goal/ui/conversations.md` § Errors & edge cases → *Controls this
// person can never use*; `docs/goal/ui/README.md` § Offer only what is
// possible, rules 2 and 6), asserted over the source as a general invariant
// (testing.md convention 17). The answer itself is shared Rust's
// (`ThreadCapabilities::possibility`, pinned there and over its wasm twin in
// `libs/fauna-wasm/tests/thread_control_possibility.rs`); this pins that the
// SPA paints from it and asks it, and decides nothing itself.
//
// Three halves:
//  1. The page never paints one of these controls off a bare capability
//     boolean — a `false` cannot say whether the control is absent or blocked.
//  2. Every such control is painted only under its answer's `shown`, so an
//     absent answer paints nothing; a member's policy reads as lines.
//  3. Every handler behind one asks the same answer (`controlPossible`)
//     before it acts.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert, assertEquals } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);
const PAGE = "../routes/conversations/+page.svelte";
const LABELERS = "./components/RoomLabelerRows.svelte";

/** The body of `function <name>(` up to the next top-level function. */
function fnBody(code: string, name: string): string {
  const start = code.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert(start >= 0, `function ${name} not found — did it move? update this contract`);
  const rest = code.slice(start + 1);
  const end = rest.search(/\n {2}(?:async )?function \w/);
  return end < 0 ? rest : rest.slice(0, end);
}

/** The nearest `{#if …}` opened before `data-testid={IDS.<id>}`. */
function guardOf(code: string, id: string): string {
  const at = code.indexOf(`data-testid={IDS.${id}}`);
  assert(at >= 0, `IDS.${id} not found`);
  const open = code.lastIndexOf("{#if", at);
  assert(open >= 0, `IDS.${id} sits under no {#if}`);
  return code.slice(open, code.indexOf("}", open) + 1);
}

Deno.test("no role- or rail-gated control is painted off a bare capability boolean", async () => {
  const page = await read(PAGE);
  for (const flag of [
    "can_invite",
    "can_remove_members",
    "can_set_policy",
    "can_appoint_admins",
    "can_transfer_ownership",
    "can_leave_room",
    "supports_local_remove",
    "supports_subject",
  ]) {
    assertEquals(
      page.match(new RegExp(`capabilities\\?\\.${flag}\\b|caps\\?\\.${flag}\\b`, "g")),
      null,
      `${PAGE} reads ${flag} itself — paint it from threadControlPossibility`,
    );
  }
  assert(/threadControlPossibility\(/.test(page), "the page never asks threadControlPossibility");
  const labelers = await read(LABELERS);
  assert(!/canSetPolicy/.test(labelers), `${LABELERS} still takes a bare canSetPolicy boolean`);
});

const PAINTED: [string, string, string][] = [
  // [file, ui id constant, the answer whose `shown` guards it]
  [PAGE, "THREAD_ADD_PARTICIPANT_BUTTON", "ctl.invite.shown"],
  [PAGE, "ROOM_JOIN_RULE_SELECT", "ctl.setPolicy.shown"],
  [PAGE, "ROOM_NEST_READ_TOGGLE", "ctl.setPolicy.shown"],
  [PAGE, "ROOM_SETTINGS_SAVE_BUTTON", "ctl.setPolicy.shown"],
  [PAGE, "ROOM_ADMIN_TOGGLE", "ctl.appointAdmins.shown"],
  [PAGE, "ROOM_OWNER_TRANSFER_BUTTON", "ctl.transferOwnership.shown"],
  [PAGE, "ROOM_LEAVE_BUTTON", "ctl.leaveRoom.shown"],
  [PAGE, "ROOM_REMOVE_BUTTON", "ctl.localRemove.shown"],
  [PAGE, "TOPIC_TOGGLE_BUTTON", "ctl.subject.shown"],
  [LABELERS, "ROOM_LABELER_TOGGLE", "setPolicy.shown"],
];

Deno.test("every role- or rail-gated control is painted only under its answer", async () => {
  for (const [file, id, shown] of PAINTED) {
    const guard = guardOf(await read(file), id);
    assert(guard.includes(shown), `${file} IDS.${id} is guarded by ${guard}, not ${shown}`);
  }
});

Deno.test("the member chip carries the remove only where its answer is possible", async () => {
  const page = await read(PAGE);
  assert(
    /@const removable = ctl\.removeMember\.shown && !ctl\.removeMember\.disabled/.test(page),
    "the chip's remove is not decided by the remove_member answer",
  );
  // An inert chip is the roster: no button role and no tab stop.
  assert(/role=\{removable \? 'button' : undefined\}/.test(page), "an inert chip still claims a button role");
});

Deno.test("the owner's blocked walk-out paints its reason within eyeshot", async () => {
  const page = await read(PAGE);
  const at = page.indexOf("data-testid={IDS.ROOM_LEAVE_BUTTON}");
  const block = page.slice(at, page.indexOf("{/if}", page.indexOf("{#if ctl.leaveRoom.reason}", at)));
  assert(/resolveLocalized\(ctl\.leaveRoom\.reason\)/.test(block), "the walk-out's reason is not painted beside it");
});

const HANDLERS: [string, string][] = [
  ["removeMember", "remove_member"],
  ["toggleTopic", "subject"],
  ["openAddParticipant", "invite"],
  ["stageJoinRule", "set_policy"],
  ["stageHistoryPolicy", "set_policy"],
  ["stageAdmin", "appoint_admins"],
  ["stageTransfer", "transfer_ownership"],
  ["saveRoomSettings", "set_policy"],
  ["stageNestRead", "set_policy"],
  ["stageLabeler", "set_policy"],
  ["openRoomLeave", "leave_room"],
  ["openRoomRemove", "local_remove"],
];

Deno.test("every handler asks the answer its control painted before it acts", async () => {
  const page = await read(PAGE);
  for (const [handler, control] of HANDLERS) {
    assert(
      fnBody(page, handler).includes(`controlPossible('${control}')`),
      `${handler} does not ask controlPossible('${control}')`,
    );
  }
});
