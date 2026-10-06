using System.Collections.Generic;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The transfer-consent-handshake rows (family-safety.md § Graduation & transfer,
/// ratified 2026-07-12): <c>FamilyWardRow.PendingTransferGuardianHandle</c> (the
/// initiator side's per-ward proposal state) and <c>FamilyIncomingTransferRow.DisplayText</c>
/// (the proposed-guardian side's prompt).
/// </summary>
[Collection("StringsGlobal")]
public class FamilyTransferRowTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static FfiFamilyPendingTransfer PendingTransfer(string proposedGuardianHandle, byte[]? proposedGuardianActorId = null, long createdAt = 0L) =>
        new(proposedGuardianActorId ?? new byte[32], proposedGuardianHandle, createdAt);

    private static FfiFamilyIncomingTransfer IncomingTransfer(
        string supervisedHandle, string guardianHandle, byte[]? supervisedActorId = null, long createdAt = 0L) =>
        new(supervisedActorId ?? new byte[32], supervisedHandle, guardianHandle, createdAt);

    [Fact]
    public void WardRow_WithNoPendingTransfer_HasNullGuardianHandle()
    {
        var ward = FfiFamilyWardInfoFixture.Make();

        var row = FamilyWardRow.From(ward);

        Assert.Null(row.PendingTransferGuardianHandle);
    }

    [Fact]
    public void WardRow_WithAPendingTransfer_CarriesTheProposedGuardianHandle()
    {
        var ward = FfiFamilyWardInfoFixture.Make(pendingTransfer: PendingTransfer("bob"));

        var row = FamilyWardRow.From(ward);

        Assert.Equal("bob", row.PendingTransferGuardianHandle);
    }

    [Fact]
    public void IncomingTransferRow_DisplayText_SubstitutesBothTheGuardianAndTheWard()
    {
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["family/incoming_transfer_text"] = "{guardian} asks you to take over supervision of {ward}",
        }));
        var entry = IncomingTransfer(supervisedHandle: "child", guardianHandle: "alice");

        var row = FamilyIncomingTransferRow.From(entry);

        Assert.Equal("alice asks you to take over supervision of child", row.DisplayText);
    }
}
