using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The invariant this file pins: <b>an awaited UniFFI machine call begun on the
/// UI thread runs to completion without ever needing that thread again.</b>
///
/// <para>Why it needs pinning. WinUI's <c>DispatcherQueueSynchronizationContext</c>
/// is single-threaded, and <c>await</c> captures it. The generated UniFFI async
/// layer drives a Rust future by calling <c>rust_future_poll</c> in a loop, and
/// every iteration of that loop sits behind an <c>await</c> — so with the
/// caller's context captured, polls 2..N are queued onto the UI thread. Poll 1
/// runs synchronously (it starts the connect); the request bytes are written on
/// a LATER poll. Occupy the UI thread in between and the call has entered Rust,
/// set its in-flight flag, and issued nothing — which is exactly what
/// `test_bundled_provider.py --app windows` saw seven runs running: a stall at a
/// different step each time, the machine's `verifying` flag set, and the
/// provider fake showing no request at all for the stalled call
/// .</para>
///
/// <para>Nothing here is WinUI-specific — any single-threaded context starves
/// the same way — which is why the fix belongs in the FFI async layer
/// (<c>libs/uniffi-bindgen-cs/templates/Async.cs</c> awaits its poll loop with
/// <c>ConfigureAwait(false)</c>) and not in a per-call-site <c>Task.Run</c> at
/// each of the ~40 machine calls a ViewModel makes. A VM's OWN <c>await</c>
/// still resumes on the UI thread, which is what a VM needs; only the poll loop
/// underneath it goes context-free.</para>
///
/// <para>Cost of the split: this asks in ~2 s, with no WinUI and no UIA, the
/// question the e2e could only ask by accident in ~26 min.</para>
/// </summary>
public class UniffiAsyncOffUiThreadTests
{
    /// <summary>
    /// A generous ceiling for one loopback HTTP round trip driven to completion
    /// (convention 14: latency-independent state, budget sized far above any
    /// non-pathological delay). The call it grades normally finishes in
    /// milliseconds; a starved poll loop never finishes at all, so this value
    /// only sets how long a red run takes to say so.
    /// </summary>
    private static readonly TimeSpan MachineCallBudget = TimeSpan.FromSeconds(20);

    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    /// <summary>Minimal one-shot-per-connection HTTP/1.1 stand-in for the bundled
    /// provider's device endpoint, recording the paths it was actually asked for.
    /// Raw sockets rather than <c>HttpListener</c> (which wants a URL ACL);
    /// `checked_base_url` admits <c>http://</c> for loopback exactly so a local
    /// fake can stand in.</summary>
    private sealed class LoopbackDeviceEndpoint : IDisposable
    {
        private readonly TcpListener _listener;
        private readonly CancellationTokenSource _cts = new();
        private readonly List<string> _paths = new();
        private readonly object _lock = new();

        public LoopbackDeviceEndpoint()
        {
            _listener = new TcpListener(IPAddress.Loopback, 0);
            _listener.Start();
            _ = Task.Run(AcceptLoop);
        }

        public string BaseUrl => $"http://127.0.0.1:{((IPEndPoint)_listener.LocalEndpoint).Port}";

        public IReadOnlyList<string> SeenPaths
        {
            get { lock (_lock) return _paths.ToArray(); }
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
                lock (_lock) _paths.Add(path);

                var contentLength = 0;
                string? line;
                while (!string.IsNullOrEmpty(line = reader.ReadLine()))
                {
                    if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase))
                        contentLength = int.Parse(line.Split(':')[1].Trim());
                }
                for (var i = 0; i < contentLength; i++) reader.Read();

                var body = $$"""
                    {"device_code":"dev-1","user_code":"FAUNA-0001",
                     "verification_uri":"{{BaseUrl}}/activate",
                     "verification_uri_complete":"{{BaseUrl}}/activate?user_code=FAUNA-0001",
                     "expires_in":300,"interval":1}
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

    private static OnboardingMachine NewMachine()
        => OnboardingMachine.NewWithPersistence(
            new FakeOnboardingObserver(), new FakeAccountRegistry().PendingProvisionStore());

    /// <summary>
    /// The whole defect in one assertion. <c>HostedAuthBegin</c> is started on the
    /// pumped thread — so its poll loop captures that context, exactly as it does
    /// on the real dispatcher — and the thread is then occupied. Completing the
    /// call needs a full loopback request/response, i.e. several more polls: if
    /// those polls need the occupied thread, the task cannot finish and nothing
    /// ever reaches the endpoint.
    /// </summary>
    [Fact]
    public void AnAwaitedMachineCall_CompletesWhileTheStartingThreadIsOccupied()
    {
        using var provider = new LoopbackDeviceEndpoint();
        using var ui = new PumpedUiThread();

        var m = NewMachine();
        m.SelectDnsProvider("bundled");
        m.SetDnsCred("base-url", provider.BaseUrl);

        Task<HostedAuthPrompt> begin = null!;
        ui.Run(() => begin = m.HostedAuthBegin(CredentialForm.Dns, "api-token"));

        bool finished;
        using (ui.Occupy())
        {
            finished = begin.Wait(MachineCallBudget);
        }

        Assert.True(finished,
            "an awaited machine call begun on the UI thread must run to completion without "
            + "needing that thread again — the FFI poll loop may not capture the caller's "
            + $"SynchronizationContext. Endpoint saw: [{string.Join(", ", provider.SeenPaths)}]");
        Assert.Contains(provider.SeenPaths, p => p.Contains("/auth/device"));
    }
}
