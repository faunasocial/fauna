<script lang="ts">
  import { page } from '$app/stores';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import AppShellFrame from '$lib/components/AppShellFrame.svelte';

  // The Settings shell is a vertical **sidebar-swap** (settings.md § Navigation
  // model, ratified 2026-06-03 — the same shape as the admin shell): while in
  // `/app/settings` this layout renders its OWN shell whose vertical rail takes
  // over the app's sidebar slot. The root layout (routes/+layout.svelte) yields
  // the full viewport for any `/app/settings` route (the same bare-canvas bypass
  // it uses for onboarding/admin), so the normal app nav (conversations / feed /
  // …) is replaced in place rather than sitting beside the settings content.
  // `settings-nav-back` (atop the rail) swaps it back to the non-settings app
  // (Conversations), parallel to `admin-nav-back`. The shared `.shell`/
  // `.sidebar`/`.content` frame lives in `AppShellFrame` (this shell's own
  // `.tab`/nav-back/title chrome stays here — see that component's doc
  // comment). Reference: linux `views/settings_shell.rs` + `views/nav_rail.rs`.

  let { children } = $props();

  // Flat rail, one entry per page (settings.md § Navigation model). `id` is the
  // sub-page slug = the e2e two-element nav id (`{"view":"settings","id":<id>}`),
  // which the web test agent maps to `/app/settings/<id>`. The Status entry is
  // the default (no slug) — the former standalone status page, folded in first.
  // Labels come from the shared i18n strings (en.yaml), mirroring the exact key
  // each native app already uses for the same settings-shell nav entry
  // (apple `Core/SettingsPage.swift` label switch; windows `SettingsShellPage.xaml`;
  // android settings nav) so all 7 apps render the same text off the same keys
  // (priority #1/#3). No web-only keys are minted — every key already exists. The
  // five mail entries deliberately drop web's former "Mail " label prefix to match
  // the canonical native labels (Aliases / Spam / Export mailbox / Lists / Members).
  const railEntries = [
    { id: '', label: t.common.status },
    { id: 'account', label: t.common.account },
    // Members To Review — the permanent post-succession unattested-member
    // review page (succession-aftermath.md § Propagation item (iv); rail
    // slot user-approved 2026-08-16, directly after Account — mirrors
    // tui/linux/android). Holds whatever a review sweep left unanswered;
    // no sweep gate of its own.
    { id: 'member-review', label: t.settings.member_review_page.title },
    { id: 'privacy', label: t.settings.privacy },
    // Muted words (moderation.md § Muted keywords; settings.md § Navigation
    // model line 17 — rail order: right after Privacy). Sealed client-side
    // user-global keyword list; the collapse render lives on the Conversations
    // page, this rail entry is only the CRUD sub-page.
    { id: 'muted-words', label: t.muted_words.title },
    // Personalization home + Community-labelers catalog
    // (content-moderation-and-ranking.md § Composition + § Tier-3) — right
    // after Muted words, its sibling personal-filtering surface.
    { id: 'personalization', label: t.personalization.title },
    { id: 'labeler-catalog', label: t.labeler_catalog.title },
    { id: 'general', label: t.settings.general },
    { id: 'encryption', label: t.settings.encryption_page.title },
    // Devices (roster) + Folders (control plane) — the 2026-06-28 sync/folder
    // UI unification moved the former top-level Devices/Peers page into the
    // Settings shell and renamed the former `sync` sub-page to `folders`
    // (devices.md / folders.md). `t.folders.title` = "Folders".
    { id: 'devices', label: t.common.devices },
    { id: 'folders', label: t.folders.title },
    { id: 'p2p', label: t.status.p2p.title },
    { id: 'nostr', label: t.nostr.title },
    // Bluesky — the ATProto login-plane settings page, right after Nostr
    // (settings.md § Navigation model — the sibling federation-protocol
    // bridge settings page; atproto-pds-full.md § App surface).
    { id: 'atproto', label: t.atproto_settings.title },
    { id: 'subscription-settings', label: t.subscriptions.title },
    { id: 'web', label: t.web_settings.title },
    { id: 'mail-settings', label: t.mail_settings.title },
    { id: 'mail-aliases', label: t.mail_aliases.title },
    { id: 'mail-spam', label: t.mail_spam.title },
    { id: 'mail-export', label: t.mail_export.title },
    { id: 'mail-import', label: t.mail_import.title },
    { id: 'mail-lists', label: t.mail_lists.title },
    { id: 'mail-list-members', label: t.mail_lists.members_title },
    { id: 'nests', label: t.nests.title },
    // After Nests — the cross-participant capstone (settings.md § Navigation model).
    { id: 'task-delegation', label: t.task_delegation.title },
    // Right after Task delegation (settings.md § Navigation model): the one
    // roster of everything acting for the user from outside the apps.
    { id: 'connected-apps', label: t.connected_apps.title },
    { id: 'logs', label: t.logs.title },
  ];

  // Current sub-page slug (undefined on the bare /app/settings → Status default).
  // "status" normalizes to the same value as the bare root — see the matching
  // comment in [[subpage]]/+page.svelte — so the rail highlights Status active
  // for either URL form.
  let rawSubpage = $derived($page.params.subpage ?? '');
  let current = $derived(rawSubpage === 'status' ? '' : rawSubpage);

  function href(id: string): string {
    return id ? `/app/settings/${id}` : '/app/settings';
  }
</script>

{#snippet sidebarContent()}
  <a href="/app/conversations" class="nav-back" data-testid={IDS.SETTINGS_NAV_BACK}>
    <span class="icon">‹</span>
    <span class="label">{t.settings.exit_settings}</span>
  </a>
  <div class="settings-title">{t.common.settings}</div>
  <div class="settings-links">
    {#each railEntries as entry}
      <a
        href={href(entry.id)}
        class="tab"
        class:active={current === entry.id}
        data-testid="settings-nav-{entry.id || 'status'}"
      >{entry.label}</a>
    {/each}
  </div>
{/snippet}
{#snippet mainContent()}
  {@render children?.()}
{/snippet}
<AppShellFrame sidebar={sidebarContent} main={mainContent} sidebarScrollable mobileSidebarLayout="scrolling" />

<style>
  .nav-back {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 1rem;
    color: var(--text-muted);
    font-size: 0.875rem;
    transition: background 0.15s, color 0.15s;
  }
  .nav-back:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .nav-back .icon { font-size: 1.125rem; line-height: 1; }
  .settings-title {
    font-size: 1.25rem;
    font-weight: 700;
    padding: 0.5rem 1rem 1rem;
    color: var(--accent);
  }
  .settings-links {
    display: flex;
    flex-direction: column;
    flex: 1;
  }
  .tab {
    display: flex;
    align-items: center;
    padding: 0.5rem 1rem;
    color: var(--text-muted);
    font-size: 0.9rem;
    transition: background 0.15s, color 0.15s;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab.active {
    color: var(--accent);
    background: var(--bg-hover);
  }

  @media (max-width: 768px) {
    .settings-title { display: none; }
    .nav-back { padding: 0.5rem 0.75rem; white-space: nowrap; }
    .nav-back .label { display: none; }
    .settings-links { flex-direction: row; flex: 1; }
    .tab { padding: 0.5rem 0.75rem; font-size: 0.8rem; white-space: nowrap; }
  }
</style>
