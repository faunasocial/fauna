using System;
using System.Collections.Generic;
using System.IO;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The SSH door's windows surface over the shared onboarding machine
/// (<c>onboarding-provisioning.md</c> § 5; <c>installers/vps.md</c> § The SSH door →
/// <i>The key</i>): the <c>vps-config-ssh-door-toggle</c> is OFF by default and its
/// warning shows only while it is on; a desktop app hands the machine its
/// <c>%USERPROFILE%\.ssh</c>; <c>provisioning-ssh-door-command</c> is empty until a door
/// run succeeded, and its text picks the template linux, tui and macos pick. The real
/// machine over the native FFI — the same state path the e2e drives. Shares the
/// process-global <see cref="Strings"/> localizer, hence the serialized collection.
/// </summary>
[Collection("StringsGlobal")]
public class OnboardingViewModelSshDoorTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    // The en templates, with their placeholders in an order the fill must not
    // depend on (a translation may reorder them): by name, never by position.
    private static void UseLocalizer() =>
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["onboarding/nest_provisioning/ssh_door_command_template"] =
                "key {path} for {command} from {address} ({fingerprint})",
            ["onboarding/nest_provisioning/ssh_door_command_no_path_template"] =
                "{fingerprint}: {command} from {address}",
            ["onboarding/nest_provisioning/ssh_door_address_unknown"] = "the setup address",
        }));

    private static OnboardingViewModel NewVm()
        => new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    [Fact]
    public void TheDoorIsOffByDefault_AndItsWarningFollowsTheToggle()
    {
        var vm = NewVm();
        Assert.False(vm.ProvisionSshDoorEnabled);
        Assert.False(vm.SshDoorWarningVisible);

        vm.SetProvisionSshDoor(true);
        Assert.True(vm.ProvisionSshDoorEnabled);
        Assert.True(vm.SshDoorWarningVisible);

        vm.SetProvisionSshDoor(false);
        Assert.False(vm.ProvisionSshDoorEnabled);
        Assert.False(vm.SshDoorWarningVisible);
    }

    [Fact]
    public void ADesktopAppKeepsTheDoorKeyInTheProfilesDotSsh()
    {
        var profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        Assert.Equal(Path.Combine(profile, ".ssh"), OnboardingViewModel.SshDoorKeyDir);
    }

    [Fact]
    public void NoDoorCommandShowsBeforeADoorRunSucceeded()
    {
        var vm = NewVm();
        vm.SetProvisionSshDoor(true);
        Assert.Equal("", vm.ProvisioningSshDoorCommandText);
    }

    [Fact]
    public void TheCommandNamesTheKeyPathAndTheAdmittedAddress_ByName()
    {
        UseLocalizer();
        var door = new SshDoorReadout(
            "box.example", "ssh box.example", @"C:\Users\a\.ssh\fauna-box.example",
            "SHA256:abc", "203.0.113.7");
        Assert.Equal(
            @"key C:\Users\a\.ssh\fauna-box.example for ssh box.example from 203.0.113.7 (SHA256:abc)",
            OnboardingViewModel.SshDoorCommandText(door));
    }

    [Fact]
    public void WithNoKeyPathOrEchoedAddress_TheCommandTakesTheNoPathFormAndTheSetupAddressWording()
    {
        UseLocalizer();
        var door = new SshDoorReadout(
            "box.example", "ssh -i fauna-box.example root@box.example", null, "SHA256:abc", null);
        Assert.Equal(
            "SHA256:abc: ssh -i fauna-box.example root@box.example from the setup address",
            OnboardingViewModel.SshDoorCommandText(door));
    }
}
