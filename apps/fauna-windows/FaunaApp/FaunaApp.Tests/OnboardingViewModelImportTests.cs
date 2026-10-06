using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The OnboardingViewModel is otherwise a thin proxy over the shared UniFFI
/// OnboardingMachine (its tests live in libs/fauna-onboarding-machine), but the
/// identity-import path carries genuine <b>client glue</b> the machine does not
/// own: the pasted/scanned field is first run through the shared
/// <c>fauna_core::identity_qr</c> parser (UniFFI <c>parse_identity_import</c>),
/// and a parse failure must surface the localized
/// <c>onboarding.identity_import.invalid_secret</c> <b>without touching the
/// machine</b> — so the user sees the parse string, not the machine's
/// <c>errors.secret_key_invalid</c>. This is the windows leg of the cross-app
/// adoption (web/android/iOS/macOS/linux already done — onboarding.md
/// §1.identity_import; priority #2/#4), mirroring apple
/// <c>OnboardingVM.importIdentity</c> / linux <c>identity_import.rs</c>.
///
/// These construct a real <see cref="OnboardingMachine"/> (the native fauna_ffi
/// dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>). The unit-test host has
/// no WinRT localizer, so <see cref="Strings.Get"/> returns the raw key; the
/// assertions compare against <c>Strings.Get(...)</c> so they hold either way.
/// </summary>
public class OnboardingViewModelImportTests
{
    // Minimal no-op observer — the parse-failure path never transitions the
    // machine, so OnChanged is never invoked here.
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    [Fact]
    public void ConfirmImportedIdentity_InvalidSecret_SurfacesLocalizedParseError()
    {
        var vm = new OnboardingViewModel(
            new FakeOnboardingObserver(), new FakeAccountRegistry());
        vm.BeginImportIdentityCommand.Execute(null);

        // Not 64 hex chars and not a fauna://identity URI → the shared parser
        // returns an empty list (parse failure).
        vm.ImportedSecret = "not-a-valid-secret";
        vm.ConfirmImportedIdentityCommand.Execute(null);

        // The localized parse string is surfaced — NOT the machine's
        // errors.secret_key_invalid (which the old raw-field routing produced).
        Assert.True(vm.HasError);
        Assert.Equal(Strings.Get("onboarding/identity_import/invalid_secret"), vm.ErrorMessage);
    }
}
