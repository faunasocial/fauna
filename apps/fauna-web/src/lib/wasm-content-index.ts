let wasmModule: typeof import('../../static/fauna_wasm_content_index.js') | null = null;
let wasmInit: Promise<void> | null = null;

// `base` (the SvelteKit `$app/paths` base) is a caller-supplied param, not a
// direct `$app/paths` import — this crate is ON HOLD with zero production
// callers today (see the suite-level skip note below), so this file stays
// importable outside a SvelteKit runtime (`wasm-content-index.test.ts`). A
// future production caller passes `base` from `$app/paths` itself.
//
// The init *promise* is memoized (same load-bearing shape as
// `wasm.ts::ensureWasm`): two concurrent callers await the SAME in-flight
// init, so `mod.default()` runs once — a second concurrent call would
// re-instantiate the chunk and reset wasm linear memory. A failed init drops
// the cached promise so a later call can retry.
export function ensureContentIndexWasm(base: string): Promise<void> {
    if (wasmModule) return Promise.resolve();
    if (!wasmInit) {
        wasmInit = (async () => {
            const mod = await import('../../static/fauna_wasm_content_index.js');
            await mod.default(`${base}/fauna_wasm_content_index_bg.wasm`);
            wasmModule = mod;
        })().catch((e) => {
            wasmInit = null;
            throw e;
        });
    }
    return wasmInit;
}

function wasm() {
    if (!wasmModule) throw new Error('content-index WASM not initialised — call ensureContentIndexWasm() first');
    return wasmModule;
}

// ── Types (mirror libs/fauna-index/src/types.rs) ──

export type ContentKind =
    | 'mail'
    | 'calendar'
    | 'conversation'
    | 'post'
    | 'file'
    | 'contact'
    | 'draft'
    | 'media';

export type FieldKind = 'title' | 'body' | 'pre_tokenized_tags';

export interface IndexedField {
    kind: FieldKind;
    text: string;
}

export interface IndexedDoc {
    kind: ContentKind;
    content_id: number[]; // Vec<u8> as JS number[]
    timestamp_ns: number;
    sender_actor_id: number[] | null;
    fields: IndexedField[];
}

export interface TimeRange {
    start_ns: number;
    end_ns: number;
}

export interface QueryHit {
    kind: ContentKind;
    content_id: number[];
    timestamp_ns: number;
    sender_actor_id: number[] | null;
    score: number;
}

// ── Surface ──

export async function contentIndexCreateInRam(base: string): Promise<void> {
    await ensureContentIndexWasm(base);
    wasm().content_index_create_in_ram();
}

export async function contentIndexAddDoc(base: string, doc: IndexedDoc): Promise<void> {
    await ensureContentIndexWasm(base);
    wasm().content_index_add_doc(JSON.stringify(doc));
}

export async function contentIndexCommit(base: string): Promise<void> {
    await ensureContentIndexWasm(base);
    wasm().content_index_commit();
}

export async function contentIndexQuery(base: string, args: {
    query: string;
    kinds: ContentKind[];
    range: TimeRange | null;
    limit: number;
}): Promise<QueryHit[]> {
    await ensureContentIndexWasm(base);
    const json = wasm().content_index_query(JSON.stringify(args));
    return JSON.parse(json);
}
