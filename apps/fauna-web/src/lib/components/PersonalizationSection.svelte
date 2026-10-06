<script lang="ts">
  // Settings → Personalization — the unified home for the user's single
  // tier-1 ruleset (content-moderation-and-ranking.md § Composition + §
  // Tier-3). Hubs three facets WITHOUT rebuilding them: "Feeds" (a link out
  // to the Feed page's create-feed dialog — the trainable factor-weight
  // authoring lands once Slice 1 ships), "Muted
  // words" (a link re-homing the ALREADY-SHIPPED muted-words sub-page —
  // reused, not rebuilt), and "Community labelers" (the caller's SUBSCRIBED
  // tier-3 labelers rendered inline, each unsubscribable, with a link out to
  // the full Community-labelers catalog sub-page). A dumb renderer of the
  // shared `LabelerCatalogMachine` (`libs/fauna-labeler-catalog-machine`,
  // WASM twin `libs/fauna-wasm-labeler-catalog`), filtered client-side to
  // `subscribed === true` — no second RPC. Mirrors
  // `apps/fauna-linux/src/views/personalization/mod.rs`'s `HomeShell`.
  import { identity } from '$lib/store';
  import { sharedRpcPort } from '$lib/rpc';
  import {
    ensureWasm,
    publishKindOptions,
    ngramDirectionLabel,
    ngramDocCountLabel,
    textModelNeedsNewerApp,
  } from '$lib/wasm';
  import { createLabelerCatalogMachine } from '$lib/wasm-labeler-catalog';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount, onDestroy } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { LabelerCatalogSnapshot } from '$lib/labeler-catalog-machine';
  import type { LabelerCatalogMachine } from '../../../static/fauna_wasm_labeler_catalog.js';
  import {
    trainedTopicsList,
    trainedTopicsCreate,
    trainedTopicsRename,
    trainedTopicsDelete,
    trainedTopicsSetLearnFromEngagement,
    publishTrainedFactorList,
    publishTrainedFactorModel,
    type TrainedTopicRow,
    type TrainedTopicsError,
    type PublishListError,
    type PublishModelError,
    type PublishNgram,
  } from '$lib/rpc';
  import { getFeedManager } from '$lib/feed';
  import { IDS } from '$lib/generated/uiIds';

  // The page-level error surface is the Settings shell's shared MessageBanner
  // (`error-message`); mirrors DevicesSection's bound `error` prop.
  let { error = $bindable('') } = $props();

  let machine: LabelerCatalogMachine | null = null;
  let snap = $state<LabelerCatalogSnapshot | null>(null);
  let ready = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;

  // ── Trained topics facet (topic-factors.md § Authoring surface & picker).
  //
  // The ratified ONE-name-input shape (linux is the reference leg): the input +
  // create-button mint a topic inline; a row's Rename retargets that same input
  // at the row (prefilled, the button relabels "Save name") and the next commit
  // renames instead of creating. Any successful commit resets to create mode.
  //
  // All four gestures are `$lib/rpc` wrappers over the SHARED
  // `fauna_client_personalization::TrainedTopics` lifecycle — the registry↔model
  // pairing is not re-coded here (priority #2). Each returns the fresh rows.
  let topics = $state<TrainedTopicRow[]>([]);
  let topicName = $state('');
  /** The hex id of the row the name-input is retargeted at, or null = create. */
  let renamingId = $state<string | null>(null);
  let busy = $state(false);

  /**
   * The wasm layer rejects with a machine-readable `code` (never a pre-formatted
   * English sentence), so the cap message is localized HERE with its limit.
   */
  function topicsError(e: unknown): string {
    const err = e as Partial<TrainedTopicsError> | undefined;
    if (err?.code === 'cap' && err.max != null) {
      return t.personalization.trained_factor_cap({ max: String(err.max) });
    }
    if (err?.message) return err.message;
    return e instanceof Error ? e.message : String(e);
  }

  async function loadTopics(secretHex: string): Promise<void> {
    try {
      topics = await trainedTopicsList(secretHex);
    } catch (e: unknown) {
      error = topicsError(e);
    }
  }

  /** Clear the page error after an unrelated gesture's success — unless the
   *  publish sheet is open, whose still-relevant error the gesture must not
   *  clobber (the clobber windows fixed in its render precedence chain
   *  2026-07-18, pinned cross-app by test_trained_topics.py's two
   *  publish-error cases). While the sheet is open its own lifecycle owns the
   *  banner: a submit success clears it, and a fresh re-open retires a stale
   *  one (`openPublishSheet` already does). */
  function clearErrorUnlessPublishOpen(): void {
    if (!publishTarget) error = '';
  }

  /** Commit the name-input: a rename when a row retargeted it, else a create. */
  async function submitTopic(): Promise<void> {
    const id = $identity;
    const name = topicName.trim();
    if (!id || !name || busy) return;
    busy = true;
    try {
      topics = renamingId
        ? await trainedTopicsRename(id.secretHex, renamingId, name)
        : await trainedTopicsCreate(id.secretHex, name);
      // Any successful commit resets the input to create mode.
      renamingId = null;
      topicName = '';
      clearErrorUnlessPublishOpen();
    } catch (e: unknown) {
      error = topicsError(e);
    } finally {
      busy = false;
    }
  }

  function startRename(row: TrainedTopicRow): void {
    renamingId = row.id;
    topicName = row.name;
  }

  async function deleteTopic(row: TrainedTopicRow): Promise<void> {
    const id = $identity;
    if (!id || busy) return;
    busy = true;
    try {
      topics = await trainedTopicsDelete(id.secretHex, row.id);
      // A delete of the row currently being renamed strands the input.
      if (renamingId === row.id) {
        renamingId = null;
        topicName = '';
      }
      clearErrorUnlessPublishOpen();
    } catch (e: unknown) {
      error = topicsError(e);
    } finally {
      busy = false;
    }
  }

  /** Flip a row's Layer-A opt-in ("Learn from my activity" —
   *  engagement-cues.md § Layer A). Registry-only; the model row is untouched. */
  async function toggleEngagement(row: TrainedTopicRow): Promise<void> {
    const id = $identity;
    if (!id || busy) return;
    busy = true;
    try {
      topics = await trainedTopicsSetLearnFromEngagement(id.secretHex, row.id, !row.learn_from_engagement);
      clearErrorUnlessPublishOpen();
    } catch (e: unknown) {
      error = topicsError(e);
    } finally {
      busy = false;
    }
  }

  /** "Clear activity data" — deletes the sealed cues:v1 rollup from the
   *  user's own nest and resets the live cue engine (engagement-cues.md §
   *  At rest; the user-revocable affordance the capture invariant requires). */
  async function clearEngagementData(): Promise<void> {
    const id = $identity;
    if (!id) return;
    try {
      const manager = await getFeedManager();
      await manager.deleteCueRollup();
      clearErrorUnlessPublishOpen();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Layer-B signal sharing (engagement-cues.md § Layer B): the opt-in toggle
  // + the transparency pane — a bool plus a read-only list, not a state
  // machine (the shape apps/fauna-tui/src/settings/engagement_cues.rs paints).
  // Both read and write the LIVE feed manager, whose producer reads the cached
  // opt-in; the toggle renders the nest-confirmed reply, never the click.
  interface SignalShareEntry {
    content_hash: string;
    factor: string;
    count: number;
  }
  let shareSignals = $state(false);
  let signalPublished = $state<SignalShareEntry[]>([]);
  let signalBusy = $state(false);

  function applySignalShare(reply: { share: boolean; published: SignalShareEntry[] }): void {
    shareSignals = reply.share;
    signalPublished = reply.published;
  }

  async function loadSignalShare(): Promise<void> {
    try {
      const manager = await getFeedManager();
      applySignalShare(await manager.signalShareStatus());
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function toggleShareSignals(): Promise<void> {
    if (signalBusy) return;
    signalBusy = true;
    try {
      const manager = await getFeedManager();
      applySignalShare(await manager.setSignalSharing(!shareSignals));
      clearErrorUnlessPublishOpen();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      signalBusy = false;
    }
  }

  // ── Publish review-prune sheet (topic-factors.md § Publishing a trained
  // factor; frame D8). Single-instance, revealed in place, pre-targeted at
  // whichever row's Publish… button opened it (the admin-dns-rename-sheet
  // shape) — mirrors apps/fauna-linux/src/views/personalization/publish_sheet.rs.
  // Both halves of the act are shared Rust (priority #2): `scoreCorpusForFactor`
  // reads the corpus (the FeedManager's loaded window), `publishTrainedFactorList`
  // owns the whole publish lifecycle. This file is genuinely platform: reveal
  // the sheet, render the rows, collect what survived the prune.
  interface PublishExemplar {
    post_id: string;
    preview: string;
    score: number;
    include: boolean;
  }
  let publishTarget = $state<TrainedTopicRow | null>(null);
  let publishExemplars = $state<PublishExemplar[]>([]);
  let publishName = $state('');
  let publishBusy = $state(false);
  /** Bumped on every open/close — a corpus read that lands after the sheet was
   *  re-opened against a different factor (or closed) belongs to nobody
   *  current; a stale callback is dropped rather than rendered under the
   *  wrong target (mirrors linux `Ctx::generation`). */
  let publishGeneration = 0;

  let publishAnyIncluded = $derived(publishExemplars.some((e) => e.include));

  function publishError(e: unknown): string {
    const err = e as Partial<PublishListError | PublishModelError> | undefined;
    if (err?.message) return err.message;
    return e instanceof Error ? e.message : String(e);
  }

  // ── the Model kind (topic-factors.md § Publishing a trained factor, v2) ──
  // RAW-VALUE: `<option value>` is the wire discriminator itself, never a
  // translated label — the option a user picks and the words they read back
  // at `labeler-catalog-item-kind` must be the same text.
  const PUBLISH_KIND_LIST = 'list';
  const PUBLISH_KIND_MODEL = 'text-model';
  interface PublishNgramUi extends PublishNgram {
    include: boolean;
    directionText: string;
    countText: string;
  }
  let publishKind = $state(PUBLISH_KIND_LIST);
  // Populated once wasm is ready (onMount) — evaluating at module-parse time
  // would call into wasm before it exists (mirrors the family +page.svelte
  // `unknownSenderOptionLabels` precedent).
  let publishKindOpts = $state<{ value: string; label: string }[]>([]);
  let publishNgrams = $state<PublishNgramUi[]>([]);
  // The Model corpus-size facts, verbatim from the shared read. Do NOT
  // shrink these on prune — they are the artifact's own class doc counters
  // (the shared lifecycle's documented rule).
  let publishMoreDocs = $state(0);
  let publishLessDocs = $state(0);
  let publishIncludedExamples = $state(0);
  let publishMarkedExamples = $state(0);

  let publishAnyNgramIncluded = $derived(publishNgrams.some((n) => n.include));

  /** `-publish-kind-select`: swap which artifact kind the open sheet reviews.
   *  DISCARDS the prune and re-reads — the two kinds review different
   *  objects through different shared faces, so a carried-over prune would
   *  paint one kind's refusal state over the other's un-read corpus. The
   *  public name survives — it is the user's own words, equally true of
   *  either kind. Re-picking the open kind is a no-op. */
  async function setPublishKind(kind: string): Promise<void> {
    if (kind === publishKind || !publishTarget) return;
    const row = publishTarget;
    publishKind = kind;
    publishGeneration += 1;
    const generation = publishGeneration;
    publishExemplars = [];
    publishNgrams = [];
    publishMoreDocs = 0;
    publishLessDocs = 0;
    publishIncludedExamples = 0;
    publishMarkedExamples = 0;
    error = '';
    if (!row.factor_key) return;
    try {
      const manager = await getFeedManager();
      if (kind === PUBLISH_KIND_MODEL) {
        const review = (await manager.scrubCorpusForFactor(row.factor_key)) as {
          more_docs: number;
          less_docs: number;
          included_examples: number;
          marked_examples: number;
          ngrams: Array<{ ngram: string; more: number; less: number }>;
        };
        if (generation !== publishGeneration) return;
        publishMoreDocs = review.more_docs;
        publishLessDocs = review.less_docs;
        publishIncludedExamples = review.included_examples;
        publishMarkedExamples = review.marked_examples;
        publishNgrams = review.ngrams.map((n) => ({
          ...n,
          include: true,
          directionText: resolveLocalized(ngramDirectionLabel(n.more, n.less)),
          countText: resolveLocalized(ngramDocCountLabel(n.more, n.less)),
        }));
      } else {
        const scored = (await manager.scoreCorpusForFactor(row.factor_key)) as Array<{
          post_id: string;
          preview: string;
          score: number;
        }>;
        if (generation !== publishGeneration) return;
        publishExemplars = scored.map((e) => ({ ...e, include: true }));
      }
    } catch (e: unknown) {
      if (generation !== publishGeneration) return;
      error = e instanceof Error ? e.message : String(e);
    }
  }

  /** Reveal the sheet against one factor and score its corpus — the loaded
   *  feed window, deliberately not a paging crawl (§ Publishing's accepted
   *  limitation). */
  async function openPublishSheet(row: TrainedTopicRow): Promise<void> {
    publishTarget = row;
    // The sheet always opens on the List kind — the weaker disclosure default.
    publishKind = PUBLISH_KIND_LIST;
    publishGeneration += 1;
    const generation = publishGeneration;
    publishExemplars = [];
    publishNgrams = [];
    publishMoreDocs = 0;
    publishLessDocs = 0;
    publishIncludedExamples = 0;
    publishMarkedExamples = 0;
    publishName = '';
    error = '';
    // A corrupt (non-16-byte) id has no addressable model — unreachable via
    // the disabled publish button, kept as a defensive no-op.
    if (!row.factor_key) return;
    try {
      const manager = await getFeedManager();
      const scored = (await manager.scoreCorpusForFactor(row.factor_key)) as Array<{
        post_id: string;
        preview: string;
        score: number;
      }>;
      if (generation !== publishGeneration) return; // superseded by a close/re-open
      publishExemplars = scored.map((e) => ({ ...e, include: true }));
    } catch (e: unknown) {
      if (generation !== publishGeneration) return;
      error = e instanceof Error ? e.message : String(e);
    }
  }

  /** Close without publishing — drop the target + reviewed rows so a re-open
   *  cannot inherit a stale prune. */
  function closePublishSheet(): void {
    publishTarget = null;
    publishGeneration += 1;
    publishExemplars = [];
    publishNgrams = [];
    publishName = '';
  }

  /** Publish what survived the prune, through whichever kind's lifecycle is
   *  open. */
  async function submitPublish(): Promise<void> {
    const id = $identity;
    if (!id || !publishTarget || publishBusy) return;
    // A blank name is refused here rather than at the wire — the same guard
    // the create/rename flow uses.
    const name = publishName.trim();
    if (!name) return;
    publishBusy = true;
    try {
      if (publishKind === PUBLISH_KIND_MODEL) {
        const ngrams: PublishNgram[] = publishNgrams
          .filter((n) => n.include)
          .map((n) => ({ ngram: n.ngram, more: n.more, less: n.less }));
        // Unreachable through the button (disabled with nothing kept); kept
        // so "never publish an empty vocabulary" holds at the call too.
        if (ngrams.length === 0) return;
        // ⚠ Passed through UNSHRUNK by the prune — a shell must not
        // "helpfully" recount from the kept rows (the shared lifecycle's
        // documented rule).
        await publishTrainedFactorModel(
          id.secretHex,
          publishTarget.id,
          name,
          publishMoreDocs,
          publishLessDocs,
          ngrams,
        );
      } else {
        const entries = publishExemplars
          .filter((e) => e.include)
          .map((e) => ({ post_id: e.post_id, score: e.score }));
        if (entries.length === 0) return;
        await publishTrainedFactorList(id.secretHex, publishTarget.id, name, entries);
      }
      error = '';
      closePublishSheet();
    } catch (e: unknown) {
      error = publishError(e);
    } finally {
      publishBusy = false;
    }
  }

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as LabelerCatalogSnapshot) : null;
    error = snap?.error ? resolveLocalized(snap.error) : '';
  }

  /** `labeler-catalog-item-kind`'s ONE non-verbatim field — the
   *  `LabelerCatalogSection.svelte` twin. See that file's `kindBadgeText`
   *  doc for the full reasoning. */
  function kindBadgeText(entry: { artifact_kind: string; artifact_version: number }): string {
    const note = textModelNeedsNewerApp(entry.artifact_kind, entry.artifact_version);
    return note ? resolveLocalized(note) : entry.artifact_kind;
  }

  onMount(async () => {
    const id = $identity;
    if (!id) {
      ready = true;
      return;
    }
    try {
      await ensureWasm();
      publishKindOpts = publishKindOptions().map((o) => ({
        value: o.value,
        label: resolveLocalized(o.label),
      }));
      machine = await createLabelerCatalogMachine(
        { onChanged: () => applySnapshot() },
        await sharedRpcPort(id.secretHex),
        id.secretHex,
      );
      ready = true;
      await machine.refresh();
      applySnapshot();
      await loadTopics(id.secretHex);
      await loadSignalShare();
      timer = setInterval(() => { machine?.refresh(); }, 15000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      ready = true;
    }
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  // The caller's SUBSCRIBED labelers, indices preserved into the full
  // `entries` so `unsubscribe(index)` targets the right row (the machine's
  // gestures are index-addressed into the unfiltered snapshot).
  let subscribed = $derived(
    (snap?.entries ?? [])
      .map((entry, index) => ({ entry, index }))
      .filter((row) => row.entry.subscribed),
  );

  async function unsubscribe(index: number): Promise<void> {
    await machine?.unsubscribe(index);
    applySnapshot();
  }
</script>

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <section class="section" data-testid="personalization">
    <h2>{t.personalization.title}</h2>

    <div class="facet-links">
      <a href="/app/feed" class="btn" data-testid={IDS.PERSONALIZATION_FEEDS_LINK}>{t.personalization.feeds_link}</a>
      <a href="/app/settings/muted-words" class="btn" data-testid={IDS.PERSONALIZATION_MUTED_WORDS_LINK}>{t.personalization.muted_words_link}</a>
    </div>

    <h3>{t.personalization.trained_topics_title}</h3>
    <div class="add-row">
      <input
        type="text"
        class="input"
        data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_NAME_INPUT}
        placeholder={t.personalization.trained_factor_placeholder}
        bind:value={topicName}
        onkeydown={(e) => { if (e.key === 'Enter') submitTopic(); }}
      />
      <button
        class="btn btn-small"
        data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_CREATE_BUTTON}
        disabled={busy}
        onclick={submitTopic}
      >
        {renamingId ? t.personalization.trained_factor_save : t.personalization.trained_factor_create}
      </button>
    </div>

    <!-- The empty-state line carries NO test id — ui.yaml ratified no
         `-empty` element for this facet, and linux renders its empty label
         bare too. Inventing one here would be a rule-A deviation. The list
         container is always present (linux keeps its ListBox mounted), so the
         e2e's row count is the empty signal. -->
    {#if topics.length === 0}
      <p class="muted">{t.personalization.trained_topics_empty}</p>
    {/if}
    <div class="labeler-list" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_LIST}>
      {#each topics as row (row.id)}
          <!-- data-factor = the minted key's hex. The registry is sealed, so the
               e2e has no wire-side way to learn the key it must pass to the
               feed-factor-select; the web bridge answers get_attr(id, "factor")
               off `data-factor` (linux stamps the same via set_test_attr). -->
          <div class="labeler-item" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_ITEM} data-factor={row.id}>
            <div class="labeler-info">
              <span data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_NAME}>{row.name}</span>
              <span data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_EXAMPLE_COUNT}>
                {t.personalization.trained_factor_examples({ count: String(row.example_count) })}
              </span>
            </div>
            <!-- Layer-A opt-in (engagement-cues.md § Layer A): "Learn from my
                 activity". `data-state` carries "true"/"false" — the uniform
                 driver.get_attr(id, "state") idiom an unmarked native
                 Switch/CheckButton falls back to (linux's trained-topics row
                 stamps no override marker, so its generic Switch resolver
                 emits the same "true"/"false" strings). -->
            <label class="combo-label">
              <input
                type="checkbox"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_ENGAGEMENT_TOGGLE}
                data-state={row.learn_from_engagement ? 'true' : 'false'}
                checked={row.learn_from_engagement}
                disabled={busy}
                onchange={() => toggleEngagement(row)}
              />
              {t.personalization.trained_factor_engagement_toggle}
            </label>
            <div class="row-actions">
              <button
                class="btn btn-small"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_RENAME_BUTTON}
                onclick={() => startRename(row)}
              >
                {t.personalization.trained_factor_rename}
              </button>
              <!-- Publish… — open the review-prune sheet against THIS factor
                   (topic-factors.md § Publishing a trained factor; frame D8).
                   A corrupt (non-16-byte) id has no addressable model, so the
                   row simply offers no publish. -->
              <button
                class="btn btn-small"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_BUTTON}
                disabled={!row.factor_key}
                onclick={() => openPublishSheet(row)}
              >
                {t.personalization.trained_factor_publish}
              </button>
              <button
                class="btn btn-small"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_DELETE_BUTTON}
                disabled={busy}
                onclick={() => deleteTopic(row)}
              >
                {t.common.delete}
              </button>
            </div>
          </div>
      {/each}
    </div>

    <!-- Publish review-prune sheet (topic-factors.md § Publishing a trained
         factor; frame D8) — single instance, revealed in place directly under
         the Trained-topics facet, pre-targeted at the row whose Publish…
         button opened it (the admin-dns-rename-sheet shape). Mirrors
         apps/fauna-linux/src/views/personalization/publish_sheet.rs. -->
    {#if publishTarget}
      <div class="publish-sheet" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SHEET}>
        <h4>{t.personalization.publish_sheet_title}</h4>
        <!-- RAW-VALUE select: `<option value>` is the wire discriminator
             itself (fauna_core::format::publish_kind_options), so the word a
             user picks and the badge they read back at
             labeler-catalog-item-kind cannot drift apart. Swapping kind
             DISCARDS the prune and re-reads. -->
        <label class="publish-name-label" for="publish-kind-select">
          {t.personalization.publish_kind_label}
        </label>
        <select
          id="publish-kind-select"
          class="input"
          data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_KIND_SELECT}
          value={publishKind}
          onchange={(e) => setPublishKind((e.target as HTMLSelectElement).value)}
        >
          {#each publishKindOpts as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
        <p class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_LIMITATION_NOTE}>
          {#if publishKind === PUBLISH_KIND_MODEL}
            {t.personalization.publish_limitation_note_model}
            {t.personalization.publish_corpus_size({
              included: String(publishIncludedExamples),
              marked: String(publishMarkedExamples),
            })}
          {:else}
            {t.personalization.publish_limitation_note}
          {/if}
        </p>
        <label class="publish-name-label" for="publish-name-input">
          {publishKind === PUBLISH_KIND_MODEL
            ? t.personalization.publish_name_label_model
            : t.personalization.publish_name_label}
        </label>
        <!-- Starts blank on every open: the sealed registry name is PRIVATE
             (§ Publishing), so prefilling it would leak the user's own label
             into a public artifact by default. -->
        <input
          id="publish-name-input"
          type="text"
          class="input"
          data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NAME_INPUT}
          placeholder={t.personalization.publish_name_placeholder}
          bind:value={publishName}
        />
        {#if publishKind === PUBLISH_KIND_MODEL}
          <!-- The Model kind's review body — EVERY surviving n-gram. Where
               the List's exemplar rows bound endorsement of already-public
               ids, these rows ARE the disclosure, so there is no top-N. -->
          <h5>{t.personalization.publish_ngrams_title}</h5>
          {#if publishNgrams.length === 0}
            <p class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_EMPTY}>
              {t.personalization.publish_ngrams_empty}
            </p>
          {/if}
          <div
            class="labeler-list"
            data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_LIST}
          >
            {#each publishNgrams as ngram, i (ngram.ngram)}
              <div
                class="labeler-item"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_ITEM}
              >
                <input
                  type="checkbox"
                  data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_CHECKBOX}
                  data-state={ngram.include ? 'true' : 'false'}
                  checked={ngram.include}
                  onchange={() => { publishNgrams[i].include = !publishNgrams[i].include; }}
                />
                <span data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_TEXT}>
                  {ngram.ngram}
                </span>
                <!-- Direction and count are BOTH shared faces, read by the
                     subscriber's labeler-inspect-model-entry-* twins too, so
                     a publisher's review cannot disagree with what a
                     subscriber later sees. -->
                <span class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_DIRECTION}>
                  {ngram.directionText}
                </span>
                <span class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_COUNT}>
                  {ngram.countText}
                </span>
              </div>
            {/each}
          </div>
        {:else}
          <h5>{t.personalization.publish_exemplars_title}</h5>
          {#if publishExemplars.length === 0}
            <p class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_EMPTY}>
              {t.personalization.publish_exemplars_empty}
            </p>
          {/if}
          <div
            class="labeler-list"
            data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_LIST}
          >
            {#each publishExemplars as exemplar, i (exemplar.post_id)}
              <!-- Prune semantics mirror restore-kind-checkbox: every exemplar
                   arrives CHECKED, the user unchecks what they'd rather not
                   endorse. `data-state` carries the uniform driver.get_attr(id,
                   "state") idiom the engagement toggle above already uses — the
                   web bridge reads DOM attributes, not the live `checked`
                   property. -->
              <div
                class="labeler-item"
                data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_ITEM}
              >
                <input
                  type="checkbox"
                  data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_CHECKBOX}
                  data-state={exemplar.include ? 'true' : 'false'}
                  checked={exemplar.include}
                  onchange={() => { publishExemplars[i].include = !publishExemplars[i].include; }}
                />
                <span data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_TEXT}>
                  {exemplar.preview}
                </span>
                <span class="muted" data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_SCORE}>
                  {t.personalization.publish_score({ score: String(exemplar.score) })}
                </span>
              </div>
            {/each}
          </div>
        {/if}
        <div class="row-actions">
          <!-- Publishing nothing is not a thing the artifact means, so the
               button goes insensitive rather than no-op'ing a click (the
               restore-confirm-button precedent). Covers both "the corpus
               scored/scrubbed nothing" and "the user unchecked everything". -->
          <button
            class="btn btn-small"
            data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SUBMIT_BUTTON}
            disabled={(publishKind === PUBLISH_KIND_MODEL ? !publishAnyNgramIncluded : !publishAnyIncluded) || publishBusy}
            onclick={submitPublish}
          >
            {t.personalization.publish_submit}
          </button>
          <button
            class="btn btn-small"
            data-testid={IDS.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_CANCEL_BUTTON}
            onclick={closePublishSheet}
          >
            {t.common.cancel}
          </button>
        </div>
      </div>
    {/if}

    <!-- "Clear activity data" (engagement-cues.md § At rest): the
         user-revocable affordance for the sealed engagement-cue rollup —
         deletes cues:v1 from the user's own nest and resets the live engine. -->
    <button
      class="btn"
      data-testid={IDS.PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON}
      onclick={clearEngagementData}
    >
      {t.personalization.clear_engagement_data}
    </button>

    <!-- Layer-B opt-in (default off). `state` carries "on"/"off" — the
         report-share toggle contract every app answers, NOT the bare-switch
         "true"/"false" the per-topic engagement toggle above uses. -->
    <h3>{t.personalization.share_signals_title}</h3>
    <label class="combo-label">
      <input
        type="checkbox"
        data-testid={IDS.PERSONALIZATION_SHARE_SIGNALS_TOGGLE}
        data-state={shareSignals ? 'on' : 'off'}
        checked={shareSignals}
        disabled={signalBusy}
        onchange={(e) => { e.currentTarget.checked = shareSignals; void toggleShareSignals(); }}
      />
      {t.personalization.share_signals_label}
    </label>
    <p class="muted">{t.personalization.share_signals_subtitle}</p>

    <!-- The transparency pane: the nest-wide ≥k export view, exactly as a peer
         nest receives it. Read-only; rows are flat-indexed. -->
    <h4 data-testid={IDS.SIGNAL_SHARE_PUBLISHED_LIST}>{t.personalization.signal_published_title}</h4>
    <p class="muted">{t.personalization.signal_published_description}</p>
    {#if signalPublished.length === 0}
      <p class="muted">{t.personalization.signal_published_empty}</p>
    {:else}
      <div class="labeler-list">
        {#each signalPublished as entry}
          <div class="labeler-item" data-testid={IDS.SIGNAL_SHARE_PUBLISHED_LIST_ITEM}>
            <div class="labeler-info">
              <span data-testid={IDS.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_HASH}>{entry.content_hash}</span>
              <span data-testid={IDS.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_FACTOR}>{entry.factor}</span>
            </div>
            <span>
              <span data-testid={IDS.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_COUNT}>{entry.count}</span>
              {t.personalization.signal_published_contributors}
            </span>
          </div>
        {/each}
      </div>
    {/if}

    <h3>{t.labeler_catalog.title}</h3>
    {#if snap?.loaded && subscribed.length === 0}
      <p class="muted" data-testid={IDS.PERSONALIZATION_LABELERS_EMPTY}>{t.personalization.labelers_empty}</p>
    {:else if subscribed.length > 0}
      <div class="labeler-list" data-testid={IDS.PERSONALIZATION_LABELERS_LIST}>
        {#each subscribed as row (row.entry.labeler_id)}
          <div class="labeler-item" data-testid={IDS.LABELER_CATALOG_ITEM}>
            <div class="labeler-info">
              <span data-testid={IDS.LABELER_CATALOG_ITEM_PUBLISHER}>{row.entry.publisher_actor}</span>
              <!-- The artifact kind (list | wasm, machine-normalized) — a
                   curated List is distinguishable from an executable module
                   BEFORE inspect (content-moderation-and-ranking.md § Tier-3
                   artifact kinds). -->
              <span data-testid={IDS.LABELER_CATALOG_ITEM_KIND}>{kindBadgeText(row.entry)}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_CONTENT_KIND}>{row.entry.content_kind}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_VERSION}>{row.entry.version}</span>
              <span data-testid={IDS.LABELER_CATALOG_ITEM_FACTOR}>{row.entry.factor}</span>
            </div>
            <button class="btn btn-small" data-testid={IDS.LABELER_CATALOG_ITEM_UNSUBSCRIBE_BUTTON} onclick={() => unsubscribe(row.index)}>
              {t.labeler_catalog.unsubscribe}
            </button>
          </div>
        {/each}
      </div>
    {/if}

    <a href="/app/settings/labeler-catalog" class="btn" data-testid={IDS.PERSONALIZATION_BROWSE_CATALOG_BUTTON}>
      {t.personalization.browse_catalog}
    </a>
  </section>
{/if}

<style>
  h2 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  h3 { font-size: 1rem; margin: 1.5rem 0 0.5rem; color: var(--text-muted); }
  h4 { font-size: 0.9375rem; margin: 0 0 0.25rem; }
  h5 { font-size: 0.8125rem; margin: 0.75rem 0 0.25rem; color: var(--text-muted); }
  .muted { color: var(--text-muted); }
  .section { margin-bottom: 2rem; }
  .publish-sheet {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
    margin-bottom: 1rem;
    background: var(--bg-surface);
  }
  .publish-name-label { font-size: 0.8125rem; color: var(--text-muted); margin-top: 0.5rem; }
  .facet-links { display: flex; gap: 0.5rem; margin-bottom: 0.5rem; }
  .labeler-list { display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 1rem; }
  .labeler-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
  }
  .labeler-info { display: flex; flex-direction: column; gap: 0.125rem; font-size: 0.8125rem; }
  .add-row { display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.75rem; }
  .row-actions { display: flex; gap: 0.5rem; }
  .combo-label {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    cursor: pointer;
    color: var(--text);
    font-size: 0.8125rem;
    white-space: nowrap;
  }
  .input {
    flex: 1;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg-surface);
    color: var(--text);
  }
  .btn {
    display: inline-block;
    padding: 0.5rem 0.875rem;
    border-radius: 6px;
    background: var(--bg-hover);
    color: var(--text);
    border: 1px solid var(--border);
    cursor: pointer;
    text-decoration: none;
    margin-top: 1rem;
  }
  .btn-small { font-size: 0.8125rem; padding: 0.375rem 0.625rem; margin-top: 0; }
</style>
