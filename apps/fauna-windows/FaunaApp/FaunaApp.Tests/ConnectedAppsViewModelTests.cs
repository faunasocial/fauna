using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_client_connected_apps;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Settings → Connected apps page VM (<c>docs/goal/ui/connected-apps.md</c> § Layout &amp;
/// flow, § Errors &amp; edge cases). windows is the last app to paint this page; these cases pin
/// what the other six already hold and are the deterministic gate on Windows (FlaUI flakes on
/// win-arm64).
///
/// <para>They drive a FAKE <see cref="IConnectedAppsMachine"/> through the VM's test constructor,
/// so no nest is needed. What they assert is the VM's job — <b>projection of a machine snapshot
/// and forwarding of gestures</b>. The roster's composition, the revoke verb and the scope words
/// are the machine's, pinned in shared Rust; a client-side copy of them is the priority-#2
/// violation this page exists to avoid.</para>
/// </summary>
/// <remarks>Joins the <c>StringsGlobal</c> collection because the row-text cases install a
/// localizer into the process-global <see cref="Strings"/>.</remarks>
[Collection("StringsGlobal")]
public class ConnectedAppsViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["connected_apps/verbatim"] = "{text}",
            ["connected_apps/row_head"] = "{name} · {badge}",
            ["connected_apps/publisher_line"] = "{publisher} — {client_id}",
            ["connected_apps/bullet"] = "  • {text}",
            ["connected_apps/indented"] = "  {text}",
            ["connected_apps/class_device"] = "App on a device",
            ["connected_apps/class_app_password"] = "Signed in with an app password",
            ["connected_apps/class_signer"] = "Nostr signer app",
            ["connected_apps/publisher"] = "From {domain}",
            ["connected_apps/not_connected"] = "Not connected right now",
            ["connected_apps/created"] = "Added {time}",
            ["connected_apps/last_used"] = "Last used {time}",
            ["connected_apps/never_used"] = "Never used",
            ["connected_apps/lasts_until"] = "Until {time}",
            ["connected_apps/open_ended"] = "Until you disconnect it",
            ["connected_apps/blocked_since"] = "Blocked {time}",
            ["connected_apps/consent_ends_holder"] =
                "This app now uses a new key. Approving ends the access its earlier key was given.",
            ["connected_apps/consent_assistant_reach"] =
                "Whatever you allow here, the AI assistant you connect can read or do.",
            ["settings/mail/credential_revoked"] = "Compromised — access revoked",
            ["atproto_settings/consent_heading"] = "An app wants to connect",
            ["atproto_settings/consent_client"] = "{name} — {client_id}",
            ["atproto_settings/consent_client_unnamed"] = "{client_id}",
            ["atproto_settings/consent_scopes_heading"] = "It is asking to:",
            ["atproto_settings/consent_code"] = "Confirmation code: {code}",
            ["atproto_settings/consent_set_heading"] = "Some of that comes from “{title}” ({nsid}):",
            ["atproto_settings/consent_set_heading_unnamed"] = "Some of that comes from {nsid}:",
            ["connected_apps/install_heading"] = "Install this plugin on your nest",
            ["connected_apps/install_heading_bind"] = "Connect this plugin on your nest to your account",
            ["connected_apps/install_framing"] = "Installing grants nothing.",
            ["connected_apps/install_form"] = "Runs as: {form}",
            ["connected_apps/install_digest"] = "Module: {digest}",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    private static void UseRealStrings() => Strings.Initialize(new FakeLocalizer());

    // Derives from the GENERATED fake base, like AtprotoViewModelTests: every member is virtual
    // and throws, so a later widening of the Rust interface cannot break this file.
    private sealed class FakeMachine : ConnectedAppsMachineFakeBase
    {
        public ConnectedAppsSnapshot Snap;
        public readonly List<string> Calls = new();

        /// <summary>What <c>Refresh</c> makes the snapshot become — the roster read is the
        /// machine's, so the fake plays back a scripted outcome.</summary>
        public Func<ConnectedAppsSnapshot>? OnRefresh;

        /// <summary>The secret <c>RevealSecret</c> answers per row key.</summary>
        public readonly Dictionary<string, string> Secrets = new();

        public FakeMachine(ConnectedAppsSnapshot snap) { Snap = snap; }

        public override ConnectedAppsSnapshot Snapshot() => Snap;

        public override Task Refresh()
        {
            Calls.Add("Refresh");
            if (OnRefresh is not null) Snap = OnRefresh();
            return Task.CompletedTask;
        }

        public override Task SubmitCode(string code) { Calls.Add($"SubmitCode:{code}"); return Task.CompletedTask; }

        public override Task OpenHandoff(string requestUri) { Calls.Add($"OpenHandoff:{requestUri}"); return Task.CompletedTask; }

        public override Task ResolveRequest(string consentIdHex, bool approved)
        {
            Calls.Add($"ResolveRequest:{consentIdHex}:{approved}");
            return Task.CompletedTask;
        }

        public override Task BlockRequest(string consentIdHex) { Calls.Add($"BlockRequest:{consentIdHex}"); return Task.CompletedTask; }

        public override Task Unblock(string clientId) { Calls.Add($"Unblock:{clientId}"); return Task.CompletedTask; }

        public override Task Revoke(string key) { Calls.Add($"Revoke:{key}"); return Task.CompletedTask; }

        public override Task SetExternalAppsEnabled(bool enabled)
        {
            Calls.Add($"SetExternalAppsEnabled:{enabled}");
            Snap = Snap with { @externalAppsOff = !enabled };
            return Task.CompletedTask;
        }

        public override Task<string?> RevealSecret(string key)
        {
            Calls.Add($"RevealSecret:{key}");
            return Task.FromResult(Secrets.TryGetValue(key, out var s) ? s : null);
        }
    }

    private sealed class StubHandoff : IConsentHandoffSource
    {
        public string? Pending { get; set; }
        public readonly List<string> Finished = new();

        public void Finish(string requestUri)
        {
            Finished.Add(requestUri);
            Pending = null;
        }
    }

    private static LocalizedText Text(string key, params (string, string)[] args) =>
        new(key, args.ToDictionary(a => a.Item1, a => a.Item2));

    private static LocalizedText Verbatim(string text) => Text("connected_apps.verbatim", ("text", text));

    private static ConnectedAppsSnapshot Snapshot(
        bool loaded = true,
        ConsentCardRow[]? requests = null,
        ConnectedAppRow[]? principals = null,
        BlockedAppRow[]? blocked = null,
        bool externalAppsOff = false) =>
        new(
            @loaded: loaded,
            @requests: requests ?? Array.Empty<ConsentCardRow>(),
            @principals: principals ?? Array.Empty<ConnectedAppRow>(),
            @blocked: blocked ?? Array.Empty<BlockedAppRow>(),
            @externalAppsOff: externalAppsOff,
            @error: null);

    private static ConnectedAppRow Row(
        string key = "k1",
        string cls = "device",
        string name = "Ivory",
        string? clientId = null,
        string? publisher = null,
        string[]? scopes = null,
        long createdAtMillis = 1_700_000_000_000,
        long? lastUsedAtMillis = null,
        long? lastsUntilMillis = null,
        bool connected = true,
        bool paused = false,
        MailAppPassword? mail = null,
        ConsentSetRow[]? sets = null) =>
        new(
            @key: key,
            @class: cls,
            @name: Verbatim(name),
            @clientId: clientId,
            @publisher: publisher,
            @scopeDescriptions: (scopes ?? Array.Empty<string>()).Select(Verbatim).ToArray(),
            @createdAtMillis: createdAtMillis,
            @lastUsedAtMillis: lastUsedAtMillis,
            @lastsUntilMillis: lastsUntilMillis,
            @connected: connected,
            @paused: paused,
            @mail: mail,
            @sets: sets);

    private static MailAppPassword MailRow(bool revoked = false) =>
        new(@muaUsername: "{handle}+abc@example.test", @kind: Verbatim("Password"), @revoked: revoked);

    private static ConsentCardRow Request(
        string consentIdHex = "c1",
        string code = "AAA-BBB",
        string clientId = "https://example.com/oauth",
        string? clientName = null,
        string[]? scopes = null,
        ConsentSetRow[]? sets = null,
        LocalizedText? ends = null) =>
        new(
            @consentIdHex: consentIdHex,
            @code: code,
            @clientId: clientId,
            @clientName: clientName,
            @scopeDescriptions: scopes ?? new[] { "See your basic account identity" },
            @sets: sets ?? Array.Empty<ConsentSetRow>(),
            @ends: ends);

    private static ConsentInstallRow Install(bool bind = false) =>
        new(
            @executionForm: "wasm",
            @requestedKinds: new[] { "ext.plugins.example.notes" },
            @settings: Array.Empty<ConsentInstallSettingRow>(),
            @publisherDomain: "plugins.example",
            @publisherKey: "did:key:z6Mk",
            @moduleDigest: "sha256:abc",
            @hosts: Array.Empty<string>(),
            @ingressPaths: Array.Empty<string>(),
            @development: false,
            @bind: bind);

    private static ConsentCardRow InstallCard(bool bind = false) =>
        Request() with { @install = Install(bind) };

    // ── The roster row's settings link (connected-apps.md § Layout & flow, region 3) ──

    [Theory]
    [InlineData("wasm", true)]
    [InlineData("container", true)]
    [InlineData("device", false)]
    [InlineData("remote", false)]
    public void Only_a_hosted_plugins_row_links_to_its_settings_card(string cls, bool links)
    {
        Assert.Equal(links, ConnectedAppsViewModel.OpensSettingsCard(Row(cls: cls)));
    }

    // ── The install section (third-party.md § The runner contract → The install-approval leg) ──

    [Fact]
    public void An_ordinary_card_paints_no_install_section()
    {
        UseRealStrings();
        Assert.Null(ConnectedAppsViewModel.InstallSection(Request()));
    }

    [Fact]
    public void An_install_card_paints_each_leaf_from_the_shared_composition()
    {
        UseRealStrings();
        var section = ConnectedAppsViewModel.InstallSection(InstallCard());
        Assert.NotNull(section);
        Assert.Equal("Install this plugin on your nest", section!.Heading);
        // The leaf's text is the composed sentence; the raw form is its `form` attr.
        Assert.Equal("Runs as: wasm", section.Form);
        Assert.Equal("wasm", section.FormValue);
        Assert.Equal("Module: sha256:abc", section.Digest);
        // An admin's install card carries the framing line under the heading.
        Assert.Equal("Installing grants nothing.", section.Framing);
    }

    [Fact]
    public void A_bind_card_carries_no_framing_line()
    {
        UseRealStrings();
        var section = ConnectedAppsViewModel.InstallSection(InstallCard(bind: true));
        Assert.NotNull(section);
        Assert.Equal("Connect this plugin on your nest to your account", section!.Heading);
        Assert.Null(section.Framing);
    }

    private static (ConnectedAppsViewModel Vm, FakeMachine M) Make(
        ConnectedAppsSnapshot snap, string handle = "alice")
    {
        var m = new FakeMachine(snap);
        return (new ConnectedAppsViewModel(m, handle), m);
    }

    // ── The external-apps switch ──

    [Fact]
    public async Task The_switch_state_attr_mirrors_the_snapshot_and_on_is_the_zero_value()
    {
        var (before, _) = Make(Snapshot(externalAppsOff: true));
        Assert.Equal("on", before.ExternalAppsState);   // before any read: ON is the zero value

        await before.VisitAsync();
        Assert.Equal("off", before.ExternalAppsState);

        var (on, _) = Make(Snapshot(externalAppsOff: false));
        await on.VisitAsync();
        Assert.Equal("on", on.ExternalAppsState);
    }

    [Fact]
    public async Task The_switch_gesture_forwards_to_the_machine_and_paints_the_reread()
    {
        var (vm, m) = Make(Snapshot());
        await vm.VisitAsync();

        await vm.SetExternalAppsEnabledAsync(false);

        Assert.Contains("SetExternalAppsEnabled:False", m.Calls);
        Assert.Equal("off", vm.ExternalAppsState);
    }

    // ── A visit starts unread ──

    [Fact]
    public async Task A_visit_reads_the_roster_and_only_then_reports_it_loaded()
    {
        var (vm, m) = Make(Snapshot(loaded: false));
        m.OnRefresh = () => Snapshot(loaded: true, principals: new[] { Row() });

        Assert.False(vm.Loaded);   // before any visit: neither rows nor the empty state

        await vm.VisitAsync();

        Assert.Contains("Refresh", m.Calls);
        Assert.True(vm.Loaded);
        Assert.Single(vm.Snapshot!.@principals);
    }

    [Fact]
    public async Task A_visit_drops_the_previous_visits_drafts_and_every_shown_secret()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row("m1", mail: MailRow()) }));
        m.Secrets["m1"] = "s3cret";
        await vm.VisitAsync();
        vm.Code = "half-typed";
        vm.ArmRevoke("m1");
        await vm.ToggleRevealAsync("m1");
        Assert.True(vm.Revealed.ContainsKey("m1"));

        await vm.VisitAsync();

        Assert.Equal(string.Empty, vm.Code);
        Assert.Null(vm.RevokeArmed);
        Assert.Empty(vm.Revealed);
    }

    // ── Gestures forward; the VM picks no verb ──

    [Fact]
    public async Task Submitting_a_code_trims_it_forwards_it_and_clears_the_field()
    {
        var (vm, m) = Make(Snapshot());
        await vm.VisitAsync();
        vm.Code = "  ABCD-1234 \n";

        await vm.SubmitCodeAsync();

        Assert.Contains("SubmitCode:ABCD-1234", m.Calls);
        Assert.Equal(string.Empty, vm.Code);
    }

    [Fact]
    public async Task A_blank_code_is_never_forwarded()
    {
        var (vm, m) = Make(Snapshot());
        await vm.VisitAsync();
        vm.Code = "   ";

        await vm.SubmitCodeAsync();

        Assert.DoesNotContain(m.Calls, c => c.StartsWith("SubmitCode"));
    }

    [Fact]
    public async Task Request_gestures_forward_to_the_machine_unchanged()
    {
        var (vm, m) = Make(Snapshot(requests: new[] { Request("deadbeef") }));
        await vm.VisitAsync();

        await vm.ResolveRequestAsync("deadbeef", approved: true);
        await vm.ResolveRequestAsync("deadbeef", approved: false);
        await vm.BlockRequestAsync("deadbeef");
        await vm.UnblockAsync("https://example.com/oauth");

        Assert.Contains("ResolveRequest:deadbeef:True", m.Calls);
        // A decline is the NEST call, never a local dismiss: it is what gives the waiting app a
        // clean refusal instead of a timeout.
        Assert.Contains("ResolveRequest:deadbeef:False", m.Calls);
        Assert.Contains("BlockRequest:deadbeef", m.Calls);
        Assert.Contains("Unblock:https://example.com/oauth", m.Calls);
    }

    [Fact]
    public async Task Revoking_passes_only_the_opaque_row_key_and_closes_the_confirm()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row("signer:7", cls: "signer") }));
        await vm.VisitAsync();
        vm.ArmRevoke("signer:7");
        Assert.Equal("signer:7", vm.RevokeArmed);

        await vm.ConfirmRevokeAsync("signer:7");

        // The machine encodes the credential class in the key and picks the verb; the app never does.
        Assert.Contains("Revoke:signer:7", m.Calls);
        Assert.Null(vm.RevokeArmed);
    }

    [Fact]
    public async Task Cancelling_a_revoke_closes_the_confirm_without_calling_the_machine()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row() }));
        await vm.VisitAsync();
        vm.ArmRevoke("k1");

        vm.CancelRevoke();

        Assert.Null(vm.RevokeArmed);
        Assert.DoesNotContain(m.Calls, c => c.StartsWith("Revoke"));
    }

    // ── Mail app-password secrets ──

    [Fact]
    public async Task A_mail_secret_is_absent_until_asked_and_the_toggle_reads_it_once()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row("m1", mail: MailRow()) }));
        m.Secrets["m1"] = "hunter2";
        await vm.VisitAsync();

        Assert.Empty(vm.Revealed);   // never in the snapshot, never painted by default

        await vm.ToggleRevealAsync("m1");
        Assert.Equal("hunter2", vm.Revealed["m1"]);

        await vm.ToggleRevealAsync("m1");   // second press hides, with no second read
        Assert.Empty(vm.Revealed);
        Assert.Equal(1, m.Calls.Count(c => c == "RevealSecret:m1"));
    }

    [Fact]
    public async Task Copying_reads_the_secret_without_showing_it()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row("m1", mail: MailRow()) }));
        m.Secrets["m1"] = "hunter2";
        await vm.VisitAsync();

        var secret = await vm.ReadSecretAsync("m1");

        Assert.Equal("hunter2", secret);
        Assert.Empty(vm.Revealed);   // copy is independent of the reveal toggle
    }

    [Fact]
    public async Task A_failed_secret_read_shows_nothing()
    {
        var (vm, _) = Make(Snapshot(principals: new[] { Row("m1", mail: MailRow()) }));
        await vm.VisitAsync();

        await vm.ToggleRevealAsync("m1");   // the fake has no secret for the key → null

        Assert.Empty(vm.Revealed);
    }

    [Fact]
    public async Task Revoking_a_row_takes_its_shown_secret_off_the_screen()
    {
        var (vm, m) = Make(Snapshot(principals: new[] { Row("m1", mail: MailRow()) }));
        m.Secrets["m1"] = "hunter2";
        await vm.VisitAsync();
        await vm.ToggleRevealAsync("m1");

        await vm.ConfirmRevokeAsync("m1");

        Assert.Empty(vm.Revealed);
    }

    [Fact]
    public async Task The_login_is_the_shared_substitution_of_the_handle()
    {
        var (vm, _) = Make(Snapshot(), handle: "alice");

        var login = vm.MuaUsername(MailRow());

        // The shared Rust resolver, never a locally built address: compare against the face itself.
        Assert.Equal(FaunaFfiMethods.ResolveMuaUsername("{handle}+abc@example.test", "alice"), login);
        Assert.DoesNotContain("{handle}", login);
    }

    // ── The staged fauna://consent route ──

    [Fact]
    public async Task A_staged_consent_route_is_opened_after_the_read_and_cleared_only_then()
    {
        var (vm, m) = Make(Snapshot());
        var handoff = new StubHandoff { Pending = "urn:ietf:params:oauth:request_uri:abc" };
        vm.HandoffSource = handoff;

        await vm.VisitAsync();

        Assert.Equal(new[] { "Refresh", "OpenHandoff:urn:ietf:params:oauth:request_uri:abc" }, m.Calls);
        Assert.Equal(new[] { "urn:ietf:params:oauth:request_uri:abc" }, handoff.Finished);
        Assert.Null(handoff.Pending);
    }

    [Fact]
    public async Task With_nothing_staged_a_visit_never_opens_a_handoff()
    {
        var (vm, m) = Make(Snapshot());
        vm.HandoffSource = new StubHandoff();

        await vm.VisitAsync();

        Assert.DoesNotContain(m.Calls, c => c.StartsWith("OpenHandoff"));
    }

    // ── Row text composition (the one windows copy; the shared-crate lift is queued) ──

    [Fact]
    public void A_row_reads_name_and_badge_then_publisher_scopes_and_facts()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RowText(Row(
            cls: "app_password",
            name: "Graysky",
            clientId: "https://graysky.example/client.json",
            publisher: "graysky.example",
            scopes: new[] { "Read your posts" },
            lastUsedAtMillis: 1_700_000_100_000));

        var lines = text.Split('\n');
        Assert.Equal("Graysky · Signed in with an app password", lines[0]);
        Assert.Equal("From graysky.example — https://graysky.example/client.json", lines[1]);
        Assert.Equal("  • Read your posts", lines[2]);
        Assert.Equal(
            $"Added {FaunaFfiMethods.FormatUnixLocalMs(1_700_000_000_000)}"
            + $" · Last used {FaunaFfiMethods.FormatUnixLocalMs(1_700_000_100_000)}"
            + " · Until you disconnect it",
            lines[3]);
    }

    [Fact]
    public void An_unknown_class_paints_no_badge_and_an_unconnected_row_says_so()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RowText(Row(cls: "some-future-class", name: "Thing", connected: false));

        var lines = text.Split('\n');
        Assert.Equal("Thing", lines[0]);   // no badge rather than a guess
        Assert.StartsWith("Not connected right now · Added ", lines[1]);
        Assert.Contains("Never used", lines[1]);
    }

    [Fact]
    public void A_burned_mail_row_says_so_directly_under_its_name()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RowText(Row(
            cls: "app_password", name: "default", mail: MailRow(revoked: true)));

        var lines = text.Split('\n');
        // Above everything a user might copy into a mail app: the login authenticates nothing now.
        Assert.Equal("Compromised — access revoked", lines[1]);
    }

    [Fact]
    public void A_lasts_until_row_reads_its_horizon()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RowText(Row(lastsUntilMillis: 1_800_000_000_000));

        Assert.Contains($"Until {FaunaFfiMethods.FormatUnixLocalMs(1_800_000_000_000)}", text);
        Assert.DoesNotContain("Until you disconnect it", text);
    }

    [Fact]
    public void A_row_names_the_permission_set_after_its_scope_lines()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RowText(Row(
            scopes: new[] { "Read your posts" },
            sets: new[]
            {
                new ConsentSetRow(
                    @nsid: "app.bsky.authFullApp", @title: "Reading",
                    @details: null, @memberDescriptions: new[] { "See posts" }),
            }));

        var lines = text.Split('\n').ToList();
        var scope = lines.IndexOf("  • Read your posts");
        var heading = lines.FindIndex(l => l.Contains("app.bsky.authFullApp"));
        Assert.True(scope >= 0 && heading > scope, text);
        Assert.Contains("Reading", lines[heading]);
        Assert.True(lines.IndexOf("  • See posts") > heading, text);
    }

    [Fact]
    public void A_composed_text_resolves_its_nested_entries_before_the_outer_template()
    {
        UseRealStrings();
        // `connected_apps.row_head` is "{name} · {badge}": both holes are nested texts, and the
        // nested badge is itself a key that must resolve before it fills the hole.
        var composed = new ComposedText(
            @text: new LocalizedText(@key: "connected_apps.row_head", @args: new Dictionary<string, string>()),
            @nested: new Dictionary<string, LocalizedText>
            {
                ["name"] = Verbatim("Ivory"),
                ["badge"] = new LocalizedText(@key: "connected_apps.class_device", @args: new Dictionary<string, string>()),
            });

        Assert.Equal($"Ivory · {Strings.Get("connected_apps/class_device")}", Strings.Resolve(composed));
    }

    // ── The request card's text ──

    [Fact]
    public void A_named_request_shows_the_name_and_the_client_id_verbatim()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(
            clientId: "https://ivory.app/client-metadata.json", clientName: "Ivory",
            scopes: new[] { "Sign in", "Post on your behalf" }));

        Assert.Contains("Ivory — https://ivory.app/client-metadata.json", text);
        Assert.Contains("  • Sign in", text);
        Assert.Contains("  • Post on your behalf", text);
    }

    [Fact]
    public void An_unnamed_request_shows_its_client_id_alone()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(
            clientId: "https://unnamed.example/oauth", clientName: null));

        Assert.Contains("https://unnamed.example/oauth", text);
        Assert.DoesNotContain(" — https://unnamed.example/oauth", text);
    }

    [Fact]
    public void A_permission_set_renders_its_identity_prose_and_every_member()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(sets: new[]
        {
            new ConsentSetRow(
                @nsid: "app.bsky.authFullApp", @title: "Reading",
                @details: "Read your feed", @memberDescriptions: new[] { "See posts", "See likes" }),
        }));

        Assert.Contains("“Reading” (app.bsky.authFullApp)", text);
        Assert.Contains("  Read your feed", text);
        Assert.Contains("  • See posts", text);
        Assert.Contains("  • See likes", text);
    }

    [Fact]
    public void A_set_with_no_declared_title_renders_its_nsid_alone()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(sets: new[]
        {
            new ConsentSetRow(@nsid: "app.bsky.untitled", @title: null, @details: null,
                @memberDescriptions: Array.Empty<string>()),
        }));

        Assert.Contains("Some of that comes from app.bsky.untitled:", text);
    }

    [Fact]
    public void A_key_replacement_card_ends_with_the_resolved_ends_line_after_the_sets()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(
            sets: new[]
            {
                new ConsentSetRow(@nsid: "app.bsky.untitled", @title: null, @details: null,
                    @memberDescriptions: Array.Empty<string>()),
            },
            ends: Text("connected_apps.consent_ends_holder")));

        const string line = "This app now uses a new key. Approving ends the access its earlier key was given.";
        Assert.EndsWith(line, text);
        Assert.True(text.IndexOf("app.bsky.untitled", StringComparison.Ordinal)
            < text.IndexOf(line, StringComparison.Ordinal));
    }

    [Fact]
    public void An_ordinary_card_shows_no_ends_line()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request());

        Assert.DoesNotContain("new key", text);
        Assert.False(text.EndsWith('\n'));
    }

    [Fact]
    public void The_clis_card_says_what_the_grant_reaches_before_what_the_approve_ends()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request(
            clientId: "https://fauna.social/apps/cli/client-metadata.json",
            ends: Text("connected_apps.consent_ends_holder")));

        var reach = text.IndexOf(Strings.Get("connected_apps/consent_assistant_reach"), StringComparison.Ordinal);
        Assert.True(reach >= 0, text);
        Assert.True(reach < text.IndexOf("This app now uses a new key.", StringComparison.Ordinal), text);
    }

    [Fact]
    public void Another_apps_card_carries_no_reach_line()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.RequestText(Request());

        Assert.DoesNotContain(Strings.Get("connected_apps/consent_assistant_reach"), text);
    }

    [Fact]
    public void The_binding_code_line_carries_the_code_and_the_raw_value_is_a_row_field()
    {
        UseRealStrings();
        var request = Request(code: "XYZ-123");

        Assert.Equal("Confirmation code: XYZ-123", ConnectedAppsViewModel.RequestCodeText(request));
        Assert.Equal("XYZ-123", request.@code);   // what the e2e reads, never the prose
    }

    // ── The row's automation attributes ──

    [Fact]
    public void A_row_no_custodian_served_publishes_no_oracle_ops_at_all()
    {
        Assert.Equal("revoked=;status=connected", ConnectedAppsViewModel.RowAttributes(Row()));
    }

    [Fact]
    public void A_served_row_publishes_its_window_count_beside_its_status()
    {
        var served = Row() with
        {
            @oracle = new OracleSummaryRow(@opsInWindow: 3, @requestsServed: 7, @lastOp: null),
        };

        Assert.Equal("revoked=;status=connected;oracle_ops=3", ConnectedAppsViewModel.RowAttributes(served));
    }

    [Fact]
    public void A_burned_mail_row_reads_revoked_and_a_paused_row_reads_paused()
    {
        Assert.Equal("revoked=true;status=not_connected",
            ConnectedAppsViewModel.RowAttributes(Row(connected: false, mail: MailRow(revoked: true))));
        Assert.Equal("revoked=;status=paused", ConnectedAppsViewModel.RowAttributes(Row(paused: true)));
    }

    // ── The blocked row ──

    [Fact]
    public void A_blocked_row_shows_the_client_id_verbatim_and_when_it_was_blocked()
    {
        UseRealStrings();
        var text = ConnectedAppsViewModel.BlockedText(new BlockedAppRow(
            @clientId: "https://spam.example/app", @blockedAtMillis: 1_700_000_000_000));

        var lines = text.Split('\n');
        Assert.Equal("https://spam.example/app", lines[0]);
        Assert.Equal($"Blocked {FaunaFfiMethods.FormatUnixLocalMs(1_700_000_000_000)}", lines[1]);
    }

    [Fact]
    public async Task The_page_error_is_the_snapshots_localized_error()
    {
        UseRealStrings();
        var snap = Snapshot() with { @error = Text("connected_apps.error_request_gone") };
        var (vm, _) = Make(snap);
        await vm.VisitAsync();

        Assert.False(string.IsNullOrEmpty(vm.PageError));
    }
}
