// The nest's typed guardian-approval refusal, as the wasm transport hands it
// to the SPA.
//
// `libs/fauna-wasm/src/rpc.rs::err_to_js` prefixes that one refusal (the
// shared `RpcError::is_guardian_approval_required`) with
// `"guardian_approval_required: "` — web's twin of the FFI's
// `FfiError::GuardianApprovalRequired`. The ward's in-place ask
// (`contact-request-guardian-button` / `bridge-source-request-button`) is
// offered ONLY on this refusal (family-safety.md § Child-initiated contact
// requests → App affordance): an ask painted on any other failure would tell
// an unsupervised user their account is supervised. Pure string logic over a
// prefix Rust writes, never the nest's message text — unit-tested.

const PREFIX = 'guardian_approval_required:';

function messageOf(raw: unknown): string {
  if (typeof raw === 'string') return raw;
  const message = (raw as { message?: unknown } | null)?.message;
  return typeof message === 'string' ? message : String(raw);
}

/** True when `raw` (a rejected wasm call's value) is the guardian gate's refusal. */
export function isGuardianApprovalRequired(raw: unknown): boolean {
  return messageOf(raw).startsWith(PREFIX);
}

/** The text to show on `error-message`: the refusal's own sentence with the
 *  routing prefix stripped; any other failure's text unchanged. */
export function refusalText(raw: unknown): string {
  const msg = messageOf(raw);
  return msg.startsWith(PREFIX) ? msg.slice(PREFIX.length).trimStart() : msg;
}
