/**
 * Which parts of the admin Nest page's Web app section (`admin-nest-web-app-origin-*`)
 * are painted, read straight off the shared view's two answers
 * (`web-content-hosting.md` § Same-origin security model → *While the central
 * origin is not live, the choice is not offered*, rule 5; `docs/goal/ui/README.md`
 * § Offer only what is possible, rule 6). `section_possibility` and
 * `central_possibility` are `fauna_client_admin::AdminWebAppOriginView`'s — the
 * page derives neither, it only paints them. Plain TS so `deno test` reaches it
 * (`admin-web-app-origin-paint.test.ts`); the tui leg is
 * `apps/fauna-tui/src/admin/nest.rs::web_app_origin_elements`.
 */
import { paintPossibility, type Possibility } from './possibility.ts';

/** The two answers of `AdminWebAppOriginView` this paint reads. */
export interface WebAppOriginPossibilities {
  section_possibility: Possibility;
  central_possibility: Possibility;
}

/** `section`: the section's elements exist. `central`: the central radio
 *  exists (never without the section). Until the read lands (`null`) nothing
 *  is painted — whether the section exists is the read's answer, and painting
 *  it to take it away again would offer a choice and withdraw it. */
export function webAppOriginPaint(
  view: WebAppOriginPossibilities | null,
): { section: boolean; central: boolean } {
  if (!view) return { section: false, central: false };
  const section = paintPossibility(view.section_possibility).shown;
  return { section, central: section && paintPossibility(view.central_possibility).shown };
}
