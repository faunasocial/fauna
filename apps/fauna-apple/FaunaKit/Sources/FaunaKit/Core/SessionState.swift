import Foundation

@Observable
public class SessionState {
    /// Convention 11's explicit agent override — the value an
    /// `set_state({"session": {"authenticated": …}})` patch injected. While
    /// non-`nil` it WINS over the derived fact below (that is what keeps an
    /// agent-driven login honest when its shell hand-off is deferred); it is
    /// cleared by `reset`/`logout` — `clearAuthenticatedOverride()` — and by
    /// nothing else. The direct peer of linux's `authenticated_override`
    /// (`apps/fauna-linux/src/main.rs`).
    public var authenticatedOverride: Bool?

    /// The MOUNTED-SHELL probe: "is this app showing its authenticated main
    /// shell **right now**?" Installed ONCE per app at construction — iOS in
    /// `AppState.init()` (both live in FaunaKit), macOS in `MacAppState.init()`
    /// — and read LIVE on every access, so it cannot go stale. `nil` (no app
    /// has installed one, e.g. a bare `SessionState()` in a unit test) reads as
    /// "not mounted".
    ///
    /// Deliberately a probe over the app's OWN root-branch expression rather
    /// than a `Bool` some teardown path has to remember to clear: that assigned
    /// shape is the bug convention 11 exists to name, and apple carried it
    /// across 7-8 clear sites per target until 2026-08-22.
    @ObservationIgnored
    public var mainShellMountedProbe: (() -> Bool)?

    /// DERIVED, never assigned — `e2e-conventions.md` § convention 11 →
    /// *What `session.authenticated` means*: "the authenticated main app is
    /// MOUNTED", never "credentials exist". Mirrors the reference
    /// implementation `authenticated_override.unwrap_or(app_state.is_some())`.
    ///
    /// ⚠ Read-only on purpose. It used to be a stored `var` that
    /// `OnboardingVM.performLoggedInHandoff` set `true` **synchronously**, one
    /// async hop before the launch actually mounted the shell — so the flag was
    /// readable as `true` while the wizard's own handle-entry page was still on
    /// screen, and `reached_authenticated_app` returned into onboarding. That is
    /// the silent-green class; `test_smoke_k_real_onboarding_completion_reaches_the_main_app`
    /// caught it deterministically on macOS.
    public var isAuthenticated: Bool {
        authenticatedOverride ?? (mainShellMountedProbe?() ?? false)
    }

    /// Drop the agent override so the flag falls back to the live mounted-shell
    /// fact. The ONLY sanctioned way to un-set it (convention 11: cleared by
    /// `reset`/`logout`, and by nothing else) — every apple teardown path that
    /// used to write `isAuthenticated = false` calls this instead.
    public func clearAuthenticatedOverride() {
        authenticatedOverride = nil
    }

    public var secretHex: String?
    public var actorId: String?
    public var nodeUrl: String?
    public var deviceId: String?
    public var handle: String?

    /// First-setup mail / CalDAV enable latch (onboarding.md § Enable-email at
    /// claim + § Enable-CalDAV at claim). The wizard runs **pre-identity**, so
    /// the `onboarding-enable-email-checkbox` / `onboarding-enable-caldav-checkbox`
    /// record intent only; `OnboardingVM.performLoggedInHandoff` latches the two
    /// `email_enable_requested()` / `caldav_enable_requested()` intents here at the
    /// fresh-onboarding `.feed` exit, and the post-auth launch glue
    /// (`MailEnableGlue`) consumes them once. `pendingFirstSetupMail == nil` means
    /// "not a fresh onboarding" — a returning-user relaunch builds a new
    /// `SessionState`, so the latch is unset and the glue no-ops (the once-only /
    /// opt-out guarantee: `disableMail` clears the MSEK, but the latch stays unset,
    /// so opt-out is never re-minted on the next launch). Mirrors android
    /// `OnboardingHost.pendingFirstSetupMail: Boolean?` + linux's `enable_email` /
    /// `enable_caldav` capture at the LoggedIn outcome.
    public var pendingFirstSetupMail: Bool?
    public var pendingCaldavEnable: Bool = false
    /// Sibling of `pendingCaldavEnable` for **contacts (CardDAV)** — CardDAV gates
    /// independently of email/calendar (`carddav-server.md` § Independent enablement),
    /// so it latches and fires on its own. Unlike WebDAV it DOES need mailbox
    /// provisioning: on a contacts-only deployment (email + CalDAV both off) the
    /// consuming glue also mints the admin's shared MSEK.
    public var pendingCarddavEnable: Bool = false
    /// Sibling of `pendingCaldavEnable` for **files (WebDAV)** — WebDAV gates
    /// independently of email/calendar/contacts (`webdav-server.md` § Independent
    /// enablement pt 1) and needs no mailbox provisioning, so it latches and fires on its
    /// own. Harmless-on: nothing is served until the user flags a folder.
    public var pendingWebdavEnable: Bool = false

    /// The one-tap "trust this box" offer's answer latch (onboarding.md
    /// § 3b-ter). The wizard runs **pre-identity**, so its `trust-box-grant-button`
    /// / `trust-box-skip-button` only latch an answer
    /// (`grant_default_trust()` / `skip_trust_prompt()`);
    /// `OnboardingVM.performLoggedInHandoff` reads the machine's
    /// `take_trust_prompt_granted()` (consume-once) at the fresh-onboarding
    /// `.feed` exit, and the post-auth launch glue
    /// (`MailEnableGlue.applyPendingTrustPromptGrant`) mints the default
    /// capability-grant set once. `false` on a returning-user relaunch (a
    /// fresh `SessionState` never reached this step) — indistinguishable
    /// from an explicit skip, both mean "nothing to mint". Sibling of
    /// `pendingCaldavEnable`.
    public var pendingTrustPromptGranted: Bool = false

    /// The recovery kit the user confirmed on the onboarding `recovery_kit`
    /// screen (`onboarding.md` § 1 Identity) — minted there but deliberately
    /// unregistered, because no nest exists at that position.
    /// `OnboardingVM.performLoggedInHandoff` latches the machine's
    /// `takePendingRecoverySecret()` (consume-once; `nil` on skip) and the
    /// post-auth launch glue (`MailEnableGlue.registerPendingRecoveryKit`)
    /// registers it once. `nil` on every returning-user relaunch. Sibling of
    /// `pendingTrustPromptGranted`.
    public var pendingRecoveryKitHex: String?

    /// User-visible "off-box recovery isn't protected" warning, set by the launch
    /// glue when deployment-seed custody is **not confirmed** (BR-2 `RefusedMismatch`
    /// or the durable transient retries exhausted). The seed's sole hand-off is
    /// the single-use claim, so a swallowed failure would leave the admin believing
    /// recovery is protected until total box loss — hence a banner, not just a log
    /// (the apple idiom of linux's `ActionResult::Failed` toast). Rendered by the
    /// shared `RecoveryCustodyBanner` at each app's root; `nil` ⇒ nothing shown
    /// (custodied, idempotent re-claim, or expected multi-nest no-op). Tap-to-dismiss
    /// clears it.
    public var recoveryCustodyWarning: String?

    /// What a sign-out's erase could not remove — set by `StatusVM.signOut`
    /// when either half of the erase left something (`account-scoping.md`
    /// § Erasure follows scope → *the residue surface*), `nil` on a clean one.
    /// Consumed once by the app root (the `onSignOut` hook, or the
    /// unreadable-index start-over), which moves it onto
    /// `OnboardingVM.signOutResidue` and clears it here before the wizard
    /// mounts. Twin of android's `AppState.signOutResidue`.
    public var signOutResidue: (any SignOutResidueSurfacing)?

    public init() {}
}
