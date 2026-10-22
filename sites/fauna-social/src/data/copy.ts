// Parsing for the copy markdown in docs/sites/fauna-social/.
//
// The body IS the page (that directory's README § How the markdown becomes
// the page). These helpers never reorder, skip or invent prose — they only
// peel apart the pieces a layout has to place in different parts of the DOM,
// such as a hero band or a heading set beside its illustration.
//
// Both helpers take already-token-filled HTML (run `fill()` first, so the
// org.ts tokens work in every position words do).

/** An `*Illustration: fox*` line — the one structural convention in the copy. */
const ILLUSTRATION = /^\s*<p>\s*<em>\s*Illustration:\s*([A-Za-z]+)\s*<\/em>\s*<\/p>/;

/** The leading `<p>…</p>` of a chunk. */
const FIRST_PARAGRAPH = /^\s*<p>([\s\S]*?)<\/p>/;

export interface Opening {
  /** Inner HTML of the `#` heading. */
  heading: string;
  /** The paragraph directly below it — the page's lead. `''` if absent. */
  lead: string;
  /** Critter named by an `*Illustration: …*` line under the heading. */
  illustration: string | null;
  /** Everything after the opening, still in document order. */
  rest: string;
}

/**
 * Split a page's opening (its `#` heading, optional illustration and lead
 * paragraph) from the rest of the body.
 */
export function takeOpening(html: string): Opening {
  const h1 = html.match(/^[\s\S]*?<h1[^>]*>([\s\S]*?)<\/h1>/);
  if (!h1) throw new Error('copy markdown has no `#` heading — the page needs one');

  let rest = html.slice(h1[0].length);

  const marker = rest.match(ILLUSTRATION);
  const illustration = marker ? marker[1].toLowerCase() : null;
  if (marker) rest = rest.slice(marker[0].length);

  const lead = rest.match(FIRST_PARAGRAPH);
  if (lead) rest = rest.slice(lead[0].length);

  return { heading: h1[1], lead: lead ? lead[1] : '', illustration, rest };
}

/**
 * Split a deck into its slides: a `---` line in the copy (a thematic break —
 * blank lines above and below, or markdown reads it as an underline) ends
 * one slide and starts the next. A page without one is a single slide.
 */
export function splitSlides(html: string): string[] {
  return html.split(/<hr\s*\/?>/).filter((chunk) => chunk.trim());
}

/** The trailing `> blockquote` of a body — the last one, and only when the
 *  body ends on it. */
const CLOSING = /<blockquote>(?:(?!<\/blockquote>)[\s\S])*<\/blockquote>\s*$/;

export interface Closing {
  /** The body with its closing blockquote removed. */
  body: string;
  /** The closing blockquote's HTML, or `''` when the body does not end on one. */
  closing: string;
}

/**
 * Peel the closing boxed aside off a body, for a layout that places it apart
 * from the prose — the talk slides put it on its own line above the meadow.
 * The story pages leave it in place, where CSS boxes it in document order.
 */
export function takeClosing(html: string): Closing {
  const m = html.match(CLOSING);
  return m ? { body: html.slice(0, m.index), closing: m[0] } : { body: html, closing: '' };
}

export interface Section {
  /** Inner HTML of the `##` heading. */
  title: string;
  /** The section's prose, illustration marker already consumed. */
  body: string;
  /** Critter named under the heading, or `null` for an ordinary section. */
  illustration: string | null;
}

/** Split a body into its `##` sections, in document order. */
export function splitSections(html: string): Section[] {
  return html
    .split(/(?=<h2[\s>])/)
    .filter((chunk) => chunk.trim())
    .map((chunk) => {
      const parsed = chunk.match(/^<h2[^>]*>([\s\S]*?)<\/h2>([\s\S]*)$/);
      if (!parsed) throw new Error(`copy markdown: unparsable section: ${chunk.slice(0, 80)}`);

      const [, title, afterHeading] = parsed;
      const marker = afterHeading.match(ILLUSTRATION);
      return {
        title,
        body: marker ? afterHeading.slice(marker[0].length) : afterHeading,
        illustration: marker ? marker[1].toLowerCase() : null,
      };
    });
}
