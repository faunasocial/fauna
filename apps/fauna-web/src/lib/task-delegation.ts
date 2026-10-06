// Shared shapes for the Task-delegation Settings sub-page (`TaskDelegationSection.svelte`)
// — the web leg of the surface every app renders over the one shared view-model
// (`fauna_client_delegation::TaskDelegationView`, wasm binding `WasmTaskDelegationView`).
// See docs/goal/behavior/participants.md § Task delegation.
//
// Mirrors `fauna_core::delegation` serde JSON (snake_case fields). The enums are
// externally tagged, so a unit arm rides as the bare Rust variant name
// (`"Automatic"`, `"Waiting"`) and a data arm as `{ Other: { who } }` — the same
// PascalCase-variant convention `devices-machine.ts` documents.
//
// **The SPA never re-derives which options a picker may offer.** It renders
// `row.pin_options` verbatim: a pin collapses the candidate set to exactly the
// pinned participant, so offering one that can never run the kind would strand the
// work forever. That rule lives once, in shared Rust, for all seven apps
// (participants.md § The assignment picker) — web is `HeavyTaskCapability::ViewerOnly`
// (a browser tab runs no lease driver), which is why "This device" never appears here.
import { type LocalizedText, resolveLocalized } from '$lib/i18n/localized';
import { hexFull, taskDelegationRunnerLabelRaw, taskDelegationOptionLabelRaw } from '$lib/wasm';

/** `fauna_core::data::ParticipantRef` — a device (hex id) or a nest (pubkey bytes).
 *  The pubkey is a byte-string field in Rust, so the WASM bridge may hand it over
 *  as a `Uint8Array`; pass it back unchanged and read it through `Uint8Array.from`. */
export type ParticipantRef =
  | { Device: { device_id: string } }
  | { Nest: { actor_pubkey: number[] | Uint8Array } };

/** `fauna_core::delegation::RunnerStatus` — who currently runs a kind. */
export type RunnerStatus = 'ThisDevice' | 'Waiting' | { Other: { who: ParticipantRef } };

/** `fauna_core::delegation::PinOption` — one selectable assignment. */
export type PinOption = 'Automatic' | 'ThisDevice' | { Other: { who: ParticipantRef } };

/** `fauna_core::delegation::TaskDelegationRow` — one `task-delegation-kind-item`. */
export interface TaskDelegationRow {
  task_kind: string;
  name: LocalizedText;
  runner: RunnerStatus;
  assignment: PinOption;
  pin_options: PinOption[];
}

/** The hex key of a participant: a device's id verbatim, a nest's pubkey bytes hex-encoded. */
export function participantKeyHex(who: ParticipantRef): string {
  if ('Device' in who) return who.Device.device_id;
  return hexFull(Uint8Array.from(who.Nest.actor_pubkey));
}

/**
 * The stable cross-app option key — `"automatic"` / `"this-device"` / a
 * participant-ref hex. It backs the `<option value>`, which is BOTH what the e2e
 * driver selects by (Playwright `select_option(value=…)`) and what it reads back
 * (`el.value`); `actions/task_delegation.py::_norm` normalizes it against GTK's
 * selected-label so one key asserts on every app. The native legs derive the
 * same three keys (windows `TaskDelegationViewModel.OptionKey`).
 */
export function pinOptionKey(option: PinOption): string {
  if (option === 'Automatic') return 'automatic';
  if (option === 'ThisDevice') return 'this-device';
  return participantKeyHex(option.Other.who);
}

/**
 * The row's runner line, as the user reads it (`task-delegation-kind-runner`) —
 * the shared `fauna_core::delegation::runner_label` decision over wasm
 * (priority #2; mirrors android/native `taskDelegationRunnerLabel`). The
 * participant-name resolution (roster label, or a short-hex fallback) lives
 * once in shared Rust rather than per client.
 */
export function runnerText(runner: RunnerStatus, labels: Map<string, string>): string {
  return resolveLocalized(taskDelegationRunnerLabelRaw(runner, labels));
}

/**
 * The visible label of a picker option — never the stable key {@link pinOptionKey}.
 * The shared `fauna_core::delegation::option_label` decision over wasm (see
 * {@link runnerText}). A foreign pin renders the participant's name (the
 * `task_delegation.assignment_other_name` key is a bare `{name}` template —
 * there is no separate i18n key for it), so a pin made on another device
 * stays legible and escapable.
 */
export function pinOptionLabel(option: PinOption, labels: Map<string, string>): string {
  return resolveLocalized(taskDelegationOptionLabelRaw(option, labels));
}
