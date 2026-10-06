using System;
using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_moderation;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Drives the standalone Moderation page: the moderation queue + per-item training
/// correction, over <see cref="INestRpcClient"/>. The queue is the <b>union</b> of the
/// server <c>fauna.moderation.actions</c> obligation rows (the shared
/// <c>FfiModerationClient</c> seam linux/apple consume) and the client's own
/// post-decrypt <b>local detections</b> from the conversations session
/// (<see cref="IModerationLocalDetections"/>), merged through the one shared
/// <c>fauna_client_moderation::merge_queue</c> façade (moderation.md § Layout &amp;
/// flow). A local row's <c>train-correction-button</c> removes the client-side flag; a
/// server row's submits <c>fauna.moderation.train</c>.
///
/// <para>The spam-filtering <b>preferences</b> (<c>fauna.spam.{get,set}_preferences</c>)
/// live on Settings → Privacy (the canonical <c>spam-moderation-controls</c> home,
/// <c>SettingsViewModel</c> + <c>SettingsPrivacyPage</c>), not here — this page is
/// queue-only, matching linux/macOS/iOS/Android and ui.yaml's <c>moderation</c> scope.</para>
/// </summary>
public partial class ModerationViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;

    // Moderation queue — the UNION of the caller's own server-issued obligation rows
    // (fauna.moderation.actions) and the client's post-decrypt local detections
    // (moderation.md § Layout & flow); each row carries a train-correction.
    public ObservableCollection<ModerationRow> Actions { get; } = new();

    private readonly INestRpcClient _rpc;
    // The client half of the queue: the conversations session's post-decrypt local
    // detections. Null when there is no MLS session (→ the server rows alone).
    private readonly IModerationLocalDetections? _local;
    // The sealed spam-model client-write path (1d — mail-spam.md § Encrypted-mode
    // interaction). Null → no sealed-write path → the correction always takes the
    // server-side fauna.moderation.train (e.g. mail not enabled).
    private readonly ISpamModelClientWrite? _spamWrite;

    internal ModerationViewModel(
        INestRpcClient rpc,
        IModerationLocalDetections? local = null,
        ISpamModelClientWrite? spamWrite = null)
    {
        _rpc = rpc;
        _local = local;
        _spamWrite = spamWrite;
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            // Moderation queue (fauna.moderation.actions) over the shared
            // FfiModerationClient seam. An empty queue is normal (nothing flagged);
            // a genuine read error surfaces via the outer catch.
            await RefreshQueueAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    [RelayCommand]
    private async Task TrainAsync(ModerationRow action)
    {
        try
        {
            if (action.IsLocal)
            {
                // A local detection has no server obligation to train against. Read the
                // retained decrypted body BEFORE dropping the flag (mirroring linux
                // correct_moderation_row order), train the caller's Bayesian model "not
                // spam" client-side over that text, then remove the false-positive flag
                // (moderation.md § State: local detections are held client-side) and
                // repaint. The machine self-gates: a nest without spam-model-sealed-at-rest
                // yields ServerPath → a silent no-op (there is no server train path for
                // local content). A null body (aged out) ⇒ just remove.
                var text = _local?.Body(action.ContentId);
                _local?.Remove(action.ContentId);
                if (_spamWrite is not null && !string.IsNullOrEmpty(text))
                    await _spamWrite.TrainSpamModelClientAsync(text, isSpam: false);
            }
            else
            {
                // A server row lists the caller's OWN flagged content, so the correction
                // is "not spam" (verdict "ham") — it down-weights the false positive in the
                // caller's Bayesian model. When the nest advertises spam-model-sealed-at-rest
                // (1d — mail-spam.md § Encrypted-mode interaction), write the re-sealed model
                // client-side over the fetched post body (mirroring linux train_moderation_flow),
                // else degrade to the server-side fauna.moderation.train. The capability
                // pre-check is load-bearing: a blind sealed write against a v-older nest would
                // double-seal → the user's model becomes unreadable → user-data loss
                // (version-compatibility.md I1/I2). ServerPath / an empty body also degrade.
                var trainedClientSide = false;
                if (_spamWrite is not null && await _spamWrite.SealedSpamWriteAvailableAsync())
                {
                    var text = await _rpc.PostBodyTextAsync(action.ContentId);
                    if (!string.IsNullOrEmpty(text))
                    {
                        var res = await _spamWrite.TrainSpamModelClientAsync(text, isSpam: false);
                        trainedClientSide = res.Sealed;
                    }
                }
                if (!trainedClientSide)
                    await _rpc.ModerationTrainAsync(action.ContentId, "ham");
            }
            // Reflect the correction: re-read the queue.
            await RefreshQueueAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    private async Task RefreshQueueAsync()
    {
        // The server half: the caller's own obligation rows (fauna.moderation.actions).
        var server = await _rpc.ModerationActionsAsync();
        // The client half: the conversations session's post-decrypt local detections
        // (empty when there is no MLS session — the server rows alone, matching linux's
        // active_session() → None). The shared classify writer already populated the
        // store on decrypt, so reading it is all this leg does (no per-app writer).
        var local = _local?.Snapshot() ?? Array.Empty<LocalDetection>();
        // Merge through the ONE shared union (dedupe by content_id, server row winning,
        // newest-first) so the rule never drifts per client (moderation.md § Layout &
        // flow; fauna_client_moderation::merge_queue via the UniFFI façade).
        var rows = FaunaFfiMethods.ModerationQueue(server.ToArray(), local.ToArray());
        Actions.Clear();
        foreach (var r in rows)
            Actions.Add(MapRow(r));
    }

    /// <summary>Map a shared <c>QueueRow</c> (server obligation ∪ local detection) to a
    /// display row, resolving the badge (label/icon/accent) from the shared
    /// <c>content_label_style</c> map — no per-app category strings or colours
    /// (moderation.md § Categories &amp; enforcement). A <b>server</b> row renders its
    /// enforcement action via <c>obligation_action_label</c>; a <b>local</b> detection
    /// (<c>action == null</c>) has a <b>blank</b> action column — never a fabricated
    /// action (§ Don't do these).</summary>
    private static ModerationRow MapRow(QueueRow r)
    {
        var style = FaunaFfiMethods.ContentLabelStyle(r.category);
        return new ModerationRow(
            ContentId: r.contentId,
            ContentType: r.contentType,
            CategoryLabel: Strings.Resolve(style.label),
            CategoryIcon: style.icon,
            CategoryAccent: style.accent,
            ActionLabel: r.action is byte action
                ? Strings.Resolve(FaunaFfiMethods.ObligationActionLabel(action))
                : string.Empty,
            // per-mille (0–1000) → whole percent, rounded half-up, single-sourced via
            // the shared fauna_core::format::confidence_percent (no per-app
            // (m + 5) / 10 — value-formatting.md § Confidence percent).
            ConfidencePercent: (int)FaunaFfiMethods.ConfidencePercent(r.confidencePerMille),
            IsLocal: r.source == QueueRowSource.Local);
    }
}

/// <summary>
/// One moderation-queue row, mapped from a shared <c>QueueRow</c> — the union of a
/// server-issued <c>ObligationAction</c> and a client-side post-decrypt local
/// detection (moderation.md § Layout &amp; flow). The badge label/icon/accent are
/// resolved from the shared <c>fauna_core::content_category</c> map — no per-app
/// category strings or colours (§ Categories &amp; enforcement). A <b>server</b> row
/// carries its enforcement <see cref="ActionLabel"/> (via <c>obligation_action_label</c>);
/// a <b>local</b> detection (<see cref="IsLocal"/>) has a <b>blank</b> action column —
/// never a fabricated action (§ Don't do these). <see cref="ContentId"/> is the content
/// ref the <c>train-correction-button</c> acts on (a server row trains
/// <c>fauna.moderation.train</c>; a local row removes the client-side flag).
/// </summary>
public record ModerationRow(
    string ContentId,
    string ContentType,
    string CategoryLabel,
    string CategoryIcon,
    string CategoryAccent,
    string ActionLabel,
    int ConfidencePercent,
    bool IsLocal);
