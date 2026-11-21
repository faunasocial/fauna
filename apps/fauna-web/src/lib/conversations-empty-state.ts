// The conversation list's "no conversations" line gate (`ui/conversations.md`
// § Layout & flow → *Loading is not empty*; `ui/README.md` § List pages). The
// line paints only once the shared `ConversationsSnapshot.loaded` bit is true
// AND the list is empty; before that no empty state paints. Import-free so
// `deno test` loads it without the page's wasm graph.
export function showEmptyState(
  loaded: boolean | null | undefined,
  threadCount: number,
): boolean {
  return loaded === true && threadCount === 0;
}
