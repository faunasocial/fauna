using System.Collections.Generic;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The family page's two age-band readouts (family-safety.md § App surface →
/// *Age-band surfaces*): <c>family-ward-age-band</c> on each ward row
/// (<see cref="FamilyWardRow.AgeBandText"/>) and <c>family-age-band-summary</c>
/// on the supervised side (<see cref="FamilyViewModel.AgeBandSummary"/>) — both
/// the shared <c>age_band_line</c>, and both ABSENT, never placeholdered, when
/// there is no band or the client cannot name it.
/// </summary>
[Collection("StringsGlobal")]
public class FamilyAgeBandTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    public FamilyAgeBandTests() => Strings.Initialize(new FakeLocalizer(new()));

    private static readonly FfiFamilyAgeBand Band = new("13-15", "guardian-asserted");

    [Fact]
    public void WardRow_WithABand_RendersTheSharedLine()
    {
        var row = FamilyWardRow.From(FfiFamilyWardInfoFixture.Make(ageBand: Band));

        Assert.True(row.HasAgeBand);
        Assert.False(string.IsNullOrEmpty(row.AgeBandText));
    }

    [Fact]
    public void WardRow_WithoutABand_IsAbsent()
    {
        var row = FamilyWardRow.From(FfiFamilyWardInfoFixture.Make());

        Assert.False(row.HasAgeBand);
        Assert.Null(row.AgeBandText);
    }

    [Fact]
    public void WardRow_WithAnUnnameableBand_IsAbsent()
    {
        var row = FamilyWardRow.From(
            FfiFamilyWardInfoFixture.Make(ageBand: new FfiFamilyAgeBand("99+", "guardian-asserted")));

        Assert.False(row.HasAgeBand);
    }

    [Fact]
    public async Task Summary_FollowsTheCallersOwnBand()
    {
        var rpc = new MockNestRpcClient { NextFamilyStatus = FfiFamilyStatusFixture.Make(ageBand: Band) };
        var vm = new FamilyViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(string.IsNullOrEmpty(vm.AgeBandSummary));

        rpc.NextFamilyStatus = FfiFamilyStatusFixture.Make();
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Null(vm.AgeBandSummary);
    }
}
