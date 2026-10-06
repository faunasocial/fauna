// The consent card's permission-set section as flat lines (`atproto-pds-full.md` § F4 detail → *Permission sets*, the card bullet).
//
// One heading per set — the publisher's title beside the NSID, or the NSID
// alone when no title was declared (never an invented label) — then the
// publisher's `details` when present, then one bulleted line per member.
// Empty for the overwhelmingly common set-less request, so that card paints
// exactly as before: no heading, no stray blank line.
//
// Rendered AFTER the flat scope list, never instead of it: every member is
// already in `scope_descriptions` (a set contributes ordinary granular
// scopes), so this section answers what the flat list cannot — which named
// bundle produced them, and what its publisher says it is for. The NSID is
// the identity and renders verbatim beside the title, exactly as `client_id`
// renders beside the client name, because the title is written by that same
// publisher. `title`/`details`/`member_descriptions` arrive already
// control-stripped and worded by the shared machine's one composition pass
// (`consent_set_row` / `describe_scope`) — never re-word or re-fence here.
//
// A pure module (relative imports only) so `just web-unit-test` pins the
// composition the way tui's element-walk and linux's `consent_set_lines` pin
// theirs.
import type { ConsentSetRow } from './atproto-settings-machine.ts';
import { t } from './i18n/strings.ts';

export function consentSetLines(sets: ConsentSetRow[]): string[] {
  const lines: string[] = [];
  for (const set of sets) {
    lines.push(
      set.title != null
        ? t.atproto_settings.consent_set_heading({ title: set.title, nsid: set.nsid })
        : t.atproto_settings.consent_set_heading_unnamed({ nsid: set.nsid }),
    );
    if (set.details != null) lines.push(set.details);
    for (const member of set.member_descriptions) lines.push(`• ${member}`);
  }
  return lines;
}
