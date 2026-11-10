"""A document element's body AS PAINTED, one string per painted line.

The shared one-line plaintext every app reports through ``get_text`` drops a
task item's box state and a nested item's indent, so it cannot witness the
render model's task lists and nested lists (``docs/goal/architecture/
render-model.md`` § D7). This read keeps both: a ticked item's line carries
``☑``, an unticked one ``☐``, and a nested item's line starts further right
than its parent's.

Each app answers from what actually holds the paint:

* **tui** publishes ``get_attr(<id>, "painted")``, answered from the very lines
  its paint draws (``apps/fauna-tui/src/document.rs::painted_text``).
* **web** reads the DOM the document rendered into (``apps/fauna-web/src/lib/
  document.ts::blockToHtml``): a task item is an ``<li>`` led by a disabled
  ``<input type="checkbox">``, whose ``checked`` IS the painted box; a nested
  list is a ``<ul>``/``<ol>`` inside an ``<li>``. The indent is the item's
  laid-out left edge against the element's own (``getBoundingClientRect``), one
  space per :data:`WEB_INDENT_PX` — so a stylesheet that flattened the nesting
  reads flat, exactly as the user would see it. The scope resolves the way the
  web bridge's ``_scoped_root`` does (``FeedActions._post_image_states_in``).
* **linux** publishes ``get_attr(<id>, "painted")`` off the labels it laid out
  (``apps/fauna-linux/src/automation/agent.rs::painted_lines``): the same rule
  as web's — one space per 8 px of a label's left offset against the element,
  a ``│`` bar where the label's computed left border is non-zero.
* **android** answers ``get_attr(<id>, "painted")`` through its driver, which
  resolves the element to its ``frame`` and asks the test agent's
  ``document_painted`` command for the laid-out text of the body painted there
  (``apps/fauna-android/.../components/DocumentPaint.kt``): each line's indent
  is its first glyph's laid-out offset, one space per 8 dp — web's rule.
* **macos + ios** answer ``get_attr(<id>, "painted")`` off FaunaKit's shared
  document walk (``apps/fauna-apple/FaunaKit/.../DocumentBodyView.swift::
  DocumentPaintedLines``): every text leaf it paints publishes its laid-out
  frame, one space per 8 pt of left offset and a ``│`` per enclosing quote.
* every other app answers ``get_attr(<id>, "painted")`` once its test agent
  publishes it; until then the ``document-painted-read`` surface
  (``surfaces.toml``) declares it unbuilt, so a test reading this skips as
  parity debt instead of failing on a ``None`` (convention 7).
"""

from __future__ import annotations

import json

#: Pixels of laid-out left offset that read as one leading space on web.
WEB_INDENT_PX = 8

_WEB_PAINTED_JS = """
(() => {
  let root = document;
  for (const [id, index] of %(steps)s) {
    root = root && root.querySelectorAll(`[data-testid="${id}"]`)[index];
  }
  if (!root) return null;
  const el = root.querySelectorAll('[data-testid="%(element_id)s"]')[%(index)d];
  if (!el) return null;
  const base = el.getBoundingClientRect().left;
  const pad = (node) =>
    ' '.repeat(Math.max(0, Math.round((node.getBoundingClientRect().left - base) / %(px)d)));
  const LISTS = ['UL', 'OL'];
  const lines = [];
  const ownText = (item) => {
    let text = '';
    for (const n of item.childNodes) {
      if (n.nodeType === 3) text += n.textContent;
      else if (n.nodeType === 1 && !LISTS.includes(n.tagName) && n.tagName !== 'INPUT')
        text += n.textContent;
    }
    return text.replace(/\\s+/g, ' ').trim();
  };
  // `bar` is the quote bar every enclosing blockquote PAINTS — its computed
  // left border, so a quote styled without one reads as no quote.
  const walk = (node, bar) => {
    let ordinal = 0;
    for (const child of node.children) {
      if (child.tagName === 'LI') {
        ordinal += 1;
        const box = child.querySelector(':scope > input[type="checkbox"]');
        const glyph = box
          ? (box.checked ? '\\u2611 ' : '\\u2610 ')
          : (node.tagName === 'OL' ? `${ordinal}. ` : '\\u2022 ');
        lines.push(pad(child) + bar + glyph + ownText(child));
        for (const n of child.children) if (LISTS.includes(n.tagName)) walk(n, bar);
      } else if (child.tagName === 'BLOCKQUOTE') {
        const painted = parseFloat(getComputedStyle(child).borderLeftWidth) > 0;
        walk(child, painted ? bar + '\\u2502 ' : bar);
      } else if (LISTS.includes(child.tagName) || child.tagName === 'DIV') {
        walk(child, bar);
      } else {
        const text = child.textContent.replace(/\\s+/g, ' ').trim();
        if (text) lines.push(pad(child) + bar + text);
      }
    }
  };
  walk(el, '');
  return lines;
})()
"""


def painted_read_built(driver) -> bool:
    """Whether this app publishes the painted read — for a witness whose
    painted half is a RIDER on assertions every app answers (the one-line
    plaintext), so an app without it keeps the rest instead of skipping."""
    from helpers.app_surface import app_name, surface_entry

    return app_name(driver) in surface_entry("document-painted-read").built


def painted_lines(driver, element_id: str, *, index: int = 0, scope: str | None = None) -> list[str]:
    """``element_id``'s ``index``-th occurrence (inside ``scope``) as painted lines.

    Declares an app with no such read unbuilt before reading (convention 7), so
    a caller never sees a ``None`` standing for "this app cannot say". An
    element that is not there answers ``[]``."""
    from helpers.app_surface import require_surface

    require_surface(driver, "document-painted-read")
    if driver.is_web():
        from drivers.scope import parse_scope

        steps = json.dumps([[s.element_id, s.index] for s in parse_scope(scope)])
        lines = driver.eval_js(_WEB_PAINTED_JS % {
            "steps": steps, "element_id": element_id, "index": index, "px": WEB_INDENT_PX,
        })
        return [str(line) for line in (lines or [])]
    painted = driver.get_attr(element_id, "painted", index, scope=scope)
    if painted is None:
        return []
    # A no-break space paints exactly as a space; it only stops a line break.
    # FaunaKit glues each list marker and task box to its item with one
    # (`DocumentBodyView.swift`, `"1. "`), so read it as the space the
    # user sees — a witness spells `1. first`, never which space kept it whole.
    return str(painted).replace(" ", " ").split("\n")
