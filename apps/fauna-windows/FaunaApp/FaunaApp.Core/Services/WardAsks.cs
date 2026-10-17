using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

// The supervised ward's in-place asks and the guardian's un-deny list — the windows
// lift of four family-safety surfaces (family-safety.md § Child-initiated contact
// requests → *App affordance*, § Feed-source approvals, § The bridge-DM gate → *The
// un-deny surface*):
//
//   * `contact-request-guardian-button` / `contact-request-pending` on the contacts
//     page's Find User result and on another actor's profile;
//   * `bridge-source-request-button` / `bridge-source-request-state` inside a
//     `bridge-card`;
//   * `family-blocked-peer-item` / `family-blocked-peer-allow-button` in the
//     guardian's per-ward editor.
//
// tui leads (`apps/fauna-tui/src/{contacts,profile/mod,bridges,family}.rs`); android
// (`core/WardAsks.kt`) and apple (`FaunaKit` `ContactAskRow` / `ProfileKnockVM`) carry
// the same shape. The rules every app copies rather than re-derives:
//   (a) the ask is offered ONLY on the typed refusal
//       (`FfiException.GuardianApprovalRequired`) — never on message text;
//   (b) the refusal stays on `error-message` — it is still a real failure;
//   (c) pending is durable — read from `status.contact_requests` /
//       `status.feed_requests`, gated on `supervised_by` ([WardAsksCache]); a
//       just-asked flag only makes the render answer before the re-read lands;
//   (d) rows are keyed on the ask data, never on compose buffers;
//   (e) an approved feed-source ask is a PROMPT to retry, never an auto-retry;
//   (f) each allow button addresses ITS OWN row's (bridge_id, peer_id);
//   (g) a knock reply carries the peer it was sent to, and paints only on that
//       peer's open;
//   (h) supervision is not re-tested at render — the inputs are supervised-only by
//       construction.
//
// The two ask-matching rules are NOT restated here: they are the shared
// `fauna_client_family::ward_asks::{contact_ask_pending, feed_request_state}`, reached
// over their UniFFI faces (`FaunaFfiMethods.WardContactAskPending` /
// `WardFeedRequestState`).

/// <summary>The <c>(bridge_id, operation, target)</c> triple a feed-source grant is
/// scoped to. <c>Operation</c> is the wire spelling
/// (<c>FaunaFfiMethods.FeedSourceOperationWire</c>), <c>Target</c> is empty for a
/// <c>link</c>.</summary>
internal readonly record struct FeedTriple(string BridgeId, string Operation, string Target);

/// <summary>The supervised caller's OWN pending asks — <c>status.contact_requests</c>
/// and <c>status.feed_requests</c> off <c>fauna.family.status</c> — plus this
/// session's typed feed-source refusals, held for the three refused-send surfaces that
/// read them back. Fed from windows' one status-read choke point
/// (<see cref="INestRpcClient.FamilyStatusAsync"/>), so every successful read
/// refreshes it; dropped by <see cref="ActorScope.DropActorScopedState"/> on every
/// identity teardown. Android's twin is <c>core/WardAsks.kt</c>.</summary>
internal static class WardAsksCache
{
    private static readonly object Gate = new();
    private static FfiFamilyContactRequest[] _contactRequests = Array.Empty<FfiFamilyContactRequest>();
    private static FfiFamilyFeedRequest[] _feedRequests = Array.Empty<FfiFamilyFeedRequest>();
    private static readonly List<FeedTriple> _refusedFeed = new();

    /// <summary>Raised (on whichever thread moved the state) whenever the held asks or
    /// refusals change, so a painted surface can re-render. Handlers marshal to the UI
    /// thread themselves.</summary>
    public static event Action? Changed;

    /// <summary>Fold one successful <c>fauna.family.status</c> read.
    /// <paramref name="supervised"/> is whether it named a guardian: a graduated
    /// account has no guardian to be waiting on, so a stale ask from an earlier read
    /// must not keep painting "asked — waiting" (the client half of the nest's own
    /// graduation drop).</summary>
    public static void SetFromStatus(
        bool supervised, FfiFamilyContactRequest[] contact, FfiFamilyFeedRequest[] feed)
    {
        lock (Gate)
        {
            _contactRequests = supervised ? contact : Array.Empty<FfiFamilyContactRequest>();
            _feedRequests = supervised ? feed : Array.Empty<FfiFamilyFeedRequest>();
        }
        Changed?.Invoke();
    }

    /// <summary>The ward's own live feed-source asks (durable, rule (c)).</summary>
    public static FfiFamilyFeedRequest[] FeedRequests
    {
        get { lock (Gate) return _feedRequests; }
    }

    /// <summary>The <c>(bridge_id, operation, target)</c> triples this session saw
    /// refused by the guardian gate — the LOCAL half of the feed-source surface (what
    /// turns a refusal the ward just hit into a visible
    /// <c>bridge-source-request-button</c>); tui's <c>BridgesState::guardian_refused</c>.
    /// Filled only on the TYPED refusal, so supervised-only by construction.</summary>
    public static IReadOnlyList<FeedTriple> RefusedFeed
    {
        get { lock (Gate) return _refusedFeed.ToArray(); }
    }

    /// <summary>Whether an ask for <paramref name="peerActorIdHex"/> is outstanding
    /// (the shared compare).</summary>
    public static bool ContactAskPending(string peerActorIdHex)
    {
        FfiFamilyContactRequest[] held;
        lock (Gate) held = _contactRequests;
        return FaunaFfiMethods.WardContactAskPending(held, peerActorIdHex);
    }

    /// <summary>Record a typed guardian refusal of one feed-source operation (a set:
    /// re-refusing the same triple adds nothing).</summary>
    public static void NoteFeedRefusal(FeedTriple triple)
    {
        lock (Gate)
        {
            if (_refusedFeed.Contains(triple)) return;
            _refusedFeed.Add(triple);
        }
        Changed?.Invoke();
    }

    /// <summary>Drop everything on an identity change: these are one account's asks
    /// and refusals, and the incoming account's first status read has not landed.</summary>
    public static void Reset()
    {
        lock (Gate)
        {
            _contactRequests = Array.Empty<FfiFamilyContactRequest>();
            _feedRequests = Array.Empty<FfiFamilyFeedRequest>();
            _refusedFeed.Clear();
        }
        Changed?.Invoke();
    }
}

// ── Contact ask (contacts page + profile page) ─────────────────────────────

/// <summary>How a knock send ended, classified once — the one failure that reveals the
/// ask (the TYPED guardian refusal, rule (a)) apart from every other one. tui's
/// <c>contacts::KnockSend</c>, android's <c>KnockSend</c>.</summary>
internal abstract record KnockSend
{
    private KnockSend() { }

    public sealed record Sent : KnockSend;
    public sealed record RefusedByGuardian : KnockSend;
    public sealed record Failed(Exception Error) : KnockSend;
}

/// <summary>The per-peer knock state a page holds for the peer it currently shows.
/// <see cref="Peer"/> names whom the flags belong to, so a new lookup / a new profile
/// open starts from <see cref="ForPeer"/> instead of carrying a refusal over to
/// somebody else.</summary>
public sealed record KnockAskState(
    string? Peer,
    bool KnockSent = false,
    bool KnockInFlight = false,
    bool GuardianRefused = false,
    bool AskSent = false,
    bool AskInFlight = false)
{
    /// <summary>A fresh state for <paramref name="peer"/> — a new lookup, or a new
    /// profile open.</summary>
    public static KnockAskState ForPeer(string? peer) => new(peer);
}

/// <summary>What <c>error-message</c> should do after a knock reply: clear it, show the
/// localized guardian-gate sentence (rule (b) — still a real failure, just no longer a
/// dead end), or show another failure's own text.</summary>
internal abstract record KnockErrorEffect
{
    private KnockErrorEffect() { }

    public sealed record Clear : KnockErrorEffect;
    public sealed record Guardian : KnockErrorEffect;
    public sealed record Failed(Exception Error) : KnockErrorEffect;
}

/// <summary>What the refused-send surface shows beside the knock.</summary>
public enum ContactAskRender { Pending, Ask }

/// <summary>The ward-ask rules that are pure functions of their inputs — the windows
/// twin of android's top-level functions in <c>WardAsks.kt</c>, kept in Core so
/// <c>FaunaApp.Tests</c> pins every arm without a page.</summary>
internal static class WardAskFolds
{
    /// <summary>Run one knock <paramref name="send"/> and classify how it ended. The
    /// guardian gate is told apart on the exception TYPE the shared <c>stringify</c>
    /// routes the nest's typed refusal to
    /// (<c>RpcError::is_guardian_approval_required</c> →
    /// <c>FfiException.GuardianApprovalRequired</c>) — never on message text, which is
    /// the nest's business and would also match an unsupervised user's unrelated
    /// failure.</summary>
    public static async Task<KnockSend> ClassifyKnockAsync(Func<Task> send)
    {
        try
        {
            await send();
            return new KnockSend.Sent();
        }
        catch (FfiException.GuardianApprovalRequired)
        {
            return new KnockSend.RefusedByGuardian();
        }
        catch (Exception ex)
        {
            return new KnockSend.Failed(ex);
        }
    }

    /// <summary>Rule (g): fold a knock reply sent to <paramref name="sentTo"/> into the
    /// state of the page as it is NOW. The reply can outlive the lookup / open it was
    /// sent from — a "Sent" or a guardian refusal for the actor you just left must not
    /// paint on the one you now see — so the flags move only when
    /// <c>state.Peer == sentTo</c>. The error effect applies regardless: the send
    /// genuinely failed (or landed).</summary>
    public static (KnockAskState State, KnockErrorEffect Effect) FoldKnockReply(
        KnockAskState state, string sentTo, KnockSend result)
    {
        var effect = EffectOf(result);
        if (!string.Equals(state.Peer, sentTo, StringComparison.OrdinalIgnoreCase))
            return (state, effect);

        var settled = state with { KnockInFlight = false };
        return result switch
        {
            KnockSend.Sent => (settled with { KnockSent = true }, effect),
            KnockSend.RefusedByGuardian => (settled with { GuardianRefused = true }, effect),
            _ => (settled, effect),
        };
    }

    private static KnockErrorEffect EffectOf(KnockSend result) => result switch
    {
        KnockSend.Sent => new KnockErrorEffect.Clear(),
        KnockSend.RefusedByGuardian => new KnockErrorEffect.Guardian(),
        KnockSend.Failed f => new KnockErrorEffect.Failed(f.Error),
        _ => new KnockErrorEffect.Clear(),
    };

    /// <summary>The guardian ask for <paramref name="askedFor"/> landed: flip the
    /// just-asked flag only on that peer's own open (rule (g), the ask half).</summary>
    public static KnockAskState FoldContactAsked(KnockAskState state, string askedFor) =>
        string.Equals(state.Peer, askedFor, StringComparison.OrdinalIgnoreCase)
            ? state with { AskSent = true, AskInFlight = false }
            : state;

    /// <summary>The ask for <paramref name="askedFor"/> failed: re-enable its button on
    /// that peer's open.</summary>
    public static KnockAskState FoldContactAskFailed(KnockAskState state, string askedFor) =>
        string.Equals(state.Peer, askedFor, StringComparison.OrdinalIgnoreCase)
            ? state with { AskInFlight = false }
            : state;

    /// <summary>The pair's render: the durable pending label (read FIRST — it survives
    /// navigation and a restart, and is honest on a session that never saw the
    /// refusal), else the ask button only after a TYPED refusal this open saw, else
    /// nothing (<c>null</c>). <paramref name="durablePending"/> is
    /// <c>WardAsksCache.ContactAskPending(peer)</c> — the shared compare over the
    /// durable list; no supervision test here (rule (h)).</summary>
    public static ContactAskRender? ContactAskRenderOf(bool durablePending, KnockAskState state)
    {
        if (durablePending || state.AskSent) return ContactAskRender.Pending;
        if (state.GuardianRefused) return ContactAskRender.Ask;
        return null;
    }

    // ── Feed-source ask (bridges page) ─────────────────────────────────────

    /// <summary>What one bridge card paints for the ask surface.</summary>
    /// <param name="States">One <c>bridge-source-request-state</c> per durable ask on
    /// this bridge, with its verdict.</param>
    /// <param name="Asks">One <c>bridge-source-request-button</c> per refused triple
    /// this session saw that has no durable row yet — each carrying ITS OWN
    /// triple.</param>
    internal sealed record SourceAskRows(
        IReadOnlyList<FfiFeedRequestState> States, IReadOnlyList<FeedTriple> Asks)
    {
        public bool IsEmpty => States.Count == 0 && Asks.Count == 0;
    }

    /// <summary>The card's ask rows (tui's <c>bridges::source_ask_rows</c>). Paints
    /// nothing in the common case: both inputs are supervised-only by construction
    /// (rule (h)). Durable rows first, then session-only refusals; a refused triple
    /// with a durable row is skipped, so an answered ask shows its verdict instead of
    /// offering the button again. An APPROVED row is a LABEL (the "try again" prompt),
    /// never a button — the ward redeems the single-use grant by repeating the
    /// ORIGINAL Link / Add Follow (rule (e)). Keyed on the ask data, never the
    /// add-follow form's buffers, which dispatch clears (rule (d)).</summary>
    public static SourceAskRows SourceAskRowsFor(
        IEnumerable<FfiFamilyFeedRequest> feedAsks,
        IEnumerable<FeedTriple> refused,
        string bridgeId)
    {
        var held = feedAsks.ToArray();
        var states = new List<FfiFeedRequestState>();
        foreach (var ask in held.Where(a => a.bridgeId == bridgeId))
        {
            var state = FaunaFfiMethods.WardFeedRequestState(
                new[] { ask }, ask.bridgeId, ask.operation, ask.target);
            if (state is { } s) states.Add(s);
        }

        var asks = refused
            .Where(t => t.BridgeId == bridgeId
                && FaunaFfiMethods.WardFeedRequestState(held, t.BridgeId, t.Operation, t.Target) is null)
            .ToList();
        return new SourceAskRows(states, asks);
    }
}
