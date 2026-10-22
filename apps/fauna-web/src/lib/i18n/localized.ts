// Relative, not the `#lib/i18n/strings` alias it resolves to: the alias is a
// SvelteKit/vite one that plain `deno test` cannot resolve, and it would make
// this shared resolver — and every module reaching it — unloadable in a unit
// test (`just web-unit-test`). Same file either way.
import { t } from './strings.ts';
import type { RowCell } from '#lib/rpc';
import type { EditorCell } from '#lib/wasm';

/**
 * A runtime localized string — an i18n key plus a flat substitution map.
 * Mirrors `fauna_core::localized::LocalizedText`; returned by wasm value
 * formatters (`byteSize`/`relativeTime`/`durationSecs`) and onboarding wizard
 * snapshot getters. The client resolves it against the generated `t` table.
 */
export interface LocalizedText {
  key: string;
  args?: Record<string, string>;
}

/**
 * Resolve a `LocalizedText` against the generated `t` table. Handles both the
 * bare-string and the parametric `(args) => string` form the i18n generator
 * emits for keys with `{placeholder}`s. Returns the key itself on a miss so a
 * mistranslated path is visible in the UI. (Lifted from the onboarding page's
 * `Lookup`/`L` helpers — one resolver for every consumer; see
 * `docs/goal/behavior/value-formatting.md`.)
 */
export function resolveLocalized(lt: LocalizedText | undefined | null): string {
  if (!lt || !lt.key) return '';
  const parts = lt.key.split('.');
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let node: any = t;
  for (const part of parts) {
    if (node == null || typeof node !== 'object') return lt.key;
    node = node[part];
  }
  if (typeof node === 'function') {
    try {
      return node(lt.args ?? {});
    } catch {
      return lt.key;
    }
  }
  if (typeof node === 'string') return node;
  return lt.key;
}

/**
 * Resolve a bare dot-notation key with no args (the keyless form the
 * onboarding provider form + admin-dns page use for display-name/label
 * lookups).
 */
export function resolveKey(key: string): string {
  return resolveLocalized({ key, args: {} });
}

/**
 * [resolveLocalized], but each **argument** is first put through the same
 * resolver too — for templates whose substitution is itself a translatable
 * term rather than raw data. The JS twin of shared Rust
 * `LocalizedText::resolve_nested`: the gated-feature plane's exhaustion
 * sentence and quota-cell label both carry a nested key as an argument (e.g.
 * `window = "features.window_day"`), so plain [resolveLocalized] would leave
 * the raw key inside the rendered sentence. An argument that is not itself a
 * key simply misses the lookup (resolveKey falls back to the value itself)
 * and substitutes verbatim.
 */
export function resolveLocalizedNested(lt: LocalizedText | undefined | null): string {
  if (!lt || !lt.key) return '';
  const nestedArgs: Record<string, string> = {};
  for (const [k, v] of Object.entries(lt.args ?? {})) {
    nestedArgs[k] = resolveKey(v);
  }
  return resolveLocalized({ key: lt.key, args: nestedArgs });
}

/**
 * A `LocalizedText` whose template arguments may themselves be translatable
 * (mirrors `fauna_client_connected_apps::compose::ComposedText`): `nested`
 * maps an argument name to the text that fills it, and wins over `text.args`
 * of the same name.
 */
export interface ComposedText {
  text: LocalizedText;
  nested: Record<string, LocalizedText>;
}

/**
 * Resolve a shared-Rust `ComposedText` — the JS twin of `ComposedText::resolve`:
 * each `nested` text resolves first and fills its argument, then the outer
 * template resolves. The composition (order, which key, which args) is the
 * shared module's; this only maps keys to the generated `t` table.
 */
export function resolveComposed(ct: ComposedText): string {
  const args: Record<string, string> = { ...(ct.text.args ?? {}) };
  for (const [name, nested] of Object.entries(ct.nested ?? {})) {
    args[name] = resolveLocalized(nested);
  }
  return resolveLocalized({ key: ct.text.key, args });
}

/**
 * A gated-feature quota cell's headroom sentence, fully composed — the JS
 * twin of shared Rust `fauna_client_features::row::cell_value_text`. When
 * `cell.magnitudes` is set (`volume` cells), its two localized magnitudes are
 * resolved first and substituted into `value`'s `{remaining}`/`{limit}`
 * holes — a `LocalizedText` argument is a flat string, so a magnitude that is
 * itself localized ("1 TB") has to be composed before the outer template
 * resolves. Without magnitudes, `{remaining}`/`{limit}` are already finished
 * numbers, so plain [resolveLocalized] is enough.
 */
export function cellValueText(cell: RowCell): string {
  if (!cell.magnitudes) return resolveLocalized(cell.value);
  const composed: LocalizedText = {
    key: cell.value.key,
    args: {
      ...cell.value.args,
      remaining: resolveLocalized(cell.magnitudes.remaining),
      limit: resolveLocalized(cell.magnitudes.limit),
    },
  };
  return resolveLocalized(composed);
}

/**
 * A feature-policy editor cell's no-effect note, fully composed (`null` when
 * the cell has none) — the JS twin of shared Rust
 * `fauna_client_features::editor_note_text`. When `note_magnitude` is set, it
 * is resolved first and substituted for the note's `{limit}` hole, for the
 * same reason as [cellValueText]: a localized magnitude ("2 GB") is a flat
 * string by the time the outer template resolves.
 */
export function editorNoteText(cell: EditorCell): string | null {
  if (!cell.note) return null;
  if (!cell.note_magnitude) return resolveLocalized(cell.note);
  return resolveLocalized({
    key: cell.note.key,
    args: { ...cell.note.args, limit: resolveLocalized(cell.note_magnitude) },
  });
}
