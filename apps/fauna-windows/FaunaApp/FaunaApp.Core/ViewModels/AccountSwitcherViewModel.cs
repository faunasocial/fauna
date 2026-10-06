using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The multi-account switcher rendered in account settings
/// (<c>account-switcher-list</c> / <c>account-switcher-item</c>;
/// <c>docs/goal/architecture/long-term-store.md</c> § Multi-account evolution). Windows is
/// the last of the seven apps to get this, so every shape here is an adoption of a
/// proven one — apple's <c>AccountSwitcherVM</c> is the closest sibling.
///
/// <para>A dumb projection over the shared <c>AccountRegistry</c> through its UniFFI
/// <see cref="IFfiAccountRegistry"/> seam (priority #2 — the registry owns the index, the
/// per-actor slots, the active pointer, and the ordering), so this VM is unit-testable
/// against a fake with no live Credential Manager and no FlaUI (which flakes on
/// win-arm64).</para>
///
/// <para><b>It holds no identity state of its own.</b> The registry is the authority for
/// which accounts exist and which is active, and it is cheap to read (a stateless view over
/// the store), so every render goes back to it rather than to a cached copy that could
/// disagree with the store the launch machine routes on.</para>
/// </summary>
public partial class AccountSwitcherViewModel : ViewModelBase
{
    private readonly IFfiAccountRegistry _registry;

    // Erase an account's scoped content stores (MLS, drafts, backup) when it is
    // removed — § Erasure follows scope. Injected so unit tests exercise Remove
    // without touching the real profile / native FFI; production and e2e get the
    // real AccountStateDir.Erase by default, so a page cannot forget to wire it.
    private readonly Action<string> _eraseAccountState;

    /// <summary>The accounts held on this client install, in the registry's add order — one
    /// <c>account-switcher-item</c> row each. Reconciled in place (never replaced), so the
    /// page binds this instance once and a later refresh updates the rendered list without
    /// re-navigation.</summary>
    public ObservableCollection<AccountRowVm> Accounts { get; } = new();

    /// <summary>The actor id (hex) of the account this window is using, or null — mirrors
    /// <c>registry.SessionAccount()</c> (the served account, not the registry's active
    /// pointer) as of the last <see cref="Refresh"/>.</summary>
    [ObservableProperty] private string? _activeActorId;

    /// <summary>
    /// The app's switch seam, filled in by the app root. Activating an account tears down
    /// and rebuilds the whole authenticated session (nest clients, MLS engine, backup
    /// driver, launch machine) — app-owned state a <c>FaunaApp.Core</c> VM cannot reach, and
    /// the reason the switch deliberately does not live here. Same shape as apple's
    /// <c>onSwitch</c> closure. Null in unit tests and before the page wires it, in which
    /// case <see cref="SwitchToAsync"/> is a no-op.
    ///
    /// <para>The second argument is the Stage-2 <c>confirmed</c> flag: <c>true</c> once a
    /// re-auth prompt has approved activating a <c>require_confirm_to_activate</c>-flagged
    /// account, so the app routes it through <c>SetActiveConfirmed</c> rather than the plain
    /// <c>SetActive</c> (which the shared registry refuses for a flagged account). Same shape
    /// as apple's <c>proceed(actorId, confirmed)</c>.</para>
    /// </summary>
    public Func<string, bool, Task>? OnSwitchRequested { get; set; }

    /// <summary>
    /// The native re-auth gate (Windows Hello / <c>UserConsentVerifier</c>), filled in by the
    /// app root — WinRT is unreachable from this crate's plain-<c>net10.0</c> TFM, so the
    /// prompt lives in the app layer and the VM owns only the branch. Returns <c>true</c> when
    /// the user approves, <c>false</c> on decline / cancel / unavailable (fail-closed). Same
    /// shape as apple's <c>AccountReauth.confirmActivation()</c> static, injected here so the
    /// switch branch is unit-testable against a fake with no real biometric prompt.
    ///
    /// <para>Null in unit tests that don't exercise the gate and before the page wires it; a
    /// null gate on a <i>flagged</i> account fails closed (the switch is declined), never open.</para>
    /// </summary>
    public Func<Task<bool>>? ConfirmReauth { get; set; }

    // The shared remove-account door (`fauna-ffi`'s `remove_account_blocked`): is
    // this account one this window serves, or one another live instance serves?
    // Injected for the same reason as the erase above; production asks through
    // AccountStateDir, which names the two bases the erase sweeps.
    private readonly Func<string, FfiEraseRemoveBlocked?> _removeAccountBlocked;

    internal AccountSwitcherViewModel(
        IFfiAccountRegistry registry,
        Action<string>? eraseAccountState = null,
        Func<string, FfiEraseRemoveBlocked?>? removeAccountBlocked = null)
    {
        _registry = registry;
        _eraseAccountState = eraseAccountState ?? Services.AccountStateDir.Erase;
        _removeAccountBlocked = removeAccountBlocked ?? Services.AccountStateDir.RemoveAccountBlocked;
    }

    /// <summary>
    /// Re-read the registry and re-render the rows. Called on page-visible and after every
    /// mutation — this is what makes the list live-refresh <b>in place</b>.
    ///
    /// <para><b>Every field is re-read here; nothing is cached at construction.</b> That is
    /// load-bearing for <c>require_confirm_to_activate</c>: a build-once settings surface
    /// renders a stale <c>false</c> over a flag the admin auto-default has since set, which
    /// makes the flag impossible to turn off — the user's tap on an OFF-looking toggle
    /// writes ON (long-term-store.md § Multi-account evolution → "Clients must read the flag
    /// fresh at activation and at render"; linux hit exactly this).</para>
    /// </summary>
    [RelayCommand]
    public void Refresh()
    {
        try
        {
            // The account this window SERVES (shared `session_account`: the process
            // holder's actor, the registry's active one only before any is admitted) —
            // never `Active()`. On a bound secondary the two differ, and a row keyed on
            // the registry offered remove on the account whose stores this process runs
            // from (account-scoping.md § Concurrent instances → "Remove-account also
            // refuses the account THIS instance serves").
            var active = _registry.SessionAccount();
            var entries = _registry.List();
            var rows = new List<AccountRowVm>(entries.Length);
            foreach (var e in entries)
            {
                rows.Add(new AccountRowVm(
                    ActorId: e.actorId,
                    // The SHARED row title, never a hand-rolled handle-else-short-id
                    // fallback: fauna_core::format::account_display_label is the one
                    // derivation site, and linux and web each once re-derived it and
                    // drifted on the empty-handle case (priority #1/#4).
                    DisplayLabel: FaunaFfiMethods.AccountDisplayLabel(e.handle, e.actorId),
                    IsActive: e.actorId == active,
                    RequireConfirmToActivate: e.requireConfirmToActivate));
            }

            ActiveActorId = active;
            // Keyed in-place reconcile rather than Clear()+rebuild: an unchanged row keeps
            // its instance (and its realized WinUI container), and only the rows that
            // actually differ raise CollectionChanged.
            ObservableCollectionReconcile.Reconcile(
                Accounts, rows, r => r.ActorId, (a, b) => a == b);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>
    /// Drop an account from this install (its per-actor slots + index entry), then re-render.
    /// No confirmation dialog — matching every sibling client; the identity itself is not
    /// destroyed, it is only forgotten here, and the user can re-add it by importing the
    /// secret.
    ///
    /// <para>Only ever offered on rows other than the account this window serves
    /// (<see cref="AccountRowVm.CanRemove"/>), so this never has to re-route a live session:
    /// the shared <c>remove()</c> promotes the first remaining account, but the running client
    /// would still be authenticated as the removed one. Because removing another account does
    /// not switch, nothing rebuilds the page — the refresh below is what updates the
    /// already-navigated list. A remove that reaches here anyway is still refused by the
    /// shared door below, which also refuses an account a sibling instance serves.</para>
    /// </summary>
    [RelayCommand]
    public void Remove(string actorId)
    {
        SetError(null);
        // Ask the shared door BEFORE the registry removal, which drops the account's
        // secret slots — a refusal after it would strand the scopes on disk with
        // nothing left to sign in to them. ServedHere (this window runs from the
        // account's stores) and ServedElsewhere (another live instance does) each
        // carry their own line, because each names a different remedy.
        var blocked = _removeAccountBlocked(actorId);
        if (blocked is not null)
        {
            SetError(Strings.Resolve(blocked switch
            {
                FfiEraseRemoveBlocked.ServedHere here => here.@line,
                FfiEraseRemoveBlocked.ServedElsewhere elsewhere => elsewhere.@line,
                _ => throw new InvalidOperationException($"unknown refusal {blocked}"),
            }));
            return;
        }
        try
        {
            _registry.Remove(actorId);
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return;
        }
        // Erasure follows scope (account-scoping.md § Erasure follows scope):
        // forgetting an account drops its per-actor slots + index entry above, so
        // its content stores (MLS, drafts, backup) must go too — leaving them
        // readable would be the same leak as leaving its secret in the store. Runs
        // only after a SUCCESSFUL registry remove (a rejection returned above).
        _eraseAccountState(actorId);
        Refresh();
    }

    /// <summary>
    /// Flip the per-account "require re-auth to activate" flag — the
    /// <c>account-require-confirm-toggle</c> write path (Stage 2, long-term-store.md
    /// § Multi-account evolution). Offered on <b>every</b> row including the active one (the
    /// natural target is the user's admin identity, which is often active). Setting the flag
    /// never prompts; only <i>activating</i> a flagged account does. The shared
    /// <c>set_require_confirm</c> also marks <c>require_confirm_user_set</c>, so an explicit
    /// OFF sticks forever against the admin auto-default.
    ///
    /// <para>No <c>[RelayCommand]</c>: the toolkit only generates commands for zero- or
    /// one-parameter methods, and the row's toggle is an event-to-method call anyway
    /// (<c>Toggled</c> → <c>SetRequireConfirm(row.ActorId, sw.IsOn)</c>).</para>
    /// </summary>
    public void SetRequireConfirm(string actorId, bool require)
    {
        SetError(null);
        try
        {
            _registry.SetRequireConfirm(actorId, require);
        }
        catch (Exception ex)
        {
            ShowError(ex);
            return;
        }
        Refresh();
    }

    /// <summary>
    /// Request a switch to <paramref name="actorId"/> — tapping an
    /// <c>account-switcher-item</c> row. Tapping the row you are already on is a no-op (no
    /// teardown, no rebuild).
    ///
    /// <para>This VM does <b>not</b> touch the registry here: the app's handler owns the
    /// whole activation (the <c>SetActive</c>/<c>SetActiveConfirmed</c> write plus the session
    /// teardown/rebuild), which preserves the client's mutation-first invariant — registry
    /// write before teardown — as a single app-side sequence rather than one split across two
    /// owners.</para>
    ///
    /// <para><b>Stage 2 — the re-auth gate.</b> An account with
    /// <c>require_confirm_to_activate</c> set demands a native re-auth prompt
    /// (<see cref="ConfirmReauth"/>) BEFORE the switch is requested. The flag is re-read from
    /// the registry here (never the rendered row, which the admin auto-default can leave
    /// stale-OFF; long-term-store.md — "read the flag fresh at activation"). Declining — or
    /// no gate wired — is a <b>pure no-op</b>: no switch is requested, nothing is torn down,
    /// no error banner (the user cancelled it themselves). The gate resolves before the
    /// app-owned switch seam, so the mutation-first invariant is unchanged.</para>
    /// </summary>
    [RelayCommand]
    public async Task SwitchToAsync(string actorId)
    {
        // Convention 14: this gesture's own completion observable
        // (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`). Counted in a `finally`
        // around the WHOLE method — including the two early refusals below — so
        // it means "the tap's handler is done", not "the tap succeeded". A
        // declined re-auth is a pure no-op by design, so on this app nothing else
        // changes for a test to watch: without this counter the negative assert
        // proving the decline did not switch would have to anchor to the click's
        // dispatch, which is the race it exists to remove.
        try
        {
            if (actorId == ActiveActorId)
            {
                return;
            }
            var handler = OnSwitchRequested;
            if (handler is null)
            {
                return;
            }
            SetError(null);
            try
            {
                // Read the flag FRESH from the store — not the cached row. The admin
                // auto-default (App → the am-i-admin gate) can flip it ON with no VM refresh,
                // and a decision from the stale row would skip the prompt on a stale false.
                var flagged = _registry.List()
                    .FirstOrDefault(e => e.actorId == actorId)?.requireConfirmToActivate ?? false;

                var confirmed = false;
                if (flagged)
                {
                    var gate = ConfirmReauth;
                    // Fail-closed: a flagged account with no gate wired, or a declined prompt,
                    // does NOT switch. Declining is a pure no-op — no mutation, no teardown, no
                    // banner (the user cancelled it themselves).
                    if (gate is null || !await gate())
                    {
                        return;
                    }
                    confirmed = true;
                }

                // No ConfigureAwait(false): the continuation must stay on the UI thread — an
                // off-thread mutation of bound state throws a silent COMException in WinUI and
                // the page renders empty with no error.
                await handler(actorId, confirmed);
            }
            catch (Exception ex)
            {
                ShowError(ex);
            }
        }
        finally
        {
            Core.Services.E2eSessionCounters.RecordActivationGesture();
        }
    }
}

/// <summary>
/// One <c>account-switcher-item</c> row: the actor id the gestures round-trip, the shared
/// display label (<c>account-item-handle</c>), whether this is the active identity
/// (<c>account-item-active-indicator</c>), and the freshly-read re-auth-on-activate flag
/// (<c>account-require-confirm-toggle</c>). A record, so the in-place reconcile can compare
/// rows by value and leave an unchanged one alone.
/// </summary>
public sealed record AccountRowVm(
    string ActorId,
    string DisplayLabel,
    bool IsActive,
    bool RequireConfirmToActivate)
{
    /// <summary>Whether this row offers <c>account-remove-button</c> — every row but the
    /// account this window serves. Removing the running identity would leave the client
    /// authenticated as an account it just forgot, running from stores the remove erased.</summary>
    public bool CanRemove => !IsActive;
}
