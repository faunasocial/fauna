using System;
using System.IO;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Windows.Security.Credentials.UI;

namespace FaunaApp.Services;

/// <summary>
/// The Stage-2 native re-auth gate — Windows Hello (<c>UserConsentVerifier</c>). The windows
/// twin of apple's <c>FaunaKit/Core/AccountReauth.swift</c>: consulted before activating an
/// account whose <c>require_confirm_to_activate</c> flag is set (long-term-store.md
/// § Multi-account evolution → Per-account re-auth), wired into
/// <see cref="App.ConfirmReauthHandler"/> and reached by the switcher VM through its injected
/// <c>ConfirmReauth</c> seam.
///
/// <para>It lives in the app layer, not <c>FaunaApp.Core</c>, because
/// <c>Windows.Security.Credentials.UI</c> is a WinRT surface unreachable from Core's
/// plain-<c>net10.0</c> TFM. The VM owns the testable branch (read the flag fresh → maybe
/// gate → request the switch confirmed/unconfirmed); this static owns only the prompt.</para>
///
/// <para><b>Fail-closed, three ways</b> (matching apple): an absent/unreadable e2e verdict, an
/// unavailable verifier, and any thrown error all resolve to <c>false</c> = decline. A device
/// with no Hello/PIN enrolled therefore declines activation — the user can still turn the flag
/// OFF from the account's own switcher row, which never prompts.</para>
/// </summary>
internal static class AccountReauth
{
    /// <summary>
    /// Prompt to confirm activating a re-auth-flagged account. Returns <c>true</c> only on an
    /// explicit approval; <c>false</c> on decline / cancel / unavailable / error.
    /// </summary>
    /// <param name="window">The app window, reserved as the consent-dialog owner for the
    /// HWND-interop path a future refinement may add; unused on the simple projection and
    /// ignored entirely under the e2e file seam.</param>
    public static async Task<bool> ConfirmActivationAsync(Window? window = null)
    {
        // E2E seam FIRST (mirrors apple's AccountReauth): when FAUNA_E2E_CREDENTIAL_DIR is
        // set, read {cred_dir}/reauth-result instead of showing the real Hello prompt — an
        // OS dialog carries no test ID (ui.yaml: apple/windows/android "never render"
        // account-activate-reauth-prompt). Read per prompt, never cached, so one app session
        // exercises both the decline and the approve arm.
        // Via E2eEnv so the seam is compiled out of release builds (convention 15).
        // This is the sharpest read in the app: ungated, setting the variable and
        // writing "approve" into {dir}/reauth-result REPLACES the Windows Hello
        // verification below with a file read — a re-auth bypass in a shipped MSI.
        var credDir = Core.Services.E2eEnv.CredentialDir;
        if (!string.IsNullOrEmpty(credDir))
        {
            try
            {
                var verdict = File.ReadAllText(Path.Combine(credDir, "reauth-result")).Trim();
                // Literal "approve" (post-trim) is the ONLY approving value; anything else
                // is a decline.
                return verdict == "approve";
            }
            catch
            {
                // Absent / unreadable → decline (fail-closed). This is the strictest arm of
                // the seam and the one the decline journey drives: no file at all.
                return false;
            }
        }

        // Real gate: Windows Hello. Pre-check availability (apple's canEvaluatePolicy analog),
        // then request verification. Every non-Verified outcome — declined, cancelled,
        // retries-exhausted, device-not-present — is a decline.
        try
        {
            var availability = await UserConsentVerifier.CheckAvailabilityAsync();
            if (availability != UserConsentVerifierAvailability.Available)
            {
                return false;
            }

            var reason = Core.Services.Strings.Get("settings/account_page/reauth_reason");
            var result = await UserConsentVerifier.RequestVerificationAsync(reason);
            return result == UserConsentVerificationResult.Verified;
        }
        catch
        {
            return false;
        }
    }
}
