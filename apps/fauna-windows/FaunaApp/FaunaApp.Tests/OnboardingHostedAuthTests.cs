using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The bundled provider's `hosted-auth` device flow (onboarding.md § 4),
/// driven through the SAME VM delegate <c>GenericProviderForm</c>'s button
/// calls — <c>HostedAuthRunDns</c>, which owns begin → open → poll as one
/// operation.
///
/// Why this exists: `test_bundled_provider.py --app windows` sat on `Pending`
/// with NO `/auth/token` request ever issued in four of five runs
/// (2026-09-03), while macOS/iOS pass the identical journey — so the defect is
/// windows-side, but the e2e cannot say WHERE. This test is the split: it
/// exercises the VM → UniFFI → shared-machine path with no WinUI dispatcher,
/// no DependencyProperty binding and no browser launch in the picture. Green
/// here means the machine and the FFI async layer are sound and the fault is
/// in the View's wiring; red here reproduces the whole thing in ~20 seconds
/// instead of ~25 minutes.
///
/// The provider is reached over a raw loopback socket rather than
/// <c>HttpListener</c> (which wants a URL ACL) — `checked_base_url` admits
/// `http://` for loopback exactly so a local fake can stand in.
/// </summary>
public class OnboardingHostedAuthTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    /// <summary>Minimal one-shot-per-connection HTTP/1.1 server. Answers the
    /// two device-flow endpoints and records which paths it saw.</summary>
    private sealed class LoopbackBundled : IDisposable
    {
        private readonly TcpListener _listener;
        private readonly CancellationTokenSource _cts = new();
        public readonly List<string> Paths = new();
        private readonly object _lock = new();

        public LoopbackBundled()
        {
            _listener = new TcpListener(IPAddress.Loopback, 0);
            _listener.Start();
            _ = Task.Run(AcceptLoop);
        }

        public string BaseUrl => $"http://127.0.0.1:{((IPEndPoint)_listener.LocalEndpoint).Port}";

        public IReadOnlyList<string> SeenPaths
        {
            get { lock (_lock) return Paths.ToArray(); }
        }

        private async Task AcceptLoop()
        {
            while (!_cts.IsCancellationRequested)
            {
                TcpClient client;
                try { client = await _listener.AcceptTcpClientAsync(_cts.Token); }
                catch { return; }
                _ = Task.Run(() => Serve(client));
            }
        }

        private void Serve(TcpClient client)
        {
            try
            {
                using var _ = client;
                using var stream = client.GetStream();
                var reader = new StreamReader(stream, Encoding.ASCII, false, 1024, true);

                var requestLine = reader.ReadLine();
                if (string.IsNullOrEmpty(requestLine)) return;
                var path = requestLine.Split(' ')[1];
                lock (_lock) Paths.Add(path);

                var contentLength = 0;
                string? line;
                while (!string.IsNullOrEmpty(line = reader.ReadLine()))
                {
                    if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase))
                        contentLength = int.Parse(line.Split(':')[1].Trim());
                }
                for (var i = 0; i < contentLength; i++) reader.Read();

                var body = path.Contains("/auth/device")
                    ? $$"""
                        {"device_code":"dev-1","user_code":"FAUNA-0001",
                         "verification_uri":"{{BaseUrl}}/activate",
                         "verification_uri_complete":"{{BaseUrl}}/activate?user_code=FAUNA-0001",
                         "expires_in":300,"interval":1}
                        """
                    : """
                        {"access_token":"bundled-test-token","token_type":"bearer","scope":"provisioning"}
                        """;

                var bytes = Encoding.UTF8.GetBytes(body);
                var header = Encoding.ASCII.GetBytes(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                    + $"Content-Length: {bytes.Length}\r\nConnection: close\r\n\r\n");
                stream.Write(header);
                stream.Write(bytes);
                stream.Flush();
            }
            catch { /* a torn-down connection is not a test failure */ }
        }

        public void Dispose()
        {
            _cts.Cancel();
            _listener.Stop();
            _cts.Dispose();
        }
    }

    private static OnboardingViewModel NewVm()
        => new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    /// <summary>
    /// The whole button flow in one call: begin hands the verification URL to
    /// the open-URL callback, and the poll that follows must actually REACH the
    /// token endpoint — leaving `CanVerifyDns` true — without the caller ever
    /// touching the token itself.
    /// </summary>
    [Fact]
    public async Task HostedAuthRun_OpensTheUrlThenPollsTheTokenEndpoint()
    {
        using var provider = new LoopbackBundled();
        var vm = NewVm();
        vm.SelectDnsProvider("bundled");
        vm.SetDnsCred("base-url", provider.BaseUrl);

        var opened = new List<string>();
        await vm.HostedAuthRunDns("api-token", opened.Add);

        Assert.Contains("/v1/auth/device", provider.SeenPaths);
        var url = Assert.Single(opened);
        Assert.Contains("/activate", url);
        Assert.Contains(provider.SeenPaths, p => p.Contains("/v1/auth/token"));
        Assert.True(vm.CanVerifyDns,
            "the token landing in the credential bag is what turns can_verify_dns true (onboarding.md § 4)");
    }

    /// <summary>
    /// The open-URL callback is the app's browser launch — on windows a
    /// fire-and-forget `Launcher.LaunchUriAsync` that can throw on a box with
    /// no default association. A refusal there must not cost the user the
    /// sign-in: the poll still runs, and the code stays reachable in the
    /// button's own `Pending` label.
    /// </summary>
    [Fact]
    public async Task HostedAuthRun_StillPolls_WhenTheBrowserLaunchThrows()
    {
        using var provider = new LoopbackBundled();
        var vm = NewVm();
        vm.SelectDnsProvider("bundled");
        vm.SetDnsCred("base-url", provider.BaseUrl);

        await vm.HostedAuthRunDns("api-token", _ => throw new InvalidOperationException("no browser"));

        Assert.Contains(provider.SeenPaths, p => p.Contains("/v1/auth/token"));
        Assert.True(vm.CanVerifyDns);
    }

    /// <summary>
    /// An observer that behaves like windows' real <c>NotifyObserver</c> plus the
    /// per-tick refresh it drives: every machine change is marshalled onto the UI
    /// thread, where the bindings synchronously read the machine back
    /// (<c>OnboardingViewModel</c>'s PropertyChanged bridge calls <c>Step()</c>,
    /// and <c>GenericProviderForm.RefreshHostedAuthButtons</c> calls
    /// <c>HostedAuthState()</c> for each hosted-auth field). Those reads take the
    /// machine's own lock from the UI thread, which is the interaction a plain
    /// no-op observer cannot exercise.
    /// </summary>
    private sealed class MarshallingObserver(PumpedUiThread ui, Func<OnboardingViewModel?> vm)
        : OnboardingObserver
    {
        public void OnChanged() => ui.Post(_ =>
        {
            var v = vm();
            if (v is null) return;
            // The two synchronous machine reads every tick performs in the app.
            _ = v.CurrentStep;
            _ = v.HostedAuthLabelDns("api-token");
        }, null);
    }

    /// <summary>
    /// The whole device flow, driven from a single-threaded context the way the
    /// app drives it: the button's Click handler runs on the UI thread, so
    /// `RunHostedAuth`'s two awaited machine calls and every observer-driven
    /// binding read are all contending for that one thread.
    ///
    /// <para>This is the shape `test_bundled_provider.py --app windows` exercises
    /// and the plain (context-free) tests above cannot: xUnit's context lets every
    /// continuation land on a pool thread, so a UI-thread affinity bug is
    /// invisible there. Here the flow must still reach the token endpoint and
    /// leave the field Connected.</para>
    /// </summary>
    [Fact]
    public void HostedAuthRun_ReachesTheTokenEndpoint_WhenDrivenFromASingleThreadedContext()
    {
        using var provider = new LoopbackBundled();
        using var ui = new PumpedUiThread();

        OnboardingViewModel? vm = null;
        var observer = new MarshallingObserver(ui, () => vm);

        // Built ON the pumped thread, exactly as OnboardingPage builds it on the
        // UI thread — so the machine's observer and every delegate are captured
        // in that context.
        ui.Run(() =>
        {
            vm = new OnboardingViewModel(observer, new FakeAccountRegistry());
            vm.SelectDnsProvider("bundled");
            vm.SetDnsCred("base-url", provider.BaseUrl);
        });

        Task flow = null!;
        ui.Run(() => flow = vm!.HostedAuthRunDns("api-token", _ => { }));

        Assert.True(flow.Wait(TimeSpan.FromSeconds(30)),
            "the hosted-auth flow must finish when driven from a single-threaded UI context. "
            + $"Provider saw: [{string.Join(", ", provider.SeenPaths)}]");
        Assert.Contains(provider.SeenPaths, p => p.Contains("/v1/auth/token"));
        Assert.True(vm!.CanVerifyDns,
            "the token landing in the credential bag is what turns can_verify_dns true");
    }

    /// <summary>
    /// A `hosted-auth` field with no provider address cannot begin — the
    /// machine refuses, the field goes Failed, and the flow must return rather
    /// than throw into the view's `async void` Click handler.
    /// </summary>
    [Fact]
    public async Task HostedAuthRun_ReturnsQuietly_WhenThereIsNoBaseUrl()
    {
        var vm = NewVm();
        vm.SelectDnsProvider("bundled");

        var opened = new List<string>();
        await vm.HostedAuthRunDns("api-token", opened.Add);

        Assert.Empty(opened);
        Assert.False(vm.CanVerifyDns);
    }
}
