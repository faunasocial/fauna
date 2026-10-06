using System.ComponentModel;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The invariant this file pins: <b>a TwoWay-bound setter handed the value it
/// already holds must not notify.</b>
///
/// <para>Why it is worth a test of its own. The machine's <c>mutate</c> calls
/// <c>on_changed</c> UNCONDITIONALLY — it never compares old and new — and windows
/// repaints an <c>x:Bind</c> page by calling <c>Bindings.Update()</c> on every
/// observer tick. Put those two together behind a TwoWay binding with no guard in
/// the setter and the page drives itself: the update re-reads the getter, WinUI
/// writes the control's value, the binding pushes that same value back into the
/// setter, the machine mutates, the observer fires, and the next full-page update
/// is queued. Nothing stops it.</para>
///
/// <para>What that cost. On <c>vps_config</c> it ran at ~300 full-page
/// re-evaluations per second at 100% of the UI thread, sustained for 52
/// consecutive 2 s windows of a 192 s run. The visible symptom was not slowness —
/// the page looks fine to a human — but that UI Automation could not resolve the
/// app's window at all (<c>ElementFromHandle</c> timing out after 22 s), because
/// UIA is served by the same thread. It cost several sessions, and two of them
/// diagnosed the wrong subsystem.</para>
///
/// <para>`SelectedVpsLocationId` is the only TwoWay binding in the onboarding views
/// today, so this is currently a test of one property — but the rule is the class,
/// and any new TwoWay binding onto this machine needs the same guard and belongs
/// here.</para>
/// </summary>
public class OnboardingSelectionFeedbackTests
{
    /// <summary>Counts observer notifications, and re-raises as INPC exactly like
    /// the real windows <c>NotifyObserver</c> does, so the VM's own
    /// PropertyChanged bridge is exercised too.</summary>
    private sealed class CountingObserver : OnboardingObserver, INotifyPropertyChanged
    {
        public int Changes;
        public event PropertyChangedEventHandler? PropertyChanged;

        public void OnChanged()
        {
            Changes++;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
        }
    }

    private static (OnboardingViewModel Vm, CountingObserver Observer) NewVm()
    {
        var observer = new CountingObserver();
        return (new OnboardingViewModel(
            observer, new FakeAccountRegistry()), observer);
    }

    [Fact]
    public void SelectingADifferentLocation_Notifies()
    {
        var (vm, observer) = NewVm();

        var before = observer.Changes;
        vm.SelectedVpsLocationId = "fsn1";

        Assert.Equal("fsn1", vm.SelectedVpsLocationId);
        Assert.True(observer.Changes > before,
            "selecting a NEW location must reach the machine and notify");
    }

    /// <summary>The loop-breaker. A TwoWay binding writes back the value it was
    /// just given on every single page update, so this path runs constantly — and
    /// each notification it produces schedules the update that produces the next
    /// one.</summary>
    [Fact]
    public void ReWritingTheCurrentLocation_DoesNotNotify()
    {
        var (vm, observer) = NewVm();
        vm.SelectedVpsLocationId = "fsn1";

        var afterFirst = observer.Changes;
        // Twenty write-backs of the SAME value stands in for twenty page updates.
        for (var i = 0; i < 20; i++)
            vm.SelectedVpsLocationId = "fsn1";

        Assert.Equal(afterFirst, observer.Changes);
    }

    [Fact]
    public void ANullWriteBack_IsIgnored_AndDoesNotClearTheSelection()
    {
        var (vm, observer) = NewVm();
        vm.SelectedVpsLocationId = "fsn1";
        var afterFirst = observer.Changes;

        // WinUI hands a picker's SelectedValue back as null while its item source
        // is being rebuilt; that must never be mistaken for the user choosing
        // "nothing".
        vm.SelectedVpsLocationId = null;

        Assert.Equal("fsn1", vm.SelectedVpsLocationId);
        Assert.Equal(afterFirst, observer.Changes);
    }
}
