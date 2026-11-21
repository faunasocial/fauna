// Deno test for `webAppOriginPaint` — the admin Nest page's Web app section painted from the
// shared view's `section_possibility` / `central_possibility` (`web-content-hosting.md`
// § Same-origin security model → *While the central origin is not live, the choice is not
// offered*, rule 5). Run via:
//
//     deno test apps/fauna-web/src/lib/admin-web-app-origin-paint.test.ts
//
// The three states the tui leg pins (`apps/fauna-tui/src/admin/nest.rs`), in the answers
// `fauna_client_admin::admin_web_app_origin_view` gives for each.

import { webAppOriginPaint } from './admin-web-app-origin-paint.ts';
import type { Possibility } from './possibility.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const possible: Possibility = { state: 'possible' };
const absent: Possibility = { state: 'absent' };

Deno.test('before the read lands no element of the section is painted', () => {
  eq(webAppOriginPaint(null), { section: false, central: false }, 'unloaded');
});

Deno.test('bundled while the central origin is not live paints no section', () => {
  eq(
    webAppOriginPaint({ section_possibility: absent, central_possibility: absent }),
    { section: false, central: false },
    'bundled, not live',
  );
});

Deno.test('a stored central while not live offers only the way back: section, no central radio', () => {
  eq(
    webAppOriginPaint({ section_possibility: possible, central_possibility: absent }),
    { section: true, central: false },
    'stored central, not live',
  );
});

Deno.test('a live central origin paints the whole section, both radios', () => {
  eq(
    webAppOriginPaint({ section_possibility: possible, central_possibility: possible }),
    { section: true, central: true },
    'central_live',
  );
});
