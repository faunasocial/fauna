namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One row of the nest_provisioning page's four-step progress. Built
/// fresh by <see cref="OnboardingViewModel.ProvisioningSteps"/> on each
/// observer tick from the corresponding <c>StepSnapshot</c> entry. No
/// mutation, no INotifyPropertyChanged — the parent VM replaces the
/// list on tick, ItemsControl rebinds.
///
/// Field meanings mirror
/// apps/fauna-linux/src/views/onboarding/nest_provisioning.rs's
/// per-row refresh closure.
/// </summary>
public sealed class ProvisioningStepItemViewModel
{
    /// <summary>ASCII glyph for the status icon (e.g. ✓, ⟳, ○).</summary>
    public string Glyph { get; init; } = "";

    /// <summary>Translated step name (Domain / Server / DNS / Online).</summary>
    public string Label { get; init; } = "";

    /// <summary>Sub-step text + optional "(attempt N of M)" suffix.</summary>
    public string Substep { get; init; } = "";

    /// <summary>True iff the row should render the substep TextBlock.</summary>
    public bool SubstepVisible { get; init; }

    /// <summary>Last error string for a Failed step. Empty otherwise.</summary>
    public string Error { get; init; } = "";

    /// <summary>True iff the row should render the error TextBlock.</summary>
    public bool ErrorVisible { get; init; }
}
