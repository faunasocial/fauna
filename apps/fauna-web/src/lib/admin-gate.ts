// The `admin-tab` gate's transition rule (`admin.md` § Navigation model;
// `transport.md` § Which surfaces a push invalidates, the `admin_gate` flag).
//
// The gate is re-read on every reconnect because a login read that finds no nest fails
// closed. Two properties, as in tui (`Outcome::GateUnreachable` + `became_admin`) and
// linux (`check_admin_status` + `admin_gate_passed`):
//   1. a FAILED read (`null`) moves nothing — a confirmed admin keeps the entry through a
//      socket that drops again;
//   2. the once-per-session admin effects fire on the false→true transition only
//      (`becameAdmin`), never on every reconnect.

/** One gate read: the nest's answer, or `null` when the read itself failed. */
export type AdminGateRead = boolean | null;

export function applyAdminGateRead(
  prev: boolean,
  read: AdminGateRead,
): { gate: boolean; becameAdmin: boolean } {
  if (read === null) return { gate: prev, becameAdmin: false };
  return { gate: read, becameAdmin: read && !prev };
}
