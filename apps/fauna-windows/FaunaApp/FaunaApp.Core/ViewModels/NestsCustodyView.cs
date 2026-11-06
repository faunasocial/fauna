using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One custodian-NEST <c>nests-item</c> row and its <c>nest-trust-custody-*</c>
/// family (ui/nests.md § Trust facet — custody rows): the label, the scope
/// line, the receipt's three-state status, the held-bytes line and the revoke
/// button's state. <see cref="GrantId"/>/<see cref="Holder"/> are the revoke
/// gesture's own args — never a row index, which a refold can re-point at a
/// different custody. <see cref="Url"/> is the custodian's address, pre-filled
/// into the row's <c>nest-trust-custody-restore-button</c> gesture.
/// </summary>
internal sealed record NestCustodyRow(
    byte[] GrantId, byte[]? Holder, string Label, string Scope, string ReceiptStatusText,
    string HeldBytesText, bool Stale, bool Pending, string Url)
{
    /// <summary>A pending ceremony has minted nothing to revoke yet.</summary>
    public bool RevokeEnabled => !Pending;
}

/// <summary>
/// The Nests page's custody-nest rows and escrow-holder badge source
/// (ui/nests.md § Trust facet — custody rows; participants.md § The participant
/// model → Roles). Reads the same custody fold the Devices page paints (its
/// complement: the rows whose <c>custodian_nest_url</c> is set) and the shared
/// <c>custody_escrow_holders</c> store read, on the page's own hydrate edge;
/// nothing here derives a row, a label or a role — the page paints what this
/// holds. The WinUI page is not reachable from the unit-test assembly, so the
/// rules live here, where <c>NestsCustodyViewTests</c> pins them.
///
/// <para>⚠ No <c>ConfigureAwait(false)</c>: the page awaits these and repaints
/// on the continuation (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
internal sealed class NestsCustodyView
{
    private CustodyFacetView? _facet;
    private HashSet<string> _escrowHolders = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>The last custody gesture's error, or <c>null</c>.</summary>
    public string? Error { get; private set; }

    /// <summary>One row per NEST-anchored custody (a Device-anchored one stays
    /// on the Devices page), in the fold's order — painted after the linked
    /// rows.</summary>
    public IReadOnlyList<NestCustodyRow> Rows => (_facet?.@rows ?? Array.Empty<CustodyHolderRowView>())
        .Where(r => r.@custodianNestUrl is not null)
        .Select(r => new NestCustodyRow(
            r.@grantId,
            r.@custodianKey,
            Strings.Format("nests/custody_nest_label",
                FaunaFfiMethods.ShortId(FaunaFfiMethods.HexFull(r.@host))),
            Strings.Get("devices/custody_holder_scope"),
            Strings.CustodyReceiptStatusText(r.@receipt),
            Strings.CustodyHeldBytesText(r.@receipt),
            r.@receiptState == CustodyReceiptStateView.Stale,
            r.@pending,
            r.@custodianNestUrl!))
        .ToList();

    /// <summary>Whether the nest row <paramref name="nestIdHex"/> is among the
    /// recorded escrow holders — the badge's predicate. Derived from recorded
    /// escrow receipts, never asserted by the nest itself.</summary>
    public bool HoldsEscrow(string nestIdHex) => _escrowHolders.Contains(nestIdHex);

    /// <summary>The page's hydrate edge: the custody fold and the escrow-holder
    /// read. Each keeps its previous answer when unreadable this pass (a live
    /// list never blanks on a transient). Never throws.</summary>
    public async Task LoadAsync(INestRpcClient rpc)
    {
        try
        {
            if (await rpc.CustodyFacetLoadAsync() is { } facet) _facet = facet;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestsCustodyView", $"[custody] load failed: {ex.GetType().Name}: {ex.Message}");
        }
        try
        {
            if (await rpc.CustodyEscrowHoldersAsync() is { } holders)
                _escrowHolders = new HashSet<string>(holders, StringComparer.OrdinalIgnoreCase);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestsCustodyView", $"[escrow] read failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>The store-change re-read (<c>account-runtime.md</c> § A runtime's
    /// own pump is a source of the notice too, part 4): the custody fold and the
    /// escrow-holder read again, answering <c>true</c> only when what the page
    /// paints from them changed. Each keeps its previous answer when unreadable
    /// (a live list never blanks on a transient); <see cref="Error"/> — a revoke's
    /// message — is never touched. Never throws.</summary>
    public async Task<bool> ReloadAsync(INestRpcClient rpc)
    {
        var before = Signature();
        await LoadAsync(rpc);
        return before != Signature();
    }

    // What the page paints from this view: the NEST-anchored custody rows (a
    // content signature — the generated rows carry byte arrays, compared by
    // reference by a record) and the escrow holders.
    private string Signature()
    {
        try
        {
            var rows = (_facet?.@rows ?? Array.Empty<CustodyHolderRowView>())
                .Where(r => r.@custodianNestUrl is not null);
            return System.Text.Json.JsonSerializer.Serialize(rows)
                + "|" + string.Join(",", _escrowHolders.Order(StringComparer.OrdinalIgnoreCase));
        }
        catch (Exception)
        {
            return Guid.NewGuid().ToString("N");
        }
    }

    /// <summary><c>nest-trust-custody-revoke-button</c> — the same revoke the
    /// Devices card runs, keyed by grant id + accept-bound custodian key. Repaints
    /// from the re-folded facet (a <c>null</c> facet keeps the painted rows) and
    /// answers on <see cref="Error"/>, never a silent drop.</summary>
    public async Task RevokeAsync(INestRpcClient rpc, byte[] grantId, byte[]? holder)
    {
        try
        {
            var outcome = await rpc.CustodyRevokeAsync(grantId, holder);
            if (outcome.@facet is { } facet) _facet = facet;
            Error = outcome.@error is { } e ? Strings.Format("devices/error_revoke_custody", e) : null;
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    // ── Restore from a custodian nest (ui/nests.md § Trust facet — restore from
    // a custodian nest): the page-level form, the last restore's notice and the
    // one command both entries dispatch. android's LinkedNestsVM.restoreFromCustodian
    // + RestoreNotice, tui's push_custody_restore. No dial, handshake or walk
    // logic here, and no outcome→string mapping: the copy is shared Rust's
    // notice_parts, the form decision its closes_form.

    /// <summary>Whether the restore form (<c>nests-custody-restore-input</c> and
    /// its submit/cancel) is revealed.</summary>
    public bool RestoreFormOpen { get; private set; }

    /// <summary>The form's address as typed — kept across every failure.</summary>
    public string RestoreAddress { get; private set; } = string.Empty;

    /// <summary>A restore is in flight (the notice reads <i>running</i>).</summary>
    public bool RestoreRunning { get; private set; }

    /// <summary><c>nests-custody-restore-notice</c>: <c>null</c> until a restore
    /// ran this session (the element is absent), the running copy while one is
    /// in flight, then the outcome's shared copy. A <c>Failed</c> gesture is no
    /// notice state — its reason rides <see cref="Error"/>.</summary>
    public string? RestoreNotice { get; private set; }

    /// <summary>Submit is offered only for a non-blank address with no restore
    /// in flight. No own-nest wire kind gates it: the restore dials the
    /// custodian, never this session's own nest.</summary>
    public bool RestoreSubmitEnabled => !string.IsNullOrWhiteSpace(RestoreAddress) && !RestoreRunning;

    /// <summary><c>nests-custody-restore-button</c> — reveal the form.</summary>
    public void OpenRestoreForm() => RestoreFormOpen = true;

    /// <summary><c>nests-custody-restore-cancel-button</c> — close the form and
    /// drop the typed address.</summary>
    public void CancelRestoreForm()
    {
        RestoreFormOpen = false;
        RestoreAddress = string.Empty;
    }

    /// <summary>The input's text changed.</summary>
    public void SetRestoreAddress(string? text) => RestoreAddress = text ?? string.Empty;

    /// <summary>Dispatch <c>RestoreFromCustodian { url }</c> — the typed address
    /// (<c>nests-custody-restore-submit-button</c>) or a custodian row's own
    /// (<c>nest-trust-custody-restore-button</c>). A blank address, or a restore
    /// already in flight, dispatches nothing. The running state is set before the
    /// first await, so the caller paints it before awaiting the returned task.
    /// A reached custodian closes the form (shared <c>closes_form</c>); every
    /// failure keeps it open with the address kept. Then the custody fold re-reads,
    /// so a restored custodian row paints beside the notice. Never throws.</summary>
    public async Task RestoreAsync(INestRpcClient rpc, string? raw)
    {
        var url = (raw ?? string.Empty).Trim();
        if (url.Length == 0 || RestoreRunning) return;
        RestoreRunning = true;
        RestoreNotice = Strings.Get("nests/custody_restore_running");
        FfiCustodianRestoreOutcome outcome;
        try
        {
            outcome = await rpc.AccountRestoreFromCustodianAsync(url);
        }
        catch (Exception ex)
        {
            outcome = new FfiCustodianRestoreOutcome.Failed(ex.Message);
        }
        RestoreRunning = false;
        if (outcome is FfiCustodianRestoreOutcome.Failed failed)
        {
            // The gesture never ran: no notice state, the reason on error-message,
            // the form left as it stood (address kept).
            RestoreNotice = null;
            Error = failed.@reason;
        }
        else
        {
            Error = null;
            RestoreNotice = NoticeText(outcome);
            if (FaunaFfiMethods.CustodianRestoreClosesForm(outcome))
            {
                RestoreFormOpen = false;
                RestoreAddress = string.Empty;
            }
        }
        await LoadAsync(rpc);
    }

    /// <summary>The outcome's notice: the shared <c>custodian_restore_notice</c>
    /// parts, each resolved NESTED (the revoked reason is itself a key) and
    /// joined with a space — android's and macOS's shape.</summary>
    private static string NoticeText(FfiCustodianRestoreOutcome outcome)
        => string.Join(" ", FaunaFfiMethods.CustodianRestoreNotice(outcome).Select(Strings.ResolveNested));
}
