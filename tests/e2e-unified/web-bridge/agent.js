// Web test agent — injected by Playwright bridge at session start.
// Reads/writes Svelte stores and localStorage for the unified test state protocol.
//
// The Svelte app exposes stores on window.__fauna_stores (set in +layout.svelte):
//   { identity, inbox, sent, groups, knocks, contacts, feedSnapshot, goto }
//
// Svelte writable stores have .subscribe() and .set() methods.
// We read values via the synchronous-subscribe pattern (same as svelte/store's get()).

/** Read the current value from a Svelte writable store. */
function readStore(store) {
  let value;
  const unsub = store.subscribe((v) => { value = v; });
  unsub();
  return value;
}

/**
 * The account registry's ACTIVE account, read straight out of its
 * localStorage shape (`fauna/index` + `fauna/{actor}/…` — web's
 * `LocalStorageSecretStore` keys every logical key verbatim), or null when no
 * account is active. The only place an identity is stored (2026-09-24); no
 * wasm needed, which is what lets this plain injected script read it.
 */
function readRegistryActive() {
  let idx;
  try {
    idx = JSON.parse(localStorage.getItem("fauna/index") || "null");
  } catch {
    return null;
  }
  if (!idx || !idx.active) return null;
  const actor = idx.active;
  const entry = (idx.accounts || []).find((a) => a.actor_id === actor) || {};
  return {
    actor_id: actor,
    secret_hex: localStorage.getItem(`fauna/${actor}/secret`),
    nest_url: localStorage.getItem(`fauna/${actor}/nest_url`),
    device_id: localStorage.getItem(`fauna/${actor}/device_id`),
    handle: entry.handle || null,
    domain: entry.domain || null,
  };
}

/**
 * Write ONE signed-in account into the registry's localStorage shape — the
 * state a real sign-in leaves behind — and make it the active account. The
 * account is UPSERTED into whatever index is there, never written over it:
 * a genuine actor switch has already emptied the index (`clearAccountRegistry`
 * in the patch branch below), so this lands as the one account, while a
 * same-actor patch over a seeded multi-account registry
 * (`test_account_switcher_web.py::_auth_on_account_page`: seed two, then
 * patch the session of the active one) keeps the siblings the test seeded.
 * Replacing the index here dropped them — every two-account web test went red
 * on 2026-09-25 with the second tab's switcher listing one row. Fields the patch does not carry
 * (tier, the re-auth flag, succession links) survive from the existing row.
 * `actor_id` is derived in Python (`drivers/web.py::set_state`) because this
 * script has no wasm to hand.
 */
function writeRegistryAccount({ actor_id, secret_hex, nest_url, device_id, handle, domain }) {
  let existing = null;
  try {
    existing = JSON.parse(localStorage.getItem("fauna/index") || "null");
  } catch {
    existing = null;
  }
  const accounts = (existing && Array.isArray(existing.accounts)) ? existing.accounts.slice() : [];
  const at = accounts.findIndex((a) => a && a.actor_id === actor_id);
  const prior = at >= 0 ? accounts[at] : {};
  const entry = {
    ...prior,
    actor_id,
    handle: handle || null,
    domain: domain || null,
    tier: prior.tier === undefined ? null : prior.tier,
    require_confirm_to_activate: prior.require_confirm_to_activate === undefined
      ? false
      : prior.require_confirm_to_activate,
  };
  if (at >= 0) accounts[at] = entry;
  else accounts.push(entry);
  const index = { ...(existing || {}), active: actor_id, accounts };
  localStorage.setItem("fauna/index", JSON.stringify(index));
  localStorage.setItem(`fauna/${actor_id}/secret`, secret_hex);
  if (nest_url) localStorage.setItem(`fauna/${actor_id}/nest_url`, nest_url);
  else localStorage.removeItem(`fauna/${actor_id}/nest_url`);
  if (device_id) localStorage.setItem(`fauna/${actor_id}/device_id`, device_id);
}

window.__faunaTestAgent = {
  getState() {
    const stores = window.__fauna_stores || {};

    // --- Session (from identity store + the account registry's localStorage) ---
    // The Identity Svelte store uses camelCase internally (secretHex, actorId),
    // so we map to the protocol's snake_case here. With no identity store yet,
    // fall back to the registry's own localStorage shape (`fauna/index` +
    // `fauna/{actor}/…`), the only place an identity is stored (2026-09-24).
    const id = stores.identity ? readStore(stores.identity) : null;
    const reg = readRegistryActive();
    const session = {
      authenticated: id ? id.registered : localStorage.getItem("fauna_registered") === "true",
      node_url: reg ? reg.nest_url : null,
      secret_hex: id ? id.secretHex : (reg ? reg.secret_hex : null),
      handle: id ? (id.handle || null) : (reg ? reg.handle : null),
      domain: id ? (id.domain || null) : (reg ? reg.domain : null),
      actor_id: id ? id.actorId : (reg ? reg.actor_id : null),
      device_id: null,
    };

    // --- Navigation (from URL path) ---
    const path = location.pathname.replace(/^\/app\/?/, "") || "feed";
    const segments = path.split("/").filter(Boolean);
    const stack = [{ view: segments[0] || "feed" }];
    if (segments.length > 1) {
      stack.push({ view: segments[0], id: segments[1] });
    }
    const nav = { stack, modal: null };

    // --- The `barrier` self-test's observable (convention 14) ---
    // Published by `$lib/barrier-e2e` through the app's automation surface, so
    // it exists only in a test-capable build (convention 15) — hence the guard
    // rather than a bare call.
    const barrierProbe =
      typeof window.__fauna_barrierProbeToken === "function"
        ? window.__fauna_barrierProbeToken()
        : null;
    // The frozen ack-time twin — the only one the self-test asserts.
    const barrierAckProbe =
      typeof window.__fauna_barrierAckProbe === "function"
        ? window.__fauna_barrierAckProbe()
        : null;
    // --- Session generation (convention 14's negative-assert observable) ---
    // Teardowns this seat has INITIATED. Backed by `sessionStorage` in
    // `$lib/generation-e2e`, deliberately: web's teardown is a document
    // navigation, so an in-memory counter would reset to 0 across exactly the
    // relaunch this key exists to detect — a false PASS, not a missing signal.
    const sessionGeneration =
      typeof window.__fauna_sessionGeneration === "function"
        ? window.__fauna_sessionGeneration()
        : 0;
    // --- The last succession's group sweep (the cross-app `succession_sweep`) ---
    // `null` until a succession runs on this seat. The ceremony renders its
    // outcome as ID-less prose and then NAVIGATES AWAY (`performSwitch`), so by
    // the time a journey can read anything, the document that held the report is
    // gone — which is why this rides `sessionStorage` rather than a store, the
    // same reasoning `$lib/generation-e2e.ts` records for the teardown counter.
    // tui's twin is `App::succession_sweep`, declared to outlive
    // `clear_session()` at the account switch.
    //
    // The object's SHAPE is the shared contract
    // (`fauna_client_recovery::ceremony::SweepStatus::state_json`) — written by
    // the wasm ceremony under `test-helpers`, so a production build publishes
    // nothing here and this reads `null`.
    const successionSweep = (() => {
      try {
        const raw = sessionStorage.getItem("fauna_e2e_succession_sweep");
        return raw === null ? null : JSON.parse(raw);
      } catch {
        // A storage-denied context, or a truncated write. `null` is the honest
        // answer and the journeys treat it as "no sweep ran", which is what a
        // seat that cannot store one is indistinguishable from anyway.
        return null;
      }
    })();
    // --- Critical-alert sweep pass counters (the sweep's causal barrier) ---
    // `{started, completed}` off the core wasm chunk's registry — the chunk the
    // session-start sweep posts to. Absent hook ⇒ null, which the shared helper
    // treats as a loud refusal rather than "no pass yet"
    // (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`).
    const alertSweepPasses =
      typeof window.__fauna_alertSweepPasses === "function"
        ? (() => {
            const pair = window.__fauna_alertSweepPasses();
            return { started: pair[0], completed: pair[1] };
          })()
        : null;
    // --- Folded-in inbound MLS commits, per channel (the twin-device barrier) ---
    // `{channel_hex: count}` off the live conversations manager's shared backend.
    // Absent hook ⇒ null, the same loud refusal as above: `{}` means "this tab has
    // folded nothing in yet", `null` means "this app has no leg", and collapsing
    // them would hang the barrier instead of naming the gap
    // (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`).
    const mlsFoldedCommits =
      typeof window.__fauna_mlsFoldedCommits === "function"
        ? window.__fauna_mlsFoldedCommits()
        : null;
    // --- Receive-loop cycles begun/finished (the delivery-path barrier) ---
    // `{started, completed, exit}` off the SPA's own receive pump, the web twin
    // of native's `ReceiveCycles`. Absent hook ⇒ null, the same loud refusal as
    // above: `{started: 0, completed: 0}` means "this tab has run no cycle yet",
    // `null` means "this app has no leg"
    // (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`). `exit` is `"stalled"` while
    // a pass has outrun the pump's ceiling (`$lib/receive-pump`), else `null` —
    // the frame checker's `receive-loop-alive` reads it.
    const convReceiveCycles =
      typeof window.__fauna_convReceiveCycles === "function"
        ? (() => {
            const pair = window.__fauna_convReceiveCycles();
            return {
              started: pair[0],
              completed: pair[1],
              exit:
                typeof window.__fauna_convReceiveExit === "function"
                  ? window.__fauna_convReceiveExit()
                  : null,
            };
          })()
        : null;
    // --- Feed reloads begun/committed (the reconnect re-fetch barrier) ---
    // `{started, completed}` off the shared wasm feed manager — the web read of
    // `fauna_e2e_agent::FEED_RELOADS_KEY`. Absent hook ⇒ null, the same loud
    // refusal as above: zeros mean "no reloads yet", `null` means "this app has
    // no leg".
    const feedReloads =
      typeof window.__fauna_feedReloads === "function"
        ? window.__fauna_feedReloads()
        : null;
    // --- Devices/Folders machine refreshes begun/committed ---
    // The `feed_reloads` twin for `DevicesMachine::refresh` — the web read of
    // `fauna_e2e_agent::DEVICES_REFRESHES_KEY`. Absent hook ⇒ null (no leg).
    const devicesRefreshes =
      typeof window.__fauna_devicesRefreshes === "function"
        ? window.__fauna_devicesRefreshes()
        : null;
    // --- Account-pump passes begun/completed + the runtime's role ---
    // `{started, completed, runtime, holder}` — the shared
    // `PumpCyclesView` off this tab's account runtime, the web read of
    // `fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`. Absent hook ⇒ null (no leg);
    // `runtime: false` means the leg is there and no runtime runs yet.
    const accountPumpCycles =
      typeof window.__fauna_accountPumpCycles === "function"
        ? window.__fauna_accountPumpCycles()
        : null;
    // --- Post-claim serving enablement begun/finished (the § 3b anchor) ---
    // `{started, completed, runs}` off the shared Rust step in the core wasm
    // chunk — the web read of `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`.
    // Absent hook ⇒ null: no runs means "not run yet", `null` means "no leg".
    const servingEnablement =
      typeof window.__fauna_servingEnablement === "function"
        ? window.__fauna_servingEnablement()
        : null;
    // --- New-message OS banners fired, + the diff-tick counters (the witness) ---
    // `{started, completed, fired: [{thread_id, label}]}` off
    // `$lib/message-banner`'s log — the web read of
    // `fauna_e2e_agent::MESSAGE_BANNERS_KEY`. Absent hook ⇒ null, and here that
    // refusal carries more weight than anywhere above: the assertions this key
    // serves are mostly negative, so an empty list from a tab with no leg would
    // pass them all.
    const messageBanners =
      typeof window.__fauna_messageBanners === "function"
        ? window.__fauna_messageBanners()
        : null;
    // --- The transport's state + the gate's verdict on it (the barrier) ---
    // `{state, online}` off `$lib/e2e-automation`'s accessor, which reads the
    // `connectionStatus` store and asks the `connectionIsOnline` wasm face.
    // Absent hook ⇒ null, the same loud refusal as above — and note the
    // boolean is NOT derived here: the offline word list has exactly one
    // owner, in Rust, and `helpers/connection.py` fails if a second one
    // appears anywhere in `tests/e2e-unified/`
    // (`fauna_e2e_agent::CONNECTION_KEY`).
    const connection =
      typeof window.__fauna_connection === "function"
        ? window.__fauna_connection()
        : null;
    // --- The launch clock this tab signs in on (the wrong-clock witness) ---
    // `{offset_secs, now_secs}` off the launch chunk's test-flavor getters
    // (`fauna_e2e_agent::CLOCK_KEY` owns the shape). Absent hook, or the launch
    // chunk not initialized yet ⇒ the key is OMITTED, never zeroed: an
    // `offset_secs: 0` from a tab with no leg would read as "the seed never
    // arrived" rather than "this app cannot answer".
    const clock =
      typeof window.__fauna_launchClock === "function"
        ? window.__fauna_launchClock()
        : null;
    // --- The held bearer's schedule on that clock (the wrong-clock REFRESH
    // witness, case M) — `{expires_in_secs, own_session_ids}`
    // (`fauna_e2e_agent::LAUNCH_TOKEN_KEY` owns the shape). OMITTED with no
    // hook or no identity, the same "cannot answer" rule as `clock`.
    const launchToken =
      typeof window.__fauna_launchToken === "function"
        ? window.__fauna_launchToken()
        : null;

    // --- Data (from Svelte stores — direct passthrough, both use snake_case) ---
    const rawInbox = stores.inbox ? readStore(stores.inbox) : [];
    const conversations = rawInbox.map((e) => ({
      post_id: e.post_id,
      from: e.from,
      to: e.to,
      subject: e.subject || "",
      body: e.body,
      timestamp: e.timestamp,
      encrypted: e.encrypted || false,
      read: true,
      attachments: (e.attachments || []).map((a) => ({
        hash: a.hash,
        media_type: a.media_type,
        size_bytes: a.size_bytes,
      })),
    }));

    const rawGroups = stores.groups ? readStore(stores.groups) : [];
    const groupsOut = rawGroups.map((g) => ({
      group_id: g.group_id,
      name: g.name,
      node_url: g.group_node,
      role: g.my_role,
      members: [],
      messages: [],
    }));

    const rawContacts = stores.contacts ? readStore(stores.contacts) : [];
    const contactsOut = rawContacts.map((c) => ({
      peer_id: c.peer_id,
      status: c.status,
      handle: null,
      node_url: null,
    }));

    const rawKnocks = stores.knocks ? readStore(stores.knocks) : [];
    const knocksOut = rawKnocks.map((k) => ({
      sender: k.sender,
      sender_node: k.sender_node,
      summary: k.summary,
      timestamp: k.created_at,
    }));

    // Conversation threads (the unified ConversationsManager snapshot, SMTP
    // receive rail). Published from the conversationThreads store, which
    // `refreshConversations()` sets straight from the wasm
    // `conversationThreadsJson()` export over
    // `state_json::conversation_threads_json` — so these rows ALREADY are the
    // shared e2e row shape, byte-for-byte the one tui/linux publish natively
    // and android/macOS/iOS re-parse.
    //
    // Passed through verbatim, never re-derived field-by-field. An explicit
    // per-field rebuild here is an allowlist: every field the shared function
    // grows and this list does not silently becomes `undefined`, and the
    // driver's `row.get(...) or []` then reads it as a legitimate empty value
    // rather than as the omission it is. That is not hypothetical — this map
    // omitted `participant_actor_ids` and `message_subject_lines`, so web
    // reported a permanently empty roster-identity list for a real, correctly
    // populated MLS group (`participant_count` was right the whole time,
    // because THAT field happened to be on the list), and no amount of polling
    // could ever have filled it.
    const rawThreads = stores.conversationThreads ? readStore(stores.conversationThreads) : [];
    const conversationThreads = rawThreads || [];
    // --- The member side of a succession (`data.succession_witness`) ---
    // Whole-object passthrough: the shape is the shared contract
    // (`fauna_client_recovery::witness::state_json`, rendered in wasm), and the
    // journeys read it in the order that report's own doc states. `null` means
    // this seat registered no witness — the honest answer, and the same one a
    // receive-only manager gives.
    const successionWitness = stores.successionWitness
      ? readStore(stores.successionWitness)
      : null;

    // The real-FaunaMls-backend opt-in flag (tier_3 test_fauna_mls_real_roundtrip).
    const convRealBackendActive = stores.convRealBackendActive
      ? readStore(stores.convRealBackendActive)
      : false;

    // The Feed page renders entirely from the shared `FeedManager` snapshot
    // (feed.md § State & data shape); its `posts` are `PostSummary` rows that
    // already carry body / tags / has_media / media_hash directly (the manager's
    // resolve_media fills media_hash lazily), so the e2e `data.feed.posts` reads
    // map 1:1 — no client-side decode cache to drain.
    const feedSnap = stores.feedSnapshot ? readStore(stores.feedSnapshot) : null;
    const feedPosts = (feedSnap?.posts || []).map((p) => ({
      post_id: p.post_id,
      author: p.author,
      body: p.body || "",
      timestamp: p.timestamp,
      tags: p.tags || [],
      has_media: p.has_media || false,
      media_hash: p.media_hash || "",
      is_reply: p.is_reply || false,
      // The four interaction counts the bar renders (feed.md § Interaction bar).
      // `?? 0` rather than `|| 0` deliberately: the count is legitimately 0 and
      // must stay a number. Omitting them made the count UNASSERTABLE here — the
      // e2e reader answered `null`, which reads like "no activity" rather than
      // "this app never told you" (convention 7's silent-no-coverage class).
      like_count: p.like_count ?? 0,
      reply_count: p.reply_count ?? 0,
      repost_count: p.repost_count ?? 0,
      quote_count: p.quote_count ?? 0,
      // The like toggle's viewer state (feed.md § Interaction bar → Repost).
      // Same reason as the counts: omitted, the e2e reader answers `null`,
      // which reads like "not liked" rather than "this app never told you".
      viewer_liked: p.viewer_liked ?? false,
      // The repost carrier + per-viewer pair (feed.md § Interaction bar →
      // Repost, ratified 2026-08-10). `reposted_post_id` is how the harness
      // tells a repost row from an empty quote until it can read the
      // `repost-attribution` element directly; `viewer_repost_id` is the
      // toggle's state (and `unrepost`'s argument). `?? null` deliberately
      // (not `|| null`): an empty string is not a legitimate post id here.
      reposted_post_id: p.reposted_post_id ?? null,
      viewer_repost_id: p.viewer_repost_id ?? null,
      // Every link preview in the body with its state, in body order (the
      // shared `RenderDocument::link_previews` — render-model.md § D4), so a
      // test can wait for a preview to FAIL before reading "no card". Omitted
      // when the SPA has no hook (the test then refuses by name).
      ...(typeof window.__fauna_feedLinkPreviews === "function"
        ? { link_previews: window.__fauna_feedLinkPreviews(p.document) }
        : {}),
    }));

    // --- Messages (from window.__fauna_messages, set by MessageBanner component) ---
    // Intentional: || coerces empty string to null. MessageBanner sets '' on dismiss,
    // which maps to null in state (= "no active message"). ActionLayer treats both the same.
    //
    // `messages` is `null` outright when no MessageBanner instance is currently
    // mounted (e.g. onboarding, which bypasses the shell — see +layout.svelte's
    // isOnboarding/isAdmin/isSettings bare-slot branch). Without this,
    // `window.__fauna_messages` — set once by ANY page's MessageBanner and never
    // cleared on navigation (it's a plain window global, not reset by SPA nav) —
    // makes this always return `{error: null, ...}` even on a page with no banner
    // at all. ActionLayer's error_text()/has_error() try this state first and only
    // fall back to the DOM `error-message` element when `messages` is `None`; an
    // always-present-but-all-null `messages` object short-circuits that fallback
    // permanently, hiding a real page-local error the DOM element does show (found
    // via onboarding's identity-import parse error, onboarding.md §1).
    // A mounted banner with NOTHING TO SAY is likewise `null` — the same defect
    // one step further in, and the one that hid the conversations send failure.
    // Keying only on "is a banner mounted" is not enough: `/app/conversations`
    // DOES take the shell branch, so the banner is mounted, so `messages` was a
    // present-but-all-null object — which short-circuits the DOM fallback exactly
    // as described above. But the banner is a CROSS-PAGE TRANSIENT surface; the
    // page's own page-level error is a DIFFERENT element that merely shares the
    // `error-message` id (`conversations/+page.svelte`'s `{#if displayError}` vs
    // `MessageBanner.svelte`'s own span — both spelled `error-message`, as
    // ui.yaml intends: every page has one, and the banner is a component that
    // renders one). So an EMPTY banner is not the authority on the page's error,
    // and reporting it as such made every conversations-page error invisible to
    // `error_text()`/`has_error()` — the over-inline-ceiling send refusal read as
    // a silent product hang for two sessions. When the banner has no active
    // message, defer to the DOM element, which is what the user actually sees.
    //
    // ⚠ **OMITTED, not published as `null`** — the two are the same to every
    // consumer but opposite to the frame-invariant checker. `_message_from_state`
    // (actions/__init__.py) reads `messages.<level>` and falls back to the DOM
    // whenever the result is `None`, which `state.get` produces for an absent key
    // and a null value alike — so omission preserves the whole DOM-fallback
    // contract reasoned out above, byte for byte. But `helpers/frame_invariants.py`
    // deliberately splits those cases: an ABSENT key is "this app does not publish
    // it" (n/a), while a PUBLISHED NULL is "the field lost its shape" (a
    // violation) — a distinction red-verified against a tui mutation that
    // serialized as null on a no-error frame and would otherwise have escaped.
    // Publishing `null` here made web violate that invariant on 100% of its frames
    // (first GUI-driver corpus, 2026-08-16, 10/10). Omitting says what web means.
    const bannerMounted = (window.__fauna_message_banner_mount_count || 0) > 0;
    const msgs = (window.__fauna_messages) || {};
    const bannerHasMessage = !!(msgs.error || msgs.warning || msgs.info);
    const messages = (bannerMounted && bannerHasMessage) ? {
      error: msgs.error || null,
      warning: msgs.warning || null,
      info: msgs.info || null,
    } : null;

    // --- The connection-gap journeys' window counters ---
    // `connection_reports` (every report the transport published vs. how many
    // changed its word) and `painted_errors` (every error surface a painted
    // frame showed), both counted by the natives' shared Rust in the core wasm
    // chunk (`$lib/loud-surface-e2e`). Absent hook, or a bundle with no leg ⇒
    // null, which the journeys refuse loudly rather than read as zero
    // (`fauna_e2e_agent::{CONNECTION_REPORTS_KEY, PAINTED_ERRORS_KEY}`).
    const connectionReports =
      typeof window.__fauna_connectionReports === "function"
        ? window.__fauna_connectionReports()
        : null;
    const paintedErrors =
      typeof window.__fauna_paintedErrors === "function"
        ? window.__fauna_paintedErrors()
        : null;

    return {
      session,
      nav,
      connection_reports: connectionReports,
      painted_errors: paintedErrors,
      barrier_probe: barrierProbe,
      barrier_ack_probe: barrierAckProbe,
      session_generation: sessionGeneration,
      alert_sweep_passes: alertSweepPasses,
      mls_folded_commits: mlsFoldedCommits,
      conv_receive_cycles: convReceiveCycles,
      feed_reloads: feedReloads,
      devices_refreshes: devicesRefreshes,
      account_pump_cycles: accountPumpCycles,
      serving_enablement: servingEnablement,
      message_banners: messageBanners,
      // Convention 17's `region-block-never-silent` counts (`$lib/region.svelte`):
      // OMITTED when the SPA has no hook, which the invariant reads as n/a.
      ...(typeof window.__fauna_regionBlockRender === "function"
        ? { region_block_render: window.__fauna_regionBlockRender() }
        : {}),
      // The `enable_caldav_mailbox` command's outcome (`$lib/mail-caldav-e2e`),
      // in linux's exact wire shape (`{ok: true}` / `{ok: false, error}`) — the
      // key `helpers/mail_dedicated_nest.py` polls. OMITTED until a run
      // completes, which is how the helper tells "not finished" from "failed".
      ...(window.__fauna_caldav_mailbox_reply
        ? { caldav_mailbox_reply: window.__fauna_caldav_mailbox_reply }
        : {}),
      // The `serve_enable_folder` command's outcome (`$lib/conversations`), in
      // tui's exact wire shape (`{ok: true, served_sets}` / `{ok: false,
      // error}`) — the key `helpers/webdav_roundtrip.py` polls, omitted until a
      // run completes.
      ...(window.__fauna_webdav_serve_reply
        ? { webdav_serve_reply: window.__fauna_webdav_serve_reply }
        : {}),
      connection,
      ...(clock === null ? {} : { clock }),
      ...(launchToken === null ? {} : { launch_token: launchToken }),
      ...(messages === null ? {} : { messages }),
      settings: (() => {
        const checked = document.querySelector('input[name="inbox-mode"]:checked');
        return { inbox_mode: checked ? checked.value : null };
      })(),
      data: {
        conversations,
        conversation_threads: conversationThreads,
        conv_real_backend_active: convRealBackendActive,
        groups: groupsOut,
        contacts: contactsOut,
        knocks: knocksOut,
        feed: { posts: feedPosts },
        events: null,
        notifications: null,
        sync: null,
        succession_sweep: successionSweep,
        succession_witness: successionWitness,
      },
    };
  },

  applyPatch(patch) {
    const stores = window.__fauna_stores || {};

    // Erase the multi-account registry namespace (`fauna/index` +
    // `fauna/{actor}/*`). The product's `identity.logout()` does this via
    // shared Rust (`AccountRegistry::clear_all`) and both callers below wait
    // for it, but only for a bounded time — and this sweep is what a sign-out
    // that overran leaves the next test. A prefix sweep is layout-agnostic: it
    // never encodes the per-actor key structure, only the namespace that
    // structure lives under. The flat `fauna_`-underscore keys are untouched
    // here and removed explicitly by each caller below.
    //
    // Leaving the registry behind is what made a seeded-actor test log in as the
    // PREVIOUS test's actor: reset cleared `fauna_secret`, `fauna/index` kept
    // pointing at the old account, and the next page load's `accountsBoot()`
    // booted that account from its still-stored secret.
    const clearAccountRegistry = () => {
      for (const key of Object.keys(localStorage)) {
        if (key.startsWith("fauna/")) localStorage.removeItem(key);
      }
    };

    // The product's own sign-out, awaited: it retires this browser's
    // enrollment over the live session before it wipes and erases — the
    // `StopReason::SignOut` teardown every native app's `reset` runs — so a
    // sweep's resets do not strand one enrolled writer per test on the shared
    // actor (`sync-agent-credentials.md` § Implementation status today, the
    // 2026-09-14 whole-suite accrual). Bounded: a sign-out that hangs must not
    // hang the suite, and whatever it left is finished by the next load's
    // reconcile, which the sign-out record it wrote first orders. Exempt from
    // the other-tab refusal, which is the Settings gesture's. The product's
    // own worst case is 10 s — the runtime's stop budget plus the store
    // erase's budget, 5 s each — so this is a backstop it should never reach.
    const SIGN_OUT_BUDGET_MS = 15000;
    const signedOut = () => {
      if (!stores.identity) return Promise.resolve();
      const done = Promise.resolve()
        .then(() => stores.identity.logout())
        .catch(() => {});
      return Promise.race([done, new Promise((r) => setTimeout(r, SIGN_OUT_BUDGET_MS))]);
    };

    // Every account store left in the origin — the IndexedDB databases and
    // OPFS directories under the account-store root
    // (`fauna_account_store::root::PLATFORM_ROOT`). The product erases the
    // stores of the accounts a sign-out reaches; this takes the ones no
    // registry names any more (a test that seeded or swept `fauna/` by hand),
    // so one test's replica is never the next test's lost-slot heal. The
    // database deletes are issued and not awaited: one blocked behind a
    // connection still open would otherwise hold the reset.
    const sweepAccountStores = async () => {
      const ROOT = "fauna-account-store";
      try {
        for (const db of await indexedDB.databases()) {
          if (db.name && db.name.startsWith(ROOT + "/")) indexedDB.deleteDatabase(db.name);
        }
      } catch {}
      try {
        const root = await navigator.storage.getDirectory();
        const names = [];
        for await (const name of root.keys()) names.push(name);
        await Promise.all(
          names
            .filter((name) => name.startsWith(ROOT))
            .map((name) => root.removeEntry(name, { recursive: true }).catch(() => {})),
        );
      } catch {}
    };

    // A barrier probe token is scoped to ONE test — the same lifetime tui's
    // `App::barrier_probe` and linux's `link::clear_barrier_probe` give it, and
    // cleared on the same two actions those apps clear it on.
    if (
      (patch.__action === "reset" || patch.__action === "logout") &&
      typeof window.__fauna_clearBarrierProbe === "function"
    ) {
      window.__fauna_clearBarrierProbe();
    }

    // --- Special actions ---
    if (patch.__action === "reset") return signedOut().then(sweepAccountStores).then(() => {
      // ...and the account registry, should the sign-out have overrun.
      clearAccountRegistry();
      // Clear all data stores
      if (stores.inbox) stores.inbox.set([]);
      if (stores.sent) stores.sent.set([]);
      if (stores.groups) stores.groups.set([]);
      if (stores.contacts) stores.contacts.set([]);
      if (stores.knocks) stores.knocks.set([]);
      if (stores.feedSnapshot) stores.feedSnapshot.set(null);
      // Clear fauna_registered too — the registry sweep above takes every
      // identity slot but not this flag, and getState() falls back to it
      // when the identity store is null (post-logout). Without this, tests
      // that reset after a set_state(authenticated=true) see
      // authenticated=true on the very next get_state. Every wizard-resume
      // slot (pending invite / awaiting DNS / pending factory reset) is a
      // per-actor `fauna/{actor}/…` row the same sweep already took.
      localStorage.removeItem("fauna_registered");
      // Clear wizard machine state. The handle-first OnboardingMachine's
      // wasm-bindgen instance lives inside the `wasm` module's linear
      // memory. We MUST do a hard `location.href` reload (not SvelteKit
      // `goto`) for the reset to take effect:
      // - SvelteKit `goto` is client-side; modules stay loaded, the
      //   wasm-bindgen module's `wasm` binding stays initialized, and
      //   any FinalizationRegistry-managed cleanup is unpredictable.
      //   Constructing a new OnboardingMachine in that environment
      //   intermittently throws "memory access out of bounds".
      // - Hard reload reloads every module fresh — wasm boilerplate
      //   reinitializes default(), the machine constructs cleanly.
      // The cost is ~200ms per test reset. Worth it for determinism.
      // Drop the nest dial override too ($lib/api). Clearing it is as
      // load-bearing as installing it: the override outlives the wizard that
      // installed it, so a stale one would point the NEXT test's launch at a
      // torn-down nest (`fauna_launch_machine::dial` — the same reason the Rust
      // seam's setter takes `None`).
      try { sessionStorage.removeItem("fauna_e2e_nest_dial_override"); } catch {}
      try { window.__fauna_resetOnboardingMachine?.(); } catch {}
      location.href = "/app/onboarding";
    });

    if (patch.__action === "logout") return signedOut().then(sweepAccountStores).then(() => {
      clearAccountRegistry();
      // Always also clear the agent's authenticated-fallback flag, which
      // neither identity.logout() nor the registry sweep touches.
      localStorage.removeItem("fauna_registered");
      if (stores.goto) {
        stores.goto("/app/onboarding");
      } else {
        location.href = "/app/onboarding";
      }
    });

    // --- Session patch ---
    // We set localStorage directly and then update the identity store value.
    // This avoids calling identity.login() which requires WASM (actorIdFromSecret).
    // The test patch already provides actor_id, so we don't need WASM at all.
    if (patch.session) {
      const s = patch.session;
      // A genuine actor switch (this patch's secret_hex differs from whatever
      // was already stored) must reset the feed + conversations manager
      // singletons FIRST, the plain-JS half of what the product's real
      // `identity.login()`/`logout()` do (store.ts) — those functions return
      // an already-built manager unconditionally regardless of which identity
      // built it, so without this reset a multi-actor test (login as author,
      // then as subscriber/buyer) keeps rendering the FIRST actor's manager to
      // every actor after it: same nest, the second actor sees the first
      // actor's still-live custody view instead of their own; a different nest
      // (a fresh per-test fixture), the manager's connection points at a
      // torn-down nest and every read comes back empty. `identity.login()`
      // itself isn't callable here (needs WASM's `actorIdFromSecret`), but the
      // reset halves are plain module-state resets — no WASM needed. That last
      // clause is a REQUIREMENT of every registered drop, not an observation:
      // `resetScreenTime` violated it by reading a lazy constructor that builds
      // through wasm, and threw here on every test login until it was fixed —
      // silently, since `actorScope.ts` guards each drop. `test_web_boot_effect_
      // loop.py::test_boot_drops_every_actor_scoped_reset_without_throwing` is
      // what keeps the clause true.
      const prior = readRegistryActive();
      const priorSecret = prior ? prior.secret_hex : null;
      if (s.secret_hex !== undefined && s.secret_hex !== priorSecret) {
        window.__fauna_resetIdentityScopedManagersForTest?.();
        // ...and REPLACE the account registry, which is the only source of
        // truth (`accounts.ts` § accountsBoot): leaving `fauna/index` naming
        // the OUTGOING actor active would have the next page load's
        // `identity.init()` rebuild the session as that actor, after the feed
        // manager was already built for the incoming one — `getClient()`
        // then RETIRES the incoming actor's WS client and every read comes
        // back empty with no error (the web half of the actor-switch
        // class; linux had the identical bug).
        //
        // A plain localStorage rewrite is the right move here, not
        // `addAccount` + `setActive`: it is synchronous (the shared-Rust path
        // needs WASM, and we navigate on the next lines), layout-agnostic,
        // and cannot trip the `require_confirm_to_activate` activation gate a
        // registry `setActive` enforces. A test that needs a MULTI-account
        // registry seeds it explicitly (`test_account_switcher_web.py::
        // _seed_registry`) and switches through the real UI path, not through
        // `set_state`.
        clearAccountRegistry();
        // ...and this TAB's own pin (+ its nest-url sibling, `sessionStorage`,
        // `$lib/tabPin.ts`) — the registry sweep above only ever touched
        // `localStorage`, so a tab pinned to the OUTGOING actor (by an earlier
        // real page load) would otherwise keep naming it for the rest of the
        // tab's life: `accountsSessionMaterial()` then fails closed for every
        // read that resolves through the pin, and `accounts.ts`'s boot
        // finds the pin no longer matching the newly-active account. `identity.logout()` reaches the same clear via
        // `accountsClearAll()`, but that path needs WASM and this patch is
        // applied with no reload — so call the SPA's own synchronous erase
        // directly instead of hand-listing the pin's keys here too.
        window.__fauna_clearTabPinForTest?.();
      }
      // The persisted half: ONE signed-in account in the registry's own
      // shape — the state a real sign-in leaves behind. A patch without a
      // secret only updates the fields it carries on the account already
      // there. The patch's device id lands in the account's own per-actor
      // slot, which is the ONE value `devices.md` § This-device marker needs
      // to do both jobs (the id the app registers with is the id the marker
      // compares against — `$lib/device-id`'s `getDeviceId(actorId)` reads it
      // back; without it web minted its own on first read, mirroring tui's `media::adopt_device_id_hex` + linux's
      // `sync::adopt_device_id_hex`).
      const current = readRegistryActive();
      const secretHex = s.secret_hex !== undefined ? s.secret_hex : (current ? current.secret_hex : null);
      const actorId = s.actor_id || (current ? current.actor_id : null);
      if (secretHex && actorId) {
        writeRegistryAccount({
          actor_id: actorId,
          secret_hex: secretHex,
          nest_url: s.node_url !== undefined ? s.node_url : (current ? current.nest_url : null),
          device_id: s.device_id !== undefined && s.device_id !== null ? s.device_id : (current ? current.device_id : null),
          handle: s.handle !== undefined ? (s.handle || null) : (current ? current.handle : null),
          domain: s.domain !== undefined ? (s.domain || null) : (current ? current.domain : null),
        });
      }
      if (s.authenticated !== undefined) localStorage.setItem("fauna_registered", String(s.authenticated));

      // Update the Svelte identity store directly (bypasses WASM).
      // The Identity store uses camelCase internally, so we map from protocol snake_case.
      if (stores.identity && stores.identity.set) {
        if (s.authenticated === false) {
          stores.identity.set(null);
        } else {
          // Build identity object from patch + the registry's active account
          const after = readRegistryActive();
          const secret = s.secret_hex || (after ? after.secret_hex : null);
          if (secret) {
            stores.identity.set({
              secretHex: secret,
              actorId: s.actor_id || (after ? after.actor_id : ""),
              handle: s.handle || (after ? after.handle : null) || undefined,
              domain: s.domain || (after ? after.domain : null) || undefined,
              registered: s.authenticated !== undefined ? s.authenticated : localStorage.getItem("fauna_registered") === "true",
            });
          }
        }
      }
    }

    // --- Navigation patch ---
    if (patch.nav) {
      const stack = patch.nav.stack;
      if (stack && stack.length > 0) {
        // Web merges some standalone native views into Settings; aliases ensure the
        // cross-app tests navigate to where the surface actually lives.
        // `moderation` is embedded in Settings → Privacy (moderation.md § Goal), so
        // there is NO /app/moderation route: navigating to one made SvelteKit fall
        // back to a FULL-PAGE load, which reloads the SPA and drops the in-memory
        // `LocalDetectionStore` behind the moderation queue's local half — the queue
        // then rendered empty no matter what. Alias it to the real route so the
        // navigation stays client-side and app state survives.
        const VIEW_ALIASES = { status: "settings", moderation: "settings/privacy" };
        const view = VIEW_ALIASES[stack[0].view] || stack[0].view;
        // A `profile` entry may carry an `actor_id` (the OTHER profile — the
        // state-protocol twin of the contacts tap-through's
        // `goto('/app/profile/<id>')`). Route it through the SAME per-target
        // route the real UI uses (before this the id was simply
        // dropped and the nav opened the SELF profile — the silent
        // dropped-command shape convention 11 forbids). NORMALIZED first,
        // matching linux/tui/android's `profile_nav_target`: an id naming the
        // VIEWER routes to the bare /app/profile (the page renders OTHER shape
        // for any non-empty param that differs from the identity, and its own
        // isSelf compare is exact — trim + case belong here, once, like the
        // other three legs). Blank → SELF, never a profile for the empty
        // actor.
        if (view === "profile" && typeof stack[0].actor_id === "string") {
          const target = stack[0].actor_id.trim();
          const id0 = stores.identity ? readStore(stores.identity) : null;
          const me = (id0 && id0.actorId ? id0.actorId : "").trim().toLowerCase();
          const isSelf = !target || (me && target.toLowerCase() === me);
          const path = isSelf ? "/app/profile" : "/app/profile/" + target;
          if (stores.goto) { stores.goto(path); } else { location.href = path; }
          return;
        }
        let id = stack.length > 1 ? stack[1].id : null;
        // The admin shell's default sub-page "dashboard" is the /app/admin INDEX
        // route on web (admin/+page.svelte renders admin-dashboard-heading there),
        // NOT a /app/admin/dashboard child. Native shells name it explicitly in the
        // nav stack (crash_recovery.inject_admin_session sends {admin},{admin,id:
        // dashboard}); collapse it back to the index so it lands on the real page
        // instead of a 404 fallback that renders no admin surface.
        if (view === "admin" && id === "dashboard") id = null;
        // The admin sub-page nav ids are the cross-app ui.yaml page ids
        // (`admin-dns`, `admin-mail`, …), but web's routes drop the doubled
        // prefix inside the /app/admin shell (`/app/admin/dns`, web.md § Route
        // Structure). Map id → route here, once; the ids stay untouched.
        if (view === "admin" && id && id.startsWith("admin-")) id = id.slice("admin-".length);
        let path = "/app/" + view;
        if (id) path += "/" + id;
        if (stores.goto) {
          stores.goto(path);
        } else {
          location.href = path;
        }
      }
    }

    // --- Messages patch (inject error/warning/info into MessageBanner) ---
    if (patch.messages) {
      if (!window.__fauna_messages) window.__fauna_messages = {};
      if (patch.messages.error !== undefined) window.__fauna_messages.error = patch.messages.error || '';
      if (patch.messages.warning !== undefined) window.__fauna_messages.warning = patch.messages.warning || '';
      if (patch.messages.info !== undefined) window.__fauna_messages.info = patch.messages.info || '';
      // Dispatch custom event so MessageBanner picks up the change
      window.dispatchEvent(new CustomEvent('fauna-message-update', { detail: window.__fauna_messages }));
    }

    // --- Data patch (direct passthrough — both protocol and stores use snake_case) ---
    if (patch.data) {
      if (patch.data.conversations && stores.inbox) {
        stores.inbox.set(patch.data.conversations.map((c) => ({
          post_id: c.post_id,
          from: c.from,
          to: c.to,
          subject: c.subject || "",
          body: c.body,
          timestamp: c.timestamp,
          valid: true,
          encrypted: c.encrypted || false,
        })));
      }
      if (patch.data.groups && stores.groups) {
        stores.groups.set(patch.data.groups.map((g) => ({
          group_id: g.group_id,
          name: g.name,
          group_node: g.node_url || "",
          my_role: g.role || "user",
        })));
      }
      if (patch.data.contacts && stores.contacts) {
        stores.contacts.set(patch.data.contacts.map((c) => ({
          peer_id: c.peer_id,
          status: c.status,
        })));
      }
      if (patch.data.knocks && stores.knocks) {
        stores.knocks.set(patch.data.knocks.map((k) => ({
          id: 0,
          sender: k.sender,
          sender_node: k.sender_node,
          summary: k.summary,
          created_at: k.timestamp,
        })));
      }
      // No `data.feed.posts` inject path: the Feed page renders from the shared
      // FeedManager snapshot, whose post list comes from the nest (feed.md
      // § State & data shape) — there's no client-side post store to patch. Web
      // feed tests create posts through the real composer (tier_3).
    }
  },
};
