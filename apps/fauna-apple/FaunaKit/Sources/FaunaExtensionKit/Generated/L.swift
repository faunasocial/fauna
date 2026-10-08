// AUTO-GENERATED from i18n/strings/en.yaml — do not edit
// swiftlint:disable all
// swiftformat:disable all

public enum L {
    public enum common {
        public static let cancel = "Cancel"

        public static let save = "Save"

        public static let delete = "Delete"

        public static let accepted = "Accepted"

        public static let pending = "Pending"

        public static let confirmed = "Confirmed"

        public static let blocked = "Blocked"

        public static let unknown = "Unknown"

        public static let loading = "Loading..."

        public static let stillLoading = "Still loading — try again in a moment"

        public static let retry = "Retry"

        public static let check = "Check"

        public static let verify = "Verify"

        public static let enable = "Enable"

        public static let disable = "Disable"

        public static let loadFailed = "Failed to load"

        public static let notFound = "Not found"

        public static let search = "Search"

        public static let clearSearch = "Clear search"

        public static let sort = "Sort"

        public static let create = "Create"

        public static let edit = "Edit"

        public static let close = "Close"

        public static let dialogAlreadyOpen = "Close the open dialog first."

        public static let quit = "Quit"

        public static let exitFauna = "Exit Fauna"

        public static let appName = "Fauna"

        public static let back = "Back"

        public static let next = "Next"

        public static let done = "Done"

        public static let no = "No"

        public static let ok = "OK"

        public static let error = "Error"

        public static let refresh = "Refresh"

        public static let settings = "Settings"

        public static let confirm = "Confirm"

        public static let confirmQ = "Confirm?"

        public static let send = "Send"

        public static let sending = "Sending..."

        public static let dismiss = "Dismiss"

        public static let remove = "Remove"

        public static let follow = "Follow"

        public static let unfollow = "Unfollow"

        public static let copy = "Copy"

        public static let copied = "Copied!"

        public static let archive = "Archive"

        public static let today = "Today"

        public static let upcoming = "Upcoming"

        public static let post = "Post"

        public static let mute = "Mute"

        public static let add = "Add"

        public static let change = "Change"

        public static let apply = "Apply"

        public static let reply = "Reply"

        public static let replyAll = "Reply All"

        public static let replyingTo = "Replying to"

        public static let name = "Name"

        public static let mode = "Mode"

        public static let value = "Value"

        public static let `type` = "Type"

        public static let verified = "Verified"

        public static let signInRequired = "Sign in to access this feature."

        public static let identityRequired = "Set up your identity in the Status tab first."

        public static let enabled = "Enabled"

        public static let disabled = "Disabled"

        public static let creating = "Creating..."

        public static let saving = "Saving..."

        public static let saved = "Saved!"

        public static let previous = "Previous"

        public static let prune = "Prune"

        public static let `continue` = "Continue"

        public static let thisNest = "this nest"

        public static let exportAction = "Export"

        public static let accept = "Accept"

        public static let decline = "Decline"

        public static let leave = "Leave"

        public static let block = "Block"

        public static let connect = "Connect"

        public static let connected = "Connected"

        public static let connectedRealtime = "Connected (real-time)"

        public static let disconnected = "Disconnected"

        public static let cannotConnect = "Cannot connect"

        public static let needsNest = "Needs a connection to your nest"

        public static let needsOtherDevice = "Waiting for another of your devices: open the app on it, or remove it under Devices if it's gone"

        public static let inactive = "Inactive"

        public static let active = "Active"

        public static let starting = "Starting..."

        public static let linking = "Linking..."

        public static let unlinking = "Unlinking..."

        public static let linked = "Linked"

        public static let notLinked = "Not linked"

        public static let notConnected = "Not connected"

        public static let refreshing = "Refreshing..."

        public static let checking = "Checking..."

        public static let verifying = "Verifying..."

        public static let actorId = "Actor ID"

        public static let deviceId = "Device ID"

        public static let dangerZone = "Danger Zone"

        public static let nodeUrl = "Node URL"

        public static let noMessagesYet = "No messages yet."

        public static func fmtQuestion(text: String) -> String {
            "\(text)?"
        }

        public static func fmtExclamation(text: String) -> String {
            "\(text)!"
        }

        public static func fmtEllipsis(text: String) -> String {
            "\(text)..."
        }

        public static let available = "Available"

        public static let searching = "Searching..."

        public static let domain = "Domain"

        public static let provider = "Provider"

        public static let connecting = "Connecting..."

        public static let feed = "Feed"

        public static let posts = "Posts"

        public static let loadMore = "Load More"

        public static let messages = "Messages"

        public static let size = "Size"

        public static let contacts = "Contacts"

        public static let `open` = "Open"

        public static let closed = "Closed"

        public static let admin = "Admin"

        public static let bridges = "Bridges"

        public static let moderation = "Moderation"

        public static let navigation = "Navigation"

        public static let more = "More"

        public static let notifications = "Notifications"

        public static let noNotifications = "No notifications"

        public static let markAllRead = "Mark All Read"

        public static func unreadCount(count: String) -> String {
            "\(count) unread"
        }

        public static let identity = "Identity"

        public static let files = "Files"

        public static let path = "Path"

        public static let status = "Status"

        public static let noFoldersConfigured = "No folders configured."

        public static let never = "Never"

        public static let retentionPolicy = "Retention Policy"

        public static let snapshots = "Snapshots"

        public static let devices = "Devices"

        public static let deleteFolder = "Delete Folder"

        public static let sync = "Sync"

        public static let peers = "Peers"

        public static let account = "Account"

        public static let actions = "Actions"

        public static let storage = "Storage"

        public static let tier = "Tier"

        public static let savePreferences = "Save Preferences"

        public static let users = "Users"

        public static let usersByTier = "Users by Tier"

        public static let inbox = "Inbox"

        public static let handle = "Handle"

        public static let download = "Download"

        public static let toggleSidebar = "Toggle sidebar"

        public static func successDetail(detail: String) -> String {
            "Success: \(detail)"
        }
    }

    public enum time {
        public static let justNow = "just now"

        public static func minutesAgo(count: String) -> String {
            "\(count)m ago"
        }

        public static func hoursAgo(count: String) -> String {
            "\(count)h ago"
        }

        public static func daysAgo(count: String) -> String {
            "\(count)d ago"
        }

        public static let yesterday = "Yesterday"

        public static let weekdayMon = "Mon"

        public static let weekdayTue = "Tue"

        public static let weekdayWed = "Wed"

        public static let weekdayThu = "Thu"

        public static let weekdayFri = "Fri"

        public static let weekdaySat = "Sat"

        public static let weekdaySun = "Sun"

        public static let weekdayFullMon = "Monday"

        public static let weekdayFullTue = "Tuesday"

        public static let weekdayFullWed = "Wednesday"

        public static let weekdayFullThu = "Thursday"

        public static let weekdayFullFri = "Friday"

        public static let weekdayFullSat = "Saturday"

        public static let weekdayFullSun = "Sunday"

        public static let monthJan = "Jan"

        public static let monthFeb = "Feb"

        public static let monthMar = "Mar"

        public static let monthApr = "Apr"

        public static let monthMay = "May"

        public static let monthJun = "Jun"

        public static let monthJul = "Jul"

        public static let monthAug = "Aug"

        public static let monthSep = "Sep"

        public static let monthOct = "Oct"

        public static let monthNov = "Nov"

        public static let monthDec = "Dec"

        public static let monthFullJan = "January"

        public static let monthFullFeb = "February"

        public static let monthFullMar = "March"

        public static let monthFullApr = "April"

        public static let monthFullMay = "May"

        public static let monthFullJun = "June"

        public static let monthFullJul = "July"

        public static let monthFullAug = "August"

        public static let monthFullSep = "September"

        public static let monthFullOct = "October"

        public static let monthFullNov = "November"

        public static let monthFullDec = "December"

        public static func uptimeDhm(days: String, hours: String, mins: String) -> String {
            "\(days)d \(hours)h \(mins)m"
        }

        public static func uptimeHm(hours: String, mins: String) -> String {
            "\(hours)h \(mins)m"
        }

        public static func uptimeM(mins: String) -> String {
            "\(mins)m"
        }

        public static func countdownDh(days: String, hours: String) -> String {
            "\(days)d \(hours)h"
        }

        public static func countdownH(hours: String) -> String {
            "\(hours)h"
        }
    }

    public enum size {
        public static func bytes(value: String) -> String {
            "\(value) B"
        }

        public static func kb(value: String) -> String {
            "\(value) KB"
        }

        public static func mb(value: String) -> String {
            "\(value) MB"
        }

        public static func gb(value: String) -> String {
            "\(value) GB"
        }

        public static func tb(value: String) -> String {
            "\(value) TB"
        }
    }

    public enum tips {
        public static func sats(value: String) -> String {
            "\(value) sats"
        }

        public static func msats(value: String) -> String {
            "\(value) msats"
        }

        public static func count(count: String) -> String {
            "\(count) tips"
        }

        public static let countOne = "1 tip"

        public static let listTitle = "Tips"

        public static let listOpen = "Who tipped"

        public static let senderUnknown = "Someone"

        public static let amountUnknown = "Amount not reported"

        public static func more(count: String) -> String {
            "and \(count) more"
        }
    }

    public enum onboarding {
        public enum welcome {
            public static let title = "Fauna"

            public static let subtitle = "Your personal, private communications nest."

            public static let tagline = "Private messaging and file sync,\nbuilt on your own server."
        }

        public enum launch {
            public static let transientError = "Couldn't reach your nest. Check your connection and try again."

            public static let useDifferentNest = "Use a different nest"

            public static let launchFailed = "Startup hit a problem. Your data is safe — you can retry, or continue setup below."

            public static let identityChangedWarning = "This nest's identity has changed, or it can no longer prove the identity you previously trusted. It may have been re-deployed or had its key rotated — or someone may be impersonating it. Don't continue unless you were expecting this change."

            public static let identityChangedTrust = "Trust this nest and continue"

            public static let identitySuperseded = "This identity was succeeded — import the new identity to continue. Your account now belongs to a new identity, and this one can no longer sign in."

            public static func identitySupersededVerified(successor: String) -> String {
                "This identity was succeeded — import the new identity to continue. Your account now belongs to \(successor)."
            }

            public static let recoverLostBox = "Recover a lost box"

            public static let retireServer = "Retire a server"

            public static let indexNewerBuild = "Your accounts were saved by a newer version of this app, so this version can't read them. Nothing has been lost — update the app and they'll be here."

            public static let indexMalformed = "Your saved accounts can't be read, and updating the app won't help. Nothing has been changed or deleted. You can start over on this device, or move this device's data somewhere safe first."

            public static let indexMalformedResetResidual = "Starting over removes every account from this device and returns the app to a fresh install. Your accounts still exist on your nest, but you will need your secret key or recovery kit to sign back in — this device's copy is removed. Some saved data on this device cannot be reached to clear it."

            public static let indexNewerBuildTitle = "Update needed"

            public static let indexMalformedTitle = "Your saved accounts can't be read"

            public static let indexMalformedReset = "Start over on this device"

            public static let indexMalformedResetConfirm = "Remove everything and start over"

            public static let indexMalformedResetBlockedOtherWindow = "Nothing was removed — another Fauna window is using an account on this device. Close it, then start over again."

            public static let signInRefused = "This nest no longer signs you in. Its admin may have suspended or removed your account — nothing on this device has changed. Contact the admin; if they restore your account, try again."

            public static let signInRefusedTitle = "Can't sign in here"

            public static let accountLocked = "This account is locked and nobody can sign in until the lock ends."

            public static func accountLockedUntil(time: String) -> String {
                "The lock ends \(time)."
            }

            public static let accountLockedNotYours = "If you did not lock it, somebody else holds your secret key, and they can lock it again. Your recovery kit moves the account to a new key they do not have — the lock does not stop that."

            public static let accountLockedTitle = "Account locked"
        }

        public enum instanceChooser {
            public static let title = "Fauna is already open"

            public static func subtitle(account: String) -> String {
                "This window can't open \(account) — it's already running. Choose another account, or switch to the open window."
            }

            public static let chooseAccount = "Open a different account"

            public static let noneAvailable = "Every account you've added is already open in another window."

            public static let focusExisting = "Switch to the open window"

            public static let addAccount = "Log in as a new user"

            public static let accountTaken = "That account was just opened in another window. Pick another."

            public static let noRunningInstance = "Couldn't switch to the window running this account. Close it and launch Fauna again."
        }

        public enum identityChoice {
            public static let title = "Set Up Your Identity"

            public static let subtitle = "Your identity is an encryption key that belongs only to you."

            public static let createNew = "Create New Identity"

            public static let importExisting = "Import from Another Device"

            public static let recoverLostBox = "Recover a lost box"

            public static let restoreFromRecoveryKit = "Restore my account from a recovery phrase"
        }

        public enum addAccount {
            public static let cancel = "Cancel"
        }

        public enum identityCreated {
            public static let title = "Identity Created"

            public static let desc = "Your new identity has been generated. Save this secret key — it's your only way to recover your account."

            public static let secretKeyLabel = "Your secret key:"

            public static let warning = "Write this down or save it in a password manager. If you lose it, your account cannot be recovered."

            public static let `continue` = "Continue"

            public static let notGenerated = "Identity not generated yet"
        }

        public enum recoveryKit {
            public static let title = "Your Recovery Kit"

            public static let desc = "This recovery phrase outranks your secret key — it is the only way to get your account back if your secret key is ever stolen or lost. Keep it offline; it is shown exactly once and never stored on any device."

            public static let escrowDeferred = "Your kit activates when your account comes online at the end of setup — until then, keep the phrase safe."

            public static let confirm = "I've saved it — continue"

            public static let skip = "Skip for now"

            public static let notMinted = "Recovery kit not minted yet"
        }

        public enum recoveryEntry {
            public static let title = "Restore from Recovery Kit"

            public static let phraseLabel = "Recovery phrase"

            public static let desc = "Paste your recovery phrase (the fauna://recovery link or the 64-character code). Your identity will be restored from your nest's sealed escrow."

            public static let accountHint = "If your phrase doesn't name your account, enter your handle (user@domain) so your nest can be found. If that domain is gone, put your nest's own address after the @ instead — like alice@192.0.2.10 or alice@nest.local."

            public static let submit = "Restore"

            public static let invalidKit = "That isn't a recovery phrase. Paste the fauna://recovery link or the 64-character recovery code — not your secret key."

            public static let accountNeeded = "Enter the handle of the account you're restoring (user@domain) — this phrase doesn't say which account it belongs to."

            public static let accountMalformed = "Enter the full handle including the domain, like alice@fauna.social — or, if that domain is gone, your handle followed by your nest's address, like alice@192.0.2.10."

            public static func accountUnknown(account: String) -> String {
                "No account named \(account) exists on that nest."
            }

            public static let noEscrow = "This account has no sealed backup to restore from. Create a new recovery kit from a device that's still signed in."

            public static let superseded = "This identity was replaced after it was compromised. Import the new identity to continue."

            public static func refused(reason: String) -> String {
                "That recovery phrase was refused — it may have been replaced by a newer kit, or belong to another account. (\(reason))"
            }

            public static func unreachable(reason: String) -> String {
                "Could not reach that account's nest. Check the handle and your connection, then try again. If the domain itself is gone, enter your handle with your nest's address instead, like alice@192.0.2.10. (\(reason))"
            }

            public static func restoredPredecessorsLost(reason: String) -> String {
                "Your account was restored. But part of the sealed backup — material from a previous identity of yours — could not be opened, so content still encrypted under that older identity may be unreadable. If another of your devices still has that identity, sign in there to finish moving your content over. (\(reason))"
            }
        }

        public enum identityImport {
            public static let title = "Import Identity"

            public static let scanSubtitle = "Scan the QR code shown on your other device."

            public static let pasteSubtitle = "Paste the secret key from your other device."

            public static let pasteLabel = "Secret key"

            public static let pastePlaceholder = "64-character hex secret key"

            public static let `import` = "Import"

            public static let scanTab = "Scan QR Code"

            public static let pasteTab = "Paste Secret Key"

            public static let invalidQr = "Not a valid Fauna identity QR code."

            public static let invalidSecret = "Secret key must be 64 hex characters."

            public static let cameraUnavailable = "Camera Not Available"

            public static let cameraUnavailableHint = "Use the paste tab instead."
        }

        public enum recovery {
            public static let title = "Recover a lost box"

            public static let subtitle = "Choose the box you lost. Fauna re-provisions a fresh box with its saved identity, so your pinned devices reconnect automatically once it's back online."

            public static let boxListLabel = "Your boxes"

            public static let boxItemHint = "Recovery key custodied"

            public static let emptyMessage = "No recovery keys are available yet. Connect to one of your other nests first — its synced configuration holds the recovery keys for every box you administer."

            public static let methodCloud = "Re-provision on a cloud host"

            public static let methodSelfhosted = "Install on my own server"

            public static let selfhostedTitle = "Install on your own server"

            public static let selfhostedDesc = "Run the installer below on a fresh box. It carries your saved deployment identity, so the rebuilt box re-presents the same nest identity and your pinned devices reconnect automatically once its DNS is re-pointed."

            public static let selfhostedCommandPending = "The installer command with your recovery seed will appear here."

            public static let selfhostedContinue = "Done"

            public static let restoreCta = "Restore your data"
        }

        public enum retire {
            public static let title = "Retire a server"

            public static let subtitle = "Delete a server Fauna created in your cloud account and clean up the DNS records that pointed at it, or get your domain's transfer code. Your cloud token is used for this visit only and is never saved."

            public static let listLabel = "Servers Fauna created in this account"

            public static let emptyMessage = "No servers created by Fauna were found in this account. Servers set up some other way carry no Fauna marker and are not listed — retire those from your provider's dashboard."

            public static let domainLabel = "Domain"

            public static let secondaryDomainsLabel = "Also serves"

            public static let addressLabel = "Address"

            public static let currentBadge = "The server you're signed into"

            public static let unmarkedNote = "Not marked as created by Fauna — check the name carefully before deleting it"

            public static let transferCodeButton = "Get the domain transfer code"

            public static let transferCodeFetching = "Asking the registrar…"

            public static let transferCodeLabel = "Transfer code"

            public static let transferCodeCopy = "Copy code"

            public static func transferCodeAvailableAfter(when: String) -> String {
                "The registry locks this domain against transfer until \(when). Ask again after that — the code is never refused, only delayed."
            }

            public static let transferCodeFromRegistrar = "Get this domain's transfer code from your registrar's own dashboard."

            public static let deleteButton = "Delete this server…"

            public static func confirmWhat(name: String, provider: String, address: String) -> String {
                "You are about to permanently delete \(name) at \(provider) (\(address))."
            }

            public static func confirmDomain(domain: String) -> String {
                "Domain: \(domain)"
            }

            public static func confirmSecondaryDomains(domains: String) -> String {
                "Also serves: \(domains)"
            }

            public static let confirmDestroyed = "Everything on this server — its disk and every account's data on it — is destroyed and cannot be recovered except from a backup."

            public static let confirmCurrent = "This is the server you're signed into. This session's nest will stop existing."

            public static func confirmDnsRemovals(records: String) -> String {
                "Before the server is deleted, these DNS records pointing at it are removed: \(records)"
            }

            public static let confirmDnsNone = "No DNS records will be removed automatically. The records to remove by hand are listed below."

            public static func confirmByHand(records: String) -> String {
                "To remove by hand afterwards: \(records)"
            }

            public static let confirmNameLabel = "Type the server's name to confirm"

            public static let confirmButton = "Delete the server and clean up DNS"

            public static let cancelButton = "Cancel"

            public static let stepDns = "DNS records"

            public static let stepServer = "Server"

            public static let stepSkippedNoVerifiedDomain = "No domain was verified for this server, so no DNS records were touched."

            public static let stepSkippedNoDnsCredential = "No DNS credential reaches this domain's zone, so nothing was removed automatically — see the list to remove by hand."

            public static let stepSkippedNoZoneForDomain = "None of your DNS credentials holds this domain's zone, so nothing was removed automatically — see the list to remove by hand."

            public static let stepSkippedDnsStepForcedPast = "Skipped — the server was deleted despite the failed DNS step."

            public static let retryButton = "Try again"

            public static let forceServerButton = "Delete the server anyway"

            public static let forceServerWarning = "Deleting the server now leaves DNS records pointing at an address your provider can hand to someone else. Remove them by hand right away — they are listed once the server is gone."

            public static func doneDeleted(name: String) -> String {
                "\(name) has been deleted."
            }

            public static func leftoverPointsAtBox(record: String) -> String {
                "Remove now — still points at the deleted server: \(record)"
            }

            public static func leftoverSharedName(record: String) -> String {
                "Stale, remove when convenient: \(record)"
            }

            public static let leftoverNone = "Nothing is left to remove by hand."

            public static let leftoverCopy = "Copy the list"

            public static let doneButton = "Done"
        }

        public enum inviteRequest {
            public static let title = "Request an invite"

            public static let subtitle = "Ask the admin of this nest to let you in."

            public static let handleLabel = "Your requested handle"

            public static let handlePlaceholder = "alice"

            public static let messageLabel = "Message to the admin (optional)"

            public static let messagePlaceholder = "Hi, I'd like to join this nest because..."

            public static let submit = "Send request"

            public static let submitting = "Sending..."

            public static let submitFailed = "Could not send the request. Please try again."

            public static let requestButton = "Request invite"

            public static let recheckButton = "Check again"

            public static let codeSectionTitle = "Have an invite code?"

            public static let codeLabel = "Invite code"

            public static let codePlaceholder = "Paste invite code"

            public static let statusIdle = "Ask for an invite above, or paste a code you already have, to continue."

            public static let oobIdle = "Paste an invite code, then press Check."

            public static let oobValid = "Code accepted"

            public static func oobInvalid(reason: String) -> String {
                "This nest did not accept that code: \(reason). Check it for typos and press Check again, or use Request invite above."
            }

            public static func oobError(cause: String) -> String {
                "Could not reach the nest to check this code, so it has not been rejected. Check your connection, then press Check again. (\(cause))"
            }
        }

        public enum provision {
            public static let complete = "Your nest is online!"

            public static let startOver = "Start over"

            public static let registeringDomain = "Registering domain..."

            public static let registeringDomainDetails = "Registering domain with registrar"

            public static let creatingServer = "Creating server..."

            public static let configuringDns = "Configuring DNS..."

            public static let fetchingDkim = "Fetching DKIM key..."

            public static let creatingDkim = "Creating DKIM record..."

            public static let settingUpVps = "Setting up VPS instance"

            public static let creatingDnsRecords = "Creating DNS records"

            public static let pollingHealth = "Polling for nest to come online"

            public static let retrievingDkim = "Retrieving email signing key"

            public static let addingDkimRecord = "Adding email DNS record"

            public static let provisioningComplete = "Provisioning complete"

            public static let time = "This usually takes 2-3 minutes."

            public static let server = "Provision Server"

            public enum step {
                public static let domain = "Domain"

                public static let server = "Server"

                public static let dns = "DNS"

                public static let online = "Online"
            }

            public static func stepFailed(step: String, cause: String) -> String {
                "Step \(step) failed: \(cause)"
            }

            public static func stepAttemptTemplate(attempt: String, maxAttempts: String) -> String {
                " (attempt \(attempt) of \(maxAttempts))"
            }

            public enum substep {
                public static let domainCheckingAvailability = "Checking domain availability"

                public static let domainRegistering = "Registering domain"

                public static let domainVerifyingZone = "Verifying DNS zone"

                public static let serverGeneratingDkim = "Generating DKIM keys"

                public static let serverCreating = "Creating server"

                public static let dnsAddingDomainRecords = "Adding domain records"

                public static let dnsAddingEmailRecords = "Adding email records"

                public static let dnsSettingReverseDns = "Setting reverse DNS"

                public static let onlineWaiting = "Waiting for your nest to start"

                public static let onlineClaiming = "Signing you in to your new nest"

                public static let statusSkipped = "Already configured — skipped"

                public static func statusRetrying(cause: String) -> String {
                    "Retrying after error: \(cause)"
                }

                public static let statusCancelling = "Cancelling…"

                public static let statusCancelled = "Cancelled"
            }
        }

        public enum complete {
            public static let title = "Your nest is ready!"

            public static let nestDetails = "Nest Details"
        }

        public enum bridges {
            public static let noLinkOptions = "No link options available."

            public static let linkMode = "Link mode"

            public static func notAvailableOnNest(name: String) -> String {
                "\(name) not available on this nest."
            }
        }

        public enum handle {
            public static let prompt = "Enter your handle"

            public static let examplesHelp = "Example: alice@example.com or alice@bsky.social. Format: user@domain."

            public static let localhostHint = "test@localhost is allowed for trying out the app."

            public static let controlCheckbox = "I control DNS for this domain"
        }

        public enum dnsConfig {
            public static let title = "Configure DNS"

            public static let buyDomainCheckbox = "Buy domain on Continue (this page)"

            public static let sameProviderCheckbox = "Buy VPS with same provider (next page)"

            public static let ineligibleNeedsRegistrar = "Can't register domains — untick the buy-domain box above to pick it."

            public static let ineligibleNeedsVps = "Doesn't sell VPS servers — untick the same-provider box above to pick it."

            public static let ineligibleNeedsRegistrarAndVps = "Can't register domains or sell VPS servers — untick both boxes above to pick it."

            public static let setUpLater = "Set up later"

            public static let setUpLaterWarning = "Your handle will not work until DNS is configured. We'll show instructions after VPS purchase."

            public static let statusPickProvider = "Choose where your domain's DNS lives to continue."

            public static let statusVerifyCredentials = "Enter this provider's credentials and press Verify to continue."

            public static func statusOwned(provider: String) -> String {
                "You own this domain at \(provider)."
            }

            public static func statusRegisteredElsewhere(provider: String) -> String {
                "This domain is already registered. Transfer it to \(provider) (or pick another provider) before continuing."
            }

            public static func statusBuyable(provider: String, price: String) -> String {
                "\(provider) will register this domain for \(price). Tick the confirm box and press Continue to buy."
            }

            public static func statusNotBuyable(provider: String) -> String {
                "\(provider) can't sell this domain. Buy it elsewhere first, then transfer it or use manual DNS."
            }

            public static let openInBrowser = "Open in browser"

            public static func noProviderCarriesTld(tld: String) -> String {
                "None of our supported registrars carry .\(tld) domains. You'll need to buy this domain elsewhere, then either transfer it to a supported registrar or set up DNS manually."
            }

            public static let contactFormHeading = "WHOIS registration contact"

            public enum contactFields {
                public static let firstName = "First name"

                public static let lastName = "Last name"

                public static let email = "Email"

                public static let phone = "Phone (E.164: +12025550100)"

                public static let address1 = "Address"

                public static let city = "City"

                public static let state = "State / region"

                public static let postalCode = "Postal code"

                public static let country = "Country (ISO 3166-1 alpha-2, e.g. US)"
            }
        }

        public enum vpsConfig {
            public static let title = "Configure VPS"

            public static let serverTypeRadioLegend = "Choose a VPS plan"

            public static let statusPickProvider = "Choose who hosts your server to continue."

            public static let statusVerifyCredentials = "Enter this provider's credentials and press Verify to continue."

            public static let statusPickLocation = "Choose where in the world your server runs to continue."

            public static let statusPickServerType = "Choose a plan for your server to continue."

            public static let locationHeading = "Location"

            public static let updateChannelHeading = "Updates"

            public static let updateChannelStableLabel = "Stable"

            public static let updateChannelStableDesc = "Released versions. Recommended."

            public static let updateChannelTestLabel = "Test"

            public static let updateChannelTestDesc = "Release candidates that are still being checked."

            public static let updateChannelDevLabel = "Dev"

            public static let updateChannelDevDesc = "The newest development builds, before any checking. Expect breakage."

            public static let mailModeLabel = "Run mail on this box"

            public static let mailModeDesc = "A mail box runs email (SMTP, IMAP) and calendar, which needs the spam and virus scanners and at least 2 GB of RAM. Turn this off for a social-only box: it runs lean and works on the cheapest 1 GB plan, but cannot add mail later without resizing the VPS."
        }

        public enum nestProvisioning {
            public static let title = "Setting up your nest…"

            public static let startButton = "Buy and set up"

            public static func elapsedTemplate(seconds: String) -> String {
                "\(seconds)s elapsed"
            }

            public static let cancelButton = "Cancel"

            public static let retryButton = "Retry"

            public static let continueBlockedIdle = "Set up your nest first — choose \"Buy and set up\" above."

            public static let continueBlockedRunning = "Continue unlocks once setup finishes."

            public static let continueBlockedFailed = "Setup did not finish — retry it to continue."

            public static let continueBlockedCancelled = "Setup was cancelled — retry it to continue."

            public static func bomLine(label: String, price: String) -> String {
                "\(label): \(price)"
            }

            public static func bomLineRecurring(label: String, price: String) -> String {
                "\(label): \(price)/month"
            }

            public static func bomLineDomain(label: String, price: String, renewal: String) -> String {
                "\(label): \(price) for the first year, then \(renewal)/year"
            }
        }

        public enum dnsPostInstructions {
            public static let title = "DNS setup instructions"

            public static let description = "Add these records at your DNS provider so your handle starts working."

            public static let copyButton = "Copy all"

            public static let recordsPending = "(no records yet — try again in a moment)"
        }

        public enum handleCheck {
            public static let idle = "Enter your handle, then press Check to continue."

            public enum phase {
                public static let parsing = "Checking format…"

                public static let dnsLookup = "Checking domain availability…"

                public static func nestProbe(domain: String) -> String {
                    "Looking for a nest at \(domain)…"
                }

                public static let challengeResponse = "Checking your account…"

                public static let priceLookup = "Looking up registration price…"
            }

            public enum outcome {
                public static let formatInvalid = "Handle format must be user@domain or user.domain (with localhost or IP also accepted)."

                public static func tldInvalid(tld: String) -> String {
                    "\(tld) is not a TLD that can be registered."
                }

                public static func domainAvailablePriced(domain: String, price: String) -> String {
                    "\(domain) is available — registration about \(price)."
                }

                public static func domainAvailableUnpriced(domain: String) -> String {
                    "\(domain) appears to be available for purchase."
                }

                public static func domainAvailableNotBuyableViaProvider(domain: String, tld: String) -> String {
                    "\(domain) appears to be available, but none of our supported registrars carry .\(tld). You'll need to buy it elsewhere."
                }

                public static func domainAvailableInsideZone(domain: String, zone: String) -> String {
                    "Nothing is set up at \(domain) yet. It sits inside \(zone): if you hold \(zone), continue and pick the DNS provider that hosts it. If not, you can register \(domain) on the next page."
                }

                public static func registeredNoNest(domain: String) -> String {
                    "\(domain) resolves but no nest is running. To set one up, confirm you control DNS for this domain."
                }

                public static func alreadyOnNestHandleMatches(handle: String) -> String {
                    "Welcome back, \(handle)."
                }

                public static func alreadyOnNestHandleDiffers(domain: String, oldHandle: String) -> String {
                    "You're already registered on \(domain) as \(oldHandle). Continue to log in as that handle (you can change it after)."
                }

                public static func userUnregistered(domain: String) -> String {
                    "There's a nest at \(domain) but you're not registered. Request an invite or paste a code below."
                }

                public static func unregisteredUnclaimedNest(domain: String) -> String {
                    "There's a nest at \(domain) but no one has claimed it yet. Continue to claim it as your own."
                }
            }

            public enum error {
                public static let noNetwork = "Couldn't reach the network. Check your connection and try again."

                public static func nestUnreachable(domain: String) -> String {
                    "\(domain) is registered but the nest didn't respond. Try again, or check the domain."
                }

                public static func nestMisbehaving(domain: String) -> String {
                    "\(domain) responded with an unexpected error. Try again later."
                }

                public static func nestProtocolMismatch(domain: String) -> String {
                    "\(domain) returned a malformed response. The nest version may be incompatible."
                }

                public static let challengeTemp = "The nest's challenge service is temporarily unavailable. Try again."

                public static let challengeFailed = "Couldn't verify your identity with the nest. This may indicate a key mismatch."

                public static func transient(cause: String) -> String {
                    "Temporary error: \(cause). Try again."
                }

                public static func terminal(cause: String) -> String {
                    "Error: \(cause)."
                }
            }
        }

        public enum invite {
            public static let idle = "Request an invite or paste a code below."

            public static let submitting = "Submitting request…"

            public static let rechecking = "Checking status…"

            public static let requestButton = "Request invite"

            public static let recheckButton = "Recheck"

            public static func denied(reason: String) -> String {
                "Request denied: \(reason)"
            }

            public static let pendingReview = "Submitted. The admin will review — you'll continue automatically once they respond."

            public enum error {
                public static let closed = "This nest is not currently accepting invite requests."

                public static let rateLimited = "Too many requests. Please try again later."

                public static let notFound = "This invite request was not found. It may have been removed by the admin."

                public static func transient(cause: String) -> String {
                    "\(cause). Try again."
                }

                public static let alreadyRegistered = "This nest already has an account for you, so it won't take a new invite request. Its admin may have suspended your account — contact the admin; if they restore it, sign in again."

                public static func terminal(cause: String) -> String {
                    "Error: \(cause)."
                }
            }
        }

        public enum oobCode {
            public static let idle = "Have an invite code? Paste it here."

            public static let placeholder = "Invite code from an admin"

            public static let verifying = "Verifying code…"

            public static let valid = "Code accepted. Click Continue to log in."

            public static func invalid(reason: String) -> String {
                "Code not recognized: \(reason)"
            }

            public static func error(cause: String) -> String {
                "Couldn't verify code: \(cause)"
            }
        }

        public enum claimCode {
            public static let title = "Claim this nest"

            public static let description = "No one has claimed this nest yet. Paste the one-time claim code printed by your nest server to become its admin."

            public static let label = "Claim code"

            public static let placeholder = "Claim code from server bootstrap"

            public static let submitButton = "Claim"

            public static let idle = "Paste the claim code from your server."

            public static let submitting = "Claiming nest…"

            public static let claimed = "Welcome, admin. Continuing…"

            public static func invalid(reason: String) -> String {
                "Code not accepted: \(reason)"
            }

            public enum error {
                public static let alreadyClaimed = "This nest has already been claimed."

                public static let transient = "Couldn't reach the nest. Try again."

                public static func terminal(cause: String) -> String {
                    "Couldn't claim: \(cause)."
                }

                public static let claimCodeUnreadable = "The nest can't read its claim code — a server setup problem, not a wrong code. Restart the nest and try again."
            }
        }

        public enum natMode {
            public static let title = "Is this nest reachable from the internet?"

            public static let description = "This sets how your nest connects to the world. The pre-selected option matches how it was installed, so you can usually just confirm — and you can change it anytime in Admin → Nest."

            public static let publicLabel = "Public (internet-facing)"

            public static let publicDesc = "This box has a public address: it can receive email, federate directly with other nests, get automatic certificates, and relay for private nests. The usual choice for a hosted server."

            public static let privateLabel = "Private (home network)"

            public static let privateDesc = "This box sits behind a home router and isn't reachable from the internet: it runs no mail receiver, keeps calendar and mail sync on your local network, and pairs with a public nest that relays for it."

            public static let confirmButton = "Confirm"

            public static let deferButton = "Decide later"

            public static let choosing = "Confirm how this nest connects, or decide later."

            public static let privateHint = "This looks like a home-network address, so Private is pre-selected."

            public static let submitting = "Saving the connection mode..."

            public static let done = "Connection mode saved."

            public enum error {
                public static func transient(cause: String) -> String {
                    "Couldn't save the connection mode: \(cause). Try again."
                }

                public static func terminal(cause: String) -> String {
                    "Couldn't save the connection mode: \(cause)."
                }
            }
        }

        public enum trustPrompt {
            public static let title = "Trust this box?"

            public static let summary = "This box can do more for you if you let it read the things it looks after: filtering your mail as it arrives, and serving your calendar to your other devices. Your trust renews itself while you use Fauna, you can take it back at any time in Settings → Nests, and every time you give or withdraw trust it is written down for you there."

            public static let grantButton = "Yes, trust this box"

            public static let skipButton = "Not now"

            public static let nothingToGrant = "There's nothing to decide yet — this box isn't running anything that would read your content. You can trust it later from Settings → Nests."
        }

        public enum awaitingDns {
            public static let title = "Almost ready"

            public static let pending = "Add the DNS records below at your registrar. We'll bring your nest online automatically once they take effect — you can leave this screen open."

            public static let serverStarting = "Your server is starting — we'll sign you in the moment it answers. You can close the app and come back."

            public static let checking = "Checking whether your nest is online…"

            public static let claiming = "Your nest is online — finishing setup…"

            public static let claimed = "All set. Continuing…"

            public static func error(cause: String) -> String {
                "Couldn't finish setting up your nest: \(cause)"
            }

            public static let recheckButton = "Check now"

            public static let copyButton = "Copy all"
        }

        public enum done {
            public static let finished = "Onboarding finished."
        }

        public enum sessionError {
            public static func persistAccount(message: String) -> String {
                "Couldn't save your new account: \(message)"
            }

            public static func persistSuccessor(message: String) -> String {
                "Couldn't save your new identity: \(message)"
            }

            public static func switchSuccessor(message: String) -> String {
                "Couldn't switch to your new identity: \(message)"
            }

            public static func switchAppended(message: String) -> String {
                "Couldn't switch to the new account: \(message)"
            }

            public static let noActiveAccount = "Couldn't find the account that was just added."

            public static func invalidSecret(message: String) -> String {
                "This device's saved secret key isn't valid: \(message)"
            }

            public static func removeAccount(message: String) -> String {
                "Couldn't remove that account: \(message)"
            }
        }
    }

    public enum launch {
        public static let signingIn = "Signing you in…"

        public static let retryTitle = "Couldn't reach your nest"

        public static let needsUpdateTitle = "Update this app to continue"

        public static let identityChangedTitle = "This nest's identity changed"

        public static let retryButton = "Try again"

        public static let useDifferentNest = "Use a different nest"

        public static let recoveryCustodyMismatch = "Off-box recovery isn't protected: this box handed off an inconsistent recovery key."

        public static let recoveryCustodyFailed = "Off-box recovery custody wasn't saved — couldn't reach the box to confirm. Your nest still works, but it isn't protected against total box loss yet."
    }

    public enum credentialStore {
        public static let sectionTitle = "Credential store"

        public static let statusSealed = "Your sign-in keys rest in a file on this device, sealed under your passphrase."

        public static let rekeyButton = "Change passphrase…"

        public static let rekeyTitle = "Change passphrase"

        public static let seedNudge = "Back up your recovery seed first (Identity export, above). The sealed store has no recovery path of its own — if the new passphrase is forgotten, the seed is the only way back into your account."

        public static let currentLabel = "Current passphrase"

        public static let newLabel = "New passphrase"

        public static let confirmLabel = "Confirm new passphrase"

        public static let submit = "Change passphrase"

        public static let success = "Passphrase changed."

        public static let errorEmpty = "Enter your current passphrase and choose a new one"

        public static let errorMismatch = "The two new entries don't match"

        public static let errorWrong = "Wrong passphrase, or the store file is corrupt"

        public static func errorFailed(message: String) -> String {
            "Could not change the passphrase: \(message)"
        }
    }

    public enum tuiUnlock {
        public static let unlockTitle = "Unlock your credentials"

        public static let unlockPrompt = "Your credentials are protected by a passphrase on this machine. Enter it to sign in."

        public static let createTitle = "Protect your credentials"

        public static let createPrompt = "No secure OS key store is reachable here, so your sign-in keys will rest in a file protected by a passphrase. Choose one to continue — you'll need it at every launch."

        public static let passphraseLabel = "Passphrase"

        public static let confirmLabel = "Confirm passphrase"

        public static let unlockButton = "Unlock"

        public static let createButton = "Set passphrase"

        public static let errorEmpty = "Enter a passphrase"

        public static let errorMismatch = "The two entries don't match"

        public static let errorWrong = "Wrong passphrase, or the store file is corrupt"

        public static func errorFailed(message: String) -> String {
            "Could not open the credential store: \(message)"
        }
    }

    public enum tuiNavHints {
        public static func pane(keys: String) -> String {
            "\(keys) pane"
        }

        public static func moveFocus(keys: String) -> String {
            "\(keys) move"
        }

        public static func `open`(keys: String) -> String {
            "\(keys) open"
        }

        public static func next(keys: String) -> String {
            "\(keys) next"
        }

        public static func quit(keys: String) -> String {
            "\(keys) quit"
        }
    }

    public enum tuiSettings {
        public static let title = "Terminal"

        public static let externalMediaLabel = "Play audio & video externally"

        public static let externalMediaSubtitle = "This terminal can't play audio or video inline, so it can hand a clip to your system's default player. Choose whether it asks first, always opens, or never opens."

        public static let externalMediaAsk = "Ask each time"

        public static let externalMediaAlways = "Always open"

        public static let externalMediaNever = "Never open (show details only)"
    }

    public enum feed {
        public static let composeEmpty = "Post cannot be empty"

        public static let composeGatePreviewEmpty = "Add a public teaser for a gated post"

        public static func composeGateNoKey(tier: String) -> String {
            "This device does not hold the key for tier \(tier)"
        }

        public static let composeRoomNoKey = "This device does not hold the key for that room yet"

        public static let referenceRestricted = "This post is for a smaller audience, and your reply would be public, so it was not sent"

        public static let composeAttachmentStale = "Attach the file again — the audience changed after it was prepared"

        public static func composeAttachmentMissing(filename: String) -> String {
            "Attach \(filename) again — the file is not on this device."
        }

        public static let composeSellPriceInvalid = "Enter a smaller price"

        public static let composeSellRankUnavailable = "Could not check your tiers to price this post — try again"

        public static func errorLoad(message: String) -> String {
            "Failed to load posts: \(message)"
        }

        public static func errorFeeds(message: String) -> String {
            "Failed to load feeds: \(message)"
        }

        public static func errorSubmit(message: String) -> String {
            "Failed to post: \(message)"
        }

        public static func errorSubscribe(message: String) -> String {
            "Failed to subscribe: \(message)"
        }

        public static func errorGatedUnlock(message: String) -> String {
            "Could not unseal this post: \(message)"
        }

        public static func errorTrainedFactor(message: String) -> String {
            "This feed's trained topic could not be applied: \(message)"
        }

        public static func errorSubscribedModel(message: String) -> String {
            "A subscribed community model could not be applied: \(message)"
        }

        public static func errorTrain(message: String) -> String {
            "Could not train on this post: \(message)"
        }

        public static func errorBuyUnlock(message: String) -> String {
            "Could not buy this post: \(message)"
        }

        public static let postActionsTooltip = "More actions"

        public static let moreLikeThis = "More like this"

        public static let lessLikeThis = "Less like this"

        public static let trainTargetTitle = "Train which topic?"

        public static let deletePost = "Delete post"

        public static let deletePostConfirm = "Delete"

        public static let deletePostConfirmTitle = "Delete post?"

        public static func errorDelete(message: String) -> String {
            "Failed to delete post: \(message)"
        }

        public static let reportPost = "Report post"

        public static let postMutedPlaceholder = "Muted word"

        public static let postMutedReveal = "Show anyway"

        public static func errorMutedKeywords(message: String) -> String {
            "Your muted words are not being applied: \(message)"
        }

        public static let unverifiedSource = "Unverified"

        public static let unverifiedSourceTooltip = "This device could not verify the author's signature on this post."

        public static let delegatedOrigin = "Via connected app"

        public static let delegatedOriginTooltip = "A connected app wrote this post as you, using the access you granted it. Manage or revoke that access on the AT Protocol settings page."

        public static let likeTooltip = "Like"

        public static let quote = "Quote"

        public static let watch = "Watch"

        public enum postType {
            public static let community = "Community"

            public static let classified = "Listing"
        }

        public enum list {
            public static let title = "Feeds"

            public static let trending = "Trending"

            public static let noPosts = "No posts yet."

            public static let noMatchingPosts = "No matching posts."

            public static let endOfFeed = "End of feed"

            public static let bridgeFeeds = "Bridge Feeds"

            public static let subscribeBridge = "Subscribe to Bridge Feed"

            public static let deleteFeed = "Delete feed"

            public static let unsubscribe = "Unsubscribe"
        }

        public enum bridgeForm {
            public static let kind = "Bridge"

            public static let uri = "Feed URI"

            public static let name = "Display Name"
        }

        public enum create {
            public static let title = "New Feed"

            public static let namePlaceholder = "My Feed"

            public static let addRule = "Add Rule"

            public static let modeAll = "All match"

            public static let modeAny = "Any match"

            public static let filterRules = "Filter Rules"

            public static let combination = "Combination"

            public static let feedName = "Feed Name"

            public static let ruleRequired = "Required"

            public static let ruleExcluded = "Excluded"

            public static let ruleCategory = "Category"

            public static let ruleThreshold = "Threshold (0-10)"

            public static let ruleThresholdShort = "0-10"

            public static let ruleValuePlaceholder = "tag1, tag2"

            public static let factors = "Factors"

            public static let factorEngagement = "Engagement"

            public static let factorTrending = "Trending"

            public static let factorWeightPlaceholder = "1.0"

            public static let factorGlobalToggle = "Apply to all feeds"

            public static let addFactor = "Add Factor"
        }

        public enum post {
            public static let postNotFound = "Post not found"

            public static func replyingToUser(user: String) -> String {
                "Replying to \(user)"
            }

            public static let repostedMarker = "reposted"

            public static let writeReply = "Write a reply..."

            public static let repost = "Repost"

            public static let addComment = "Add your comment..."

            public static let whatsOnYourMind = "What's on your mind?"

            public static let hasMedia = "[media]"

            public static let viewThread = "View Thread"

            public static let thread = "Thread"

            public static let noThreadData = "No thread data"

            public static let noThreadDesc = "Could not load thread data."

            public static let attachImage = "Attach Image"

            public static let noFeedsConfigured = "No feeds configured."

            public static let noBridgeFeeds = "No bridge feeds available."

            public static let createTooltip = "Create Feed"

            public static let writePost = "Write a post..."

            public static let tagsPlaceholder = "Tags (comma-separated)"

            public static let searchPlaceholder = "Search this feed..."

            public static let compose = "Compose"

            public static let composePost = "Compose Post"

            public static let composeDropHint = "Compose post — drop files here to attach"

            public static let gateAudience = "Audience"

            public static let gatePublic = "Public"

            public static let gatePreviewPlaceholder = "Public teaser shown to non-subscribers..."

            public static func gatedBadgeTooltip(tier: String) -> String {
                "Subscribers only: \(tier)"
            }

            public static func gatedBadgeRoomTooltip(room: String) -> String {
                "Room members only: \(room)"
            }

            public static let gateSell = "Sell this post…"

            public static func gateRoom(room: String) -> String {
                "Room: \(room)"
            }

            public static func replyAudienceRoom(room: String) -> String {
                "Reply goes to Room: \(room)"
            }

            public static func replyAudienceTier(tier: String) -> String {
                "Reply goes to your tier \(tier)"
            }

            public static let replyAudiencePublic = "This post is for a smaller audience — a reply from you would be public"

            public static let replyPublicConfirm = "Post my reply publicly"

            public static let sellPricePlaceholder = "Price, e.g. $3"

            public static let sellAskingPricePlaceholder = "Machine price in sats (optional)"

            public static let sellSubscribersFree = "Subscribers get it free"

            public static let buyButton = "Buy"

            public static let openRichCompose = "Open rich compose dialog"

            public static let posting = "Posting…"

            public static let postDetail = "Post detail"
        }

        public enum ruleTypes {
            public static let hasHashtag = "Has Hashtag"

            public static let source = "Protocol Source"

            public static let hasMedia = "Has Media"

            public static let isReply = "Is Reply"

            public static let minReplies = "Min Replies"

            public static let minReposts = "Min Reposts"

            public static let createdAfter = "Created After"

            public static let bodyContains = "Body contains"

            public static let bodyExcludes = "Body Excludes"

            public static let labelBelow = "Label Below (exclude spam)"

            public static let labelAbove = "Label Above (show only)"
        }

        public enum ruleChip {
            public static func hasHashtag(tags: String) -> String {
                "\(tags)"
            }

            public static func source(value: String) -> String {
                "source: \(value)"
            }

            public static let hasMediaYes = "media: yes"

            public static let hasMediaNo = "media: no"

            public static let isReplyYes = "reply: yes"

            public static let isReplyNo = "reply: no"

            public static func minReplies(count: String) -> String {
                "replies >= \(count)"
            }

            public static func minReposts(count: String) -> String {
                "reposts >= \(count)"
            }

            public static func createdAfter(hours: String) -> String {
                "age < \(hours)h"
            }

            public static func bodyContains(value: String) -> String {
                "contains: \(value)"
            }

            public static func bodyExcludes(value: String) -> String {
                "excludes: \(value)"
            }

            public static func labelBelow(value: String) -> String {
                "label below: \(value)"
            }

            public static func labelAbove(value: String) -> String {
                "label above: \(value)"
            }
        }
    }

    public enum conversations {
        public enum errors {
            public static let servedElsewhere = "Conversations are open in another instance of this app. Use them there — everything else works here."

            public static let receiveStopped = "New messages stopped arriving because of an internal error. Restart the app (or reload the page) to receive them again."

            public static func mailUnopenable(count: String) -> String {
                "\(count) received messages could not be opened on this device. They were sealed to mail keys this account no longer holds, and were skipped."
            }
        }

        public enum list {
            public static let title = "Conversations"

            public static let newConversation = "New Conversation"

            public static let sort = "Sort"

            public static let searchPlaceholder = "Search conversations..."

            public static let selectConversation = "Select a conversation to view."

            public static let selectConversationShort = "Select a conversation"

            public static let noConversations = "No conversations yet."
        }

        public enum compose {
            public static let title = "Compose"

            public static let newMessage = "New Message"

            public static let to = "To"

            public static let subject = "Subject"

            public static let body = "Body"

            public static let resolve = "Resolve"

            public static let writeMessage = "Write your message..."

            public static let encrypted = "Encrypted"
        }

        public enum detail {
            public static let badgeEncrypted = "End-to-end encrypted"

            public static let badgeSigned = "Cryptographically signed"

            public static let badgeVerified = "Verified sender"

            public static let badgeC2pa = "C2PA content credentials present"

            public static let deleteMessage = "Delete message"

            public static let deleteMessageConfirm = "Delete"

            public static let deleteMessageConfirmTitle = "Delete message?"

            public static let messageDeleted = "This message was deleted"

            public static let selectedMessage = "Your search result"

            public static let messageActions = "More actions"

            public static let addReaction = "Add reaction"

            public static let moreReactions = "More reactions"

            public static let mailbox = "Mailbox"

            public static let noSubject = "(no subject)"

            public static let title = "Conversation"

            public static let noMessages = "No messages in this conversation."

            public static let loadRemoteContent = "Load remote images"

            public static let remoteImageBlocked = "Remote image blocked"

            public static let memberUnattestedMark = "Was in this group before you recovered your account. Keep them, or remove them if you don't recognise them."

            public static let memberKeep = "Keep"

            public static let mutedWord = "Muted word"

            public static let mutedReveal = "Show anyway"

            public static let markAsSpam = "Mark as spam"

            public static let reportMessage = "Report message"
        }

        public enum message {
            public static let signed = "Signed"
        }

        public enum unified {
            public static let attachmentButton = "Attach file"

            public static let attachmentRemove = "Remove attachment"

            public static func errorAddParticipant(message: String) -> String {
                "Could not add them to this conversation: \(message)"
            }

            public static func errorAddParticipantAfterHeal(reason: String) -> String {
                "their undelivered earlier invitation was removed first, so they are no longer in the group — adding them again starts cleanly. The re-invitation failed: \(reason)"
            }

            public static func errorRemoveParticipant(message: String) -> String {
                "Could not remove them from this conversation: \(message)"
            }

            public static func errorRenameThread(message: String) -> String {
                "Could not rename this conversation: \(message)"
            }

            public static func errorLeaveRoom(message: String) -> String {
                "Could not leave this conversation: \(message)"
            }

            public static func errorSetRoomPolicy(message: String) -> String {
                "Could not change this room's settings: \(message)"
            }

            public static func errorRoomInvitation(message: String) -> String {
                "Could not answer this invitation: \(message)"
            }

            public static func errorWithdrawRoomInvite(message: String) -> String {
                "Could not withdraw this invitation: \(message)"
            }

            public static let roomClassEndToEnd = "End-to-end encrypted"

            public static let roomClassCommunity = "Community — searched and labelled by the home nest"

            public static let roomClassTransportOnly = "Transport-only"

            public static let guardianStateHeld = "Waiting for your guardian"

            public static let guardianStateBlocked = "Blocked by your guardian"

            public static let bridgedOneRecipient = "A conversation over this bridge is with one person. Remove the other recipients and send again."

            public static let bridgedNoRecipientKey = "This account has no mail key yet, so a copy of the message cannot be kept. Set up mail, then send again."

            public static let roomNoticeModerationUnverified = "Some moderation in this room couldn't be verified on this device, so the affected messages are still shown."

            public static let roomNoticeAwaitingKey = "Waiting for a room key — messages will appear once an owner or admin keys you in."

            public static let roomRoleOwner = "owner"

            public static let roomRoleAdmin = "admin"

            public static let roomJoinRuleLabel = "Who can invite"

            public static let roomJoinRuleInvite = "Owner and admins"

            public static let roomJoinRuleMemberInvite = "Any member"

            public static let roomHistoryPolicyLabel = "History for new members"

            public static let roomHistoryPolicyNone = "Nothing before they join"

            public static let roomHistoryPolicyFull = "The whole conversation"

            public static let threadRoomSettings = "Room settings"

            public static let roomLeave = "Leave room"

            public static let roomHomeNestYes = "Home nest joins: yes"

            public static let roomHomeNestNo = "Home nest joins: no"

            public static let roomNestReadYes = "Home nest reads this room: yes"

            public static let roomNestReadNo = "Home nest reads this room: no"

            public static func roomInvitationMember(inviter: String) -> String {
                "\(inviter) invited you to a room"
            }

            public static func roomInvitationAdmin(inviter: String) -> String {
                "\(inviter) invited you to a room as an admin"
            }

            public static let roomPendingInvitesLabel = "Pending invitations"

            public static func roomPendingInviteMember(invitee: String, inviter: String) -> String {
                "\(invitee) — invited by \(inviter)"
            }

            public static func roomPendingInviteAdmin(invitee: String, inviter: String) -> String {
                "\(invitee) — invited by \(inviter) as an admin"
            }

            public static func roomPendingInviteLapsed(sentence: String) -> String {
                "\(sentence) (can no longer be accepted)"
            }

            public static let roomPendingInviteWithdraw = "Withdraw"

            public static let roomAdminYes = "admin: yes"

            public static let roomAdminNo = "admin: no"

            public static let roomTransferMark = "make owner"

            public static let roomTransferStaged = "new owner"

            public static let roomLabelersLabel = "Labels the home nest adds to every message"

            public static let roomLabelerOn = "labels: on"

            public static let roomLabelerOff = "labels: off"

            public static func errorSend(message: String) -> String {
                "Could not send this message: \(message)"
            }

            public static func listSendWarning(count: String, list: String, used: String, limit: String) -> String {
                "This will send to \(count) subscribed recipients on \(list). Today's quota: \(used) / \(limit)."
            }

            public static func listSendWarningNoLimit(count: String, list: String) -> String {
                "This will send to \(count) subscribed recipients on \(list)."
            }

            public static func listQuotaApproaching(remaining: String) -> String {
                "Approaching daily limit — \(remaining) more recipients today"
            }

            public static func listQuotaOver(count: String, limit: String, remaining: String) -> String {
                "Sending to \(count) recipients would pass today's limit of \(limit) (\(remaining) left). Try again tomorrow."
            }

            public static func listSendProgress(list: String, delivered: String, count: String) -> String {
                "Sent to \(list): \(delivered) of \(count) recipients delivered"
            }

            public static let errorAttachmentNoComposer = "Open a message composer before attaching a file."

            public static let groupConversationHint = "This will start a group conversation."

            public static let recipientPickerPlaceholder = "Type a handle, email, npub, DID, or @user@instance"

            public static func recipientPickerBridges(bridges: String) -> String {
                "Also reaches people on: \(bridges)"
            }

            public static let recipientResolveError = "Lookup failed — try again"

            public static let recipientResolveNotFound = "Not found — check the address"

            public static let recipientResolveResolved = "Resolved"

            public static let recipientResolveResolving = "Resolving…"

            public static let replyAll = "Reply all"

            public static let replyRecipientAddPlaceholder = "Add recipient…"

            public static let showFullHeaders = "Show full headers"

            public static let threadAddParticipant = "Add someone…"

            public static let threadRename = "Rename"

            public static let threadRenamePlaceholder = "New name"

            public static let toLineLabel = "To:"

            public static let topicInputPlaceholder = "Topic (optional)"

            public static let topicToggleAdd = "+ topic"
        }
    }

    public enum contacts {
        public static let title = "Contacts"

        public static let detailTitle = "Contact"

        public static let signInPrompt = "Sign in to view your contacts."

        public enum findUser {
            public static let title = "Find User"

            public static let description = "Enter a handle (e.g. alice@fauna.social) or actor ID hex to find a user."

            public static let placeholder = "alice@fauna.social or actor ID hex"

            public static let find = "Find"
        }

        public static let message = "Message"

        public static let handleNotFound = "Handle not found"

        public static let guardianApprovalRequired = "This account can only message approved contacts."

        public static let askGuardian = "Ask your guardian"

        public static let contactRequestPending = "Asked — waiting for your guardian"

        public enum messageRequests {
            public static let title = "Message Requests"

            public static let none = "No pending message requests."

            public static func count(count: String) -> String {
                "Message Requests (\(count))"
            }
        }

        public static let noContactSelected = "Select a contact to view details."

        public static let knock = "Knock"

        public static let sent = "Sent"

        public static let lookingUp = "Looking up..."

        public static let wantsToConnect = "wants to connect"

        public static let searchResults = "Search Results"

        public static let knocks = "Knocks"

        public static let noPendingKnocks = "No pending knocks."

        public static let noContacts = "No contacts yet."

        public static let noMatchingContacts = "No matching contacts."

        public static let handleOrActorId = "Handle, handle@domain, or actor ID..."

        public static let pendingRequests = "Pending Requests"

        public static let noPending = "No pending requests."

        public static let unattestedMark = "Not reviewed since you recovered your account"

        public static let findPlaceholder = "Find by handle..."

        public static let filterPlaceholder = "Filter contacts..."

        public static let addContact = "Add Contact"

        public static let requestSent = "Contact request sent!"

        public static func lookingUpHandle(handle: String, domain: String) -> String {
            "Looking up @\(handle)@\(domain)…"
        }

        public enum addressBook {
            public static let title = "Address Book"

            public static let noAddressbooks = "No address books yet."

            public static let noCards = "No contacts yet."

            public static let cardNotFound = "That contact is no longer in your address books."

            public static let selectCard = "Select a contact to view details."

            public static let email = "Email"

            public static let phone = "Phone"

            public static let address = "Address"

            public static let organization = "Organization"

            public static let note = "Note"
        }
    }

    public enum events {
        public static let title = "Events"

        public static func calendarExported(path: String) -> String {
            "Calendar exported to \(path)"
        }

        public static let calendars = "Calendars"

        public static let newCalendar = "New Calendar"

        public static let calendarName = "Calendar name"

        public static let noCalendars = "No calendars yet."

        public static let invitedEvents = "Invited Events"

        public static let newEvent = "New Event"

        public static let moreOptions = "More options..."

        public static let importIcs = "Import .ics"

        public static let importing = "Importing..."

        public static let exportIcs = "Export .ics"

        public static let exporting = "Exporting..."

        public static let exportIcsTooltip = "Export a calendar as .ics file"

        public static let importIcsTooltip = "Import events from .ics file"

        public static let exportCalendarTitle = "Export Calendar as ICS"

        public static let importCalendarTitle = "Import ICS Calendar"

        public static let summary = "Summary"

        public static let summaryPlaceholder = "New event"

        public static let start = "Start"

        public static let end = "End"

        public static let endOptional = "End (optional)"

        public static let description = "Description"

        public static let location = "Location"

        public static let locationPlaceholder = "Add a location..."

        public static let attendanceMode = "Attendance Mode"

        public static let capacity = "Capacity (0 = unlimited)"

        public static let viewWeek = "Week"

        public static let allDay = "All day"

        public enum rsvp {
            public static let title = "RSVP"

            public static let going = "Going"

            public static let interested = "Interested"

            public static let decline = "Decline"

            public static let tentative = "Tentative"

            public static let declined = "Declined"

            public static let waitlisted = "Waitlisted"

            public static let invited = "Invited"
        }

        public enum invite {
            public static let button = "Invite"

            public static let title = "Invite Attendee"

            public static let emailLabel = "Email"

            public static let emailPlaceholder = "Attendee email"

            public static let inviting = "Inviting..."
        }

        public static let attendees = "Attendees"

        public static func attendeesCount(count: String) -> String {
            "Attendees (\(count))"
        }

        public static let noAttendees = "No attendees yet."

        public static let eventCountOne = "1 event"

        public static func eventCount(count: String) -> String {
            "\(count) events"
        }

        public enum reminder {
            public static let title = "Reminder"

            public static let `set` = "Set"

            public static let current = "Current:"

            public static let selectPlaceholder = "Select…"

            public static let min15 = "15 min before"

            public static let hour1 = "1 hour before"

            public static let day1 = "1 day before"
        }

        public static let noEventsYet = "No events yet"

        public static let selectCalendar = "Select a calendar"

        public static let eventNotFound = "Event not found"

        public static let detailTitle = "Event"

        public static let noEventSelected = "No event selected"

        public static let selectEventHint = "Select an event to see its details."

        public static let removeReminder = "Remove Reminder"

        public static let deleteEvent = "Delete Event"

        public static let deleteEventConfirm = "Are you sure you want to delete this event?"

        public static let createEvent = "Create Event"

        public static let startDate = "Start Date"

        public static let endDate = "End Date"

        public static let invalidDatetime = "Enter a date and time as YYYY-MM-DDTHH:MM."

        public static let time = "Time"

        public static let deleting = "Deleting..."

        public static let noUpcomingEvents = "No upcoming events"

        public static let noUpcomingEventsDesc = "Events from selected calendars will appear here."

        public static let noEvents = "No events"

        public static let attendance = "Attendance"

        public static let inviteOnly = "Invite Only"

        public static let groupOnly = "Group Only"

        public static let linkCode = "Link Code"

        public static let calendar = "Calendar"

        public static let remindMe = "Remind me"

        public static let yourStatus = "Your status:"

        public static let sendInvite = "Send Invite"

        public static let viewAgenda = "Agenda"

        public static let viewMonth = "Month"

        public static let viewDay = "Day"

        public static let mailRequired = "Calendars and events require mail to be enabled. Enable mail in Settings to use the calendar."

        public static let loadingEvents = "Loading events..."

        public static let noEventsInCalendar = "No events in this calendar."

        public static let selectCalendarEventHint = "Select a calendar and event to view details."

        public static func importResult(imported: String, skipped: String, total: String) -> String {
            "Imported: \(imported), Skipped: \(skipped), Total: \(total)"
        }

        public static let icsPathRequired = "Type the path to an .ics file first, then press Import."

        public static let icsFileRequired = "Choose an .ics file first, then press Import."

        public static let setReminder = "Set Reminder"

        public static let setting = "Setting..."

        public enum refusedChanges {
            public static let title = "Refused changes"

            public static func cancelAttempt(title: String) -> String {
                "Someone tried to cancel \"\(title)\""
            }

            public static func updateAttempt(title: String) -> String {
                "Someone tried to change \"\(title)\""
            }

            public static func replyAttempt(title: String) -> String {
                "Someone tried to answer for \"\(title)\""
            }

            public static let otherAttempt = "Someone tried to change an event on your calendar"

            public static func sender(who: String) -> String {
                "Sent by \(who)"
            }

            public static func senderViaNest(who: String, nest: String) -> String {
                "Sent by \(who), according to \(nest)"
            }

            public static let unknownSender = "Sender could not be identified"

            public static func attempts(count: String) -> String {
                "Tried \(count) times"
            }

            public static let dismiss = "Dismiss"

            public enum reason {
                public static let notTheOrganizer = "They are not the organizer of this event."

                public static let organizerChanged = "The message tried to change who organizes the event."

                public static let organizerUnresolvable = "No one could be confirmed as this event's organizer."

                public static let noAttestedAuthor = "Your nest could not confirm who sent the message."

                public static let spoofedOrganizer = "The sender was not the organizer they claimed to be."

                public static let notTheAttendee = "They are not the guest they answered for."

                public static let attendeeUnresolvable = "The guest they answered for could not be confirmed."

                public static let senderUnauthenticated = "The message did not come from a confirmed sender."

                public static let other = "The change was refused."
            }
        }

        public enum error {
            public static let loadCalendars = "Failed to load calendars"

            public static let loadEvents = "Failed to load events"

            public static let createCalendar = "Failed to create calendar"

            public static let createEvent = "Failed to create event"

            public static let deleteEvent = "Failed to delete event"

            public static let invite = "Failed to invite"

            public static let rsvp = "RSVP failed"

            public static let setReminder = "Failed to set reminder"

            public static let removeReminder = "Failed to remove reminder"

            public static let `import` = "Import failed"

            public static let export = "Export failed"
        }
    }

    public enum groups {
        public static let title = "Groups"

        public static let createGroup = "Create Group"

        public static let groupName = "Group name"

        public static let myGroups = "My Groups"

        public static let noGroups = "No groups yet."

        public static let selectPrompt = "Select or create a group to get started."

        public static let loadingGroup = "Loading group..."

        public static let members = "Members"

        public static let inviteMember = "Invite Member"

        public static let invitePlaceholder = "alice@fauna.social or actor ID"

        public static let inviteNestUrlPlaceholder = "Nest URL (optional, for cross-nest)"

        public static let nodeUrlPlaceholder = "Node URL (for cross-nest invites)"

        public static let replyingTo = "Replying to"

        public static let noChannel = "No encrypted channel for this group"

        public static let viewThreaded = "Threaded"

        public static let groupChat = "Group Chat"

        public static let makeAdmin = "Make Admin"

        public static let demote = "Demote"

        public static let role = "Role"

        public static let memberRole = "Member"

        public static let invite = "Invite"

        public static let group = "Group"

        public static let selectGroup = "Select a group to view."

        public static let markSpam = "Mark Spam"

        public static let notSpam = "Not Spam"

        public static let cancelReply = "Cancel reply"

        public static let inviteToGroup = "Invite to Group"

        public static let groupMembers = "Group Members"

        public static let noMembers = "No members yet."

        public static let owner = "Owner"

        public static func membersTitle(name: String) -> String {
            "Members — \(name)"
        }

        public static func inviteMemberTitle(name: String) -> String {
            "Invite Member — \(name)"
        }

        public static func replyingToUser(user: String) -> String {
            "Replying to \(user)"
        }

        public static let createGroupHint = "Create a group to start messaging."

        public static let newGroup = "New Group"

        public static let invitee = "Invitee"

        public static let muteGroup = "Mute Group"

        public static let react = "React"

        public static let messagePlaceholder = "Type a message..."

        public enum message {
            public static let encrypted = "Encrypted"

            public static let encryptedTitle = "End-to-end encrypted via MLS"

            public static let signed = "Signed"

            public static let signedTitle = "Sender signature verified — content is plaintext in the nest"
        }
    }

    public enum atprotoSettings {
        public static let title = "AT Protocol"

        public static let appCredentialsHeading = "App credentials"

        public static let appCredentialsEmpty = "No app credentials yet."

        public static let mintButton = "New app credential"

        public static func defaultCredentialLabel(count: String) -> String {
            "App credential \(count)"
        }

        public static let revealButton = "Reveal"

        public static let revokeButton = "Revoke"

        public static func credentialCreatedPrefix(date: String) -> String {
            "Created \(date)"
        }

        public static func credentialLastUsedPrefix(date: String) -> String {
            "Last used \(date)"
        }

        public static let credentialNeverUsed = "Never used"

        public static let connectedAppsHeading = "Connected apps"

        public static let connectedAppsEmpty = "No connected apps yet."

        public static func sessionCreatedPrefix(date: String) -> String {
            "Connected \(date)"
        }

        public static func sessionExpiresPrefix(date: String) -> String {
            "Expires \(date)"
        }

        public static func sessionScopesPrefix(scopes: String) -> String {
            "Approved for \(scopes)"
        }

        public static func sessionSetsPrefix(sets: String) -> String {
            "Granted via \(sets)"
        }

        public static func sessionSetNamed(title: String, nsid: String) -> String {
            "“\(title)” (\(nsid))"
        }

        public static func sessionLastUsedPrefix(date: String) -> String {
            "Last seen \(date)"
        }

        public static let sessionNeverUsed = "Not seen since connecting"

        public static let sessionStatusLive = "Working"

        public static let sessionStatusSuspended = "Paused — turn Bluesky back on to let this app work again"

        public static let externalAppsToggle = "Allow external apps"

        public static let depthHeading = "Integration depth"

        public static let depthOffTitle = "Off"

        public static let depthOffDesc = "No Bluesky presence."

        public static let depthLinkedTitle = "Linked account"

        public static let depthLinkedDesc = "Read, interact, and cross-post through an existing Bluesky account."

        public static let depthHostedVisibleTitle = "Hosted here — visible"

        public static let depthHostedVisibleDesc = "This nest holds your identity and publishes your public posts to the Bluesky network."

        public static let depthHostedFullTitle = "Hosted here — full access"

        public static let depthHostedFullDesc = "Additionally, other Bluesky apps can log in as you through this nest."

        public static let depthCardHeading = "Confirm this change"

        public static let depthConfirmButton = "Confirm"

        public static let depthCancelButton = "Cancel"

        public static let contestCardHeading = "Your AT Protocol identity may have been taken over"

        public static let contestButton = "Undo this change…"

        public static let contestConfirmButton = "Undo it now"

        public static let contestCancelButton = "Not now"

        public static let didMethodHeading = "Identity method"

        public static let didMethodPlcTitle = "did:plc — recommended"

        public static let didMethodPlcDesc = "A portable identity you can move to another server later."

        public static let didMethodWebTitle = "did:web"

        public static let didMethodWebDesc = "Ties your identity to this nest's domain."

        public static func handleEitherWay(handle: String) -> String {
            "Your Bluesky handle will be @\(handle) either way."
        }

        public static let historyBackfillLabel = "Also publish my existing public posts."

        public static func hostedHandlePrefix(handle: String) -> String {
            "Your Bluesky handle: @\(handle)"
        }

        public static func hostedMethodPrefix(method: String) -> String {
            "Method: \(method)"
        }

        public static let identityStatusActive = "Active"

        public static let identityStatusPending = "Setting up…"

        public static let identityStatusDeactivated = "Deactivated"

        public static let identityStatusDeleted = "Deleted"

        public static let identityStatusTombstoned = "Permanently retired"

        public static let deletePresenceButton = "Delete my Bluesky presence"

        public static let deleteConfirmButton = "Delete my presence"

        public static let deleteCancelButton = "Keep my presence"

        public static let deleteRetireIdentityLabel = "Also permanently retire my AT Protocol identity — this cannot be undone"

        public static let delegationHeading = "Posting from other apps"

        public static let delegationEmpty = "Other Bluesky apps can sign in, but cannot post as you yet."

        public static func delegationScopePrefix(capabilities: String) -> String {
            "Allowed: \(capabilities)"
        }

        public static let delegationCapabilityPost = "post"

        public static let delegationCapabilityUpdateProfile = "update your profile"

        public static func delegationLastsUntil(authorized: String, expires: String) -> String {
            "Authorized \(authorized) · until \(expires)"
        }

        public static func delegationLastsUntilNoExpiry(authorized: String) -> String {
            "Authorized \(authorized) · no expiry"
        }

        public static let delegationStatusActive = "Active"

        public static let delegationStatusExpiringSoon = "Expiring soon — re-authorize to keep other apps posting"

        public static let delegationStatusExpired = "Expired — other apps can no longer post as you"

        public static let delegationStatusNeverExpires = "No expiry"

        public static func delegationLastUsed(when: String) -> String {
            "Last reported use: \(when)"
        }

        public static let delegationLastUsedNever = "No use reported yet"

        public static let delegationLastUsedHint = "Reported by your nest, so treat it as a hint — check your feed for posts marked \"Via connected app\" to see what was actually written."

        public static let delegationAuthorizeButton = "Let other apps post as me"

        public static let delegationReauthorizeButton = "Re-authorize"

        public static let delegationRevokeButton = "Stop other apps posting as me"

        public static let consentHeading = "An app wants to sign in as you"

        public static func consentClient(name: String, clientId: String) -> String {
            "\(name) — \(clientId)"
        }

        public static func consentClientUnnamed(clientId: String) -> String {
            "\(clientId)"
        }

        public static func consentCode(code: String) -> String {
            "Confirmation code: \(code)"
        }

        public static let consentCodeHint = "Approve only if your browser is showing this same code."

        public static let consentScopesHeading = "It is asking to:"

        public static func consentSetHeading(title: String, nsid: String) -> String {
            "Some of that comes from “\(title)” (\(nsid)):"
        }

        public static func consentSetHeadingUnnamed(nsid: String) -> String {
            "Some of that comes from \(nsid):"
        }

        public static let consentApproveButton = "Approve"

        public static let consentDenyButton = "Deny"

        public static func errorRefresh(message: String) -> String {
            "Failed to load app credentials: \(message)"
        }

        public static func errorMint(message: String) -> String {
            "Failed to create the app credential: \(message)"
        }

        public static func errorRevoke(message: String) -> String {
            "Failed to revoke the app credential: \(message)"
        }

        public static func errorRevokeSession(message: String) -> String {
            "Failed to disconnect the app: \(message)"
        }

        public static func errorToggle(message: String) -> String {
            "Failed to change external app access: \(message)"
        }

        public static func errorAuthorize(message: String) -> String {
            "Failed to let external apps post as you: \(message)"
        }

        public static func errorDeauthorize(message: String) -> String {
            "Failed to stop external apps posting as you: \(message)"
        }

        public static let errorNoIdentity = "This app cannot authorize posting from external apps. Use another of your devices to turn it on."

        public static func errorDelegationUntrusted(message: String) -> String {
            "The stored authorization for external apps was not created by this account, so it is not being shown. (\(message))"
        }

        public static func errorSaveLocal(message: String) -> String {
            "Created, but this device could not save a copy — copy the password now, it cannot be shown again later. (\(message))"
        }

        public static func errorTransition(message: String) -> String {
            "Failed to change the Bluesky integration level: \(message)"
        }

        public static func gateReason(domain: String) -> String {
            "Hosting an AT Protocol identity needs a public domain — this nest is reachable at \"\(domain)\", which the Bluesky network cannot resolve. Claim a real domain to enable these options."
        }

        public static let gateReasonPending = "Checking whether this nest has a public domain — hosting an AT Protocol identity needs one."

        public static func cardUnlink(account: String) -> String {
            "The link to \(account) is removed. The external account itself is untouched — it keeps existing on its own server and is not migrated."
        }

        public static func cardMint(handle: String) -> String {
            "A new public identity \(handle) is created on the Bluesky network."
        }

        public static func cardReactivate(handle: String) -> String {
            "Your identity \(handle) is restored — the same identity you had before, nothing new is created."
        }

        public static let cardPublishConsent = "Your public posts become visible to everyone on the Bluesky network."

        public static let cardDeactivate = "Publishing stops and the network no longer serves your profile or posts. Your identity is kept — re-enabling restores it exactly."

        public static let cardNoRecall = "Copies of already-published posts held by other servers cannot be recalled."

        public static let cardDeletePointer = "\"Delete my Bluesky presence\" below is the separate, stronger action."

        public static let cardOpenPlane = "Third-party Bluesky apps will be able to log in as this identity once you create an app credential."

        public static let cardDmHonesty = "Bluesky direct messages are not end-to-end encrypted and pass through this nest in transit."

        public static let cardSuspendPlane = "Connected apps stop working immediately. Nothing is deleted — your app credentials stay listed, and stepping back up restores them."

        public static let deleteConfirmSweep = "Every post published to Bluesky is deleted, and the network is told to remove them."

        public static func deleteConfirmIdentityKept(handle: String) -> String {
            "Your AT Protocol identity @\(handle) is kept. This removes what you published, not who you are — turning AT Protocol hosting back on later restores the same identity."
        }

        public static let deleteConfirmAppsDisconnected = "Bluesky apps you are signed in to are disconnected, and other apps can no longer post as you."

        public static let deleteConfirmLevelOff = "Your Bluesky setting returns to Off."

        public static func deleteConfirmIdentityRetired(handle: String) -> String {
            "Your AT Protocol identity @\(handle) is also permanently retired once the deletion finishes. This cannot be undone: the identity stops existing on the Bluesky network, and no one — not you, not this nest — can ever restore it. Turning AT Protocol hosting back on later creates a new, different identity."
        }

        public static let deleteRetireUnavailableWeb = "This identity is tied to your domain, so there is no separate record to retire — it ends when your domain stops serving it."

        public static let deleteRetireUnavailableUnpublished = "This identity has not been published yet, so there is nothing to retire."

        public static func errorDeletePresence(message: String) -> String {
            "Could not delete your Bluesky presence: \(message)"
        }

        public static let errorNothingToDelete = "There is no Bluesky presence left to delete."

        public static func errorConsent(message: String) -> String {
            "Could not send your answer: \(message)"
        }

        public static let errorConsentGone = "That request is no longer waiting for an answer — it may have expired, or you may have already answered it on another device. Start the sign-in again from the app that asked."

        public static func contestDetailContestable(handle: String) -> String {
            "A change to your AT Protocol identity \(handle) was made with a key this device does not hold. You can undo it: the change and everything built on it are reversed, and afterwards only your own keys can change who controls this identity. Your posts and profile keep working through this nest — which also means it keeps the key it uses to publish them, so it could still post as you. Undoing the change does not take that key away; replacing it is a separate step."
        }

        public static func contestDetailWindowClosed(handle: String) -> String {
            "A change to your AT Protocol identity \(handle) was made with a key this device does not hold, and the time limit for undoing it has passed. The change now stands permanently. Contact whoever runs your nest."
        }

        public static func contestDetailGenesis(handle: String) -> String {
            "Your AT Protocol identity \(handle) was created with a key this device does not hold, so there is no earlier state to return it to. This identity cannot be recovered — create a new one, and contact whoever runs your nest."
        }

        public static func contestDetailUnauthenticated(handle: String) -> String {
            "The public record of your AT Protocol identity \(handle) does not check out: the changes it lists are not signed by keys this identity's own history allows. That points at the record being tampered with, or your connection to it being intercepted — so nothing has been signed or changed from here, and undoing is not offered, because acting on a false record would destroy your real history. Try again from another network, and contact whoever runs your nest."
        }

        public static func contestDeadline(hours: String) -> String {
            "About \(hours) hours left to undo this."
        }

        public static let contestDeadlineSoon = "Less than an hour left to undo this."

        public static func errorRequestContest(message: String) -> String {
            "Could not start undoing the change: \(message)"
        }

        public static let errorContestNotContestable = "This change cannot be undone from here."

        public static func contestConfirmUndo(handle: String) -> String {
            "You are about to undo the change to \(handle), and everything published on top of it, by signing an earlier state of your identity back into place."
        }

        public static let contestConfirmSigns = "This device signs with the recovery key it holds, which puts your own keys back in charge of who can change this identity. Your nest keeps the separate key it publishes your posts with, so this does not stop it posting as you; replacing that key is a separate step."

        public static let contestConfirmDirectoryRules = "The public directory decides whether to accept the undo. If it refuses, nothing about your identity changes and trying again is safe."
    }

    public enum criticalAlerts {
        public static func atprotoCustodyMismatch(handle: String) -> String {
            "Security alert: the published record of your AT Protocol identity \(handle) names a recovery key this device does not hold. Your identity may not be under your control — do not trust it for anything sensitive. You may be able to undo this yourself from Settings → AT Protocol, and there is a time limit; you can also contact whoever runs your nest."
        }

        public static func atprotoHandleUnbound(domain: String, published: String) -> String {
            "Security alert: the published record of your AT Protocol identity does not name a handle at \(domain). It is published as \(published) instead — people looking for you may be finding someone else. Do not trust this identity for anything sensitive, and contact whoever runs your nest."
        }

        public static func atprotoHandleUnboundNone(domain: String) -> String {
            "Security alert: the published record of your AT Protocol identity names no handle at all, so nobody can find you at \(domain). Do not trust this identity for anything sensitive, and contact whoever runs your nest."
        }

        public static let recoveryReplacementPending = "Security alert: someone used your identity secret to request a replacement of your account recovery key. If that was not you, someone else has your identity secret. Cancel it in Settings, under Recovery kit."

        public static func recoveryReplacementPendingDetail(days: String, fingerprint: String) -> String {
            "The replacement takes effect in \(days) days unless cancelled. The pending recovery key begins \(fingerprint) — if you hold a recovery kit and it does not begin with those characters, the request was not made with your kit."
        }

        public static func domainExpiringAdmin(domain: String, days: String) -> String {
            "Urgent: \(domain) — the name this deployment runs on — expires in \(days) days. If it lapses, whoever registers it next receives your mail (including password resets for accounts tied to those addresses), and anyone recovering an account from their recovery phrase alone will no longer be able to find this nest. Renew it at your registrar now."
        }

        public static func domainExpiringResident(domain: String, days: String) -> String {
            "Urgent: \(domain) — the name this deployment runs on — expires in \(days) days. If it lapses, mail sent to your address there will go to whoever registers the name next, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address."
        }

        public static func domainExpiredAdmin(domain: String) -> String {
            "Urgent: \(domain) — the name this deployment runs on — has expired. Whoever registers it next receives your mail, including password resets for accounts tied to those addresses. Renew or redeem it at your registrar immediately; if it is gone, move the deployment to a new domain."
        }

        public static func domainExpiredResident(domain: String) -> String {
            "Urgent: \(domain) — the name this deployment runs on — has expired. Mail sent to your address there may now reach someone else, and recovering your account from your recovery phrase alone will not work. Contact whoever runs this nest, and make sure you know its direct address."
        }

        public static func domainLapsingAdmin(domain: String, status: String) -> String {
            "Urgent: the registration for \(domain) — the name this deployment runs on — is in a hold or deletion state (\(status)) and is being withdrawn from DNS. Whoever registers it next receives your mail. Contact your registrar now; renewal is usually still possible at this stage."
        }

        public static func domainLapsingResident(domain: String, status: String) -> String {
            "Urgent: the registration for \(domain) — the name this deployment runs on — is being withdrawn (\(status)). Mail to your address there will stop arriving, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address."
        }
    }

    public enum bridges {
        public static let title = "Bridges"

        public static let detailTitle = "Bridge"

        public static let follows = "Follows"

        public static let removeFollow = "Remove Follow"

        public static let refreshList = "Refresh bridge list"

        public static let linkAction = "Link"

        public static let unlinkAction = "Unlink"

        public static let unlinkConfirmMsg = "This will disconnect the bridge. You can re-link it later."

        public static let noFollows = "No follows yet."

        public static func notAvailable(name: String) -> String {
            "\(name) not available on this nest."
        }

        public static let unsafeRedirect = "This nest returned an unsafe link (links must be https). Not following it."

        public static func unlink(name: String) -> String {
            "Unlink \(name)"
        }

        public static func link(name: String) -> String {
            "Link \(name)"
        }

        public static let noLinkMethod = "This bridge has no link method available right now."

        public static let idToFollow = "ID to follow"

        public static let idToFollowPlaceholder = "e.g. did:plc:... or user.bsky.social"

        public static let petnameOptional = "Petname (optional)"

        public static let notAvailableNode = "Not available on this node"

        public static let loadingBridges = "Loading bridges..."

        public static let linkMethod = "Link method"

        public static let markAllRead = "Mark All Read"

        public static let noNotifications = "No notifications"

        public static let linkStatus = "Link Status"

        public static let statusUnavailable = "Unavailable"

        public static let linkBridge = "Link Bridge"

        public static let unlinkBridge = "Unlink Bridge"

        public static let bridgeSettingsDesc = "Configure bridge-specific settings."

        public static let noSettings = "No settings available."

        public static let noFollowsConfigured = "No follows configured."

        public static let addFollow = "Add Follow"

        public static let noBridges = "No bridges available."

        public static let noBridgesDesc = "Bridges connect your nest to other networks."

        public static let selectBridge = "Select a bridge"

        public static let selectBridgeDesc = "Select a bridge to view details."

        public static let id = "ID"

        public static let petname = "Petname"

        public static let port = "Port"

        public static let password = "Password"

        public static let subscribe = "Subscribe"

        public static let handle = "Handle"

        public static let or = "or"

        public static let friendlyNamePlaceholder = "Friendly name"

        public static let sourceBlocked = "This account can only add sources your guardian approves."

        public static let sourceRequestButton = "Ask your guardian"

        public static let sourceRequestPending = "Asked — waiting for your guardian"

        public static let sourceRequestApproved = "Approved — try again"
    }

    public enum media {
        public static let title = "Media"

        public static let viewList = "List"

        public static let viewGrid = "Grid"

        public static let sortName = "Name"

        public static let sortSize = "Size"

        public static let sortDate = "Date"

        public static let sortAscending = "Ascending"

        public static let sortDescending = "Descending"

        public static let filterAll = "All media"

        public static let noMediaYet = "No media yet"

        public static let chooseFile = "Choose file…"

        public static let typeFilePath = "Type a file path…"

        public static let filePathRequired = "Type the path to a file first, then press Upload."

        public static let fileRequired = "Choose a file first, then press Upload."

        public static let fileNotFound = "That file is no longer in your folders."

        public static let uploadFailed = "Upload failed"

        public static let upload = "Upload"

        public static func uploading(progress: String) -> String {
            "Uploading... \(progress)%"
        }

        public static let noFolders = "No folders available. Configure a sync source first."

        public static let sourceOnline = "Files reachable"

        public static let sourceOffline = "Files unreachable"

        public static let sourceOfflineNotice = "Files unreachable — no device holding them is connected"

        public static let watchedDirectories = "Watched Directories"

        public static let addFile = "Add File"

        public enum fileDetail {
            public static let deleteFile = "Delete File"

            public static func deleteConfirm(name: String) -> String {
                "Are you sure you want to delete \"\(name)\"? This cannot be undone."
            }

            public static let deleteConfirmTitle = "Delete this file?"

            public static let deleteConfirmButton = "Delete"
        }

        public enum statusLabel {
            public static let synced = "Synced"

            public static let uploading = "Uploading"

            public static let downloading = "Downloading"

            public static let conflict = "Conflict"

            public static let localOnly = "Local Only"

            public static let remoteOnly = "Remote Only"
        }

        public enum watched {
            public static let scanNow = "Scan Now"

            public static let scanning = "Scanning..."

            public static let noWatched = "No watched directories"

            public static let addDirectory = "Add Watched Directory"

            public static let removeDirectory = "Remove directory"

            public static let directoryLabel = "Directory"

            public static func errorRead(directory: String, message: String) -> String {
                "Failed to read \(directory): \(message)"
            }

            public static func errorUpload(directory: String, message: String) -> String {
                "Failed to upload \(directory): \(message)"
            }

            public static let targetFolder = "Back up into"

            public static let noFolders = "You don't have a folder yet. Create one under Settings → Folders first."

            public static func errorFolders(message: String) -> String {
                "Failed to load your folders: \(message)"
            }
        }

        public static func errorRefresh(message: String) -> String {
            "Failed to load media: \(message)"
        }

        public static func errorUpload(message: String) -> String {
            "Failed to upload: \(message)"
        }

        public static func errorDelete(message: String) -> String {
            "Failed to delete: \(message)"
        }

        public static let errorNoSet = "You don't have a folder to upload into yet. Create a folder under Settings → Folders first."

        public static func errorRestore(message: String) -> String {
            "Failed to restore version: \(message)"
        }

        public static let errorFollowedUnavailable = "This folder is no longer shared publicly. Its owner may have stopped sharing it, or removed it."

        public static func errorFollowedFetch(message: String) -> String {
            "Couldn't read that followed folder: \(message)"
        }

        public static let errorFollowedReadOnly = "You follow this folder — it's read-only here. Switch to one of your own folders to upload."

        public static let errorMetadataOnlyFolder = "This folder's content stays on your devices, so it can't be uploaded here. Put the file in the folder on a device that syncs it."

        public static let versionsTitle = "Version history"

        public static let versionsLoading = "Loading versions…"

        public static func versionsError(message: String) -> String {
            "Failed to load versions: \(message)"
        }

        public static func versionAuthor(author: String) -> String {
            "Edited by \(author)"
        }

        public static let versionRestore = "Restore"

        public static let versionsShowPruned = "Show recently pruned"

        public static let versionPrunedBadge = "Pruned"

        public static let versionUndelete = "Recover"

        public static func errorUndelete(message: String) -> String {
            "Failed to recover version: \(message)"
        }

        public static let restoreConfirmTitle = "Restore this version?"

        public static let restoreConfirmBody = "The file will return to this version on all your devices. The current version stays in the history."

        public static let restoreConfirm = "Restore"

        public static let restoreCancel = "Cancel"

        public static let detailClose = "Close"

        public static let download = "Download"

        public static func errorDownload(message: String) -> String {
            "Failed to download: \(message)"
        }

        public static let externalOpen = "Open externally"

        public static func externalOpenConfirmTitle(name: String) -> String {
            "Open \"\(name)\" externally?"
        }

        public static let externalOpenConfirmBody = "The clip is decrypted to a private temporary file and handed to your system's media player."

        public static let externalOpenConfirm = "Open"

        public static let externalOpenCancel = "Cancel"

        public static func errorExternalOpen(message: String) -> String {
            "Failed to open externally: \(message)"
        }
    }

    public enum backups {
        public static let title = "Backups"

        public static let backupNow = "Backup Now"

        public static func errorRefresh(message: String) -> String {
            "Failed to load backups: \(message)"
        }

        public static func errorCreateSnapshot(message: String) -> String {
            "Failed to create snapshot: \(message)"
        }

        public static func errorDeleteSnapshot(message: String) -> String {
            "Failed to delete snapshot: \(message)"
        }

        public static func errorDeleteSnapshotImmediate(message: String) -> String {
            "Failed to delete snapshot immediately: \(message)"
        }

        public static func errorUndeleteSnapshot(message: String) -> String {
            "Failed to recover snapshot: \(message)"
        }

        public static func errorPrune(message: String) -> String {
            "Failed to apply the retention policy: \(message)"
        }

        public static func errorCheck(message: String) -> String {
            "Failed to run the integrity check: \(message)"
        }

        public static func errorDetail(message: String) -> String {
            "Failed to open the snapshot: \(message)"
        }

        public static let errorDownloadManifest = "This file's content address could not be read, so it cannot be downloaded."

        public static let lastBackedUp = "Last backed up:"

        public static let loadingSnapshots = "Loading snapshots..."

        public static let noSnapshots = "No snapshots yet."

        public static func snapshot(id: String) -> String {
            "Snapshot #\(id)"
        }

        public static let folder = "Folder"

        public static func fileCount(count: String) -> String {
            "\(count) files"
        }

        public static let file = "File"

        public static let date = "Date"

        public static let download = "Download"

        public static let noSnapshotsDesc = "Snapshots are created when you back up files"

        public static let snapshotTitle = "Snapshot"

        public static let selectSnapshot = "Select a snapshot to view files."

        public static let noFilesInSnapshot = "No files in this snapshot."

        public static let createSnapshot = "Create Snapshot"

        public static let snapshotsTitle = "Snapshots"

        public static let lastBackedUpNever = "Last backed up: never"

        public static func lastBackedUpAt(when: String) -> String {
            "Last backed up: \(when)"
        }

        public static let noFolders = "No folders yet — create one under Settings → Folders."

        public static let folderLabel = "Folder"

        public static func snapshotRow(id: String, when: String, files: String, size: String) -> String {
            "#\(id) · \(when) · \(files) · \(size)"
        }

        public static func snapshotStateDeletionPending(when: String) -> String {
            "Deletion scheduled — cancel before \(when)"
        }

        public static func snapshotStateSoftDeleted(when: String) -> String {
            "Deleted — recoverable until \(when)"
        }

        public static let snapshotStateDeletionPendingUndated = "Deletion scheduled"

        public static let snapshotStateSoftDeletedUndated = "Deleted — still recoverable"

        public static let snapshotIntegrityOk = "integrity verified"

        public static let snapshotIntegrityImplicated = "integrity problem found in this snapshot"

        public static let snapshotDeleteButton = "Delete"

        public static let snapshotUndeleteButton = "Recover"

        public static let pruneButton = "Apply retention policy"

        public static let prunePreviewTitle = "Retention policy preview"

        public static func prunePreviewCounts(wouldPrune: String, remaining: String) -> String {
            "\(wouldPrune) would be deleted, \(remaining) kept."
        }

        public static let prunePreviewNothing = "Nothing to prune — every snapshot is within this set's retention policy."

        public static func prunePreviewCandidate(id: String, when: String) -> String {
            "#\(id) · \(when)"
        }

        public static let prunePolicyNotSet = "No retention policy configured for this set. Set one on the Folders page."

        public static let prunePolicyUnparseable = "This set's retention policy could not be read, so nothing was pruned. Re-set it on the Folders page."

        public static let pruneExecuteButton = "Delete them"

        public static let pruneCancelButton = "Cancel"

        public static let checkButton = "Check integrity"

        public static func checkResultOk(snapshots: String, files: String, chunks: String) -> String {
            "Integrity check passed — \(snapshots) snapshots, \(files) files, \(chunks) chunks verified."
        }

        public static func checkResultErrors(missingManifests: String, missingChunks: String, corruptManifests: String) -> String {
            "Integrity check found problems: \(missingManifests) missing manifests, \(missingChunks) missing chunks, \(corruptManifests) corrupt manifests."
        }

        public static let busyCreate = "Creating a snapshot…"

        public static let busyDelete = "Deleting a snapshot…"

        public static let busyImmediateDelete = "Deleting a snapshot immediately…"

        public static let busyUndelete = "Recovering a snapshot…"

        public static let busyPrune = "Applying the retention policy…"

        public static let busyCheck = "Checking integrity…"

        public static let busyRefresh = "Loading…"

        public static let noBackups = "No backups yet."

        public static let restore = "Restore"

        public static let restoreComplete = "Restore Complete"

        public static let restoreFailed = "Restore Failed"

        public static let startRestore = "Start Restore"

        public static let restoreSectionTitle = "Restore history"

        public static let restoreLocalTitle = "Restore from a local snapshot"

        public static let restoreSourceLocal = "local snapshot"

        public static func restoreHistoryRow(kinds: String, source: String, when: String) -> String {
            "\(kinds) from \(source) — \(when)"
        }

        public static let restoreKindsMail = "mail"

        public static let restoreKindsCalendar = "calendar"

        public static let restoreConfirmPlaceholder = "Re-type the snapshot id to confirm"

        public static let restoreConfirmButton = "Restore"

        public static let restoreProgressIdle = "Select a snapshot and re-type its id to restore."

        public static let restoreProgressRunning = "Restoring…"

        public static let restoreProgressDone = "Done — restart the bridge."

        public static let restoreWarningConfigAbsent = "Restored, but the bridge's sign-in keys were not part of the restore — the bridge can't sign in after it restarts until they are restored too."

        public static let restoreNoSnapshots = "No local snapshots available to restore."

        public static let restoreSourceLabel = "Backup destination"

        public static let restoreSnapshotLabel = "Snapshot"

        public static let restoreNoDestinations = "No backup destinations yet — add one under \"Backup destinations\" above."

        public static func restoreDivergenceBanner(count: String) -> String {
            "\(count) MUAs reconnected with newer state"
        }

        public static let restoreDivergenceModalTitle = "Restore divergence (forensic)"

        public static func restoreDivergenceDetailRow(collection: String, mua: String, client: String, server: String, lost: String) -> String {
            "\(collection) · \(mua) · client modseq \(client) / server modseq \(server) · ~\(lost) writes lost"
        }

        public static let restoreDivergenceUnknownMua = "(unknown)"

        public static let restoreDivergenceFooter = "Lost writes cannot be recovered — they died with the source nest. This list is forensic."

        public static let restoreDivergenceClose = "Close"

        public static let immediateDeleteButton = "Delete now"

        public static func immediateDeleteModalTitle(id: String) -> String {
            "Delete snapshot #\(id) immediately?"
        }

        public static let immediateDeleteWarning = "This permanently deletes the snapshot now, skipping the soft-delete window, and cannot be undone. At least three snapshots are always kept."

        public static let immediateDeleteConfirmIdPlaceholder = "Re-type the snapshot id to confirm"

        public static let immediateDeleteAcknowledgePrompt = "Type this exact phrase to confirm:"

        public static let immediateDeleteAcknowledgePlaceholder = "Acknowledgement phrase"

        public static let immediateDeleteConfirmButton = "Delete immediately"

        public static let immediateDeleteCancelButton = "Cancel"

        public static let backupDestinationsTitle = "Backup destinations"

        public static let backupDestinationsDesc = "Replicate your data to another nest you control. Chunks are sealed under your backup key — the destination never reads them."

        public static let backupDestinationsEmpty = "No backup destinations configured."

        public static let backupDestinationAddButton = "Add destination"

        public static let backupDestinationFormAddTitle = "Add a backup destination"

        public static let backupDestinationFormEditTitle = "Edit backup destination"

        public static let backupDestinationUrlPlaceholder = "Destination nest URL (https://…)"

        public static let backupDestinationNamePlaceholder = "Friendly name (optional)"

        public static let backupDestinationAddConfirm = "Save"

        public static let backupDestinationAddCancel = "Cancel"

        public static let backupDestinationEditButton = "Edit"

        public static let backupDestinationRemoveButton = "Remove"

        public static let backupDestinationUnattestedMark = "Added before you recovered this account — still backing up. Keep it, or remove it if you don't recognise it."

        public static let backupDestinationKeepButton = "Keep"

        public static let backupDestinationRemoveConfirmTitle = "Remove this backup destination?"

        public static let backupDestinationRemoveConfirmButton = "Remove"

        public static let backupDestinationRemoveCancelButton = "Cancel"

        public static let backupDestinationEditDifferentNest = "That URL points to a different nest. Remove this destination and add the new one."

        public static let backupDestinationsFull = "This box's backup list is full. Remove a destination or a covered folder, then try again."

        public static let backupDestinationResolving = "Resolving destination…"

        public static let backupDestinationLastUploadNever = "Last synced: never"

        public static func backupDestinationLastUpload(when: String) -> String {
            "Last synced: \(when)"
        }

        public static func backupDestinationBacklog(count: String) -> String {
            "\(count) queued"
        }

        public static let backupDestinationLastAuditNever = "Last checked: never"

        public static func backupDestinationLastAudit(when: String) -> String {
            "Last checked: \(when)"
        }

        public static func backupAuditAlertFreshness(destination: String, days: String) -> String {
            "\(destination) is \(days) days behind your data. Your backup there is not keeping up."
        }

        public static func backupAuditAlertInclusion(destination: String, missing: String, sampled: String) -> String {
            "\(destination) is missing \(missing) of \(sampled) records we checked for. Your backup there is incomplete."
        }

        public static func backupAuditAlertOverdue(destination: String, days: String) -> String {
            "\(destination) has not been checked for \(days) days. We cannot confirm your backup there is intact."
        }

        public static let backupDestinationLastSelfAuditNever = "Self-checked: not yet"

        public static func backupDestinationLastSelfAudit(when: String) -> String {
            "Self-checked: \(when)"
        }

        public static func backupAuditAlertSelfReported(destination: String) -> String {
            "\(destination) reports that its own copy of your data failed its check. That copy cannot be relied on."
        }

        public static func backupAuditAlertSourceRegressed(destination: String, days: String) -> String {
            "\(destination) still holds data your nest lost when it went back to an older copy. It will be kept there for about \(days) more days — ask whoever restored your nest whether a newer copy exists."
        }

        public static func backupAuditAlertSourceRegressedUntilRecovered(destination: String) -> String {
            "\(destination) still holds data your nest lost when it went back to an older copy. It will be kept there until it is recovered — ask whoever restored your nest whether a newer copy exists."
        }

        public static let backupDestinationKindNest = "Another nest"

        public static let backupDestinationKindClientDevice = "This device"

        public static func backupDestinationKindUnknown(kind: String) -> String {
            "Unsupported destination (\(kind))"
        }

        public static let backupDestinationKindSelectLabel = "Where should the copy live?"

        public static let backupDestinationCapacityPlaceholder = "Storage limit, e.g. 50 GB"

        public static let backupDestinationCapacityInvalid = "Enter a storage limit like \"50 GB\" or \"500 MB\"."

        public static func backupDestinationUsage(held: String, cap: String) -> String {
            "\(held) of \(cap) used"
        }

        public static func backupDestinationUsageCapReached(held: String, cap: String) -> String {
            "\(held) of \(cap) used — full, older copies are being dropped"
        }

        public static func backupDestinationUsageUncapped(held: String) -> String {
            "\(held) held, no limit set"
        }

        public static let backupDestinationUsageUnknown = "Nothing held yet"

        public static let backupSoleClientDestinationWarning = "Every backup destination you have is one of your own devices. Devices get lost, wiped and replaced — add a nest destination so a copy lives somewhere else."

        public static let backupDestinationCustodianExposure = "This device will keep a complete offline copy of your data. Anyone who can unlock it can read all of it, not just what is on screen."

        public static func backupOrphanedStoreRow(held: String) -> String {
            "This device is still holding \(held) of a backup copy. No destination uses it any more."
        }

        public static let backupDestinationReclaimButton = "Free up this space"

        public static let backupReclaimConfirmTitle = "Delete this device's backup copy?"

        public static let backupReclaimConfirmBody = "This device can currently restore your data on its own, with no nest reachable. Deleting the copy ends that. You can rebuild it by making this device a backup destination again."

        public static let backupReclaimConfirmButton = "Delete the copy"

        public static let backupReclaimCancelButton = "Keep it"

        public static let backupReclaimStillHosting = "This device is still backing up right now, so nothing was deleted. Try again in a moment."

        public static let backupDestinationReseedButton = "Restore my data to this nest"

        public static let backupReseedConfirmTitle = "Restore this device's copy to this nest?"

        public static let backupReseedConfirmBody = "This copies the backup this device holds onto this nest and makes it live again. Nothing is deleted, here or on the nest. If this nest already holds your data, it stops rather than mixing the two."

        public static let backupReseedConfirmButton = "Restore"

        public static let backupReseedCancelButton = "Not now"

        public static let backupReseedRunning = "Restoring your data to this nest…"

        public static let backupReseedNoAgent = "This device runs no backup service, so it holds no copy to restore from."

        public static func backupReseedFailed(reason: String) -> String {
            "The restore stopped before anything was made live: \(reason)"
        }

        public static func backupReseedReenrollFailed(reason: String) -> String {
            "Your data is back on this nest, but this device could not sign up again as its backup: \(reason). Add this device as a backup destination to keep a copy here."
        }

        public static let reseedResultWhole = "Your data is back on this nest."

        public static let reseedResultIncomplete = "The restore is not complete yet. Run it again to finish what is missing."

        public static let reseedSetMail = "Mail"

        public static func reseedSetRestored(set: String, count: String) -> String {
            "\(set): \(count) restored"
        }

        public static func reseedSetAlreadyRestored(set: String) -> String {
            "\(set): already restored"
        }

        public static func reseedSetRefused(set: String, remedy: String) -> String {
            "\(set): not restored. \(remedy)"
        }

        public static let reseedRefusedTargetNotFresh = "A folder with this name is already shared or published. Rename it or clear that setting, then run the restore again."

        public static let reseedRefusedCustodyIncomplete = "The copy on this nest is not complete yet. Run the restore again."

        public static let reseedRefusedCustodyUnsealed = "Some files arrived without their names. Run the restore again after this device's next backup."

        public static let reseedRefusedQuotaExceeded = "This nest does not have enough storage for the copy. An admin can raise the limit in the admin app."

        public static let reseedRefusedNotEnrolled = "This nest does not hold your backup key. Run the restore again."

        public static let reseedRefusedFolderUnnamed = "This device does not know the folder's name, so it was left on the nest without being restored."

        public static let reseedRefusedTargetMissing = "The folder to restore into could not be created on this nest. Run the restore again."

        public static let reseedRefusedRehomeUnsigned = "This device could not sign the folder's files for the restore, so it was left on the nest without being restored. Sign in on this device again, then run the restore again."

        public static let reseedRefusedOther = "The nest refused it."

        public static func reseedGapSidecarlessSegments(count: String) -> String {
            "\(count) parts of your mail arrived without their index, so some mail is still missing. Run the restore again after this device's next backup."
        }

        public static func reseedGapUnnamedFiles(count: String) -> String {
            "\(count) files arrived without their names, so some files are still missing."
        }

        public static let backupDestinationRemoveReclaimCheckbox = "Also delete this device's copy now"

        public static func backupReclaimAfterRemoveFailed(reason: String) -> String {
            "The destination was removed, but this device's copy could not be deleted: \(reason)"
        }

        public static let backupReclaimNoAgent = "This device is not running a sync agent, so it holds no backup copy to delete."

        public static let chooseDestination = "Choose Destination..."

        public static let noDestination = "No destination selected"

        public static let revealInFinder = "Reveal in Finder"

        public static let aboutRestore = "About Restore"

        public static let verifyIntegrity = "Verify Integrity"

        public static let retentionAndPrune = "Retention & Prune"

        public static let statistics = "Statistics"

        public static let syncConflicts = "Sync Conflicts"

        public static let allDevices = "All Devices"

        public static let deleteSnapshot = "Delete Snapshot"

        public static let deleteSnapshotConfirm = "Delete this snapshot? This action cannot be undone."

        public static let pruneSnapshots = "Prune Old Snapshots"

        public static let pruneConfirm = "Prune old snapshots? Only the latest 3 will be kept."

        public static func pruneResult(count: String) -> String {
            "Pruned \(count) snapshot(s)."
        }

        public static let checkIntegrity = "Check Integrity"

        public static let integrityPassed = "Integrity check passed."

        public static let integrityFailed = "Integrity check found errors:"

        public enum integrityCheck {
            public static let title = "Backup Integrity Check"

            public static func description(folder: String) -> String {
                "Verify that all snapshots, manifests, and chunks are intact for \"\(folder)\"."
            }

            public static let options = "Options"

            public static let verifyContent = "Verify content (slower, more thorough)"

            public static let results = "Results"

            public static let snapshotsChecked = "Snapshots Checked"

            public static let filesChecked = "Files Checked"

            public static let manifestsChecked = "Manifests Checked"

            public static let chunksChecked = "Chunks Checked"

            public static let issues = "Issues"

            public static let missingManifests = "Missing Manifests"

            public static let missingChunks = "Missing Chunks"

            public static let corruptManifests = "Corrupt Manifests"

            public static let startCheck = "Start Check"

            public static let allOk = "All OK"

            public static func errorsFound(count: String) -> String {
                "\(count) errors found"
            }
        }

        public enum retention {
            public static let keepLast = "Keep Last"

            public static let keepDaily = "Keep Daily"

            public static let keepWeekly = "Keep Weekly"

            public static let keepMonthly = "Keep Monthly"

            public static let keepYearly = "Keep Yearly"

            public static let prunePreview = "Prune Preview"

            public static let wouldPrune = "Would Prune"

            public static let wouldKeep = "Would Keep"

            public static let preview = "Preview"

            public static func keepLastCount(count: String) -> String {
                "Keep Last: \(count)"
            }

            public static func keepDailyCount(count: String) -> String {
                "Keep Daily: \(count)"
            }

            public static func keepWeeklyCount(count: String) -> String {
                "Keep Weekly: \(count)"
            }

            public static func keepMonthlyCount(count: String) -> String {
                "Keep Monthly: \(count)"
            }

            public static func keepYearlyCount(count: String) -> String {
                "Keep Yearly: \(count)"
            }

            public static let pruneNow = "Prune Now"
        }

        public static func retentionTitle(name: String) -> String {
            "Retention Policy — \(name)"
        }

        public enum repoStats {
            public static let title = "Repository Statistics"

            public static let totalFiles = "Total Files"

            public static let rawSize = "Raw Size"

            public static let storedSize = "Stored Size"

            public static let dedupRatio = "Dedup Ratio"

            public static let storageBackend = "Storage Backend"

            public static let encryption = "Encryption & Compression"

            public static let encryptionAlgo = "ChaCha20-Poly1305"

            public static let compression = "zstd level 3"

            public static let chunkSizes = "512 KB - 16 MB avg 4 MB"

            public static let loadingStats = "Loading statistics..."
        }

        public enum detail {
            public static let snapshotId = "Snapshot ID"

            public static let created = "Created"

            public static let deviceId = "Device ID"

            public static let tags = "Tags"

            public static let parentId = "Parent ID"

            public static let none = "None"

            public static let fileCount = "File Count"

            public static let totalSize = "Total Size"
        }

        public enum diff {
            public static let title = "Compare"

            public static let compareWith = "Compare with:"

            public static let selectSnapshot = "Select snapshot..."

            public static let compare = "Compare"

            public static let noComparison = "No Comparison"

            public static let noComparisonDesc = "Select a snapshot and tap Compare to view differences."

            public static func addedCount(count: String) -> String {
                "+\(count) added"
            }

            public static func removedCount(count: String) -> String {
                "-\(count) removed"
            }

            public static func modifiedCount(count: String) -> String {
                "~\(count) modified"
            }

            public static func net(size: String) -> String {
                "Net: \(size)"
            }

            public static let added = "Added"

            public static let removed = "Removed"

            public static let modified = "Modified"
        }

        public enum notification {
            public static let viewBackups = "View Backups"

            public static let completeTitle = "Backup Complete"

            public static func completeBody(folder: String, count: String, size: String) -> String {
                "\(folder): \(count) files (\(size))"
            }

            public static let failedTitle = "Backup Failed"

            public static func failedBody(folder: String, error: String) -> String {
                "\(folder): \(error)"
            }
        }

        public static let restoreIntoDirectoryDesc = "Restores every file in this snapshot into the chosen directory."

        public static func restoreFilesWritten(count: String, path: String) -> String {
            "Restored \(count) files to \(path)"
        }

        public static func restoreAboutDetail(id: String) -> String {
            "Fetches every file in snapshot #\(id), decrypts it on this device, and writes it into the chosen directory."
        }

        public static let restoreDirectoryPanelMessage = "Choose a directory to restore this snapshot into"

        public static func snapshotCount(count: String) -> String {
            "\(count) snapshots"
        }

        public static let statusTitle = "Backup Status"

        public static func lastSeq(seq: String) -> String {
            "Last Seq: \(seq)"
        }
    }

    public enum folders {
        public static let title = "Folders"

        public static let photoLibrarySection = "Photo Library"

        public static let syncedLocations = "Synced Locations"

        public static let noLocationsBound = "No locations bound to this set."

        public static let locationPathPlaceholder = "Location path"

        public static let bindLocation = "Bind Location"

        public static let choose = "Choose…"

        public static func deletesHeld(count: String) -> String {
            "This folder looks empty. Deletions held: \(count). Reconnect the folder, or apply them to your nest."
        }

        public static func applyDeletes(count: String) -> String {
            "Apply held deletions (\(count))"
        }

        public static func unreadable(count: String) -> String {
            "Fauna couldn't read \(count) items in this folder, so it has stopped syncing them. Check that the drive is connected and that Fauna can open the folder."
        }

        public static let keepSyncingWhenLoggedOut = "Keep syncing when logged out"

        public static let keepSyncingHelpOff = "The sync agent only runs while you are logged in. Turn this on to keep it syncing on this machine after you disconnect."

        public static let keepSyncingHelpOn = "The sync agent keeps running on this machine after you log out."

        public static let offlineShareSection = "Share with someone next to you"

        public static let offlineShareStart = "Share a folder"

        public static let offlineShareReceive = "Receive a folder"

        public static let offlineShareOwnCodeLabel = "Your code"

        public static let offlineShareOwnCodeHelp = "Give this to the person next to you — read it out, or let them read it off your screen — and check that what they type back matches. It ends with where your device can be reached, so copy all of it. Never send it in a message: exchanging it in person is what makes it safe."

        public static let offlineSharePeerCodeLabel = "Their code"

        public static let offlineShareBegin = "Begin sharing"

        public static let offlineShareExpect = "Ready to receive"

        public static let offlineShareStatusIdle = "Not started"

        public static let offlineShareStatusExpecting = "Waiting for their invitation…"

        public static let offlineShareStatusOfferSent = "Invitation sent"

        public static let offlineShareStatusAwaitingConsent = "Waiting for them to accept…"

        public static let offlineShareStatusDelivering = "Setting up the shared folder…"

        public static let offlineShareStatusDelivered = "Shared"

        public static let offlineShareStatusAdmitted = "Joined"

        public static let offlineShareStatusFailed = "Did not finish"

        public static func offlineShareFrom(who: String, code: String) -> String {
            "\(who) wants to share a folder with you (\(code))"
        }

        public static func offlineShareSet(code: String) -> String {
            "Shared folder \(code)"
        }

        public static let offlineShareCodeMalformed = "That does not look like a code. It should be 64 letters and numbers."

        public static let offlineShareCodeOwn = "That is this device's own code — type the other person's."

        public static func errorOfflineShare(message: String) -> String {
            "Sharing did not finish: \(message)"
        }

        public static let shareTransferSection = "Peer transfers"

        public static let shareServeStatusOff = "Peer transfers are off — this nest does not enable them"

        public static let shareServeStatusParticipationOff = "Peer transfers are off on this device"

        public static let shareServeStatusNoSets = "No shared folders to serve"

        public static func shareServeStatusServing(count: String) -> String {
            "Serving \(count) shared folder(s) to members"
        }

        public static func shareTransferPeerRow(folder: String, who: String) -> String {
            "\(folder) — \(who)"
        }

        public static func shareTransferProgress(files: String, rows: String) -> String {
            "\(files) file(s), \(rows) change(s) this pass"
        }

        public static let shareTransferStateAdmissionPending = "Waiting to be admitted"

        public static let shareTransferStatePulling = "Receiving"

        public static let shareTransferStateUpToDate = "Up to date"

        public static func shareTransferStateLimited(source: String) -> String {
            "Limited by \(source)"
        }

        public static let shareTransferSourceFreeSpace = "free space on this device"
    }

    public enum devices {
        public static let title = "Devices"

        public static let myDevices = "My Devices"

        public static let noDevices = "No devices registered. Sign in to Fauna on a device to register it."

        public static let noFolders = "No folders created."

        public static let copyActorId = "Copy ID"

        public static let online = "Online"

        public static let offline = "Offline"

        public static func placeTwo(first: String, second: String) -> String {
            "\(first) · \(second)"
        }

        public static func placeThree(first: String, second: String, third: String) -> String {
            "\(first) · \(second) · \(third)"
        }

        public static let placeNone = "Doesn't send or receive changes"

        public static let lastSeen = "Last seen"

        public static let guardianMarkedBadge = "Guardian device"

        public static let thisDeviceBadge = "This device"

        public static let p2pParticipationOwn = "Peer transfers on this device"

        public static let p2pParticipation = "Peer transfers"

        public static let p2pParticipationUnreported = "Peer transfers (not reported yet)"

        public static let p2pParticipationOffRequested = "Peer transfers (turning off)"

        public static func ownFingerprint(fingerprint: String) -> String {
            "Key \(fingerprint)"
        }

        public static let membersTitle = "Signed-in devices without a matching entry"

        public static func memberFingerprint(fingerprint: String) -> String {
            "Device \(fingerprint)"
        }

        public static func memberEnrolledAt(when: String) -> String {
            "Says it signed in \(when)"
        }

        public static let memberNote = "Removing a device here is permanent: there is no undo, and it must sign in again from scratch. A listed device is not necessarily a problem — one that has signed in but not yet registered with the server appears here until it does. Before removing one, compare its fingerprint with the devices you still have: each shows its own on its Devices page, and the one to remove matches none of them. If two appear where you expected one, one of them is a device you hold."

        public static let memberRemoveConfirm = "Yes, remove it permanently"

        public static let folders = "Folders"

        public static let addFolder = "Add Folder"

        public static let retention = "Retention"

        public static let enrolledDevices = "Enrolled Devices"

        public static func enrolledDevicesCount(count: String) -> String {
            "Enrolled Devices (\(count))"
        }

        public static func foldersCount(count: String) -> String {
            "Folders (\(count))"
        }

        public static let loadingMembers = "Loading members..."

        public static let noDevicesEnrolled = "No devices enrolled."

        public static let deviceActivity = "Device activity"

        public static let colChanges = "Changes"

        public static let noDeviceActivity = "No recorded activity yet."

        public static let selectiveSync = "Selective Sync"

        public static let includePaths = "Include Paths (comma-separated)"

        public static let excludePaths = "Exclude Paths (comma-separated)"

        public static let savePaths = "Save Paths"

        public static let nestPlaceSection = "What your nest keeps"

        public static let nestSnapshots = "Keep snapshots"

        public static let nestSnapshotsDefaultLabel = "Use the default"

        public static let nestSnapshotsOnLabel = "Keep snapshots"

        public static let nestSnapshotsOffLabel = "Don't keep snapshots"

        public static let nestQuiet = "Wait for quiet (seconds)"

        public static let nestRetentionSnapshots = "Keep at most (snapshots)"

        public static let nestRetentionDays = "Keep for at most (days)"

        public static let versionRetentionCount = "Keep at most (versions per file)"

        public static let versionRetentionDays = "Keep versions for at most (days)"

        public static let nestPlaceBlankHint = "Leave a box empty to use the default. Empty is a choice — saving applies every box together."

        public static let nestSave = "Save Nest Settings"

        public static let keylessPostureBadge = "Relay only — holds no keys"

        public static let custodyHolderSection = "Custodians — who holds sealed copies of your data"

        public static let custodyHolderScope = "Trusted to hold sealed copies — cannot read them"

        public static func custodyReceiptFresh(when: String) -> String {
            "Last confirmed \(when)"
        }

        public static func custodyReceiptStale(when: String) -> String {
            "Stale — last confirmed \(when). Treat this copy as degraded."
        }

        public static let custodyReceiptNone = "No confirmation yet"

        public static func custodyHeldBytes(held: String, cap: String) -> String {
            "Holding \(held) of \(cap)"
        }

        public static let custodyRevoke = "Stop trusting this custodian"

        public static let custodyRevokeBoundNote = "Stops future copies and serving on honest devices. Copies already held stay held — and stay sealed forever."

        public static let custodyHeldSection = "Held for others — sealed copies this device keeps"

        public static func custodyHeldOwner(owner: String) -> String {
            "Holding for \(owner)"
        }

        public static let custodyHeldScopeAccount = "Their account's sealed planes — unreadable on this device"

        public static let custodyBudgetLabel = "Keep at most (bytes)"

        public static let custodyStop = "Stop holding"

        public static let custodyStoppedBytesRemain = "Stopped — stored bytes remain until removed"

        public static let custodyRemove = "Remove and free the space"

        public static let custodyRemoveDone = "Removed — the space is free."

        public static func custodyOfferTitle(owner: String) -> String {
            "\(owner) asks this device to hold sealed copies"
        }

        public static let custodyOfferFloor = "This device would store sealed data it cannot read. It would see only the shape: which scopes exist, how much is stored, and when it changes — never the content."

        public static let custodyOfferAccept = "Hold for them"

        public static let custodyOfferDecline = "Decline"

        public static let custodyOfferTargetLabel = "Where to hold"

        public static let custodyOfferTargetDevice = "This device"

        public static let custodyOfferTargetNest = "My nest"

        public static let custodyDegradedBadge = "Degraded — some copies were dropped under the budget"

        public static let custodyMintButton = "Ask a friend to hold sealed copies"

        public static let custodyMintHostLabel = "Who to ask"

        public static let custodyMintHostPlaceholder = "Choose a contact"

        public static let custodyMintFloor = "Their device will store sealed copies it cannot read. They will see the shape of your data — which scopes exist, how much is stored, and when it changes — never the content. Choosing custodians is choosing who sees that shape."

        public static let custodyMintConfirm = "Send the request"

        public static let custodyMintNoContacts = "Start a conversation with them first — the request travels over it."

        public static let pathsPlaceholder = "e.g. Documents, Photos"

        public static let excludePlaceholder = "e.g. node_modules, .git"

        public static let followPublicFolder = "Follow a public folder"

        public static let followPublicFolderHint = "Read someone else's public folder from your own app. You need their handle and the folder's name."

        public static let followFolderName = "Folder name"

        public static let followFolderNameHint = "Public folders are named in the clear — type the name exactly as its owner published it."

        public static let followConfirm = "Follow"

        public static let followedFoldersSection = "Folders you follow"

        public static let followedStatusFollowing = "Following"

        public static let followedStatusUnavailable = "No longer available"

        public static let followedUnavailableHint = "Its owner stopped sharing it publicly, or removed it. If they publish it again, it will start working here."

        public static let unfollowFolder = "Remove"

        public static let followedPublicBadge = "Public"

        public static func followedOwner(owner: String) -> String {
            "By \(owner)"
        }

        public static let errorFollowNotFound = "No public folder by that name for that person. Check the handle and the folder name."

        public static func errorFollowFailed(message: String) -> String {
            "Couldn't follow that folder: \(message)"
        }

        public static func errorUnfollowFailed(message: String) -> String {
            "Couldn't remove that folder: \(message)"
        }

        public static let folderAudience = "Who can see this folder"

        public static let folderAudiencePrivate = "Private"

        public static let folderAudiencePublic = "Public"

        public static let folderAudienceShared = "Shared"

        public static let folderAudienceHint = "Private folders are encrypted so only you can read them. Public folders are readable by anyone on the web."

        public static let folderAudienceSharedHint = "This folder is shared with other people, so it can't be made private while it's shared. To make it private, remove the sharing in the section below first — or change who it is shared with there."

        public static let folderAudiencePublicBoundHint = "This folder is shared with other people and currently public. Pick Shared to re-seal it so only those people can read it. Anything published while it was public should still be treated as public."

        public static let declassifyTitle = "Make this folder public?"

        public static let declassifyBody = "Anyone on the web will be able to read this folder's files, and its file and folder names too — they become part of the address of each file."

        public static let declassifyIrreversible = "Making it private again protects only files you add afterwards. Anything published while the folder is public should be treated as public for good."

        public static let declassifyConfirm = "Make public"

        public static let folderAudienceUnattested = "This folder is public, but this app can't confirm that you made it public. Until you confirm again, up-to-date apps may keep its files encrypted, and your website may not show them."

        public static let folderAudienceReconfirm = "Confirm public"

        public static let folderResidency = "Content kept on the nest"

        public static let folderResidencyFull = "Full — the nest keeps this folder's content"

        public static let folderResidencyMetadataOnly = "Metadata only — content stays on my devices"

        public static let folderResidencyHint = "The nest keeps a copy of this folder's content, so a device can catch up while your other devices are offline."

        public static let folderResidencyMetadataOnlyHint = "This folder's content stays on your devices only. It moves between them while one of them holding it is online, and the nest cannot restore it. File names, changes and snapshots still sync through the nest."

        public static let residencyBlockedByServing = "Turn off website serving, WebDAV serving and any paywall first — those serve this folder's content from the nest, which needs a copy of it."

        public static let residencyConfirmTitle = "Stop keeping this folder's content on the nest?"

        public static let residencyConfirmBody = "The nest's copy of this folder's content is deleted now, and your devices become the only holders. Content moves between your devices only while one of them holding it is online. If your devices lose it, the nest cannot restore it."

        public static let residencyConfirm = "Delete the nest's copy"

        public static func errorSetResidency(message: String) -> String {
            "Failed to change where this folder's content is kept: \(message)"
        }

        public static let folderExclusiveEditing = "One device at a time may edit this folder"

        public static let folderLeaseFree = "No device is editing this folder right now."

        public static let folderLeaseHeldHere = "This device is editing this folder right now. Your other devices keep their own changes and upload them when it finishes."

        public static func folderLeaseHeldBy(device: String) -> String {
            "\(device) is editing this folder right now. Changes you make here are kept on this device and upload when it finishes."
        }

        public static let folderLeaseHeldElsewhere = "Another device is editing this folder right now. Changes you make here are kept on this device and upload when it finishes."

        public static func errorSetExclusiveEditing(message: String) -> String {
            "Failed to change exclusive editing for this folder: \(message)"
        }

        public static let serveWebsite = "Serve this folder as your website"

        public static let serveWebsiteHint = "Your site is published from this folder — an index.html here becomes your home page. Switch on your web address in Settings → Web to make it reachable."

        public static let serveWebsiteLive = "Your site is served from this folder at your web address — an index.html here becomes your home page."

        public static let folderPlacesTitle = "Device places"

        public static func errorSetPlace(message: String) -> String {
            "Failed to change the device's place: \(message)"
        }

        public static let folderDestinationsTitle = "Destination places"

        public static let folderDestinationAttach = "Attach"

        public static let folderDestinationAttachLabel = "Add a destination place"

        public static let folderDestinationDetach = "Detach"

        public static func errorFolderDestination(message: String) -> String {
            "Failed to change the folder's destination places: \(message)"
        }

        public static let serveWebsiteAddressOff = "Your site is published from this folder, but your web address is switched off, so nobody can reach it yet. Switch it on in Settings → Web."

        public static let serveWebsiteNeedsAudience = "Make this folder public, or paywall it to a tier, for the site to be visible to visitors."

        public static let audiencePublicBlockedByWebdav = "Turn off WebDAV serving first — WebDAV needs the folder encrypted, and a public folder is not."

        public static let audiencePublicBlockedByPaywall = "Remove the paywall first — a paywalled folder cannot also be public to everyone."

        public static let serveWebdavBlockedByPublic = "This folder is public, so there is nothing for WebDAV to encrypt. Make it private to serve it over WebDAV."

        public static let paywallBlockedByPublic = "This folder is public, so a paywall would not restrict anyone. Make it private to paywall it."

        public static func errorSetAudience(message: String) -> String {
            "Failed to change who can see this folder: \(message)"
        }

        public static func errorServeWebsite(message: String) -> String {
            "Failed to change website serving: \(message)"
        }

        public static let serveWebdav = "Serve over WebDAV"

        public static let serveWebdavHint = "Browse and edit this set from any WebDAV client (Finder, GNOME Files, rclone)."

        public static let serveWebdavNeedsMail = "Set up mail first — serving over WebDAV uses your mail encryption key."

        public static let showOnDemandFinder = "Show in Finder"

        public static let showOnDemandFiles = "Show in Files"

        public static let showOnDemandHint = "Browse this set on demand — files download when opened and can be freed again."

        public static let onDemandBoundHint = "This set syncs to its bound location; remove the binding to show it on demand instead."

        public static let paywallTier = "Paywall to tier"

        public static let paywallTierHint = "Only subscribers to the chosen tier can view this set's files; other visitors see a teaser."

        public static let paywallTierNone = "Not paywalled (public)"

        public static let paywallTierNeedsTier = "Create a subscription tier first to paywall this set."

        public static let conflictPolicy = "Conflict policy"

        public static let conflictPolicyAuto = "Auto (merge text, else latest wins)"

        public static let conflictPolicyLatestWins = "Latest edit wins"

        public static let syncDefaults = "Sync defaults"

        public static let defaultConflictPolicy = "Default conflict policy for new sets"

        public static let colSnapshots = "Snapshots"

        public static let colSize = "Size"

        public static let colRole = "Role"

        public static func errorRefresh(message: String) -> String {
            "Failed to load devices: \(message)"
        }

        public static func errorRemoveDevice(message: String) -> String {
            "Failed to remove device: \(message)"
        }

        public static func errorSetP2pParticipation(message: String) -> String {
            "Couldn't change peer transfers: \(message)"
        }

        public static let errorP2pRemoteEnable = "Peer transfers can only be turned on from that device itself. From here you can only turn them off."

        public static func errorRemoveFleetDevice(message: String) -> String {
            "Removed the device, but couldn't finish cleanup: \(message)"
        }

        public static let errorRemoveOwnDevice = "This is the device you're using, so it wasn't removed. To remove it, sign out on this device."

        public static let errorRemoveUnverifiedDevice = "Couldn't confirm which of your devices this is, so nothing was removed. Try again in a moment."

        public static let errorRemoveRowMismatch = "This entry doesn't match what the device itself reports, so nothing was removed. Trying again won't change that. If the device is not one you hold, remove it by its key from the signed-in devices without a matching entry, below the list."

        public static func errorDeleteFolder(message: String) -> String {
            "Failed to delete folder: \(message)"
        }

        public static func errorResolveConflict(message: String) -> String {
            "Failed to resolve conflict: \(message)"
        }

        public static func errorSavePaths(message: String) -> String {
            "Failed to save paths: \(message)"
        }

        public static func errorSetConflictPolicy(message: String) -> String {
            "Failed to set conflict policy: \(message)"
        }

        public static func errorSetNestPlace(message: String) -> String {
            "Failed to save the snapshot settings: \(message)"
        }

        public static func errorUseOtherVersion(message: String) -> String {
            "Failed to use the other version: \(message)"
        }

        public static let errorOtherVersionUnverified = "That version is no longer in this file's verified history, so nothing was changed."

        public static let errorOtherVersionNeedsHistory = "That version was saved under a previous identity of this account. Restore it from the file's version history instead."

        public static func errorServeWebdav(message: String) -> String {
            "Failed to change WebDAV serving: \(message)"
        }

        public static func errorRevokeCustody(message: String) -> String {
            "Couldn't stop trusting this custodian: \(message)"
        }

        public static func errorPaywallSet(message: String) -> String {
            "Failed to paywall the folder: \(message)"
        }

        public static func errorSetDefaultConflictPolicy(message: String) -> String {
            "Failed to set the default conflict policy: \(message)"
        }

        public static let deleteFolder = "Delete Folder"

        public static let deleteConfirmTitle = "Delete folder?"

        public static func deleteConfirmBody(name: String) -> String {
            "Delete folder \"\(name)\"? All data including snapshots will be permanently removed."
        }

        public static let removeMember = "Remove"

        public static let sharedWith = "Shared with"

        public static let shareButton = "Share…"

        public static func sharedBadge(count: String) -> String {
            "Shared · \(count)"
        }

        public static let notSharedYet = "Not shared with anyone yet."

        public static let memberAccess = "Access"

        public static let memberAccessReader = "Reader"

        public static let memberAccessWriter = "Writer"

        public static let memberByteCap = "Storage cap"

        public static let memberByteCapPlaceholder = "No cap"

        public static let writerUncappedWarning = "Without a cap, this member can use your entire storage quota."

        public static let writerPublicWarning = "This folder is public, so this member can change what anyone can see."

        public static let writerPaywalledWarning = "This folder is sold to subscribers, so this member can change what they see."

        public static let accessRevokedWarning = "The owner removed your permission to make changes, so this location is no longer syncing. Your local files are untouched."

        public static func errorShareSet(message: String) -> String {
            "Failed to share folder: \(message)"
        }

        public static func errorRemoveMember(message: String) -> String {
            "Failed to remove member: \(message)"
        }

        public static func errorSetMemberAccess(message: String) -> String {
            "Failed to change member access: \(message)"
        }

        public static let sharedWithYou = "Shared with you"

        public static func sharedBy(who: String) -> String {
            "Shared by \(who)"
        }

        public static func errorAcceptShare(message: String) -> String {
            "Failed to accept share: \(message)"
        }

        public static func errorDeclineShare(message: String) -> String {
            "Failed to decline share: \(message)"
        }

        public static func errorLeaveShare(message: String) -> String {
            "Failed to leave share: \(message)"
        }

        public static func errorBindLocation(message: String) -> String {
            "Failed to sync this location: \(message)"
        }

        public enum conflicts {
            public static let title = "sync conflicts need attention"

            public static let folder = "Folder"

            public static let device = "Device"

            public static let sectionTitle = "Sync Conflicts"

            public static let keepVersion = "Keep this version"

            public static let resolve = "Resolve"

            public static func candidateDetail(device: String, size: String) -> String {
                "\(device) · \(size)"
            }

            public static let colFile = "File"

            public static let colType = "Type"

            public static let colTime = "Time"

            public static let typeBinary = "Binary"

            public static let typeMerge = "Merge"

            public static let typeConcurrent = "Concurrent edits"

            public static let typeOther = "Conflict"

            public static let resolvedMerged = "Merged"

            public static let resolvedLatestWins = "Latest kept"

            public static let deleteDeclined = "Delete declined"

            public static let typeCatchupFailed = "Not applied"

            public static let unreadablePath = "(unreadable file name)"

            public static let awaitingDevice = "Awaiting device"

            public static let useOtherVersion = "Use the other version"
        }

        public enum wizard {
            public static let namePlaceholder = "my-photos"

            public static let nameLabel = "Name"

            public static let nameRequired = "Enter a name to continue."

            public static let selectDevices = "Select Devices"

            public static let schedule = "Schedule"

            public static let retentionSnapshots = "Keep snapshots"

            public static let retentionDays = "Keep days"

            public static let review = "Review"

            public static let creatingFolder = "Creating folder..."

            public static let failedCreate = "Failed to create folder."

            public static let selectDevicesRoles = "Select devices and choose what each one does (optional — skip to just store files here):"

            public static let reviewName = "Name"

            public static let reviewRetention = "Retention"

            public static let reviewDevices = "Devices"

            public static func createError(message: String) -> String {
                "Failed to create folder: \(message)"
            }

            public static func createMemberError(message: String) -> String {
                "Folder created, but some devices could not be enrolled: \(message)"
            }

            public static let noDevicesAvailable = "No devices available. Register a device first."

            public static let next = "Next"

            public static let back = "Back"

            public static let create = "Create"

            public static let placeOriginates = "Uploads what I change here"

            public static let placeOriginatesDesc = "Files you add or edit on this device are sent to the rest of the folder."

            public static let placeAccepts = "Receives changes from elsewhere"

            public static let placeAcceptsDesc = "Changes made on your other devices land on this one."

            public static let placeAppliesDeletes = "Applies deletions"

            public static let placeAppliesDeletesDesc = "When a file is deleted somewhere else, delete it here too. Leave this off and the device keeps every file forever — an archive."

            public static let newFolder = "New Folder"

            public static let reviewNoDevices = "None selected"

            public static func retentionSummary(snapshots: String, days: String) -> String {
                "\(snapshots) snapshots, \(days) days"
            }
        }

        public enum detail {
            public static let deviceInfo = "Device Info"

            public static let deviceId = "Device ID"

            public static let capabilities = "Capabilities"

            public static let registered = "Registered"

            public static let noFolders = "No folders assigned to this device."

            public static let removeDevice = "Remove Device"

            public static func removeConfirmText(label: String) -> String {
                "Are you sure you want to remove \"\(label)\"? This device will lose access to all folders."
            }
        }

        public enum folderDetail {
            public static let info = "Folder Info"

            public static let totalSize = "Total Size"

            public static let updateSchedule = "Update Schedule"

            public static let rescanInterval = "Rescan Interval"

            public static func deleteConfirmText(name: String) -> String {
                "Are you sure you want to delete \"\(name)\"? All snapshots and membership data will be removed."
            }
        }

        public enum peers {
            public static let noPeers = "No peers yet"

            public static let noPeersDesc = "Exchange QR codes to add P2P contacts."

            public static let selectPeer = "Select a peer"

            public static let selectPeerDesc = "Select a peer to view details."

            public static let displayName = "Display Name"

            public static let notSet = "Not set"

            public static let connection = "Connection"

            public static let pathType = "Path Type"

            public static let pathLan = "LAN"

            public static let pathWanDirect = "WAN Direct"

            public static let pathRelay = "Relay"

            public static let latency = "Latency"

            public static let lastEndpoint = "Last Endpoint"

            public static let successRate = "Success Rate"

            public static let lastConnected = "Last Connected"

            public static let lanEndpoints = "LAN Endpoints"

            public static let removePeer = "Remove Peer"
        }

        public enum syncLocations {
            public static let title = "Sync Locations"

            public static let locationPlaceholder = "Location path (or use Browse...)"

            public static let browse = "Browse..."

            public static let addLocation = "Add Location"

            public static let noLocations = "No sync locations configured."

            public static let folderPlaceholder = "Folder name"

            public static let unbound = "(unbound)"

            public static let onDemandLabel = "On-demand"

            public static let onDemandNeedsFuse3 = "On-demand needs the fuse3 package. Install it, then restart Fauna."

            public static let onDemandNoFuseDevice = "On-demand isn't available on this system."

            public static let onDemandUnavailable = "On-demand isn't available here."

            public static let onDemandMountRefused = "On-demand can't be used in this location, so only files already on this device are kept in sync. Choose a folder inside your home folder, or turn on-demand off."

            public static let onDemandMountFailed = "On-demand couldn't start for this folder, so only files already on this device are kept in sync. Turn on-demand off to keep every file here."

            public static let remove = "Remove"

            public static let helperUnavailable = "Sync service is not running. Start it and try again."
        }
    }

    public enum sessions {
        public static let title = "Sessions"

        public static let kindApp = "App sign-in"

        public static func kindDevice(name: String) -> String {
            "Device: \(name)"
        }

        public static let kindUnknownDevice = "A device key not in your device list"

        public static let markThisApp = "This app"

        public static let markThisDevice = "This device"

        public static func detail(created: String, lastUsed: String, expires: String, address: String) -> String {
            "Signed in \(created) · last active \(lastUsed) · expires \(expires) · \(address)"
        }

        public static let addressNotRecorded = "address not recorded"

        public static let empty = "No sessions to show."

        public static let revoke = "Revoke"

        public static let revokeOthers = "Sign Out Everywhere Else"

        public static let revokeOthersConfirm = "Yes, Sign Out Everywhere Else"

        public static let revokeOthersCancel = "Cancel"

        public static let revokeNote = "Revoking ends a sign-in and makes whoever held it prove themselves again. It does not sign a device out — a device that holds your secret key or a device grant signs itself straight back in. To end a device for good, remove it under Settings → Devices. If someone else holds your secret key, only your recovery kit ends them (Settings → Account → Recovery Kit → My Identity Was Stolen)."

        public static let lockoutWarning = "Lock this account for 24 hours. Every device is signed out, this one too. Nobody can sign in for 24 hours, you included, and there is no unlock. It does not remove somebody who holds your secret key — they come back when the lock ends. They can do the same to you: anyone with your secret key can sign you out and lock the account, again every 24 hours. Your recovery kit still works while the account is locked and moves it to a new key the thief cannot use (Settings → Account → Recovery Kit → My Identity Was Stolen) — a lock you did not set is itself the sign to use it."

        public static let lockoutConfirmPlaceholder = "Type LOCK to confirm"

        public static let lockoutButton = "Lock for 24 Hours"

        public static func lockoutWrongWord(word: String) -> String {
            "Type \(word) exactly to lock the account."
        }

        public static func loadFailed(error: String) -> String {
            "Could not load your sessions: \(error)"
        }

        public static func actFailed(error: String) -> String {
            "That did not go through: \(error)"
        }

        public static let noOwnSession = "This app does not know its own sign-in yet, so it cannot tell which one to keep. Try again in a moment."
    }

    public enum settings {
        public static let title = "Settings"

        public enum identityExport {
            public static let title = "Export Identity"

            public static let desc = "Show a QR code that another device can scan to import your identity."

            public static let warning = "Anyone who scans this QR code gets full access to your identity. Only show it in a trusted environment."

            public static let showQr = "Show QR Code"

            public static let hideQr = "Hide QR Code"
        }

        public enum recoveryKit {
            public static let title = "Recovery Kit"

            public static let desc = "An offline key that can recover your account if you lose your identity secret — or take it back if someone steals it. It is shown once and never stored on this device."

            public static let statusNeverCreated = "No recovery kit. If you lose your identity secret, your account cannot be recovered, and if someone steals it, you cannot take it back."

            public static let statusRegistered = "Your recovery kit is active, and a sealed copy of your identity secret is stored for it."

            public static let statusRegisteredNoEscrow = "Your recovery kit is active, but no sealed copy of your identity secret is stored — your recovery phrase cannot recover this account right now. Enter your kit below and press Restore Phrase Recovery to fix this."

            public static func statusReplacementPending(days: String) -> String {
                "A replacement of your recovery kit was requested with your identity secret alone, and takes effect in \(days) days. Cancel it below if it was not you."
            }

            public static let statusLoading = "Checking your recovery kit…"

            public static let create = "Create Recovery Kit"

            public static let replace = "Replace Using My Kit"

            public static let lost = "I Lost My Kit"

            public static let escrowReseal = "Restore Phrase Recovery"

            public static let sweepRetry = "Finish Moving Your Groups"

            public static let sweepRetryNoOldState = "This device does not have the conversation history from your previous identity, so it cannot finish the move. If another of your devices still has those conversations, press Finish Moving Your Groups there; otherwise ask another member of each group to remove the old identity and add your new one."

            public static let sweepRetryNotLanded = "No move of this account has been recorded, so there is nothing to finish. If you have just taken your account back, wait a moment and try once more."

            public static let sweepRetryLandedForAnother = "This account was moved to a different identity than the one signed in here. Sign in as your current identity to finish moving your groups."

            public static func sweepRetryFailed(reason: String) -> String {
                "Your groups could not be updated: \(reason). Nothing was lost — press Finish Moving Your Groups to try once more."
            }

            public static let stolen = "My Identity Was Stolen"

            public static let veto = "Cancel The Pending Replacement"

            public static let stolenConfirmPlaceholder = "Type SUCCEED to confirm"

            public static let stolenWarning = "This mints a new identity and re-points your account to it. It cannot be undone, your old identity stops working, and you will need to re-add your devices. Your handle stays yours. Afterwards, create a new recovery kit — this one retires with the old identity."

            public static func unreadableStatus(rows: String, since: String) -> String {
                "\(rows) items of your account data, saved since \(since), cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in since \(since), it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take."
            }

            public static func unreadableStatusUndated(rows: String) -> String {
                "\(rows) items of your account data cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in for a while, it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take."
            }

            public static let letGo = "Let Go Of Unreadable Data"

            public static let letGoConfirmPlaceholder = "Type LET GO to confirm"

            public static func letGoDone(retired: String) -> String {
                "\(retired) unreadable items were let go."
            }

            public static let letGoKept = "Some of this data became readable again and was kept."

            public static func letGoFailed(message: String) -> String {
                "Couldn't let the data go: \(message)"
            }

            public static func stolenPersistFailed(secret: String) -> String {
                "Your account was re-pointed to a new identity, but saving it on this device failed. Write this secret key down NOW and import it — it is the only way back into your account: \(secret)"
            }

            public static func stolenCeremonyFailed(message: String) -> String {
                "Couldn't recover your account: \(message)"
            }

            public static func stolenOutcomeUnknownSaved(cause: String, reported: String) -> String {
                "Couldn't confirm whether your account was recovered (\(cause)). Your new identity is saved on this device — reopen the app to sign in with it. Details: \(reported)"
            }

            public static func stolenOutcomeUnknownUnsaved(cause: String, secret: String, reported: String) -> String {
                "Couldn't confirm whether your account was recovered (\(cause)), and this device couldn't save your new identity. Write down this secret key now and import it — it is the only way back into your account: \(secret). Details: \(reported)"
            }

            public static func stolenLandedForAnother(actor: String) -> String {
                "Your account has already been moved to a different identity (\(actor)) — another device with the same recovery kit got there first. Import that identity to get back into your account."
            }

            public static func statusFailed(message: String) -> String {
                "Could not check your recovery kit: \(message)"
            }

            public static func actionFailed(message: String) -> String {
                "Could not update your recovery kit: \(message)"
            }

            public static func vetoFailed(message: String) -> String {
                "Could not cancel the pending replacement: \(message)"
            }

            public static let kitPhrasePlaceholder = "Paste your recovery phrase (fauna://recovery link or 64-character code)"

            public static let kitPhraseRequired = "Paste the recovery phrase for this account first."

            public static let sweepNone = "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Press Finish Moving Your Groups below to complete it now, or ask another member of each group to remove the old identity and add your new one."

            public static func sweepFailed(reason: String) -> String {
                "Your account moved to your new identity, but your groups could not be updated: \(reason). Your old identity may still be able to read them."
            }

            public static func sweepAllRemoved(groups: String) -> String {
                "Your old identity was removed from all \(groups) of your group conversations."
            }

            public static func sweepPartial(removed: String, groups: String) -> String {
                "Your old identity was removed from \(removed) of your \(groups) group conversations. It can still read the rest — press Finish Moving Your Groups below to finish, or ask another member of those to remove it."
            }

            public static let sweepNoneNoRetry = "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Ask another member of each group to remove the old identity and add your new one."

            public static func sweepPartialNoRetry(removed: String, groups: String) -> String {
                "Your old identity was removed from \(removed) of your \(groups) group conversations. It can still read the rest — ask another member of those to remove it."
            }

            public static func sweepUnattested(count: String) -> String {
                "There are \(count) other members across your groups that this cannot confirm you added yourself. If your identity was stolen, one of them could be the thief under another name."
            }

            public static let reviewIntro = "Keep the people you recognise. Anyone you don't, you can remove from your groups. You do not have to finish now."

            public static func reviewRow(who: String, reason: String) -> String {
                "\(who) — \(reason)"
            }

            public static let reviewReasonCompromise = "was in your groups before you recovered your account"

            public static let reviewReasonOther = "this could not confirm their identity"

            public static let reviewUnknownPerson = "Someone no longer in any of your groups"

            public static let reviewKeep = "Keep"

            public static let reviewRemove = "Remove From My Groups"

            public static let reviewDefer = "Review The Rest Later"

            public static func reviewRemovePartial(who: String, removed: String, groups: String) -> String {
                "Removed \(who) from \(removed) of \(groups) of your group conversations. The rest still include them — try again, or ask another member of those groups to remove them."
            }

            public static func reviewRemoveDoneHere(who: String, removed: String) -> String {
                "Removed \(who) from \(removed) of your group conversations."
            }

            public static func reviewRemoveNoneHere(who: String) -> String {
                "\(who) is not in any of your group conversations on this device."
            }

            public static func reviewRemoveFolderSeats(seats: String) -> String {
                "They are also in \(seats) shared folder(s). Remove them in each folder's sharing settings — or leave the set, if it is not yours — then choose Remove again."
            }

            public static func reviewRemoveUnsyncedSeats(seats: String) -> String {
                "They are also in \(seats) group chat(s) not yet synced to this device. Choose Remove again after syncing completes, or from a device that has those chats."
            }

            public static func reviewVerdictFailed(who: String, reason: String) -> String {
                "Removed \(who) from your group conversations, but your review list could not be updated yet: \(reason). They may still be listed here until it succeeds."
            }

            public static let backupRegrantRunning = "Restarting your backups under your new identity…"

            public static let backupRegrantDone = "Your backups are running again under your new identity."

            public static func backupRegrantFailed(reason: String) -> String {
                "Your backups could not be restarted under your new identity yet: \(reason). This will be retried the next time you sign in."
            }

            public static let mlsResealRunning = "Unlocking your conversations under your new identity…"

            public static let mlsResealDone = "Your conversations are now held under your new identity."

            public static let mlsResealPartlyOwedElsewhere = "Some of your conversations are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there."

            public static let mlsResealOwedElsewhere = "Your conversations are still held under your previous identity, and this device does not have the key to unlock them. Sign in on the device you used to take back your account, and it will finish there."

            public static func mlsResealFailed(reason: String) -> String {
                "Your conversations could not be moved to your new identity yet: \(reason). This will be retried the next time you sign in."
            }

            public static let grantRemintRunning = "Restoring the access you had granted to services, under your new identity…"

            public static let grantRemintDone = "The access you had granted to services (mail filtering, search and similar) is restored under your new identity. Review each one on the Nests page — if your identity was stolen, the thief could have granted access you never did."

            public static let grantRemintPartial = "Some of the access you had granted to services could not be restored yet. This will be retried the next time you sign in — anything already restored is listed on the Nests page for review."

            public static func grantRemintFailed(reason: String) -> String {
                "The access you had granted to services could not be restored under your new identity yet: \(reason). This will be retried the next time you sign in."
            }

            public static let corpusResealRunning = "Moving your files across to your new identity…"

            public static let corpusResealDone = "Your files are now held under your new identity."

            public static func corpusResealPartlyOwed(done: String, remaining: String) -> String {
                "Moving your files across to your new identity: \(done) done, \(remaining) still to go. This continues on its own, and picks up where it left off each time you sign in."
            }

            public static let corpusResealOwedElsewhere = "Your files are still held under your previous identity, and this device does not have the key to move them. Sign in on the device you used to take back your account, and it will finish there."

            public static func corpusResealFailed(reason: String) -> String {
                "Your files could not be moved to your new identity yet: \(reason). This will be retried the next time you sign in."
            }

            public static let mailBurnRunning = "Replacing your mail keys, because your previous identity's passwords could open your mailbox…"

            public static func mailBurnDone(count: String) -> String {
                "Your mail encryption key was replaced and your \(count) mail app password(s) were revoked — whoever held your previous identity could have used them to read your mail. Your mailbox keeps receiving as normal, but each mail app needs setting up again with a new password from this page."
            }

            public static func inheritedFilters(count: String) -> String {
                "\(count) of your email filter rule(s) were set up before your account recovery and are still unchecked. A filter can silently bin or redirect incoming mail, so it is worth confirming you recognise each one — Settings ▸ Privacy ▸ Email Filters."
            }

            public static func mailBurnFailed(reason: String) -> String {
                "Your mail keys could not be replaced yet: \(reason). Until this finishes, anyone who had your previous identity can still read new mail. This will be retried the next time you sign in."
            }

            public static let tierPeriodRotationRunning = "Replacing the keys your subscriber-only posts are locked with, because your previous identity could open them…"

            public static func tierPeriodRotationDone(count: String) -> String {
                "New posts to your \(count) subscriber tier(s) are now locked with keys your previous identity never had. Your subscribers keep their access, and posts you published before taking your account back stay readable to them — and to anyone who had the old keys, which is why only new posts are covered."
            }

            public static let tierPeriodRotationNestTooOld = "The keys for your subscriber-only posts could not be replaced: this nest is too old to accept them. Until it is updated, anyone who had your previous identity can read the subscriber-only posts you publish from now on."

            public static func tierPeriodRotationPartial(count: String, failed: String) -> String {
                "Keys were replaced for \(count) of your subscriber tier(s), but \(failed) could not be finished. Until they are, anyone who had your previous identity can read new posts to those tiers. This will be retried the next time you sign in."
            }

            public static func tierPeriodRotationFailed(reason: String) -> String {
                "The keys for your subscriber-only posts could not be replaced yet: \(reason). Until this finishes, anyone who had your previous identity can read the subscriber-only posts you publish from now on. This will be retried the next time you sign in."
            }

            public static let draftsResealRunning = "Recovering your unsent drafts under your new identity…"

            public static let draftsResealDone = "Your unsent drafts are available again under your new identity."

            public static let draftsResealPartlyOwedElsewhere = "Some of your unsent drafts are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there."

            public static let draftsResealOwedElsewhere = "Your unsent drafts are still held under your previous identity, and this device does not have the key to unlock them. They are safe. Sign in on the device you used to take back your account, and it will finish there."

            public static func draftsResealFailed(reason: String) -> String {
                "Your unsent drafts could not be recovered yet: \(reason). They are safe, and this will be retried the next time you sign in."
            }
        }

        public enum icloudBackup {
            public static let title = "iCloud Backup"

            public static let toggle = "Back up identity to iCloud Keychain"

            public static let footer = "When off (the default), your identity stays on this device and never syncs to iCloud. Turn it on to let iCloud Keychain restore your identity on a new device. Fauna's own device-add and recovery flows are the primary way to use multiple devices."
        }

        public static let checkForUpdates = "Check for Updates"

        public static let upToDate = "Up to date"

        public static let checkFailed = "Check failed"

        public static func updateAvailableNotice(version: String, url: String) -> String {
            "Version \(version) is available. Get it at \(url)"
        }

        public static let mlsAvailable = "MLS encryption: Available"

        public static let mlsNotAvailable = "MLS encryption: Not available"

        public enum pushNotifications {
            public static let title = "Push Notifications"

            public static let description = "Receive notifications in this browser even when the app is not open."

            public static let updateFailed = "Failed to update push notification settings."

            public static let deviceDescription = "Get a notification for new messages, knocks and invites on this device, even when Fauna is closed."

            public static let optInLabel = "Notify me on this device"

            public static let agentUnreachable = "The Fauna sync agent is not running on this computer, so it cannot show notifications while Fauna is closed."

            public static let noSink = "This computer has no desktop notification service, so notifications cannot be shown here."
        }

        public static let configuration = "Configuration"

        public static let privacy = "Privacy"

        public static let moderation = "Moderation"

        public static let data = "Data"

        public static let nestAdmin = "Nest Admin"

        public static let signOut = "Sign Out"

        public static let exitSettings = "Exit settings"

        public enum mail {
            public static let sectionTitle = "Mail & Calendar"

            public static let sectionDescription = "Enable to set up third-party mail and calendar apps like Apple Mail, Thunderbird, and Apple Calendar."

            public static let enableTitle = "Enable mail"

            public static let enableSubtitle = "Allow a third-party mail app to connect over IMAP and SMTP"

            public static let disableTitle = "Disable mail?"

            public static let disableWarning = "This revokes all your mail credentials and clears your mail encryption key. Third-party mail apps (IMAP/CalDAV) will stop working until you re-enable mail."

            public static let disableConfirm = "Disable mail"

            public static let statusDisabled = "Mail is disabled"

            public static let statusEnabled = "All up to date"

            public static let statusSyncing = "Syncing mail credentials…"

            public static func statusRotation(count: String) -> String {
                "Rotation in progress (\(count) remaining)"
            }

            public static let credentialsTitle = "Credentials"

            public static let credentialsDescription = "Mail credentials you've created for your mail apps"

            public static let credentialsEmpty = "No mail credentials yet"

            public static let credentialsEmptySubtitle = "Add a credential to connect a mail app"

            public static let addCredential = "Add credential"

            public static let rotateKeys = "Rotate mail keys"

            public static let keysTitle = "Mail encryption keys"

            public static let bannerTitle = "A previous mail-credential rotation didn't finish."

            public static let bannerSubtitle = "Resume to complete it."

            public static let resume = "Resume"

            public static let addTitle = "Add mail credential"

            public static let namePlaceholder = "Credential name (e.g. iPhone Mail)"

            public static let typeSelector = "Use a password (PLAIN) instead of a bearer token"

            public static let passwordPlaceholder = "Password"

            public static let show = "Show"

            public static let hide = "Hide"

            public static let submitEnable = "Enable mail"

            public static let submitAdd = "Add"

            public static let cancel = "Cancel"

            public static let done = "Done"

            public static let passwordRequired = "Enter a password for this credential."

            public static let strengthWeak = "Weak"

            public static let strengthFair = "Fair"

            public static let strengthStrong = "Strong"

            public static let autogenerate = "Auto-generate a strong password"

            public static let weakPasswordWarning = "A password you choose yourself limits how strongly your stored mail is protected at rest on an encrypted nest. Letting Fauna generate one is recommended."

            public static let tokenWarning = "Copy this token into your mail app now — it is shown only once."

            public static let copyToken = "Copy token"

            public static let copied = "Copied"

            public static let rotateTitle = "Rotate mail keys"

            public static let rotateWarning = "Rotating replaces your mail encryption key and re-wraps it under every surviving credential. Already-received mail stays readable. Every connected mail app must re-authenticate, and any credential you mark below as compromised loses access. This is safe to interrupt — it resumes automatically."

            public static let rotateExcludeCaption = "Exclude compromised credentials (they lose access):"

            public static let rotateConfirm = "Rotate keys"

            public static let kindPassword = "Password"

            public static let kindBearer = "Bearer token"

            public static let credentialRevoked = "Access revoked — set this mail app up again with a new password"

            public static let revoke = "Revoke"

            public static let revokeConfirm = "Confirm?"

            public static let createdPrefix = "created"

            public static let lastUsedNever = "Never"

            public static let copyUsername = "Copy address"

            public static let revealSecret = "Reveal secret"

            public static let hideSecret = "Hide secret"

            public static let copySecret = "Copy secret"

            public static func revealSecretFailed(error: String) -> String {
                "Could not reveal secret: \(error)"
            }

            public static func copySecretFailed(error: String) -> String {
                "Could not copy secret: \(error)"
            }

            public static let secretLabel = "Secret"

            public static let muaTitle = "Mail, calendar & files app setup"

            public static let muaDescription = "Connection details to enter in your mail, calendar and file apps"

            public static let muaImapHost = "IMAP host"

            public static let muaImapPort = "IMAP port"

            public static let muaSmtpHost = "SMTP host"

            public static let muaSmtpPort = "SMTP port"

            public static let muaCaldavHost = "CalDAV host"

            public static let muaCaldavPort = "CalDAV port"

            public static let muaWebdavUrl = "WebDAV URL"

            public static let muaUsername = "Username"

            public static let muaAuth = "Authentication"
        }

        public enum accountPage {
            public static let title = "Account"

            public static let identity = "Identity"

            public static let actorId = "Actor ID"

            public static let node = "Node"

            public static let bluesky = "Bluesky"

            public static let unlink = "Unlink"

            public static let link = "Link"

            public static let blueskyHandlePlaceholder = "yourname.bsky.social"

            public static let copiedClipboard = "Copied to clipboard"

            public static let changeHandle = "Change Handle"

            public static let newHandle = "New handle"

            public static let newHandlePlaceholder = "Enter new handle"

            public static let handleChanged = "Handle changed"

            public static let deleteRequested = "Account deletion scheduled"

            public static let dataExport = "Data Export"

            public static let exportMyData = "Export My Data"

            public static let deleteAccount = "Delete Account"

            public static let deleteConfirmText = "This permanently removes your handle and data from this node. Your key is not affected."

            public static let connectedServices = "Connected Services"

            public static let usage = "Usage"

            public static let bridgesDescription = "Connect to other social networks"

            public static let bridgeManagement = "Bridge Management"

            public static let bridgeManagementSubtitle = "Open the Bridges section in the sidebar to link or manage accounts on other networks (Bluesky, ActivityPub, Nostr, Email)"

            public static let data = "Data"

            public static let exportSubtitle = "Download a copy of all your data, content included — it may be large"

            public static let exportDialogTitle = "Export Account Data"

            public static let session = "Session"

            public static let signOutSubtitle = "Remove local credentials and return to onboarding. Your secret key is still required to sign back in."

            public static let deleteSubtitle = "Permanently delete your account and all data"

            public static let accounts = "Accounts"

            public static let accountsSubtitle = "Switch between the identities on this device, or add another."

            public static let addAccount = "Add account"

            public static let openNewInstance = "Open in new window"

            public static func openNewInstanceCopied(command: String, account: String) -> String {
                "Copied: \(command) — paste it into a new terminal window to open \(account)"
            }

            public static let requireConfirmToggle = "Require confirmation to switch"

            public static let reauthReason = "switch to this account"

            public static let reauthPromptTitle = "Confirm account switch"

            public static func reauthPromptBody(account: String) -> String {
                "This account asks for confirmation before you switch to it. Switch to \(account) now?"
            }

            public static let reauthConfirm = "Switch"
        }

        public enum pendingActions {
            public static let title = "Pending actions"

            public static func titleCount(count: String) -> String {
                "Pending actions (\(count))"
            }

            public static let noneScheduled = "Nothing is scheduled."

            public static func applies(time: String) -> String {
                "Applies \(time)"
            }

            public static let cancel = "Cancel"

            public static func changeHandleTo(handle: String) -> String {
                "Change handle to \(handle)"
            }

            public static let deleteAccount = "Delete this account"

            public static func deleteSnapshot(snapshot: String) -> String {
                "Delete snapshot \(snapshot)"
            }

            public static func adminDeleteUser(target: String) -> String {
                "Delete the account \(target)"
            }

            public static func adminAdd(target: String) -> String {
                "Grant \(target) the admin role"
            }

            public static func adminRemove(target: String) -> String {
                "Revoke the admin role of \(target)"
            }

            public static func adminChangeRole(target: String) -> String {
                "Change the admin role of \(target)"
            }
        }

        public enum memberReviewPage {
            public static let title = "Members To Review"

            public static let intro = "These are the people you have not decided about since you recovered your account. Keep the ones you recognise, and remove anyone you do not from your groups."

            public static let empty = "There is nobody waiting for you to review."
        }

        public enum encryptionPage {
            public static let title = "Encryption"

            public static let mlsKeyPackages = "MLS Key Packages"

            public static let mlsDescription = "Key material used for end-to-end encrypted group messaging"

            public static let refreshKeys = "Refresh Keys"

            public static let refreshKeysDescription = "Generate and upload new key packages to your nest"

            public static let availableKeyPackages = "Available key packages"

            public static let lowKeyWarningTitle = "Low Key Packages"

            public static let lowKeyWarningSubtitle = "Generate new key packages to ensure uninterrupted encrypted messaging"

            public static func lowKeyWarningBody(count: String) -> String {
                "Only \(count) key package(s) remaining. Refresh to generate more and maintain end-to-end encryption availability."
            }

            public static func errorKeyCount(message: String) -> String {
                "Could not load key count: \(message)"
            }

            public static func errorRefreshKeys(message: String) -> String {
                "Failed to refresh keys: \(message)"
            }
        }

        public enum generalPage {
            public static let appearanceNote = "Fauna follows your terminal's own color scheme — there is no separate theme picker here."

            public static let theme = "Theme"

            public static let themeSubtitle = "Choose the application colour scheme"

            public static let themeFollowSystem = "Follow System"

            public static let themeLight = "Light"

            public static let themeDark = "Dark"

            public static let launchAtLogin = "Launch at login"

            public static let launchAtLoginSubtitle = "Start Fauna automatically when you log in"

            public static let behaviour = "Behaviour"

            public static let noTraySubtitle = "No system tray detected — closing the window will quit Fauna"

            public static let notificationSound = "Notification sound"

            public static let notificationSoundSubtitle = "Play a sound when a new message arrives"

            public static let keyboardShortcuts = "Keyboard Shortcuts"

            public static let keyboardShortcutsDescription = "Shortcuts available while the application window is focused"

            public static let raiseWindow = "Raise window from tray"

            public static let raiseWindowSubtitle = "Click the system tray icon to bring the window back"

            public static let version = "Version"

            public static func updateAvailable(version: String) -> String {
                "\(version) available"
            }

            public static let shortcutNewMessage = "New message"

            public static let shortcutNewGroup = "New group"

            public static let shortcutComposeEmail = "Compose email (SMTP)"

            public static let shortcutCloseWindow = "Close window / hide to tray"

            public static let shortcutHideToTray = "Hide to system tray"

            public static let shortcutMinimizeToTray = "Minimize to tray (when enabled)"

            public static let shortcutQuit = "Quit application"

            public static let shortcutQuickSwitcher = "Quick switcher"

            public static let shortcutPreferences = "Preferences"

            public static let shortcutSwitchSection = "Switch sidebar section"
        }

        public enum moderationPage {
            public static let title = "Content Moderation"
        }

        public enum privacyPage {
            public static let title = "Privacy"

            public static let emailFilters = "Email Filters"

            public static let noFilters = "No filters"

            public static let addFilter = "Add Filter"

            public static let editFilter = "Edit filter"

            public static let deleteFilter = "Delete filter"

            public static let newFilter = "New Filter"

            public static let action = "Action"

            public static let forwardAddress = "Forward to"

            public static let keepLocalCopy = "Keep a local copy"

            public static let spamPreferences = "Spam Preferences"

            public static let inboxModeDescription = "Control who can send you messages"

            public static let inboxModeSubtitle = "Who can message you directly"

            public static let inboxModeUnknown = "Your current inbox mode has not loaded, so none of the four below is marked. Your setting is unchanged — reopen this page once your nest is reachable to see and change it."

            public static let emailFiltersDescription = "Rules applied to incoming messages"

            public static let noFiltersConfigured = "No filters configured"

            public static let filterInherited = "From before your account recovery — check you recognise this rule"

            public static let filterKeep = "I recognise this"

            public static let noFiltersSubtitle = "Add a filter to automatically sort or reject messages"

            public static let matchValue = "Match value"

            public static let spamProtection = "Spam Protection"

            public static let spamProtectionDescription = "Threshold scores for automatic filtering"

            public static let spamThresholdSubtitle = "Messages above this score are marked as spam"

            public static let phishingThresholdSubtitle = "Messages above this score are flagged as phishing"

            public static let applyChanges = "Apply changes"
        }

        public enum adminPage {
            public static let overview = "Overview"

            public static let total = "Total"
        }

        public enum syncPage {
            public static let title = "Sync Settings"

            public static let syncedLocations = "Synced Locations"

            public static let noLocationsSynced = "No locations synced"

            public static let locationPath = "Location path"

            public static let addLocation = "Add location"

            public static let addLocationSubtitle = "Add the typed location path, bound to the named folder"

            public static let selectDirectoryDialog = "Select Directory to Sync"

            public static let openDirectory = "Open directory"
        }

        public enum p2pPage {
            public static let title = "P2P"

            public static let tunnelGroupTitle = "P2P Tunnel"

            public static let tunnelGroupDescription = "Direct peer-to-peer connections between your devices"

            public static let start = "Start"

            public static let stop = "Stop"

            public static let tunnelRowTitle = "Tunnel"

            public static let tunnelRowSubtitle = "Start or stop the P2P tunnel"

            public static let nodeId = "Node ID"

            public static let contactsGroupTitle = "Contacts"

            public static let contactsGroupDescription = "P2P peers you can reach directly (local to this device)"

            public static let connectionGroupTitle = "Connection"

            public static let connectionGroupDescription = "Network information for P2P connectivity"

            public static let lanAddresses = "LAN Addresses"

            public static let lanNone = "(no active interfaces detected)"
        }

        public static let signOutConfirm = "Are you sure you want to sign out? You will need your secret key to sign back in."

        public static func signOutResidue(count: String) -> String {
            "Signed out, but \(count) item(s) of your data could not be removed from this device — another program may still be using them. Press Remove Again to try once more."
        }

        public static let signOutResidueCredentials = "Signed out, but your sign-in credentials could not be removed from this device — its secure storage may be locked or unavailable. Press Remove Again to try once more."

        public static func signOutResidueWithCredentials(count: String) -> String {
            "Signed out, but your sign-in credentials and \(count) item(s) of your data could not be removed from this device. Press Remove Again to try once more."
        }

        public static let signOutResidueRetry = "Remove Again"

        public static let signOutResidueRetryBlockedOtherWindow = "Not removed — another Fauna window is using some of this data. Close it, then press Remove Again."

        public static let signOutBlockedOtherWindow = "Still signed in — another Fauna window is using this account on this device. Close it, then sign out again."

        public static let removeAccountBlockedOtherWindow = "Not removed — another Fauna window is using that account on this device. Close it, then remove the account again."

        public static let removeAccountBlockedThisWindow = "Not removed — this window is using that account. Close this window, then remove the account from another one."

        public static func switchRefusedNoSecret(account: String) -> String {
            "Not switched — this device can no longer sign in as \(account): its secret key is missing here. You are still on the identity you were using. To use \(account) here again, add it back with its secret key or recovery kit."
        }

        public static func switchRefused(account: String) -> String {
            "Not switched to \(account) — you are still on the identity you were using."
        }

        public static let accountListFull = "Not added — this device's account list is full. Remove an account this device no longer uses, then try again."

        public static let publishingKeyPackages = "Publishing key packages..."

        public static let filterName = "Filter name"

        public static let ruleType = "Rule type"

        public static let ruleValue = "Rule value"

        public static let spamThreshold = "Spam threshold"

        public static let nestUrl = "Nest URL"

        public static let startup = "Startup"

        public static let autostartHeader = "Start Fauna when you sign in"

        public static let closeToTrayHeader = "Close to tray"

        public static let closeToTraySubtitle = "Keep Fauna running in the system tray when the window is closed"

        public static let privacyDesc = "Control who can contact you."

        public static let inboxMode = "Inbox Mode"

        public static let configureNest = "Configure Nest"

        public static let configureNestDesc = "Change the nest URL this app connects to."

        public static let newNestUrl = "New Nest URL"

        public static let newNestUrlPlaceholder = "https://nest.fauna.social"

        public static let updateButton = "Update"

        public static let storageDesc = "Account storage usage."

        public static let about = "About"

        public static let aboutName = "Fauna for Windows"

        public static let aboutDesc = "Encrypted messaging, contacts, and file sync."

        public static let dangerZoneDesc = "Permanently delete your account and all associated data. This action cannot be undone."

        public static let deleteConfirmPlaceholder = "Type DELETE to confirm"

        public static let general = "General"

        public static let appearance = "Appearance"

        public static let showInDock = "Show in Dock"

        public static let p2pRedirect = "Peer connections and bridges are managed on the Devices and Bridges pages."

        public static let openDevices = "Open Devices"

        public static let openBridges = "Open Bridges"

        public enum errors {
            public static let publishKeys = "Failed to publish keys"

            public static let updateInbox = "Failed to update inbox mode"

            public static let changeHandle = "Failed to change handle"

            public static let deleteAccount = "Deletion failed"

            public static let export = "Export failed"

            public static func exportStatus(status: String) -> String {
                "Export failed: \(status)"
            }

            public static let createFilter = "Failed to create filter"

            public static let updateFilter = "Failed to save filter"

            public static let deleteFilter = "Failed to delete filter"

            public static let keepFilter = "Failed to record that you recognise this rule"

            public static let savePrefs = "Failed to save preferences"

            public static let enableNotifications = "Failed to enable notifications."

            public static let disableNotifications = "Failed to disable notifications."

            public static let train = "Training failed"

            public static let taskAssignmentStale = "That option is no longer available — reopen the page and try again"
        }
    }

    public enum nests {
        public static let title = "Nests"

        public static let viewNow = "Now"

        public static let viewHistory = "History"

        public static let notTrusted = "This nest is not trusted to read any of your content."

        public static let escrowHolderBadge = "Holds your recovery escrow"

        public static func custodyNestLabel(host: String) -> String {
            "\(host)'s nest — trusted to hold sealed copies"
        }

        public static let trustedToRead = "Trusted to read:"

        public static let scopeMail = "Mail — and the spam-filter model and training history derived from it"

        public static let scopeCalendar = "Calendar"

        public static let scopePosts = "Posts"

        public static let scopeSpamLabels = "Write spam labels"

        public static func scopeMailLabeler(labeler: String) -> String {
            "Mail — only to run community labeler \(labeler) over it"
        }

        public static func scopeLabelerLabels(labeler: String) -> String {
            "Write labels for community labeler \(labeler)"
        }

        public static let scopeSpamModel = "Your spam-filter training, for the shared spam baseline"

        public static func scopePostsTier(tier: String) -> String {
            "Posts — \(tier)"
        }

        public static func scopeFolder(folder: String) -> String {
            "Your folder \"\(folder)\""
        }

        public static let scopeFolderDeleted = "A folder you have since deleted"

        public static let lastsUntil = "Trusted until:"

        public static let statusActive = "Active"

        public static let statusExpiring = "Expiring soon"

        public static let statusExpired = "Paused — renew to resume"

        public static let statusAutoRenewing = "Auto-renewing"

        public static let renew = "Renew"

        public static let revoke = "Revoke"

        public static let grantUnattestedMark = "Given before you recovered this account — still active. Keep it, or revoke it if you don't recognise it."

        public static let grantKeepButton = "Keep"

        public static let boundNoteStanding = "On an honest nest, revoking stops future access and re-acquisition. It cannot un-see what was already read, and does not yet block content arriving after revocation."

        public static let boundNoteBoundedMail = "This trust is cryptographically time-boxed: once its window ends (accurate to within about a week), this nest can no longer read new mail at all — not even by re-acquiring a key. Within the window it can only open mail sealed under that period's rotating keys, so content sealed before the schedule caught up may still be unreadable to it."

        public static func historyMinted(scope: String, when: String) -> String {
            "Trusted to read \(scope) · \(when)"
        }

        public static func historyRenewed(scope: String, when: String) -> String {
            "Trust renewed: \(scope) · \(when)"
        }

        public static func historyRevoked(scope: String, when: String) -> String {
            "Trust revoked: \(scope) · \(when)"
        }

        public static let backupScopeSeal = "Backs up your messages for you"

        public static func backupScopeWriter(destination: String) -> String {
            "Writes your backups to \(destination)"
        }

        public static let backupSince = "Trusted since:"

        public static let backupStatusActive = "Active"

        public static let backupStatusUnreachable = "Could not reach this destination"

        public static let backupStatusMissing = "Not set up to accept your backups"

        public static let backupRevoke = "Stop backing up"

        public static let backupBoundNoteSeal = "Stopping this means your nest can no longer make new backups of your messages. Backups it already made stay where they are until the box holding them clears them out."

        public static let backupBoundNoteWriter = "Stopping this means your nest can no longer send new backups to this destination. Backups already stored there stay until that box clears them out. You can stop it here even if your own nest is misbehaving."

        public static func generationPath(path: String) -> String {
            "Backup of \(path)"
        }

        public static func generationPathUnknown(hash: String) -> String {
            "Backup \(hash)"
        }

        public static let generationSuperseded = "Replaced:"

        public static func generationExpires(when: String) -> String {
            "Can be restored until \(when), and counts against your storage until then"
        }

        public static let generationStatusListed = "Can be restored"

        public static let generationStatusUnreachable = "Could not reach this destination"

        public static let generationRestore = "Restore this version"

        public static let generationRestored = "That version has been restored."

        public static let generationPastWindow = "That version is past the recovery window, so it can no longer be restored."

        public static let mintButton = "Add trust…"

        public static let mintScopePlaceholder = "Choose what to trust it with…"

        public static let mintOptionMail = "Read and filter my mail — and the spam-filter model and training history derived from it"

        public static let mintOptionCalendar = "Read my calendar"

        public static func mintOptionPaywalled(tier: String) -> String {
            "Serve paywalled posts — \(tier)"
        }

        public static let mintHolderPlaceholder = "Which service on this nest?"

        public static let mintDurationOneOff = "For a few hours"

        public static let mintDurationStandard = "For 90 days"

        public static let mintConfirm = "Trust"

        public static let blessedToggle = "Keep this box's trust renewed"

        public static let description = "Nests you have linked to sync your account's content."

        public static let empty = "No linked nests yet."

        public static let addButton = "Link a nest"

        public static let authorizeSubtitle = "Authorize one of your nests to sync this account"

        public static let addInputPlaceholder = "Nest address (https://…) — links both ends — or a 64-hex identity"

        public static let nestToLink = "Nest to link"

        public static let addSubmit = "Link"

        public static let addCancel = "Cancel"

        public static let listTitle = "Your linked nests"

        public static let unlink = "Unlink"

        public static let capabilitiesLabel = "Syncs:"

        public static let capabilityAccountReplica = "sealed copy of your account settings"

        public static let expiryLabel = "Expires:"

        public static let expiryNever = "Never expires"

        public static let linkRecoveryKeysDiffer = "These two nests hold different recovery keys for this account, so they can't be linked."

        public static func forwardQueueSummary(count: String) -> String {
            "\(count) of your posts are waiting to reach your relay."
        }

        public static func forwardQueueStuck(count: String) -> String {
            "\(count) of them have been refused for more than eight hours. Check that this nest is allowed to forward your posts on the relay — linking the relay again from here grants that — or stop forwarding them below."
        }

        public static func forwardQueueLastError(error: String) -> String {
            "Last attempt failed: \(error)"
        }

        public static let forwardRetry = "Retry now"

        public static let forwardDiscard = "Stop forwarding these"
    }

    public enum linkedNests {
        public static let title = "Linked nests"

        public static let unlink = "Unlink"
    }

    public enum logs {
        public static let title = "Logs"

        public static let description = "Recent activity recorded on this device, newest first. No message contents or secrets are ever logged — only what happened, when, and where."

        public static let filterLabel = "Severity"

        public static let filterAll = "All"

        public static let levelError = "Error"

        public static let levelWarn = "Warn"

        public static let levelInfo = "Info"

        public static let levelDebug = "Debug"

        public static let levelTrace = "Trace"

        public static let copyButton = "Copy"

        public static let clearButton = "Clear"

        public static let empty = "No log entries yet."
    }

    public enum mailSettings {
        public static let title = "Mail & Calendar"

        public static let keysInfo = "Your mail is protected by an encryption key held on your Fauna devices. Each app password or token you add unlocks that same key for one email app (Thunderbird, Apple Mail, …). Rotate your keys if a password or token may have leaked, or if a device that had your mail set up was lost or stolen — rotating replaces the key so the exposed credential can no longer read your mail (messages you've already received stay readable). If you're just retiring an app you no longer use, revoke that one credential instead — you don't need to rotate."

        public static let credentialsOnConnectedApps = "Your app passwords are listed under Settings → Connected apps — copy, reveal or disconnect each one there."

        public static let serveHereLabel = "Serve my mail & calendar over IMAP/CalDAV on this nest"

        public static let serveHereSubtitle = "When on, this nest answers IMAP and CalDAV for your mailbox so email and calendar apps can connect here. Turn it off if you read your mail on a different nest — your own Fauna apps are unaffected either way."

        public static let forwardingTitle = "Forwarding"

        public static let forwardAllToLabel = "Forward all incoming mail to"

        public static let forwardAllToSubtitle = "Every message you receive is also sent on to this address, and you keep your own copy. Clear the field to stop forwarding."

        public static let forwardPerHourLabel = "Hourly forwarding limit"

        public static func forwardPerHourSubtitle(ceiling: String) -> String {
            "The most messages forwarded for you in one hour, up to \(ceiling). Past the limit, forwards wait for the next hour."
        }

        public static let forwardAllToInvalid = "That doesn't look like an email address."

        public static let forwardPerHourNotANumber = "Enter the hourly forwarding limit as a whole number."

        public static let forwardPerHourZero = "The hourly forwarding limit must be at least 1."

        public static func forwardPerHourAboveCeiling(ceiling: String) -> String {
            "The hourly forwarding limit can't be more than \(ceiling)."
        }
    }

    public enum webSettings {
        public static let title = "Web"

        public static let subdomainToggleLabel = "Publish my website"

        public static let subdomainToggleSubtitle = "When on, this nest serves your Web files and web-published posts at your personal address. Off by default."

        public static let subdomainUrlLabel = "Your site"

        public static let subdomainNoHandle = "Set a handle first to get a personal web address."

        public static let subdomainReserved = "Your handle is a reserved name and can't host a website."

        public static let subdomainNoServingDomain = "This nest has no web address yet, so it can't serve websites. An admin needs to give it a domain first."

        public static let renderStatusDown = "Your published pages are temporarily unavailable. This nest is restoring them by itself — there is nothing you need to do. Synced files are not affected."

        public static let contentInfo = "Add content by turning on a folder's website toggle under Settings → Folders, or by publishing individual posts to the web."

        public static let publishedPostsTitle = "Published posts"

        public static let publishedPostsEmpty = "No published posts yet"

        public static func publishedPostGatedBadge(tier: String) -> String {
            "Paid: \(tier)"
        }

        public static let linkDisabledSubdomainOff = "Turn on \"Publish my website\" above to get links you can share."

        public static let linkDisabledNoHandle = "Set a handle first — your published posts need a web address before they can be linked to."

        public static let linkDisabledReserved = "Your handle is a reserved name, so your published posts have no public address to link to."
    }

    public enum webPublish {
        public static let publishToWeb = "Publish to web"

        public static let unpublish = "Unpublish"

        public static let copyWebLink = "Copy web link"

        public static let copyPaywallLink = "Copy paywall link"

        public static let paywallLinkNote = "A paywall link opens the full post for anyone, but only for about 10 minutes — it's for a quick preview, not for giving lasting free access. For that, send a claim code instead."

        public static func copiedLink(url: String) -> String {
            "Copied: \(url)"
        }

        public static func copiedPaywallLink(url: String) -> String {
            "Copied, works for about 10 minutes: \(url)"
        }

        public static let menuNoLinkReason = "These links need a web address. Turn on your website in Settings → Web."

        public static func errorPublish(message: String) -> String {
            "Failed to publish post: \(message)"
        }

        public static func errorUnpublish(message: String) -> String {
            "Failed to unpublish post: \(message)"
        }

        public static func errorPaywallLink(message: String) -> String {
            "Failed to create paywall link: \(message)"
        }
    }

    public enum mailAliases {
        public static let title = "Aliases"

        public static let description = "Extra mail addresses that all deliver to you — share a different one with each service so you can see who leaked your address and turn any of them off."

        public static let addButton = "Add alias"

        public static let generateButton = "Generate disposable"

        public static let empty = "No aliases yet"

        public static let loading = "Loading your aliases…"

        public static let formTitle = "Add alias"

        public static let kindWildcardLabel = "Wildcard prefix (matches anything starting with it)"

        public static let patternPlaceholder = "Address (e.g. shop, news-)"

        public static let labelPlaceholder = "Label (optional)"

        public static let spamThresholdPlaceholder = "Spam threshold override (0–15, optional)"

        public static let ratePerHourPlaceholder = "Rate limit per hour (optional)"

        public static let ttlPlaceholder = "Disposable lifetime in days (optional)"

        public static let usesPlaceholder = "Disposable max uses (optional)"

        public static let submit = "Add"

        public static let cancel = "Cancel"

        public static let revoke = "Revoke"

        public static let delete = "Delete"

        public static let edit = "Edit"

        public static let showAudit = "Show audit"

        public static let disabledBadge = "disabled"

        public static let activeToggleLabel = "Active"

        public static let activeToggleTooltip = "When on, this address receives mail. Turn off to bounce mail to it without deleting the address — you can turn it back on anytime."

        public static let primaryAddressBadge = "Primary address"

        public static let primaryAddressTooltip = "Your main address and sign-in identity. It can't be disabled, renamed, or deleted so your mail and login always work."

        public static let copied = "Copied address"

        public static let kindExact = "Exact"

        public static let kindSubaddress = "+suffix"

        public static let kindWildcard = "Wildcard"

        public static let kindDisposable = "Disposable"

        public static let kindCatchall = "Catch-all"

        public static let kindForwarder = "Forwarder"

        public static let kindOther = "Alias"

        public static let noDefaultDomain = "Enable mail before adding aliases."

        public static func hits(count: String) -> String {
            "\(count) hits"
        }

        public static func hitsWithLast(count: String, date: String) -> String {
            "\(count) hits · last \(date)"
        }

        public static let importButton = "Import addresses"

        public static let importTitle = "Import addresses"

        public static let importSubtitle = "Paste one address per line. Each becomes an exact alias that delivers to you; addresses you already have are skipped."

        public static let importPlaceholder = "One address per line"

        public static let importSubmit = "Import"

        public static let importCancel = "Cancel"

        public static func importResult(created: String, existed: String, invalid: String) -> String {
            "\(created) created · \(existed) already existed · \(invalid) invalid"
        }

        public static func importInvalidLine(address: String, reason: String) -> String {
            "\(address) — \(reason)"
        }
    }

    public enum mailSpam {
        public static let title = "Spam"

        public static let description = "Your spam filter learns from what you mark as spam or not-spam. Reset its training, choose whether to help the deployment's shared filter, and undo any past training here."

        public static let resetButton = "Reset spam classifier"

        public static let resetConfirm = "Reset for good? This cannot be undone."

        public static let resetSubtitle = "Deletes your spam-training model and history. Future mail starts from scratch. This cannot be undone."

        public static let contributeBaselineLabel = "Contribute to deployment spam baseline"

        public static let contributeBaselineSubtitle = "Off by default. When on, your training helps seed the shared filter new accounts start from — your individual messages are never shared."

        public static let shareReportsLabel = "Share spam reports (anonymized)"

        public static let shareReportsSubtitle = "Off by default. When on, the fact that you flagged a message as spam joins an anonymized count your nest shares — but only once at least 3 people here have flagged the same content, and never your identity or the message itself."

        public static let thresholdOverrideLabel = "Spam-folder threshold override (0–15, optional)"

        public static let thresholdOverrideSubtitle = "Messages scoring at or above this override are filed to Junk, instead of the deployment default. 0 turns automatic filing off for this account; leave blank to follow the default."

        public static let publishedTitle = "What this nest publishes"

        public static let publishedDescription = "The anonymized report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 reporters."

        public static let publishedEmpty = "This nest publishes no report aggregates yet"

        public static let publishedReporters = "reporters"

        public static let historyTitle = "Training history"

        public static let empty = "No training history yet"

        public static let undo = "Undo"

        public static let labelSpam = "Spam"

        public static let labelHam = "Not spam"

        public static let labelUnknown = "Other"

        public static let sourceExplicitButton = "Fauna app"

        public static let sourceImapJunkFlag = "Junk flag"

        public static let sourceImapJunkMove = "Junk move"

        public static let sourceUnknown = "Other"

        public static let backendUnbuilt = "Spam-classifier training is not available on this nest yet."
    }

    public enum mailExport {
        public static let title = "Export mailbox"

        public static let description = "Download your whole mailbox in a standard format you can import into another mail app. The export is encrypted until you download it."

        public static let formatTitle = "Step 1 — Format"

        public static let formatMbox = "mbox (one file per mailbox — broadest support)"

        public static let formatMaildir = "Maildir++ (one file per message; preserves flags)"

        public static let formatEml = "EML zip (one .eml per message + manifest)"

        public static let scopeTitle = "Step 2 — What to include"

        public static let scopeMailboxesLabel = "Mailboxes"

        public static let scopeMailboxesEmpty = "No mailboxes to export."

        public static let scopeDateFromPlaceholder = "From date (optional, YYYY-MM-DD)"

        public static let scopeDateToPlaceholder = "To date (optional, YYYY-MM-DD)"

        public static let scopeStripHeadersLabel = "Strip transit headers"

        public static let scopeStripHeadersSubtitle = "Removes the headers mail servers add in transit — relay hops, server names and IP addresses. Off keeps full forensic fidelity."

        public static let confirmTitle = "Step 3 — Confirm"

        public static let confirmPending = "Estimate unavailable until the export backend is ready."

        public static let startButton = "Start export"

        public static let progressTitle = "Step 4 — Exporting"

        public static let pauseButton = "Pause"

        public static let resumeButton = "Resume"

        public static let cancelButton = "Cancel"

        public static let errorLogTitle = "Skipped / errored messages"

        public static let doneTitle = "Step 5 — Done"

        public static let downloadButton = "Download export"

        public static let downloadUrlLabel = "Download link (for another device)"

        public static let discardButton = "Discard now"

        public static let next = "Next"

        public static let back = "Back"

        public static let backendUnbuilt = "Mailbox export is not available on this nest yet."

        public static func confirmSummaryFmt(format: String, mailboxes: String) -> String {
            "\(format) · \(mailboxes) mailbox(es)"
        }

        public static func progressSummaryFmt(exported: String, total: String, skipped: String, errored: String) -> String {
            "\(exported) of \(total) · \(skipped) skipped · \(errored) errored"
        }

        public static func doneSummaryFmt(format: String, bytes: String) -> String {
            "\(format) · \(bytes) bytes"
        }

        public static func savedSummaryFmt(format: String, bytes: String, path: String) -> String {
            "\(format) · \(bytes) bytes · saved to \(path)"
        }
    }

    public enum mailLists {
        public static let title = "Lists"

        public static let description = "Run a newsletter or mailing list from your own address, with one-click unsubscribe built in."

        public static let addButton = "Add list"

        public static let empty = "No lists yet"

        public static let loading = "Loading your lists…"

        public static let formTitle = "Add list"

        public static let namePlaceholder = "List name (e.g. Bob's Weekly)"

        public static let localPartPlaceholder = "Address (e.g. newsletter)"

        public static let domainLabel = "Domain"

        public static let descriptionPlaceholder = "Description (optional)"

        public static let listHelpPlaceholder = "List-Help URL (optional)"

        public static let listArchivePlaceholder = "List-Archive URL (optional)"

        public static let perSendPlaceholder = "Recipients per send (optional)"

        public static let submit = "Add"

        public static let cancel = "Cancel"

        public static let edit = "Edit"

        public static let delete = "Delete"

        public static let deleteConfirm = "Delete the list and all its members?"

        public static let archiveOffServerConfirm = "This archive link is not on your server and goes out with every message. Save anyway?"

        public static let members = "Members"

        public static let noDomain = "Add a mail domain before creating lists."

        public static let membersTitle = "Members"

        public static let membersNoList = "Open a list from the Lists page to manage its members."

        public static let membersLoading = "Loading members…"

        public static func summaryFmt(subscribed: String, unsubscribed: String) -> String {
            "\(subscribed) subscribed · \(unsubscribed) unsubscribed"
        }

        public static let addMemberButton = "Add member"

        public static let addMemberPlaceholder = "Email address"

        public static let addMemberSubmit = "Add"

        public static let addMemberCancel = "Cancel"

        public static let importButton = "Import"

        public static let importPlaceholder = "One email address per line"

        public static let importSubmit = "Import"

        public static let importCancel = "Cancel"

        public static func importResult(added: String, existed: String, invalid: String) -> String {
            "\(added) added · \(existed) already subscribed · \(invalid) invalid"
        }

        public static let unsubscribe = "Unsubscribe"

        public static let resubscribe = "Resubscribe"

        public static let statusSubscribed = "Subscribed"

        public static let statusUnsubscribed = "Unsubscribed"

        public static let backendUnbuilt = "Mailing lists are not available on this nest yet."
    }

    public enum mailImport {
        public static let title = "Import mailbox"

        public static let description = "Pull your existing mail from Gmail, Outlook, iCloud, or any IMAP server into your Fauna mailbox. Your credentials never leave this device."

        public static let sourceTitle = "Step 1 — Source"

        public static let sourceUnavailable = "Importing from another mail server isn't available in the browser yet. You can start an import from the desktop or terminal app, and watch or pause it here."

        public static let sourceGmail = "Gmail"

        public static let sourceOutlook = "Outlook / Hotmail / Office365"

        public static let sourceIcloud = "iCloud"

        public static let sourceGeneric = "Generic IMAP"

        public static let sourceAppPasswordLabel = "App password"

        public static let sourceAppPasswordHelpGmail = "Requires 2FA on your Google account. Generate one at myaccount.google.com/apppasswords and paste it here."

        public static let sourceAppPasswordHelpIcloud = "Requires 2FA on your Apple ID. Generate one at appleid.apple.com and paste it here."

        public static let sourceOauthButton = "Connect with Microsoft"

        public static let sourceHostPlaceholder = "Server hostname"

        public static let sourcePortPlaceholder = "Port (default 993)"

        public static let tlsImplicit = "Implicit TLS (993)"

        public static let tlsStarttls = "STARTTLS (143)"

        public static let sourceUsernamePlaceholder = "Username"

        public static let sourcePasswordPlaceholder = "Password"

        public static let connectButton = "Connect"

        public static let scopeTitle = "Step 2 — What to import"

        public static let scopeMailboxesLabel = "Mailboxes"

        public static let scopeMailboxesEmpty = "No mailboxes found on the source server."

        public static let scopeDateFromPlaceholder = "From date (optional, YYYY-MM-DD)"

        public static let scopeMaxSizeLabel = "Max message size (MB)"

        public static let scopeMailboxMappingLabel = "Mailboxes map 1:1 by name — INBOX to INBOX, Sent to Sent, and so on. Mailboxes with no matching Fauna mailbox are created, named after the source."

        public static let confirmTitle = "Step 3 — Confirm"

        public static func confirmSummaryFmt(source: String, mailboxes: String, messages: String) -> String {
            "\(source) · \(mailboxes) mailbox(es) · \(messages) messages"
        }

        public static let startButton = "Start import"

        public static let progressTitle = "Step 4 — Importing"

        public static func progressSummaryFmt(imported: String, total: String, skipped: String, errored: String) -> String {
            "\(imported) of \(total) · \(skipped) skipped · \(errored) errored"
        }

        public static func progressRowFmt(count: String) -> String {
            "\(count) messages"
        }

        public static let pauseButton = "Pause"

        public static let resumeButton = "Resume"

        public static let cancelButton = "Cancel"

        public static let errorLogTitle = "Skipped / errored messages"

        public static let doneTitle = "Step 5 — Done"

        public static func doneSummaryFmt(imported: String, skipped: String, errored: String) -> String {
            "\(imported) imported · \(skipped) skipped · \(errored) errored"
        }

        public static let viewImportedButton = "View imported messages"

        public static let reviewSkippedButton = "Review skipped"

        public static let next = "Next"

        public static let back = "Back"
    }

    public enum archiveImport {
        public static let title = "Import from other services"

        public static let description = "Bring your posts, photos and events from a Facebook or Instagram export archive into Fauna, at their original dates and audiences. The archive itself stays in a sealed folder on your nest."

        public static let unavailable = "Importing needs a session that holds your identity key; this one does not."

        public static let sourceTitle = "Step 1 — Where the archive comes from"

        public static let sourceHelpFacebook = "On Facebook, open Settings & privacy → Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON, any media quality. The download link expires after a few days, so save the zip as soon as it is ready."

        public static let sourceHelpInstagram = "On Instagram, open Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON. The download link expires after a few days, so save the zip as soon as it is ready."

        public static let archiveTitle = "Step 2 — The archive"

        public static let archivePathPlaceholder = "Path to the export .zip"

        public static let archiveOpenButton = "Open archive"

        public static func archiveSummaryFmt(platform: String, owner: String, first: String, last: String, archiveSize: String, mediaSize: String) -> String {
            "\(platform) · \(owner) · \(first) – \(last) · \(archiveSize) archive, \(mediaSize) of photos and videos"
        }

        public static func archiveSummaryUndated(platform: String, owner: String, archiveSize: String, mediaSize: String) -> String {
            "\(platform) · \(owner) · no dated records · \(archiveSize) archive, \(mediaSize) of photos and videos"
        }

        public static let scopeTitle = "Step 3 — What to import"

        public static let scopeCategoriesLabel = "Categories"

        public static func scopeCategoryRowFmt(category: String, count: String) -> String {
            "\(category) (\(count))"
        }

        public static func scopeCategoryKeptFmt(category: String, count: String) -> String {
            "\(category) (\(count)) — kept in the archive for later"
        }

        public static let scopeAudienceModeLabel = "Audience"

        public static let audienceOriginal = "Keep original audiences"

        public static let audienceOnlyMe = "Only me"

        public static func scopeAudienceSummaryFmt(known: String, unknown: String) -> String {
            "\(known) posts and albums have a recorded audience and import to it; \(unknown) have none recorded and will be visible only to you."
        }

        public static let scopeAudienceSummaryOnlyMe = "Everything will be visible only to you."

        public static let scopeHiddenTiersUnavailable = "This nest predates hidden tiers, so only public posts can be imported now. Update the nest, then import again to pick up the rest."

        public static let scopeDateFromPlaceholder = "From date (optional, YYYY-MM-DD)"

        public static let scopeDateToPlaceholder = "To date (optional, YYYY-MM-DD)"

        public static let confirmTitle = "Step 4 — Confirm"

        public static func confirmSummaryFmt(records: String, bytes: String) -> String {
            "\(records) records · about \(bytes) to upload"
        }

        public static let startButton = "Start import"

        public static let progressTitle = "Step 5 — Importing"

        public static func progressSummaryFmt(state: String, imported: String, total: String, skipped: String) -> String {
            "\(state) · \(imported) of \(total) · \(skipped) skipped"
        }

        public static func progressRowFmt(imported: String, count: String, skipped: String) -> String {
            "\(imported) of \(count) · \(skipped) skipped"
        }

        public static let stateRunning = "Importing"

        public static let statePaused = "Paused"

        public static let stateCancelled = "Cancelled"

        public static let stateCompleted = "Done"

        public static let stateErrored = "Stopped after an error"

        public static let pauseButton = "Pause"

        public static let resumeButton = "Resume"

        public static let cancelButton = "Cancel"

        public static let errorLogTitle = "Skipped records"

        public static let doneTitle = "Step 6 — Done"

        public static func doneSummaryFmt(imported: String, skipped: String) -> String {
            "\(imported) imported · \(skipped) skipped"
        }

        public static let viewImportedButton = "View imported posts"

        public static let reviewSkippedButton = "Review skipped"

        public static let profilePrefillButton = "Use the archive's profile name and bio"

        public static func folderLinkFmt(folder: String) -> String {
            "Archive folder: \(folder)"
        }

        public static let categoryPosts = "Posts"

        public static let categoryAlbums = "Albums"

        public static let categoryComments = "Comments"

        public static let categoryReactions = "Reactions"

        public static let categoryEvents = "Events"

        public static let categoryGroups = "Groups"

        public static let categoryFriends = "Friends"

        public static let categoryThreads = "Message threads"

        public static let categoryMessages = "Messages"

        public static let categoryProfile = "Profile"

        public static let next = "Next"

        public static let back = "Back"
    }

    public enum searchPage {
        public static let title = "Search Results"

        public static let signInPrompt = "Sign in to search."

        public static let placeholder = "Search posts, profiles, email..."

        public static let hideSearchBar = "Hide search bar"

        public static let showSearchBar = "Show search bar"

        public static let noResults = "No results for"

        public static let noResultsShort = "No results"

        public static let all = "All"

        public static let clear = "Clear"

        public static let searchFailed = "Search failed"

        public static func searchFailedReason(reason: String) -> String {
            "Search failed: \(reason)"
        }

        public static let loadMoreFailed = "Load more failed"

        public static let searchMessages = "Search messages..."

        public static let badgePost = "Post"

        public static let badgeProfile = "Profile"

        public static let badgeEmail = "Email"

        public static let badgeEvent = "Event"

        public static let badgeMessage = "Message"

        public static let badgeContact = "Contact"

        public static let badgeFile = "File"

        public static let badgeDraft = "Draft"

        public static let badgeMedia = "Media"
    }

    public enum nostr {
        public static let title = "Nostr"

        public static let unavailable = "Nostr isn't available on this nest — it was built without Nostr support."

        public enum linkAccount {
            public static let title = "Link Nostr Account"

            public static let description = "Connect a Nostr identity to your Fauna account."

            public static let modeLabel = "Link mode"

            public static let generate = "Generate new keypair"

            public static let importNsec = "Import nsec"

            public static let nip07 = "NIP-07 browser extension"

            public static let nsecLabel = "nsec key"

            public static let nsecPlaceholder = "nsec1..."

            public static let nip07Prompt = "Your browser extension will be prompted for the public key."

            public static let linkButton = "Link Account"

            public static let enterNsec = "Please enter an nsec key"

            public static let noNip07 = "No NIP-07 browser extension detected"
        }

        public enum account {
            public static let title = "Nostr Account"

            public static let description = "Link a Nostr identity to your Fauna account"

            public static let publicKey = "Public Key"

            public static let signingMode = "Signing Mode"

            public static let modeGenerated = "Generated keypair"

            public static let modeImported = "Imported nsec"

            public static let modeRemote = "NIP-46 bunker"

            public static let modeNip07 = "NIP-07 extension"

            public static let modeProxied = "Proxied (paired nest signs)"

            public static let generateKeyButton = "Generate Key"

            public static let linkSubtitle = "Generate a new Nostr keypair on the nest"

            public static let unlink = "Unlink Account"

            public static let unlinkButton = "Unlink"

            public static let unlinkSubtitle = "Remove Nostr identity from your nest account"

            public static let statusNoClient = "No client"

            public static let statusUnavailable = "Nostr unavailable on this nest"
        }

        public enum npubConfirm {
            public static func banner(npub: String) -> String {
                "A recent account recovery changed your Nostr key. Please confirm the public key shown above (\(npub)) is yours."
            }

            public static let yesButton = "Yes, that's my npub"

            public static let noButton = "No / nothing is linked"
        }

        public enum settings {
            public static let title = "Content Settings"

            public static let description = "Configure Nostr publishing behavior"

            public static let autoPublish = "Auto-publish posts"

            public static let autoPublishSubtitle = "Automatically publish Fauna posts to Nostr relays"

            public static let publishReplies = "Publish replies"

            public static let publishReactions = "Publish reactions"

            public static let inboundTitle = "Inbound to feed"

            public static let inboundSubtitle = "Show events from followed Nostr users in your feed"

            public static let exposeTitle = "Expose content"

            public static let exposeSubtitle = "Allow Nostr users to see your Fauna content"
        }

        public enum relays {
            public static let title = "Relays"

            public static let none = "No relays configured. Default relays will be used."

            public static let add = "Add Relay"

            public static let placeholder = "wss://relay.example.com"

            public static let invalidUrl = "Relay URL must start with wss:// or ws://"

            public static let privateAddress = "Relays on a private network or on this device can't be used. Use a public relay address."
        }

        public enum follows {
            public static let title = "Follows"

            public static let add = "Add"

            public static let none = "No Nostr follows yet."

            public static let pubkeyPlaceholder = "npub1... or hex pubkey"

            public static let petnamePlaceholder = "Petname"
        }

        public enum connectedApps {
            public static let title = "Connected apps"

            public static let description = "Sign in to Nostr apps with your nest using Nostr Connect. Your key stays on the nest — apps only ask it to sign."

            public static let connectButton = "Connect an app"

            public static let revealTitle = "Scan or paste this in your Nostr app"

            public static let revealHint = "Shown once — copy it now."

            public static let qrAlt = "Nostr Connect QR code"

            public static let none = "No connected apps yet."

            public static let unnamed = "Unnamed app"

            public static let pending = "Waiting to connect…"

            public static func lastUsed(time: String) -> String {
                "Last used \(time)"
            }

            public static let neverUsed = "Never used"

            public static func expires(time: String) -> String {
                "Expires \(time)"
            }

            public static let disconnect = "Disconnect"
        }

        public enum zapSigners {
            public static let title = "Zap signers"

            public static let description = "Zaps are Lightning tips. A zap receipt is signed by the wallet provider that received the payment — not by the sender — so your nest only believes receipts from signers you name here."

            public static let pubkeyPlaceholder = "64-character hex signer pubkey"

            public static let labelPlaceholder = "Label (optional)"

            public static let add = "Designate signer"

            public static let remove = "Stop trusting"

            public static let none = "You have not designated any signer, so no zap is counted as paid. Add your wallet provider's signer key to start believing its receipts."

            public static let unnamed = "Unnamed signer"

            public static let invalidPubkey = "A signer pubkey must be 64 hexadecimal characters."
        }
    }

    public enum status {
        public enum identity {
            public static let notConfigured = "Not configured"
        }

        public enum connection {
            public static let service = "Service"
        }

        public enum syncAgent {
            public static let running = "Running"

            public static let restartPending = "Restart pending"

            public static let notRunning = "Not running"

            public static let keysPending = "Keys pending"

            public static let notEnrolled = "Not enrolled"
        }

        public enum sync {
            public static let syncing = "Syncing"

            public static let stopped = "Stopped"

            public static func menuStatus(status: String) -> String {
                "Sync: \(status)"
            }

            public static let filesSynced = "Files Synced"

            public static let filesPending = "Files Pending"

            public static let bytesPending = "Bytes Pending"

            public static let lastSync = "Last Sync"

            public static func pendingSummary(files: String, bytes: String) -> String {
                "\(files) files, \(bytes)"
            }
        }

        public enum quota {
            public static let title = "Quota"

            public static let inboxUsage = "Inbox Usage"

            public static let storageUsage = "Storage Usage"
        }

        public enum p2p {
            public static let title = "P2P"

            public static let tunnel = "Tunnel"

            public static func tunnelActiveWithAddress(address: String) -> String {
                "Active (\(address))"
            }
        }

        public enum node {
            public static let title = "Node"
        }

        public enum actions {
            public static let clearCache = "Clear Cache"
        }

        public enum inboxPrivacy {
            public static let title = "Inbox Privacy"

            public static let description = "Control who can send you messages."

            public static let `open` = "Open"

            public static let openDesc = "Anyone can message you directly."

            public static let allowKnock = "Allow Knocks"

            public static let allowKnockDesc = "New contacts must send a knock request first."

            public static let contactsOnly = "Contacts Only"

            public static let contactsOnlyDesc = "Only confirmed contacts can message you."

            public static let closed = "Closed"

            public static let closedDesc = "No new messages accepted."
        }

        public enum encryption {
            public static let keyPackages = "Key Packages"

            public static func available(count: String) -> String {
                "\(count) available"
            }

            public static let lowKeys = "Low key packages. Publishing more..."

            public static let checking = "Checking encryption status..."

            public static let publishing = "Publishing..."

            public static let mlsEngine = "MLS Engine"

            public static let dmChannels = "DM Channels"
        }

        public enum notifications {
            public static let enabled = "Push notifications are enabled."

            public static let description = "Get notified when messages arrive, even when Fauna is closed."

            public static let enable = "Enable Notifications"

            public static let disable = "Disable Notifications"

            public static let enabling = "Enabling..."

            public static let disabling = "Disabling..."

            public static let permissionDenied = "Notification permission was denied."

            public static let title = "Push Notifications"

            public static let contentEncrypted = "Notification content is encrypted end-to-end."

            public static let registered = "Registered with server"

            public static let deniedTitle = "Notifications Disabled"

            public static let deniedHint = "Enable notifications in system Settings to receive alerts."

            public static let openSettings = "Open Settings"

            public static let unavailable = "Push notifications are not available in this app build."
        }

        public enum dataExport {
            public static let description = "Download all your data, content included, as a zip archive. It may be large."

            public static func exportedTo(path: String) -> String {
                "Data exported to \(path)"
            }
        }

        public enum emailFilters {
            public static let none = "No email filters configured."

            public static let senderIs = "Sender is"

            public static let senderDomain = "Sender domain"

            public static let subjectContains = "Subject contains"

            public static let bodyContains = "Body contains"

            public static let headerExists = "Header exists"

            public static let actionAllow = "Allow"

            public static let actionDiscard = "Discard"

            public static let actionReject = "Reject"

            public static let actionFileInto = "File"

            public static let actionForward = "Forward"

            public static let actionAutoReply = "Auto-reply"

            public static let actionAddLabel = "Label"
        }

        public enum spam {
            public static let title = "Spam Filtering"

            public static let description = "Adjust how aggressively spam is filtered from your inbox and feeds."

            public static let spamThreshold = "Spam threshold"

            public static let phishingThreshold = "Phishing threshold"

            public static let aggressive = "Aggressive"

            public static let moderate = "Moderate"

            public static let permissive = "Permissive"

            public static let save = "Save Spam Preferences"
        }

        public enum changeHandle {
            public static let placeholder = "new-handle"

            public static let changing = "Changing..."
        }

        public enum dangerZone {
            public static let deleteHint = "Permanently removes your handle and data from this node. Your key is not affected."
        }

        public enum adminSection {
            public static let title = "Nest Administration"

            public static let description = "You are an admin of this nest."

            public static let dashboard = "Admin Dashboard"
        }

        public enum selfHost {
            public static let title = "Run your own nest"
        }

        public enum build {
            public static let title = "Build"

            public static let commit = "Commit"

            public static let verifyHint = "Verify this build: clone the repo at this commit, build locally, and compare file hashes."
        }
    }

    public enum admin {
        public static let aliases = "Aliases"

        public static let exit = "Exit admin"

        public static func actorIdFallbackLabel(short: String) -> String {
            "actor \(short)…"
        }

        public enum dashboard {
            public static let title = "Dashboard"

            public static let loading = "Loading stats..."

            public static func loadError(message: String) -> String {
                "Failed to load admin stats: \(message)"
            }

            public static let nestDomain = "Nest Domain"

            public static let version = "Version"

            public static let mail = "Mail"

            public static let totalStorage = "Total Storage"

            public static let email = "Email"

            public static let tls = "TLS"

            public static let registration = "Registration"

            public static let pairedNests = "Paired Nests"

            public static let loadingDashboard = "Loading dashboard..."

            public static let notAdmin = "Not an Admin"

            public static let notAdminDesc = "Enter a valid admin token to access the dashboard."

            public static let adminToken = "Admin token"

            public static let tokenPrompt = "Enter your admin Bearer token to view nest statistics."

            public static let nestDashboard = "Nest Dashboard"

            public static let suspended = "Suspended"

            public static let connections = "Connections"

            public static let noUsers = "No users"
        }

        public enum usersPage {
            public static let title = "Users"

            public static let loading = "Loading users..."

            public static func total(count: String) -> String {
                "\(count) users total"
            }

            public static let noHandle = "no handle"

            public static let evict = "Evict"

            public static let suspend = "Suspend"

            public static let cancelEviction = "Cancel Eviction"

            public static func evictConfirm(id: String) -> String {
                "Start eviction for \(id)? The user will be warned and given time to export data."
            }

            public static let evictDefaultReason = "Evicted by admin"

            public static let suspendDefaultReason = "Suspended by admin"

            public static let makeAdmin = "Make Admin"

            public static let removeAdmin = "Remove Admin"

            public static let prevPage = "Previous"

            public static let nextPage = "Next"

            public static func pageIndicator(current: String, pages: String) -> String {
                "Page \(current) of \(pages)"
            }

            public static let sectionRequests = "Pending requests"

            public static let sectionRegistration = "Registration"

            public static let sectionAdmit = "Admit someone directly"

            public static let sectionInvite = "Invite"

            public static let sectionUsers = "Users"

            public static let sectionPending = "Pending admin actions"

            public static func pendingCount(count: String) -> String {
                "Pending admin actions (\(count))"
            }

            public static let pendingNone = "Nothing is pending."

            public static func pendingBy(who: String) -> String {
                "by \(who)"
            }

            public static func pendingApprovals(given: String, needed: String) -> String {
                "\(given) of \(needed) approvals"
            }

            public static let pendingApprove = "Approve"

            public static let admitActorLabel = "Their account key (64 characters)"

            public static let admitHandleLabel = "Their handle (blank admits without one — they cannot send email until they have a handle)"

            public static let admitButton = "Admit"

            public static let admitActorHint = "The account key must be exactly 64 hex characters."

            public static let registrationModeLabel = "Who may create an account"

            public static let registrationModeOpen = "Anyone"

            public static let registrationModeInviteRequired = "Only people with an invite code"

            public static let registrationModeClosed = "Nobody — only I can admit people"

            public static let maxFreeUsersLabel = "Limit free accounts"

            public static let maxFreeUsersHint = "Blank for no limit. Counts every free account, including yours."

            public static let registrationSave = "Save"

            public static func registrationModeUnknown(mode: String) -> String {
                "This nest uses a registration setting this app version does not recognize (\(mode)). Update the app to change it."
            }

            public static let copyCode = "Copy code"

            public static func mintedCode(code: String) -> String {
                "New invite code: \(code)"
            }

            public static let servingHere = "Serving here"

            public static let servingDisabled = "Not serving"

            public static let noPendingRequests = "No pending requests."

            public static let noUsers = "No users."

            public static let codeMinted = "Code minted —"

            public static func userCount(count: String) -> String {
                "\(count) users"
            }

            public static let guardianLabel = "Guardian"

            public static let guardianNone = "None"

            public static let ageVerificationRequiredLabel = "Accept only signups carrying app age verification"

            public static let cancel = "Cancel"

            public static let delete = "Delete"
        }

        public enum aliasesPage {
            public static let title = "External Forwarders"

            public static let loading = "Loading aliases..."

            public static let localPart = "Local part"

            public static let targetAddress = "Target address"

            public static let domainOptional = "Domain (optional)"

            public static let createButton = "Create Alias"

            public static func count(count: String) -> String {
                "\(count) aliases"
            }

            public static let noAliases = "No aliases configured yet."

            public static let targetCol = "Target"

            public static let createdCol = "Created"

            public static let forward = "Forward"

            public static let forwardersTitle = "External Forwarders"

            public static let forwardersDesc = "Map an address on one of your domains to an external destination. Forwarded addresses have no local mailbox."

            public static let forwarderDomain = "Domain"

            public static let forwarderLocalPart = "Local part"

            public static let forwarderTarget = "Forwards to"

            public static let forwarderTargetPlaceholder = "name@example.com"

            public static let forwarderLocalPartPlaceholder = "info"

            public static let createForwarder = "Add Forwarder"

            public static let deleteForwarder = "Delete"

            public static let noForwarders = "No external forwarders configured yet."

            public static func forwarderRow(address: String, target: String) -> String {
                "\(address) → \(target)"
            }
        }

        public enum settingsPage {
            public static let title = "Tiers"

            public static func loadTiersError(message: String) -> String {
                "Failed to load tiers: \(message)"
            }

            public static func saveTierError(message: String) -> String {
                "Failed to save tier: \(message)"
            }

            public static let saveTierErrorInvalidCap = "Can't save this tier — every limit must be a whole number."

            public static let saveMembershipTierErrorNoTier = "Can't save this row — pick an \"Admits at\" tier first."

            public static let inviteCodes = "Invite Codes"

            public static let loadingCodes = "Loading invite codes..."

            public static let noCodes = "No invite codes."

            public static func usesLeft(remaining: String, total: String) -> String {
                "\(remaining)/\(total) uses left"
            }

            public static func usesLeftN(count: String) -> String {
                "\(count) uses left"
            }

            public static func created(date: String) -> String {
                "created \(date)"
            }

            public static let createCode = "Create Invite Code"

            public static let maxUses = "Max Uses"

            public static let tiers = "Tiers"

            public static let loadingTiers = "Loading tiers..."

            public static let noTiers = "No tier definitions found."

            public static let inboxLimit = "Inbox Limit"

            public static let storageLimit = "Storage Limit"

            public static func tierCaps(inbox: String, storage: String, devices: String) -> String {
                "Inbox \(inbox) · Storage \(storage) · \(devices) devices"
            }

            public static let capInboxBytes = "Inbox (bytes)"

            public static let capStorageBytes = "Storage (bytes)"

            public static let capDevices = "Devices"

            public static let capBlobSize = "Blob size (bytes)"

            public static let capFeeds = "Feeds"

            public static let saveTier = "Save"

            public static let addTierSection = "Define a new tier"

            public static let addTierName = "Tier name"

            public static let addTier = "Add tier"

            public static func addTierError(message: String) -> String {
                "Failed to add tier: \(message)"
            }

            public static let addTierErrorEmptyName = "Can't add this tier — give it a name."

            public static let addTierErrorInvalidCap = "Can't add this tier — every limit must be a whole number."

            public static let membershipSection = "Membership"

            public static let loadingMembership = "Loading membership designations..."

            public static let noMembershipTiers = "You have no subscription tiers yet — create one in your Tiers tab, then designate it here for paid nest access."

            public static let membershipAdmitsAt = "Admits at"

            public static let membershipLapsesTo = "Lapses to"

            public static let membershipSave = "Save"

            public static let membershipClear = "Clear"

            public static func loadMembershipTiersError(message: String) -> String {
                "Failed to load membership designations: \(message)"
            }

            public static func saveMembershipTierError(message: String) -> String {
                "Failed to save membership designation: \(message)"
            }

            public static func clearMembershipTierError(message: String) -> String {
                "Failed to clear membership designation: \(message)"
            }

            public static let emailDomains = "Email Domains"

            public static let loadingDomains = "Loading email domains..."

            public static let noDomains = "No email domains configured."

            public static let unverified = "Unverified"

            public static let factoryResetSection = "Danger Zone"

            public static let factoryResetTitle = "Factory Reset This Nest"

            public static let factoryResetDesc = "Wipe all deployment state (users, mail, stored content) and return this nest to a fresh, unclaimed state. The nest identity and TLS certificate are preserved. You will re-claim the nest immediately afterward."

            public static let factoryResetButton = "Factory Reset…"

            public static let factoryResetConfirmTitle = "Factory reset this nest?"

            public static let factoryResetConfirmBody = "This permanently deletes all users, mail, and deployment configuration on this nest and returns it to an unclaimed state. The nest identity and TLS certificate are kept, and you will be guided through re-claiming it. This cannot be undone."

            public static let factoryResetConfirmButton = "Factory Reset"

            public static let factoryResetCancel = "Cancel"

            public static let factoryResetFailed = "Factory reset failed. The nest is unchanged."

            public static let factoryResetPersistFailed = "Could not save the new setup code on this device, so the reset was not started and your nest is unchanged. Free up storage and try again."
        }

        public enum nestPage {
            public static let title = "Nest"

            public static let description = "Nest-wide settings for this deployment."

            public static let retireTitle = "Retire this server"

            public static let retireDesc = "Delete this server at your cloud provider and remove the DNS records that point at it. Unlike a factory reset, which wipes a server you keep, this destroys the server itself. You'll need your cloud provider's token."

            public static let retireButton = "Retire this server…"

            public static func loadSettingsError(message: String) -> String {
                "Failed to load nest settings: \(message)"
            }

            public static func updateSettingError(message: String) -> String {
                "Failed to update nest setting: \(message)"
            }

            public static func loadServingPortError(message: String) -> String {
                "Failed to load serving port: \(message)"
            }

            public static func setServingPortError(message: String) -> String {
                "Failed to set serving port: \(message)"
            }

            public static let servingPortLabel = "Serving port"

            public static let servingPortDesc = "The port this nest's client-facing API and web app listen on for desktop or IP-only nests with no router in front — reach it at https://this-host:port/. Default 443. Behind the cloud router this is ignored: the external port is set by the deployment. Takes effect after the nest restarts."

            public static let servingPortSave = "Save port"

            public static let servingPortInvalid = "Enter a port number between 1 and 65535."

            public static let servingPortFrontedHint = "Served on 443 by this deployment."

            public static let natModeLabel = "Network mode"

            public static let natModeLoading = "Loading network mode…"

            public static let natModeChoosing = "Applies to mail serving immediately; certificates and connectivity re-check at the next restart."

            public static let natModeSubmitting = "Saving network mode…"

            public static let natModeSaved = "Network mode saved. Mail serving updated now; certificates and connectivity re-check at the next restart."

            public static let natModeSave = "Save mode"

            public static func natModeErrorLoad(cause: String) -> String {
                "Couldn't load the current network mode: \(cause). You can still pick and save a mode."
            }

            public static func natModeErrorTransient(cause: String) -> String {
                "Couldn't save the network mode: \(cause). Try again."
            }

            public static func natModeErrorTerminal(cause: String) -> String {
                "Couldn't save the network mode: \(cause)."
            }

            public static let webAppOriginLabel = "Web app"

            public static let webAppOriginDesc = "What this server's own address answers when someone opens the app there."

            public static let webAppOriginBundled = "Serve the app this server ships"

            public static func webAppOriginCentral(origin: String) -> String {
                "Send people to \(origin), with this server filled in"
            }

            public static let webAppOriginSave = "Save web app choice"

            public static let webAppOriginLoading = "Loading the web app choice…"

            public static let webAppOriginStatusBundled = "This server's address serves the app it ships."

            public static func webAppOriginStatusCentral(target: String) -> String {
                "People who open this server's address are sent to \(target)"
            }

            public static func webAppOriginStatusDomainless(origin: String) -> String {
                "Sending people to \(origin) is chosen, but this server has no domain to fill in yet, so its address keeps serving the app it ships."
            }

            public static func webAppOriginStatusUnknownMode(mode: String) -> String {
                "This server uses a web app choice this app doesn't recognize (\(mode)). Update the app to change it."
            }

            public static let webAppOriginStatusPredates = "This server is too old to offer this choice; its address always serves the app it ships. Update the server to change it."

            public static func webAppOriginScope(origin: String) -> String {
                "This changes only what this server's own address answers. Anyone who opens \(origin) directly loads the app from there either way, and an address someone types or bookmarks always wins."
            }

            public static let osUpToDate = "OS up to date"

            public static let osUpdatesPending = "Security updates pending"

            public static let osRestartPending = "Restart pending — will restart automatically when idle"

            public static let osRestartNow = "Restart now"

            public static func osRestartNowError(message: String) -> String {
                "Failed to request a host restart: \(message)"
            }

            public static let regionLabel = "Region"

            public static let regionDesc = "The region whose laws this deployment operates under. You declare it; it is never detected from an address or a network. If that region has an authority publishing feature rules, they apply to accounts hosted here."

            public static let regionNone = "No region declared"

            public static func regionDeclared(region: String) -> String {
                "Declared region: \(region)"
            }

            public static let regionUnreadable = "The stored region declaration can't be read. Declare the region again (or withdraw it) to fix this; any region rules already received stay in force."

            public static let regionPlaceholder = "Country or region code, e.g. NO"

            public static let regionSave = "Declare region"

            public static let regionWithdraw = "Withdraw declaration"

            public static let regionNotEnrolled = "No authority is enrolled for this region, so no region rules apply here."

            public static let regionEnrolledNoDocument = "An authority is enrolled for this region; no rules have been published yet."

            public static func regionDocument(authority: String, sequence: String) -> String {
                "Region rules in force, published by \(authority) (version \(sequence))."
            }

            public static let regionStale = "Haven't been able to check for updated region rules recently. The rules already received stay in force."

            public static let regionInvalid = "Enter a 2–8 character region code in capitals, like NO or EU."

            public static func regionLoadError(message: String) -> String {
                "Failed to load the declared region: \(message)"
            }

            public static func regionSaveError(message: String) -> String {
                "Failed to save the declared region: \(message)"
            }

            public static let rotateSeedLabel = "Deployment identity"

            public static let rotateSeedDesc = "Give this nest a brand-new identity. Apps that already trust it re-trust it automatically, and anyone still holding the old identity — a removed admin, a lost device — stops being able to use it. It does not undo anything they already saw. Remove the admin first: everyone on the roster inherits the new identity."

            public static let rotateSeedButton = "Rotate deployment identity"

            public static let rotateSeedConfirmBody = "These admins inherit the new identity and can still recover this nest. Anyone not listed loses that ability. This cannot be undone."

            public static let rotateSeedConfirmButton = "Rotate now"

            public static let rotateSeedCancelButton = "Cancel"

            public static let rotateSeedRosterLoading = "Checking who currently administers this nest…"

            public static func rotateSeedRosterError(cause: String) -> String {
                "Couldn't check who currently administers this nest: \(cause). Nothing was rotated — try again."
            }

            public static let rotateSeedRosterEmpty = "This nest reported no administrators, which can't be right. Nothing was rotated — reload this page and try again."

            public static let rotateSeedWorking = "Rotating the deployment identity…"

            public static let rotateSeedDone = "Deployment identity rotated. Apps re-trust this nest automatically."

            public static let rotateSeedDoneUnmarked = "Deployment identity rotated. Your recovery list still shows the old identity — reconnect from this device to clear it."

            public static let rotateSeedMismatch = "This nest reported a different identity than the one that was sent. Nothing further was changed; check the nest before trying again."

            public static func rotateSeedFailed(cause: String) -> String {
                "Couldn't rotate the deployment identity: \(cause)"
            }

            public static let takedownLabel = "Legal takedown"

            public static let takedownDesc = "The one nest-wide content removal, for legal compulsion only (a court order, a statutory demand). Every takedown serves a visible tombstone in place of the content, can be appealed by the author, and writes a permanent audit record. It is never a policy or opinion lever."

            public static let takedownContentIdLabel = "Content id"

            public static let takedownTypePost = "Post"

            public static let takedownTypeConversation = "Conversation message"

            public static let takedownReferenceLabel = "Legal reference"

            public static let takedownRestoreLabel = "Overturn an existing takedown (restore)"

            public static let takedownArmTakedown = "Take down…"

            public static let takedownArmRestore = "Restore…"

            public static let takedownBlockedNoContent = "Enter the content id of the item named by the legal obligation."

            public static let takedownBlockedNoReference = "A legal reference is required — a takedown without one is refused."

            public static func takedownConfirmTakedown(contentType: String, contentId: String, reference: String) -> String {
                "Take down \(contentType) \(contentId), citing \"\(reference)\"? A visible tombstone will be served in its place, the author can appeal, and a permanent audit row records this action."
            }

            public static func takedownConfirmRestore(contentType: String, contentId: String) -> String {
                "Overturn the takedown of \(contentType) \(contentId)? The content serves again; the takedown record remains as history."
            }

            public static let takedownConfirmButtonTakedown = "Confirm takedown"

            public static let takedownConfirmButtonRestore = "Confirm restore"

            public static let takedownCancelButton = "Cancel"

            public static let takedownWorking = "Submitting…"

            public static let takedownDone = "Taken down. A tombstone is served in its place and the author can appeal."

            public static let takedownRestored = "Restored. The content is served again; the takedown stays on record."

            public static func takedownFailed(error: String) -> String {
                "The nest refused the request: \(error)"
            }

            public static let reportsLabel = "Reports"

            public static let reportsDesc = "Reports from users of this nest, and reports forwarded from other nests about accounts hosted here. A report is evidence for you to weigh; it removes nothing by itself. Acting means the legal-takedown console or a suspension — resolving a row only records what you decided."

            public static let reportsLoading = "Loading reports…"

            public static let reportsEmpty = "No open reports."

            public static func reportsOriginLocal(handle: String) -> String {
                "Reported by \(handle)"
            }

            public static func reportsOriginForwarded(nest: String) -> String {
                "Reported by a user of \(nest)"
            }

            public static let reportsOpenTakedown = "Open in takedown console"

            public static let reportsActed = "Mark as acted on"

            public static let reportsDismiss = "Dismiss"

            public static let reportsResolvedActed = "Recorded as acted on. The reporter is told the outcome, nothing more."

            public static let reportsResolvedDismissed = "Dismissed. The reporter is told the outcome, nothing more."

            public static func reportsFailed(error: String) -> String {
                "Could not update the report: \(error)"
            }

            public static let oauthLabel = "Outside-app sign-in keys"

            public static let oauthDesc = "The keys this nest signs outside apps' sign-in passes with, and the secret behind their saved sign-ins. Replacing them is a response to an exposure, never routine upkeep."

            public static let oauthKeysLoading = "Checking which sign-in keys this nest uses…"

            public static func oauthKeysError(cause: String) -> String {
                "Couldn't read this nest's sign-in keys: \(cause). Reload this page to try again."
            }

            public static func oauthKeySigning(kid: String) -> String {
                "\(kid) — signing now"
            }

            public static func oauthKeyRetiring(kid: String, minutes: String) -> String {
                "\(kid) — replaced; still accepted for \(minutes) min"
            }

            public static func oauthKeyRetired(kid: String) -> String {
                "\(kid) — replaced; no longer accepted"
            }

            public static let oauthRotateButton = "Replace sign-in key"

            public static func oauthRotateDesc(minutes: String) -> String {
                "A precaution: the current key stays accepted for \(minutes) min after it is replaced, so nobody is signed out. Use this for a suspected exposure."
            }

            public static let oauthForceRotateButton = "Replace sign-in key at once…"

            public static let oauthSecretForceRotateButton = "End all saved sign-ins…"

            public static let oauthForceRotateConfirmOne = "Replace the sign-in key at once? The key in use stops being accepted immediately, so every outside app signed in with it must sign in again. Use this when the key is known to have leaked."

            public static func oauthForceRotateConfirmMany(count: String) -> String {
                "Replace the sign-in key at once? All \(count) keys accepted now stop being accepted immediately, so every outside app signed in with them must sign in again. Use this when a key is known to have leaked."
            }

            public static let oauthForceRotateConfirmButton = "Replace at once"

            public static let oauthSecretForceRotateConfirm = "End every saved sign-in? Every connected outside app must be approved again. After a known leak, do this as well as replacing the sign-in key — replacing the key alone leaves saved sign-ins able to get new passes."

            public static let oauthSecretForceRotateConfirmButton = "End saved sign-ins"

            public static let oauthCancelButton = "Cancel"

            public static let oauthWorking = "Working…"

            public static func oauthRotateDone(kid: String) -> String {
                "Replaced. \(kid) signs from now on; the previous key stays accepted until it times out."
            }

            public static func oauthForceRotateDone(kid: String, dropped: String) -> String {
                "Replaced at once. \(kid) is now the only key accepted. Stopped working: \(dropped)."
            }

            public static func oauthForceRotateDoneNone(kid: String) -> String {
                "Replaced at once. \(kid) is now the only key accepted."
            }

            public static func oauthSecretForceRotateDone(minted: String, apps: String) -> String {
                "Ended. Every saved sign-in issued since \(minted) stopped working, and \(apps) outside apps were signed out; each must be approved again the next time it is used."
            }

            public static func oauthSecretForceRotateDoneOne(minted: String) -> String {
                "Ended. Every saved sign-in issued since \(minted) stopped working, and one outside app was signed out; it must be approved again the next time it is used."
            }

            public static func oauthSecretForceRotateDoneNone(minted: String) -> String {
                "Ended. Every saved sign-in issued since \(minted) stopped working. No outside apps were connected here."
            }

            public static let oauthSecretForceRotateFirst = "Done. There were no saved sign-ins to end."

            public static func oauthRotateFailed(cause: String) -> String {
                "The nest didn't confirm the change: \(cause). Check the keys listed here before trying again."
            }
        }

        public enum webPage {
            public static let title = "Web"

            public static let description = "Web-content hosting for this deployment."

            public static let apexSelectLabel = "Home page"

            public static let apexSelectSubtitle = "Choose whose website serves at this deployment's main address. None serves the built-in info page."

            public static let apexNone = "None (info page)"

            public static func apexInfo(url: String) -> String {
                "The main address serves at \(url)."
            }
        }

        public enum calendarPage {
            public static let title = "Calendar"

            public static let description = "Calendar (CalDAV) sync for this deployment."

            public static let enabledLabel = "Enable calendar (CalDAV) on this nest"

            public static let enabledSubtitle = "Serve calendar sync (CalDAV) for all users. Needs only a real domain with a public certificate — no email infrastructure — so calendar can run with or without email. The shared mail-and-calendar bridge runs whenever this or Enable mail is on."

            public static let caldavPortLabel = "CalDAV port"

            public static let caldavPortDesc = "The port the calendar (CalDAV) server listens on for desktop or IP-only nests that have no domain — reach it at https://this-host:port/. Default 8443. On a domain nest this is ignored: CalDAV is served at mail.your-domain on port 443."

            public static let caldavPortSave = "Save port"

            public static let caldavPortInvalid = "Enter a port number between 1 and 65535."
        }

        public enum contactsPage {
            public static let title = "Contacts"

            public static let description = "Contact (CardDAV) sync for this deployment."

            public static let enabledLabel = "Enable contacts (CardDAV) on this nest"

            public static let enabledSubtitle = "Serve contact sync (CardDAV) for all users. Rides the same server and certificate as calendar — no email infrastructure — so contacts can run with or without email or calendar. The shared bridge runs whenever this, Enable calendar, or Enable mail is on."
        }

        public enum filesPage {
            public static let title = "Files"

            public static let description = "File (WebDAV) sync for this deployment."

            public static let enabledLabel = "Enable files (WebDAV) on this nest"

            public static let enabledSubtitle = "Serve file access (WebDAV) for all users. Rides the same server and certificate as calendar and contacts — no email infrastructure — so files can run with or without email, calendar, or contacts. Nothing is served until a user flags a folder for WebDAV. The shared bridge runs whenever this, Enable contacts, Enable calendar, or Enable mail is on."
        }

        public enum inviteRequestsPage {
            public static let title = "Invite Requests"

            public static let description = "Review and approve or deny invite requests submitted by users."

            public static let empty = "No pending invite requests."

            public static let columnHandle = "Handle"

            public static let columnActor = "Actor"

            public static let columnMessage = "Message"

            public static let approve = "Approve"

            public static let deny = "Deny"

            public static let denyReasonPlaceholder = "Optional reason"

            public static let approving = "Approving..."

            public static let denying = "Denying..."

            public static let approveFailed = "Could not approve. Please try again."

            public static let denyFailed = "Could not deny. Please try again."
        }

        public enum servicesPage {
            public static let title = "Services"

            public static let description = "Enable or disable nest sidecar services."

            public static let bridge = "Email Bridge"

            public static let bridgeDesc = "IMAP, SMTP, and CalDAV (calendar) access for all users."

            public static let dns = "DNS"

            public static let dnsDesc = "Automatic DNS record management."

            public static let pairing = "Nest Pairing"

            public static let pairingDesc = "Let users link their own nests to sync their account (per-user multi-homing)."

            public static let enabled = "Enabled"

            public static let disabled = "Disabled"

            public static let manageDns = "Manage DNS"
        }

        public enum logsPage {
            public static let title = "Nest Logs"

            public static let description = "Recent activity recorded on the nest, newest first. No message contents or secrets are logged — only what happened, when, and where."

            public static let empty = "No nest log entries yet."
        }

        public enum custodyHosting {
            public static let title = "Held Custody"

            public static let description = "Data this nest holds on behalf of other people's accounts, at the request of an account holder here. Each row makes this nest dial an outside address on a schedule and keep what it serves."

            public static let empty = "No account here has asked this nest to hold data for anyone."

            public static func count(count: String) -> String {
                "\(count) held for others"
            }

            public static let host = "Requested by"

            public static let owner = "Held for"

            public static let url = "Pulled from"

            public static let budget = "Budget"

            public static let budgetDefault = "Default"

            public static let held = "Now holding"

            public static let stopped = "Paused"

            public static let active = "Active"

            public static let receiptFresh = "Confirmed recently"

            public static let receiptStale = "Not confirmed lately"

            public static let receiptNone = "Never confirmed"

            public static let remove = "Remove"

            public static let removeConfirmTitle = "Remove this held custody?"

            public static let removeConfirmBody = "This frees the space now. Pausing only stops the schedule and keeps what is already stored. Removing cannot be undone from here — the account holder would have to ask again."

            public static let removeConfirm = "Remove it"

            public static let removeCancel = "Keep it"

            public static let removed = "Removed."

            public static let removedWithStore = "Removed, and the stored copy was freed."

            public static let removeMissing = "That row was already gone."
        }

        public enum bridgesPending {
            public static let title = "Bridges"

            public static let description = "Mail and calendar bridges awaiting your approval."

            public static let empty = "No bridges awaiting approval."

            public static let emptyDesc = "Mail and calendar bridges that connect to this nest appear here for approval."

            public static let pubkey = "Public key"

            public static let role = "Role"

            public static let sourceIp = "Source IP"

            public static let firstSeen = "First seen"

            public static let sourceIpUnknown = "—"

            public static let approve = "Approve"

            public static let reject = "Reject"

            public static let nameMailCalendar = "Mail & calendar bridge"

            public static let nameMail = "Mail bridge"

            public static let nameBluesky = "Bluesky bridge"

            public static let nameBridge = "Bridge"

            public static let pendingSection = "Pending approval"

            public static let approvedSection = "Approved bridges"

            public static let approvedEmpty = "No approved bridges yet."

            public static let approvedAt = "Approved"

            public static let rotate = "Rotate service-user key"
        }

        public enum bridgesRotate {
            public static let title = "Rotate service-user key?"

            public static let warning = "The bridge will be marked revoked and will exit; the supervisor restarts it with a fresh key. On a mail-enabled box the new key is approved automatically."

            public static let confirm = "Rotate key"

            public static let cancel = "Cancel"
        }

        public enum mailPage {
            public static let title = "Mail"

            public static let description = "Box-wide mail policy — enable mail and tune the inbound perimeter and authentication enforcement. DKIM, TLS, and DNS records are managed automatically."

            public static let enabledLabel = "Enable mail"

            public static let enabledSubtitle = "Run the mail subsystem (SMTP / IMAP / CalDAV) for this nest."

            public static let healthTitle = "Mail health"

            public static let healthStateOff = "Mail: off"

            public static let healthStateBridgeDown = "Mail: mail service not connected"

            public static let healthStateBlocklisted = "Mail: server address is blocklisted"

            public static let healthStateQueueStalled = "Mail: outgoing mail is delayed"

            public static let healthStateRecordsFailing = "Mail: DNS records need attention"

            public static let healthStateWarmingUp = "Mail: warming up"

            public static let healthStateDelivering = "Mail: delivering"

            public static let healthStateUnknown = "Mail: needs attention"

            public static let healthCheckBridge = "Mail service connection"

            public static let healthCheckBlocklist = "Blocklist check"

            public static let healthCheckQueue = "Outgoing queue"

            public static let healthCheckRecords = "DNS and authentication records"

            public static let healthCheckWarmup = "Sending warm-up"

            public static let healthCheckLastDelivered = "Last delivered"

            public static let healthCheckLastReceived = "Last received"

            public static let healthCheckPass = "OK"

            public static let healthCheckWarn = "Warning"

            public static let healthCheckFail = "Problem"

            public static let healthCheckInfo = "Info"

            public static let healthNever = "Never"

            public static func healthStatusLine(state: String, delivered: String, received: String) -> String {
                "\(state) — last delivered: \(delivered) · last received: \(received)"
            }

            public static let healthDelist = "Request removal from the blocklist"

            public static let healthRecheck = "Check again"

            public static let healthWarmupReset = "Restart warm-up"

            public static let healthWarmupResetConfirm = "Restart the sending warm-up at day 1? Do this only after the server's outgoing IP address changed."

            public static let autoEnableNewUsersLabel = "Auto-enable mail for new users"

            public static let autoEnableNewUsersSubtitle = "New users automatically get a mailbox at their handle on first sign-in. Each user can still turn their own mail off."

            public static let spamGroupTitle = "Spam and inbound perimeter"

            public static let spamGroupDesc = "How inbound mail is scored, rate-limited, and gated before delivery."

            public static let thresholdJunkLabel = "Junk threshold"

            public static let thresholdJunkSubtitle = "Combined score (0–15) above which mail is delivered to Junk. 0 disables."

            public static let thresholdRejectLabel = "Reject threshold"

            public static let thresholdRejectSubtitle = "Score above which mail is rejected outright. 0 disables."

            public static let dnsblLabel = "DNS blocklists"

            public static let dnsblSubtitle = "One blocklist host per line, queried during delivery."

            public static let rejectNoRdnsLabel = "Reject senders with no rDNS"

            public static let greylistEnabledLabel = "Greylisting"

            public static let greylistDelayLabel = "Greylist delay (seconds)"

            public static let maxConnPerMinLabel = "Max connections / minute (per IP)"

            public static let fcrdnsModeLabel = "Forward-confirmed rDNS"

            public static let fcrdnsOff = "Off"

            public static let fcrdnsScoreSignal = "Score signal"

            public static let fcrdnsEnforce = "Enforce"

            public static let heloIdentityLabel = "Require HELO identity"

            public static let rejectFcrdnsFailLabel = "Reject on FCrDNS failure"

            public static let maxMessageBytesLabel = "Max message size (bytes)"

            public static let bayesianWeightLabel = "Bayesian weight (milli)"

            public static let bayesianWeightSubtitle = "Weight of each user's own model in the combined spam score, in milli — 700 = 0.7. Range 0–1000."

            public static let bayesianMinSamplesLabel = "Bayesian min samples"

            public static let bayesianMinSamplesSubtitle = "Training samples below which a user's own model is ignored (default 50)."

            public static let bayesianFullConfidenceSamplesLabel = "Bayesian full-confidence samples"

            public static let bayesianFullConfidenceSamplesSubtitle = "Samples at which a user's model reaches full weight (default 200). Must be above min samples."

            public static let trainingHistoryRetentionLabel = "Training history retention (days)"

            public static let trainingHistoryRetentionSubtitle = "How long each user's per-message training-undo history is kept (default 30)."

            public static let unlistedRecipientPenaltyLabel = "Unlisted-recipient penalty (points)"

            public static let unlistedRecipientPenaltySubtitle = "Extra spam points added when mail arrives at an address that isn't one of a user's aliases (delivered via catch-all). 0 = off; a large value (e.g. 1000) forces such mail to Junk."

            public static let spamSave = "Save spam policy"

            public static let publishSpamBaselineButton = "Publish deployment baseline"

            public static let publishSpamBaselineSubtitle = "Aggregate every opted-in user's spam training into a baseline that new users start from. Never reveals who contributed, and needs at least 3 contributors."

            public static func spamBaselinePublished(contributors: String, samples: String) -> String {
                "Published from \(contributors) contributors (\(samples) samples)."
            }

            public static func spamBaselineWithheld(contributors: String) -> String {
                "Not published — too few contributors (\(contributors)); at least 3 must opt in."
            }

            public static func spamBaselineSkippedContributors(count: String) -> String {
                "\(count) opted-in contributor(s) could not be merged this run."
            }

            public static let spamBaselineStandingLabel = "Keep a shared spam baseline published"

            public static let spamBaselineStandingSubtitle = "Republishes the baseline every 24 hours while enough users contribute. Turning this off withdraws the published baseline."

            public static func spamBaselineStatePublished(contributors: String, date: String) -> String {
                "Published over \(contributors) contributors on \(date)."
            }

            public static let spamBaselineStateNone = "No baseline published."

            public static let spamBaselineWaiting = "Waiting for more contributor activity."

            public static let authGroupTitle = "Authentication enforcement"

            public static let authGroupDesc = "Which SPF / DKIM / DMARC failures reject inbound mail at delivery."

            public static let enforceDmarcLabel = "Enforce DMARC reject"

            public static let enforceDmarcQuarantineLabel = "Enforce DMARC quarantine"

            public static let enforceSpfHardfailLabel = "Enforce SPF hardfail"

            public static let enforceDkimLabel = "Enforce DKIM"

            public static let logOnlyLabel = "Log only (never reject)"

            public static let maxFailuresLabel = "AUTH failure limit / minute"

            public static let maxConnPerIpLabel = "Max concurrent connections (per IP)"

            public static let authSave = "Save authentication policy"

            public static let submissionGroupTitle = "Submission quotas"

            public static let submissionGroupDesc = "Per-actor ceilings on outbound message submission."

            public static let submissionMaxPerDayLabel = "Messages per day (per actor)"

            public static let submissionMaxPerDaySubtitle = "How many messages each account may submit per day."

            public static let submissionMaxRecipientsLabel = "Recipients per message"

            public static let submissionMaxRecipientsSubtitle = "Maximum recipients allowed on a single submitted message."

            public static let submissionSave = "Save submission policy"

            public static let imapGroupTitle = "IMAP server policy"

            public static let imapGroupDesc = "How the IMAP/MDA serves mailboxes to mail clients."

            public static let imapIdleTimeoutLabel = "IDLE timeout (seconds)"

            public static let imapIdleTimeoutSubtitle = "How long an idle IMAP session is held before the server ends it."

            public static let imapTombstoneRetentionLabel = "Tombstone retention (days)"

            public static let imapTombstoneRetentionSubtitle = "How long expunged-message markers are kept for resync (minimum 7)."

            public static let imapDeleteNonemptyLabel = "Delete non-empty mailbox"

            public static let imapDeleteForbidden = "Forbidden"

            public static let imapDeleteAllowed = "Allowed"

            public static let imapBodystructureCacheLabel = "BodyStructure cache size (entries)"

            public static let imapBodystructureCacheSubtitle = "In-memory derivation cache the MDA keeps per session."

            public static let imapStorageBytesLabel = "Storage quota (bytes)"

            public static let imapStorageBytesSubtitle = "Per-actor storage ceiling across all mailboxes."

            public static let imapMessageCountLabel = "Message-count quota"

            public static let imapMessageCountSubtitle = "Per-actor message-count ceiling across all mailboxes."

            public static let imapSave = "Save IMAP policy"

            public static let outboundGroupTitle = "Outbound delivery"

            public static let outboundGroupDesc = "Retry, bounce, and TLS-reporting behavior for outbound mail."

            public static let outboundRetryScheduleLabel = "Retry schedule (seconds)"

            public static let outboundRetryScheduleSubtitle = "Delay before each successive attempt — one value per line."

            public static let outboundPermfailTimeoutLabel = "Permanent-failure timeout (hours)"

            public static let outboundPermfailTimeoutSubtitle = "Total retry budget before a message permanently fails."

            public static let outboundDelayWarningLabel = "Delay-warning time (hours)"

            public static let outboundDelayWarningSubtitle = "When a delay-warning notice is sent to the sender."

            public static let outboundNdrRateLimitLabel = "Bounce rate-limit window (days)"

            public static let outboundNdrRateLimitSubtitle = "Per-recipient window for suppressing repeated bounce notices."

            public static let outboundSuppressNdrSpfLabel = "Suppress bounce on SPF hardfail"

            public static let outboundSuppressNdrDmarcLabel = "Suppress bounce on DMARC reject"

            public static let outboundPostmasterCcLabel = "CC postmaster on bounces"

            public static let outboundPostmasterCcSubtitle = "Disabled — project policy never copies the postmaster."

            public static let outboundTlsrptSendLabel = "Send TLSRPT reports"

            public static let outboundIpv6Label = "IPv6 outbound"

            public static let outboundTreat5xxLabel = "Treat as transient (5xx codes)"

            public static let outboundTreat5xxSubtitle = "Enhanced-status codes to retry even when the reply is 5xx — one per line."

            public static let outboundSave = "Save outbound policy"

            public static let aliasGroupTitle = "Aliases"

            public static let aliasGroupDesc = "Per-account alias limits and the inbound address-resolution rules."

            public static let aliasExactMaxLabel = "Exact aliases per account (max)"

            public static let aliasExactMaxSubtitle = "Cap on user-added exact aliases beyond the signup address."

            public static let aliasReservedLabel = "Reserved local-parts"

            public static let aliasReservedSubtitle = "Role addresses users cannot claim — one local-part per line; empty clears the reservation."

            public static let aliasSubaddressingLabel = "Sub-addressing (plus-suffix)"

            public static let aliasSubaddressingSubtitle = "Allow plus-tagged aliases that route to the base address."

            public static let aliasWildcardPrefixLabel = "Wildcard-prefix aliases"

            public static let aliasWildcardPrefixSubtitle = "Allow name-prefixed aliases that route to the same user."

            public static let aliasSave = "Save alias policy"
        }

        public enum dns {
            public static let title = "DNS"

            public static let description = "Every DNS record each of your domains needs, with the exact value to set and a live check against public DNS."

            public static let empty = "No domains yet."

            public static let emptyDesc = "Add a mail domain and its required DNS records appear here."

            public static let fieldName = "Name"

            public static let fieldType = "Type"

            public static let fieldValue = "Value"

            public static let copy = "Copy"

            public static let ptrProviderNote = "Reverse DNS (PTR) is set at your server's IP provider, not published here. Most VPS providers let you set it in their control panel."

            public static let statusOk = "OK"

            public static let statusMissing = "Missing"

            public static let statusMismatch = "Mismatch"

            public static func statusMismatchFound(found: String) -> String {
                "Mismatch — found \(found)"
            }

            public static let statusChecking = "Checking…"

            public static let addDomain = "Add domain"

            public static let addDomainPlaceholder = "example.com"

            public static let addDomainSubmit = "Add"

            public static let addDomainPrimaryWarning = "This becomes your nest's primary domain and can never be removed — undoing it later requires renaming onto a different domain."

            public static let primaryBadge = "Primary"

            public static let remove = "Remove"

            public static let removedTitle = "Recently removed"

            public static let removedDesc = "Restorable for 30 days."

            public static let restore = "Restore"

            public static let refresh = "Refresh"

            public static let credentialsTitle = "DNS-provider credentials"

            public static let credentialsEmpty = "No DNS-provider credentials yet."

            public static let credentialZones = "Zones"

            public static let addCredential = "Add credential"

            public static let addCredentialSubmit = "Add"

            public static let manageAll = "Fauna controls all domains"

            public static let modeManaged = "Fauna-managed"

            public static let modeManual = "Manual"

            public static let catchAllLabel = "Catch-all:"

            public static let catchAllNone = "None"

            public static let catchAllClearedBySuccession = "A succession cleared this domain's catch-all — unmatched mail now bounces. Re-designate below if you want a new one."

            public static let roleAddressLabel = "Role addresses:"

            public static let roleAddressAdminDefault = "Admin (default)"

            public enum cert {
                public static let label = "Certificate:"

                public static let statusValid = "Valid"

                public static let statusOnFloor = "Renew needed"

                public static let statusExpiring = "Expiring soon"

                public static let selfSigned = "self-signed"

                public static func expires(date: String) -> String {
                    "expires \(date)"
                }

                public static let autoRenew = "Auto-renew"

                public static let issue = "Get certificate"

                public static let issueComplete = "I've added the record"

                public static let issueCancel = "Cancel"

                public static let pasteInstructions = "Add this DNS record at your registrar, then confirm:"

                public static let delegate = "Automate renewals"

                public static let delegateZoneLabel = "Delegate to zone:"

                public static let delegateSubmit = "Delegate"

                public static let delegateCancel = "Cancel"

                public static let removeDelegation = "Remove delegation"

                public static let renewalsAutomated = "Renewals automated"

                public static let delegateNoZones = "Add a DNS-provider credential first to delegate renewals."
            }

            public enum rename {
                public static let button = "Rename primary domain"

                public static let promote = "Promote to primary"

                public static let renamingTo = "Renaming to"

                public static let sheetTitle = "Rename primary domain"

                public static let newPrimaryLabel = "New primary domain"

                public static let graceDaysLabel = "Grace window (days, default 7)"

                public static let submit = "Start rename"

                public static let cancel = "Cancel"

                public static let bannerTitle = "Primary-domain rename in progress"

                public static let stateLabel = "State:"

                public static let graceEnds = "Grace ends:"

                public static let graceElapsed = "Grace window elapsed"

                public static let complete = "Complete now"

                public static let completeConfirm = "Confirm complete"

                public static let completeForceWarning = "Completing before the grace window ends may briefly break mail delivery for peers whose caches have not yet refreshed."

                public static let extend = "Extend grace"

                public static let extendDaysLabel = "Extend by (days):"

                public static let abort = "Abort rename"

                public static let abortConfirm = "Confirm abort"

                public static let abortPostflipWarning = "Aborting after the anchor flip re-flips the primary and rewrites every domain's DNS — expensive but safe."
            }
        }

        public enum view {
            public static let nestStatistics = "Nest Statistics"

            public static let registeredUsers = "Registered Users"

            public static let totalStorageUsed = "Total Storage Used"

            public static let totalInboxMessages = "Total Inbox Messages"

            public static let activeSessions = "Active Sessions"

            public static let recentUsers = "Recent Users"

            public static let recentUsersDesc = "Last 10 registered users"

            public static let noUsersLoaded = "No users loaded yet."

            public static let serverStatus = "Server Status"

            public static let uptime = "Uptime"

            public static let workers = "Workers"

            public static let noRegisteredUsers = "No registered users."

            public static let noUserData = "No user data available."
        }
    }

    public enum setup {
        public static let back = "Back"

        public enum dns {
            public static let title = "Configure DNS"

            public static func description(domain: String) -> String {
                "We need API access to your DNS provider to create records for \(domain)."
            }

            public static let verified = "Token verified."

            public static let provision = "Provision"
        }

        public enum server {
            public static let title = "Choose a server provider"
        }

        public enum byo {
            public static let title = "Run Fauna on your server"
        }

        public enum byoStatus {
            public static let title = "Connecting to your nest"
        }
    }

    public enum c2pa {
        public static let provenance = "Content Provenance"

        public static let signer = "Signer"

        public static let tool = "Tool"

        public static let valid = "Valid"

        public static let validationIssue = "Validation issue"

        public static let verifiedTitle = "C2PA verified provenance"

        public static let invalidTitle = "C2PA provenance (validation issue)"

        public static let viewLabel = "View content provenance"

        public static let imageViewer = "Image viewer"

        public static let badgeLabel = "C2PA"
    }

    public enum p2p {
        public static let title = "P2P Contacts"

        public static let noContacts = "No P2P contacts yet"

        public static let deleteContact = "Delete Contact"

        public static let copyToClipboard = "Copy to Clipboard"
    }

    public enum moderation {
        public static let spamProtection = "Spam Protection"

        public static let spamHint = "Content scoring above this threshold is filtered as spam."

        public static let phishingHint = "Content scoring above this threshold is flagged as phishing."

        public static let statsTitle = "Moderation Stats"

        public static let totalLabels = "Total labels:"

        public static let spamDetected = "Spam detected:"

        public static let avgConfidence = "Avg spam confidence:"

        public static let enforcementTitle = "Enforcement Actions"

        public static let noActions = "No enforcement actions on your content."

        public static let confidence = "confidence"

        public static let correct = "Correct"

        public static func flaggedCount(count: String) -> String {
            "\(count) flagged"
        }

        public static let appeal = "Appeal"

        public static let appealReasonLabel = "Why should this decision be reviewed?"

        public static func appealSummary(contentId: String) -> String {
            "Appealing the enforcement action on \(contentId)."
        }

        public static let appealSubmit = "Submit appeal"

        public static let appealCancel = "Cancel"

        public static let appealBlockedNoContent = "No content selected to appeal."

        public static let appealBlockedNoReason = "Enter a reason before submitting the appeal."

        public static let appealBlockedReasonTooLong = "The reason is too long. Shorten it before submitting the appeal."

        public static let appealRecorded = "Appeal recorded. An administrator will review it."

        public static func appealFailed(error: String) -> String {
            "Appeal failed: \(error)"
        }

        public enum report {
            public static let title = "Report"

            public static let reasonLabel = "Why are you reporting this?"

            public enum reason {
                public static let spam = "Spam"

                public static let harassment = "Harassment"

                public static let hate = "Hateful content"

                public static let violence = "Violence or threats"

                public static let sexual = "Sexual content"

                public static let illegal = "Illegal content"

                public static let impersonation = "Impersonation"

                public static let other = "Something else"
            }

            public static let noteLabel = "Anything the admins should know? (optional)"

            public static let includeTextLabel = "Include the text of this message — the admins will be able to read it"

            public static let blockAuthorLabel = "Also block this person"

            public static let submit = "Send report"

            public static let cancel = "Cancel"

            public static let blockedNoReason = "Choose a reason before sending the report."

            public static let blockedNoteTooLong = "The note is too long. Shorten it before sending the report."

            public static func sentLocal(nest: String) -> String {
                "Report sent to the admins of \(nest)."
            }

            public static func sentForwarded(nest: String, homeNest: String) -> String {
                "Report sent to the admins of \(nest) and forwarded, without your name, to the admins of \(homeNest)."
            }

            public static func failed(error: String) -> String {
                "The report could not be sent: \(error)"
            }

            public static let hiddenPlaceholder = "You reported this"

            public static let ledgerTitle = "Your reports"

            public static let ledgerEmpty = "You have not reported anything."

            public static func ledgerRoutedTo(destinations: String) -> String {
                "Sent to \(destinations)"
            }

            public static let statusOpen = "Open"

            public static let statusResolved = "Resolved"

            public static let statusWithdrawn = "Withdrawn"

            public static let outcomeActed = "Acted on"

            public static let outcomeDismissed = "Dismissed"

            public static let withdraw = "Withdraw"

            public static let withdrawn = "Report withdrawn. Your note and any attached text were deleted everywhere they went."
        }

        public enum category {
            public static let spam = "Spam"

            public static let trusted = "Trusted"

            public static let nsfw = "NSFW"

            public static let phishing = "Phishing"

            public static let commercial = "Commercial"
        }

        public enum action {
            public static let rejected = "Rejected"

            public static let quarantined = "Quarantined"

            public static let suppressed = "Hidden from feeds"

            public static let rateLimited = "Rate limited"

            public static let logged = "Logged"

            public static let labeled = "Labeled"

            public static let flagged = "Flagged"

            public static let takenDown = "Removed under legal obligation"
        }

        public enum legalTakedown {
            public static func tombstone(reference: String) -> String {
                "Removed under legal obligation (\(reference))"
            }
        }
    }

    public enum mutedWords {
        public static let title = "Muted words"

        public static let description = "Conversation messages containing one of these words are collapsed behind a “Show anyway” button. This list stays on your devices — the server never sees it."

        public static let inputPlaceholder = "Add a word to mute"

        public static let add = "Mute word"

        public static let empty = "You haven’t muted any words yet."

        public static let remove = "Un-mute"
    }

    public enum personalization {
        public static let title = "Personalization"

        public static let feedsLink = "Feeds"

        public static let mutedWordsLink = "Muted words"

        public static let labelersEmpty = "You haven’t subscribed to any community labelers yet."

        public static let browseCatalog = "Browse labeler catalog"

        public static let trainedTopicsTitle = "Trained topics"

        public static let trainedTopicsEmpty = "You haven’t created any trained topics yet."

        public static let trainedFactorPlaceholder = "Topic name"

        public static let trainedFactorCreate = "New trained topic"

        public static let trainedFactorRename = "Rename"

        public static let trainedFactorSave = "Save name"

        public static func trainedFactorExamples(count: String) -> String {
            "\(count) examples"
        }

        public static func trainedFactorCap(max: String) -> String {
            "You already have \(max) trained topics — delete one to create another."
        }

        public static let trainedFactorBlankName = "A trained topic needs a name."

        public static let trainedFactorEngagementToggle = "Learn from my activity"

        public static let clearEngagementData = "Clear activity data"

        public static let trainedFactorPublish = "Publish…"

        public static let publishSheetTitle = "Publish this topic"

        public static let publishNameLabel = "Public name for this list"

        public static let publishNamePlaceholder = "e.g. Small orange cats"

        public static let publishLimitationNote = "You’re sharing the posts below — not the topic itself or anything it learned about you. Only posts this device has already loaded and seen can be included, so the list won’t cover posts you never saw. It’s published anonymously: nothing links the list back to you or your account."

        public static let publishExemplarsTitle = "Posts to include"

        public static let publishExemplarsEmpty = "This topic hasn’t scored any of the posts loaded so far. Open a feed it ranks, then try again."

        public static let publishExemplarInclude = "Include"

        public static func publishScore(score: String) -> String {
            "Score \(score)"
        }

        public static let publishSubmit = "Publish"

        public static let publishNameBlank = "A published name can’t be blank."

        public static func publishNameTooLong(max: String) -> String {
            "A published name is at most \(max) characters."
        }

        public static let publishKindLabel = "Share as"

        public static let publishKindList = "List of posts"

        public static let publishKindModel = "Word-pattern model"

        public static func publishKindUnknown(kind: String) -> String {
            "\(kind)"
        }

        public static let publishNameLabelModel = "Public name for this model"

        public static let publishLimitationNoteModel = "You’re sharing the word patterns this topic learned — not the topic itself, and none of the posts. Unlike a list, a model also matches posts nobody here has seen yet. Only patterns that appear in at least 3 of your marked public posts are included, and that covers both what you marked as more like this and what you marked as less like this — the direction column below shows which is which. It’s published anonymously, and it’s built only from posts you marked by hand: nothing you merely read or watched goes into it."

        public static func publishCorpusSize(included: String, marked: String) -> String {
            "Built from \(included) public examples of your \(marked) marked posts."
        }

        public static let publishNgramsTitle = "Word patterns to include"

        public static let publishNgramsEmpty = "No word pattern appears in at least 3 of this topic’s public examples yet, so there’s nothing that can be shared without quoting a single post. Mark a few more public posts for this topic, then try again."

        public static let publishVocabularyEmpty = "This topic needs more public examples before it can be shared as a model."

        public static let publishNgramDirectionMore = "More like this"

        public static let publishNgramDirectionLess = "Less like this"

        public static let publishNgramDirectionBoth = "Both"

        public static func publishNgramCount(count: String) -> String {
            "In \(count) posts"
        }

        public static let shareSignalsTitle = "Anonymous signal sharing"

        public static let shareSignalsLabel = "Share anonymous engagement signals"

        public static let shareSignalsSubtitle = "Off by default. When on, whether you watched or skipped a public post joins an anonymized count your nest shares — but only once at least 3 people here have the same verdict on the same post, and never your identity or your activity."

        public static let signalPublishedTitle = "What this nest publishes"

        public static let signalPublishedDescription = "The anonymized signal and report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 contributors."

        public static let signalPublishedEmpty = "This nest publishes no signal aggregates yet"

        public static let signalPublishedContributors = "contributors"
    }

    public enum labelerCatalog {
        public static let title = "Community labelers"

        public static let empty = "No community labelers published yet."

        public static let inspect = "Inspect"

        public static let subscribe = "Subscribe"

        public static let unsubscribe = "Unsubscribe"

        public static let closeInspect = "Close"

        public static func listName(name: String) -> String {
            "List name: \(name)"
        }

        public static func listEntryCount(count: String) -> String {
            "\(count) entries"
        }

        public static let unnamedList = "Unnamed list"

        public static func modelName(name: String) -> String {
            "Model name: \(name)"
        }

        public static func modelNgramCount(count: String) -> String {
            "\(count) word patterns"
        }

        public static let unnamedModel = "Unnamed model"

        public static let kindNeedsNewerApp = "needs a newer app"

        public static func errorRefresh(message: String) -> String {
            "Failed to load community labelers: \(message)"
        }

        public static func errorInspect(message: String) -> String {
            "Failed to inspect this labeler: \(message)"
        }

        public static func errorSubscribe(message: String) -> String {
            "Failed to subscribe: \(message)"
        }

        public static func errorUnsubscribe(message: String) -> String {
            "Failed to unsubscribe: \(message)"
        }

        public static let subscribedWithoutMailHolder = "Subscribed, but this labeler cannot run over your mail yet: this nest has no mail service to trust with it."

        public static let subscribedWithoutMail = "Subscribed, but this labeler cannot run over your mail until mail is set up for your account."
    }

    public enum taskDelegation {
        public static let title = "Task delegation"

        public static let description = "Heavy background tasks run on one capable, always-on device — a nest or a plugged-in computer — and stay off battery phones. Each task picks its device automatically; pin one if you prefer."

        public static let kindBackupUpload = "Backup uploads"

        public static let kindContentRescore = "Content re-scoring"

        public static let kindIndex = "Search indexing"

        public static let assignmentAutomatic = "Automatic"

        public static let assignmentThisDevice = "This device"

        public static func assignmentOtherName(name: String) -> String {
            "\(name)"
        }

        public static let runnerThisDevice = "Running on this device"

        public static func runnerOtherDevice(device: String) -> String {
            "Running on \(device)"
        }

        public static let runnerWaiting = "Waiting for an eligible device"

        public static let errorDeviceId = "This device could not load its own identity, so task assignments cannot be shown or changed here. Restart the app; if that does not help, its local data directory may not be writable."
    }

    public enum features {
        public static let namePayments = "Payments"

        public static let nameZaps = "Zaps"

        public static let nameP2pShare = "File sharing"

        public static let nameOther = "A feature this app does not know yet"

        public static let tierStructural = "Fauna's built-in limits"

        public static let tierRegion = "Your region's rules"

        public static let tierAdmin = "Your nest admin"

        public static let tierGuardian = "Your guardian"

        public static let tierSelf = "Your own setting"

        public static let tierOther = "Another rule-setter"

        public static let deniedByStructural = "Turned off by Fauna's built-in limits."

        public static let deniedByRegion = "Turned off by your region's rules."

        public static let deniedByAdmin = "Turned off by your nest admin."

        public static let deniedByGuardian = "Turned off by your guardian."

        public static let deniedBySelf = "You turned this off."

        public static let deniedByOther = "Turned off by another rule-setter."

        public static func exhaustedStructural(window: String) -> String {
            "You've used up Fauna's built-in limit for this \(window)."
        }

        public static func exhaustedRegion(window: String) -> String {
            "You've used up the limit your region's rules set for this \(window)."
        }

        public static func exhaustedAdmin(window: String) -> String {
            "You've used up the limit your nest admin set for this \(window)."
        }

        public static func exhaustedGuardian(window: String) -> String {
            "You've used up the limit your guardian set for this \(window)."
        }

        public static func exhaustedSelf(window: String) -> String {
            "You've used up the limit you set for this \(window)."
        }

        public static func exhaustedOther(window: String) -> String {
            "You've used up the limit another rule-setter set for this \(window)."
        }

        public static let windowDay = "day"

        public static let windowWeek = "week"

        public static let windowMonth = "month"

        public static let sectionTitle = "Feature limits"

        public static let empty = "No feature limits apply on this nest."

        public static let statusAvailable = "Available"

        public static let statusRestricted = "Restricted"

        public static let dimensionOperations = "Uses"

        public static let dimensionCounterparties = "People"

        public static let dimensionVolume = "Amount"

        public static func quotaLabel(dimension: String, window: String) -> String {
            "\(dimension) per \(window)"
        }

        public static func quotaValue(remaining: String, limit: String) -> String {
            "\(remaining) left of \(limit)"
        }

        public static func quotaValueExhausted(limit: String) -> String {
            "none left of \(limit)"
        }

        public static func magnitudeSats(value: String) -> String {
            "\(value) sats"
        }

        public static let adminSectionTitle = "Feature limits for everyone"

        public static let adminSectionDesc = "Limits set here apply to every account on this nest. They can only tighten what Fauna and your region already allow."

        public static let authoredNone = "No limit set"

        public static let authoredOff = "Turned off"

        public static let authoredOn = "On, no limits"

        public static let authoredLimitedOne = "On, 1 limit"

        public static func authoredLimited(count: String) -> String {
            "On, \(count) limits"
        }

        public static let authoredUnreadable = "This limit can't be read, so the feature is off until it's set again or removed."

        public static let ownLabel = "Your own limit"

        public static let ownEdit = "Set your own limit"

        public static let adminEdit = "Edit"

        public static func editorTitleAdmin(feature: String) -> String {
            "\(feature): limits for everyone on this nest"
        }

        public static func editorTitleSelf(feature: String) -> String {
            "\(feature): limits only for you"
        }

        public static func editorTitleGuardian(feature: String, ward: String) -> String {
            "\(feature): limits only for \(ward)"
        }

        public static let editorOn = "On"

        public static let editorOff = "Off"

        public static let editorHint = "Leave a box empty for no limit. Zero is a limit."

        public static func editorVolumeLabelBytes(window: String) -> String {
            "Amount per \(window) (for example 50 GB)"
        }

        public static func editorVolumeLabelSats(window: String) -> String {
            "Amount per \(window), in sats"
        }

        public static let editorPerOperationBytes = "Largest single item (for example 2 GB)"

        public static let editorPerOperationSats = "Largest single payment, in sats"

        public static let editorSave = "Save"

        public static let editorRemove = "Remove limit"

        public static let editorCancel = "Cancel"

        public static let editorSaved = "Saved."

        public static let editorRemoved = "Limit removed."

        public static func editorInvalidCount(value: String) -> String {
            "\"\(value)\" isn't a whole number. Type one, or leave the box empty."
        }

        public static func editorInvalidSize(value: String) -> String {
            "\"\(value)\" isn't a size. Try something like 50 GB, or leave the box empty."
        }

        public static func editorInvalidSats(value: String) -> String {
            "\"\(value)\" isn't a whole number of sats. Type one, or leave the box empty."
        }

        public static func editorSaveFailed(error: String) -> String {
            "Could not save the limit: \(error)"
        }

        public static func editorLoadFailed(error: String) -> String {
            "Could not load the limits: \(error)"
        }

        public static let editorWardMissing = "This account is no longer in your family, so the limit was not saved."

        public static let guardianSectionTitle = "Feature limits"

        public static let guardianSectionDesc = "Limits set here apply only to this child. They can only tighten what already applies."

        public static func noEffectStructural(limit: String) -> String {
            "No effect: Fauna's built-in limit is already \(limit)."
        }

        public static func noEffectRegion(limit: String) -> String {
            "No effect: your region's rules already limit this to \(limit)."
        }

        public static func noEffectAdmin(limit: String) -> String {
            "No effect: your nest admin already limits this to \(limit)."
        }

        public static func noEffectGuardian(limit: String) -> String {
            "No effect: your guardian already limits this to \(limit)."
        }

        public static func noEffectSelf(limit: String) -> String {
            "No effect: your own setting already limits this to \(limit)."
        }

        public static func noEffectOther(limit: String) -> String {
            "No effect: another rule-setter already limits this to \(limit)."
        }
    }

    public enum family {
        public static let title = "Family"

        public static func supervisedIndicator(guardian: String) -> String {
            "This account is supervised by \(guardian)"
        }

        public static func supervisedNoticeOnboarding(guardian: String) -> String {
            "This account will be supervised by \(guardian)"
        }

        public static let wardsHeading = "Accounts you supervise"

        public static let noWards = "You are not supervising any accounts."

        public static let policyContactApprovalLabel = "Require my approval for new contacts"

        public static let policyUnknownSenderLabel = "Unknown email senders"

        public static let policyFederationLabel = "Allow contact from other nests"

        public static let policyFeedSourcesLabel = "New feed sources"

        public static let policyFeedSourcesCaveat = "Blocks new external accounts and follows. Messages arriving through an already-connected account are governed by \"Unknown message senders\"."

        public static let policyUnknownPeerDmLabel = "Unknown message senders"

        public static let policyContentNsfwLabel = "Adult content"

        public static let policyContentSpamLabel = "Spam"

        public static let policyContentPhishingLabel = "Phishing and scams"

        public static let policyContentCommercialLabel = "Ads and promotions"

        public static let policyContentNotifyLabel = "Notify me about flagged content"

        public static let policyScreenHeading = "Screen time"

        public static let policyScreenWindowStartLabel = "Usable from (HH:MM)"

        public static let policyScreenWindowEndLabel = "Usable until (HH:MM)"

        public static let policyScreenDailyMinutesLabel = "Daily limit (minutes, all devices)"

        public static let policyScreenCaveat = "Enforced by the apps on your child's devices. Leave a field empty to remove that limit."

        public static let policySaveButton = "Save policy"

        public static let valueAllow = "Allow"

        public static let valueHold = "Hold for review"

        public static let valueReject = "Reject"

        public static let valueBlock = "Block"

        public static let valueInherit = "Use my settings"

        public static let valueCollapse = "Collapse"

        public static let contentBlockedNotice = "Hidden by your family policy"

        public static let contentCollapsedNotice = "Flagged content"

        public static let contentRevealButton = "Show anyway"

        public static func wardContentNoticeCount(count: String) -> String {
            "\(count) flagged today"
        }

        public static let wardUsageTodayLabel = "Screen time today"

        public static func wardUsageTodayOfBudget(used: String, budget: String) -> String {
            "\(used) of \(budget) minutes"
        }

        public static func wardUsageToday(minutes: String) -> String {
            "\(minutes) minutes"
        }

        public static let wardDevicesHeading = "Devices"

        public static func wardDevicesHint(handle: String) -> String {
            "Mark the device you enrolled into this account. \(handle) cannot remove a marked device, and graduating un-enrolls it automatically."
        }

        public static let noWardDevices = "No devices registered yet."

        public static let deviceMarkLabel = "Guardian device"

        public static let blockedPeersHeading = "Denied message senders"

        public static func blockedPeersHint(handle: String) -> String {
            "People you denied for \(handle). Their new messages are refused; anything already delivered stays readable. Allowing lets them message again."
        }

        public static let noBlockedPeers = "Nobody denied."

        public static let blockedPeerAllow = "Allow again"

        public static let screenLockTitle = "Screen time is off"

        public static func screenLockWindow(resumes: String, guardian: String) -> String {
            "Your screen time starts again at \(resumes). Set by \(guardian)."
        }

        public static func screenLockBudget(minutes: String, guardian: String) -> String {
            "You have used today's \(minutes) minutes. Set by \(guardian)."
        }

        public static let screenLockFamilyHint = "You can still open Family to see your settings."

        public static let approvalsHeading = "Approvals"

        public static let noApprovals = "No pending approvals."

        public static let approvalNoSender = "No sender (delivery notice)"

        public static let approve = "Approve"

        public static let deny = "Deny"

        public static let contactAddPlaceholder = "Actor ID (hex)"

        public static let contactAddButton = "Pre-approve contact"

        public static let contactAddInvalidActorId = "Not a valid actor ID (expected hex)"

        public static let graduateButton = "Graduate to full account"

        public static func graduateConfirmButton(handle: String) -> String {
            "Yes, graduate \(handle)"
        }

        public static let transferPlaceholder = "New guardian actor ID (hex)"

        public static let transferButton = "Propose new guardian"

        public static func transferPending(handle: String) -> String {
            "Waiting for \(handle) to accept guardianship"
        }

        public static let transferCancelButton = "Cancel proposal"

        public static let incomingTransfersHeading = "Guardianship requests"

        public static func incomingTransferText(guardian: String, ward: String) -> String {
            "\(guardian) asks you to take over supervision of \(ward)"
        }

        public static let incomingTransferAcceptButton = "Accept guardianship"

        public static let incomingTransferDeclineButton = "Decline"

        public static func guardianLabel(guardian: String) -> String {
            "Supervised by \(guardian)"
        }

        public static let policySummaryHeading = "Current policy"

        public enum ageBand {
            public static let label = "Age band"

            public static let notSet = "Not set"

            public static let u13 = "Under 13"

            public static let teen1315 = "13–15"

            public static let teen1617 = "16–17"

            public static let adult = "18+"

            public static let provenanceGuardianAsserted = "set by guardian"

            public static let provenanceAttestedAndroid = "verified on Android"

            public static let provenanceAttestedIos = "verified on iOS"

            public static let provenanceNone = "declared, not verified"

            public static func wardLine(band: String, provenance: String) -> String {
                "Age band: \(band) · \(provenance)"
            }

            public static func wardLineBandOnly(band: String) -> String {
                "Age band: \(band)"
            }

            public static func ownSummary(band: String, provenance: String) -> String {
                "Your age band: \(band) · \(provenance), set at admission"
            }

            public static func ownSummaryBandOnly(band: String) -> String {
                "Your age band: \(band), set at admission"
            }

            public static func claimLine(band: String, provenance: String) -> String {
                "Age \(band) · \(provenance)"
            }

            public static let claimNone = "No app age verification"

            public static func noticeAttested(band: String, store: String, verifier: String) -> String {
                "Your age range (\(band)) from \(store) will be shared with this nest's admin, verified by \(verifier)"
            }

            public static func noticeDeclared(band: String) -> String {
                "Your age range (\(band)) will be shared with this nest's admin as declared, not verified"
            }

            public static let storeAndroid = "Google Play"

            public static let storeIos = "the App Store"

            public static let verifierAndroid = "Google"

            public static let verifierIos = "Apple"
        }
    }

    public enum navigation {
        public static let quickSwitcher = "Quick Switcher"

        public static let quickSwitcherPlaceholder = "Search conversations, groups…"

        public static let noMatches = "No matches"

        public static let navigate = "Navigate"

        public static func members(count: String) -> String {
            "\(count) members"
        }

        public static let section = "Section"

        public static let categoryConversation = "Conversation"

        public static let categoryGroup = "Group"

        public static let categoryFile = "File"

        public static let categoryAction = "Go to"
    }

    public enum notifications {
        public static func messageFrom(sender: String) -> String {
            "Message from \(sender)"
        }

        public static func groupMessage(sender: String, group: String) -> String {
            "\(sender) in \(group)"
        }

        public static let knockTitle = "New Contact Request"

        public static func knockBody(name: String) -> String {
            "\(name) wants to connect"
        }

        public static let syncCompleteTitle = "Sync Complete"

        public static func syncCompleteBody(filename: String) -> String {
            "\(filename) uploaded"
        }

        public static let eventReminderTitle = "Upcoming Event"

        public static func eventReminderBody(name: String, minutes: String) -> String {
            "\(name) in \(minutes) minutes"
        }

        public static let groupInviteTitle = "Group Invite"

        public static func groupInviteBody(name: String, group: String) -> String {
            "\(name) invited you to \(group)"
        }

        public static let defaultBody = "New notification"

        public static let typeLike = "Like"

        public static let typeRepost = "Repost"

        public static let typeMention = "Mention"

        public static let typeQuote = "Quote"

        public static let typeMessage = "Message"

        public static let typeFollow = "Follow"

        public static let typeReply = "Reply"

        public static let typeEventInvite = "Event invite"

        public static let typeGroupInvite = "Group invite"

        public static let typeKnock = "Contact request"

        public static let typeReport = "Report"

        public static let typeDefault = "Notification"

        public static let updateAvailableSummary = "Fauna update available"

        public static func updateAvailableBody(version: String) -> String {
            "Version \(version) is available. Visit fauna.social to download."
        }

        public static func rowLike(sender: String) -> String {
            "\(sender) liked your post"
        }

        public static func rowReply(sender: String) -> String {
            "\(sender) replied to your post"
        }

        public static func rowRepost(sender: String) -> String {
            "\(sender) reposted your post"
        }

        public static func rowQuote(sender: String) -> String {
            "\(sender) quoted your post"
        }

        public static func rowMention(sender: String) -> String {
            "\(sender) mentioned you"
        }

        public static func rowFollow(sender: String) -> String {
            "\(sender) followed you"
        }

        public static func rowInteraction(sender: String) -> String {
            "\(sender) interacted with your content"
        }

        public static func rowKnock(sender: String, message: String) -> String {
            "\(sender) wants to connect: \(message)"
        }

        public static func rowForwardQueueEvicted(dest: String, cap: String) -> String {
            "Forward to \(dest) dropped because your forward queue is full. Configured rate: \(cap)/hour. Reduce inbound or increase the cap."
        }

        public static let rowFamilyContentNotice = "Filtered content was seen on an account you supervise. Open Family to review."

        public static let rowFamilyContactRequest = "An account you supervise asked to add a contact. Open Family to decide."

        public static let rowFamilyFeedSourceRequest = "An account you supervise asked to add a source. Open Family to decide."

        public static let rowFamilyFeedSourceApproved = "Your guardian approved the source you asked for. Try adding it again."

        public static let rowAbuseReportReceived = "A report is waiting in the reports queue. Open Admin → Nest to review it."

        public static func rowAbuseReportResolved(outcome: String) -> String {
            "Your report was reviewed: \(outcome)."
        }

        public static func rowSecurityPendingActionQueued(actionType: String, actionId: String, executeAfter: String) -> String {
            "Security: a pending action (\(actionType), #\(actionId)) was queued by your account and runs at \(executeAfter). If this was not you, cancel it under Settings → Pending actions."
        }

        public static func rowSecurityActionExecuted(actionType: String) -> String {
            "Security: a pending action (\(actionType)) was executed on your account. If you did not authorize it, contact your nest administrator."
        }

        public static func rowSecurityActionCancelled(actionType: String, cancelledBy: String) -> String {
            "Security: a pending action (\(actionType)) on your account was cancelled by \(cancelledBy)."
        }

        public static func rowSecurityActionExpired(actionType: String, actionId: String) -> String {
            "Security: a pending action (\(actionType), #\(actionId)) on your account expired without the approvals it needed. Nothing changed."
        }

        public static func rowSecurityPendingActionAgainstYou(by: String, actionType: String, actionId: String, executeAfter: String) -> String {
            "Security: administrator \(by) scheduled \(actionType) (#\(actionId)) against your account; it runs at \(executeAfter). You can cancel it under Settings → Pending actions."
        }

        public static func rowSecurityAdminActionPending(by: String, actionType: String, actionId: String, target: String, executeAfter: String, approvalsNeeded: String) -> String {
            "Security: admin \(by) scheduled \(actionType) (#\(actionId)) on \(target); it runs at \(executeAfter) and still needs \(approvalsNeeded) approval(s). Approve or cancel it under Admin → Users → Pending admin actions."
        }

        public static func rowSecurityAdminActionCancelled(actionType: String, actionId: String, target: String, cancelledBy: String) -> String {
            "Security: the pending admin action \(actionType) (#\(actionId)) on \(target) was cancelled by \(cancelledBy)."
        }

        public static func rowSecurityAdminActionExpired(actionType: String, actionId: String, target: String) -> String {
            "Security: the pending admin action \(actionType) (#\(actionId)) on \(target) expired without the approvals it needed. Nothing changed."
        }

        public static func rowSecurityNewToken(ip: String) -> String {
            "Security: new sign-in from a different IP address (\(ip)). If this was not you, change your keys and contact your nest administrator."
        }

        public static func rowSecurityAdminChange(changeType: String, target: String) -> String {
            "Security: an administrative change (\(changeType)) was made to \(target). If you did not request it, contact your nest administrator."
        }

        public static func rowSecurityRecoveryReplacementPending(newKey: String, landsAt: String) -> String {
            "Security: a recovery key replacement was requested (new key \(newKey)) and lands at \(landsAt). If this was not you, veto it within 30 days."
        }

        public static func rowSecurityRecoveryReplacementLanded(newKey: String) -> String {
            "Security: your recovery key was replaced (new key \(newKey))."
        }

        public static func rowSecurityRecoveryReplacementCancelled(cancelledBy: String) -> String {
            "Security: the pending recovery key replacement was cancelled by \(cancelledBy)."
        }

        public static func rowSecurityIdentitySucceeded(newActorId: String) -> String {
            "Security: this identity was succeeded. Your account moved to a new key (\(newActorId))."
        }

        public static func rowSecurityArchiveExported(ip: String) -> String {
            "Security: your full account archive was downloaded from \(ip). If this was not you, change your keys and contact your nest administrator."
        }

        public static func rowSecurityMailboxExportDownloaded(format: String, ip: String) -> String {
            "Security: a mailbox export (\(format)) was downloaded from \(ip). If this was not you, change your keys and contact your nest administrator."
        }
    }

    public enum widget {
        public static let description = "Shows unread message count and quick compose"

        public static let unreadLabel = "unread"
    }

    public enum markdown {
        public static let bold = "Bold"

        public static let italic = "Italic"

        public static let code = "Code"

        public static let link = "Link"

        public static let heading = "Heading"

        public static let list = "List"

        public static let listItem = "List item"

        public static let toggleMarkers = "Show/hide markdown markers"
    }

    public enum composer {
        public static let quote = "Quote"

        public static let quotePost = "Quote Post"

        public static let newPost = "New Post"
    }

    public enum profile {
        public static let title = "Profile"

        public static let following = "Following"

        public static let followers = "Followers"

        public static let noPosts = "No posts"

        public static let posts = "Posts"

        public static let tiers = "Tiers"

        public static let edit = "Edit Profile"

        public static let follow = "Follow"

        public static let copyId = "Copy ID"

        public static let startDm = "Message"

        public static let block = "Block"

        public static let blocked = "Blocked"

        public static let unblock = "Unblock"

        public static let requestContact = "Request contact"

        public static let requestContactSent = "Request sent"

        public static let report = "Report account"

        public static let editDisplayName = "Display name"

        public static let editBio = "Bio"

        public static let editLinkLabel = "Label"

        public static let editLinkUrl = "URL"

        public static let editAddLink = "Add link"

        public static let editRemoveLink = "Remove"

        public static let editAvatar = "Avatar image path"

        public static let editRemoveAvatar = "Remove avatar"

        public static let editBanner = "Banner image path"

        public static let editRemoveBanner = "Remove banner"

        public static let editSave = "Save"

        public static let editCancel = "Cancel"

        public static let privateTitle = "Only you can see this"

        public static let privateNickname = "Nickname"

        public static let privateNotes = "Notes"

        public static let privateLabels = "Labels"

        public static let privateLabelAdd = "Add label"

        public static let privateLabelRemove = "Remove"

        public static let privateSave = "Save"

        public static func privateNicknameTooLong(max: String) -> String {
            "Nickname is too long — keep it to \(max) characters"
        }

        public static func privateNotesTooLong(max: String) -> String {
            "Notes are too long — keep them under \(max) KB"
        }

        public static let privateLabelEmpty = "Type a label before adding it"

        public static func privateLabelTooLong(max: String) -> String {
            "Label is too long — keep it to \(max) characters"
        }

        public static func privateTooManyLabels(max: String) -> String {
            "This person already has \(max) labels — remove one first"
        }

        public static let privateLabelHistoryFull = "This person has had too many labels — this one cannot be added"

        public static func privateSaveFailed(reason: String) -> String {
            "Could not save your private notes: \(reason)"
        }
    }

    public enum subscriptions {
        public static let title = "Subscriptions"

        public static let myTiers = "My Tiers"

        public static let mySubscriptions = "My Subscriptions"

        public static let pendingRequests = "Pending Requests"

        public static let subscribers = "Subscribers"

        public static let createTier = "Create Tier"

        public static let noTiers = "No tiers yet"

        public static let noRequests = "No pending requests"

        public static let noSubscribers = "No subscribers"

        public static let noSubscriptions = "You have no subscriptions yet"

        public static let tierName = "Name"

        public static let rank = "Rank"

        public static let description = "Description"

        public static let priceHint = "Price"

        public static let askingPrice = "Asking price (sats, optional)"

        public static let paymentUrl = "Payment Link"

        public static let unsafePaymentUrl = "This payment link is unsafe (links must be https). Not opening it."

        public static let autoApprove = "Auto-approve"

        public static let save = "Save"

        public static let cancel = "Cancel"

        public static let edit = "Edit"

        public static let delete = "Delete"

        public static let approve = "Approve"

        public static let reject = "Reject"

        public static let remove = "Remove"

        public static let approving = "Approving — minting keys…"

        public static let subscribe = "Subscribe"

        public static let unsubscribe = "Unsubscribe"

        public static let tierSelectLabel = "Tier:"

        public static let offers = "Subscription Tiers"

        public static let noOffers = "This creator offers no subscription tiers yet"

        public static let paymentProviders = "Payment Providers"

        public static let addProvider = "Add Provider"

        public static let noProviders = "No payment providers yet"

        public static let providerKindLabel = "Provider:"

        public static let providerTierLabel = "Tier:"

        public static let webhookSecret = "Webhook secret"

        public static let webhookUrlLabel = "Webhook URL:"

        public static let redeemClaimTitle = "Redeem a claim code"

        public static let claimCode = "Claim code"

        public static let redeem = "Redeem"

        public static let offerStatusNone = "Not subscribed"

        public static let offerStatusPending = "Pending approval"

        public static let offerStatusActive = "Subscribed"

        public static let paid = "Paid"

        public static let manualClaims = "Manual Claim Codes"

        public static let mintClaim = "Mint Code"

        public static let noClaims = "No claim codes yet"

        public static let claimStatusUnredeemed = "Unredeemed"

        public static let claimStatusRedeemed = "Redeemed"

        public static let claimStatusVoided = "Voided"

        public static let providerStatusConfigured = "Configured"

        public static let providerStatusVerified = "Verified"

        public static let providerStatusError = "Error"
    }

    public enum photoBackup {
        public static let title = "Photo Backup"

        public static let enable = "Enable Photo Backup"

        public static let photoAccessRequired = "Photo library access required. Grant access in Settings."

        public static let wifiOnly = "WiFi Only"

        public static func syncing(uploaded: String, total: String) -> String {
            "Syncing \(uploaded) of \(total)..."
        }

        public static let lastSync = "Last Sync"

        public static func pendingCount(count: String) -> String {
            "\(count) pending"
        }

        public static let syncNow = "Sync Now"

        public static let backUpPhotos = "Back up Photos library"

        public static let photosAccessGranted = "Photos access granted"

        public static let autoUploadDesc = "New photos and videos will be automatically uploaded to your nest."

        public static let backupInProgress = "Backup in progress..."

        public static let backedUp = "Backed up"

        public static func photosCount(count: String) -> String {
            "\(count) photos"
        }

        public static func remainingCount(count: String) -> String {
            "\(count) remaining"
        }

        public static let lastBackup = "Last backup"

        public static let notificationStarting = "Starting backup..."

        public static func notificationProgress(uploaded: String, total: String) -> String {
            "Backing up photos... \(uploaded)/\(total)"
        }

        public static let notificationComplete = "Backup complete"

        public static func errorPrepareSet(message: String) -> String {
            "Could not prepare the photo library folder: \(message)"
        }

        public static let errorWifiLost = "WiFi lost, sync paused"

        public static func errorUploadItem(uri: String, message: String) -> String {
            "Failed to upload \(uri): \(message)"
        }
    }

    public enum fileSync {
        public static let newFolder = "New Folder"

        public static let info = "Info"

        public static let safRootSummary = "Synced files"
    }

    public enum conflicts {
        public static let noConflicts = "No sync conflicts."

        public static let resolve = "Resolve"

        public static let local = "Local"
    }

    public enum fileContextMenu {
        public static let share = "Share"

        public static let versionHistory = "Version history"

        public static let infoUnavailable = "File info unavailable"

        public static let devicesNone = "Not synced to any device"

        public static let devicesOne = "On this device only"

        public static func devicesCount(count: String) -> String {
            "Synced to \(count) devices"
        }

        public static let versionsNone = "No saved versions"

        public static let versionsOne = "1 saved version"

        public static func versionsOneDated(date: String) -> String {
            "1 saved version (\(date))"
        }

        public static func versionsCount(count: String) -> String {
            "\(count) saved versions"
        }

        public static func versionsCountDated(count: String, date: String) -> String {
            "\(count) saved versions, latest \(date)"
        }

        public static let versionRestored = "Version restored."

        public static let versionRestoreFailed = "Could not restore version."

        public static func versionRestoreFailedDetail(message: String) -> String {
            "Could not restore version: \(message)"
        }

        public static let shareNotAvailable = "This item can't be shared from here"

        public static let shareOpenFailed = "Couldn't open Fauna to share this item"

        public static let keepOnDevice = "Always keep on this device"

        public static let makeOnDemand = "Make available on-demand"
    }

    public enum error {
        public static let unexpected = "Something went wrong talking to the nest. Please try again."

        public static let authorization = "You do not have permission to do this."

        public static let rateLimited = "This is happening too quickly. Please wait a moment and try again."

        public enum conversations {
            public static let forbidden = "This person isn't accepting new conversations right now. You can send them a contact request instead."

            public static let rateLimited = "This is happening too quickly. Please wait a moment and try again."
        }

        public enum profile {
            public static let handleTaken = "That handle already belongs to someone else on your nest. Choose a different one."

            public static let handleCooldown = "That handle was released recently and can't be taken yet. Choose a different one, or try again later."
        }

        public enum bridges {
            public static let recipientOnLocalDomain = "That address is on your own server, so it can't be a list member. Add it as an alias instead."

            public static let forwardTargetOnLocalDomain = "That address is on your own server, so mail can't be forwarded to it. Add it as an alias instead."

            public static let overQuota = "Your storage is full, so this wasn't saved. Delete something to make room, or ask your nest admin for more space."

            public static let addressRefused = "None of your connected bridges can reach that address. Check how it is written."

            public static let conversationStoreFull = "This bridge has too many messages waiting, so this wasn't sent. Try again once it catches up."

            public static let guardianApprovalRequired = "This account can only message approved contacts."
        }

        public enum activitypub {
            public static let notLinked = "This post came from the fediverse, and ActivityPub isn't enabled for your account yet. Enable it on the Bridges page first."

            public static let switchedOff = "Your ActivityPub federation is switched off, so this can't be sent to the fediverse. Turn it back on from the Bridges page."
        }

        public enum bluesky {
            public static let notLinked = "This post came from Bluesky, and no Bluesky account is linked for you. Link one on the AT Protocol page first."
        }

        public enum nostr {
            public static let notLinked = "This post came from Nostr, and no Nostr key is linked for you. Link one on the Nostr page first."

            public static let noCustodialKey = "Your nest doesn't hold your Nostr key, so it can't sign this for you. Switch to a generated or imported key on the Nostr page."

            public static let noRelaysConfigured = "You've removed every Nostr relay, so there is nowhere to publish this. Add a relay on the Nostr page first."

            public static let repliesOff = "Replies to Nostr are switched off, so this wasn't sent. Turn on Publish replies on the Nostr page."
        }

        public enum nest {
            public static let outdated = "This nest is running an outdated version and must be updated before you can connect."

            public static let schemaMismatch = "This nest's database does not match its software version and must be updated."
        }

        public enum federation {
            public static let peerNestOutdated = "The other side's nest is running an outdated version and does not support this yet."
        }

        public enum `protocol` {
            public static let unknownKind = "This nest does not support that request. It may need to be updated."

            public static let malformed = "The request could not be processed."

            public static let timeout = "The nest took too long to respond. Please try again."

            public static let cancelled = "The request was cancelled."

            public static let `internal` = "The nest ran into an unexpected error. Please try again."

            public static let encode = "The nest sent a response that could not be read."

            public static let replayTooLarge = "There is too much to catch up on at once. Please try again."

            public static let disconnected = "The connection to the nest was lost."
        }

        public enum email {
            public static let tooLarge = "This message is too large to send. Remove attachments or shorten it and try again."

            public static let noHandle = "You need to set a handle for your account before you can send email."

            public static let permissionDenied = "You can only send email from your own address. Check the account you are sending from."

            public static let rateLimited = "You have reached your sending limit for now. Try again later."
        }

        public enum sync {
            public static let deviceLimitExceeded = "This account already has as many devices as its plan allows, so this device could not be added. Remove a device you no longer use under Settings → Devices, or ask your admin for a bigger tier."
        }

        public enum send {
            public static let generic = "Something went wrong. Please try again."

            public static let authRequired = "Sign in again to send this message."

            public static let notSupported = "This action is not available for this conversation."

            public static let noRecipients = "There are no recipients to send this message to."

            public static let roomInviteNotPermitted = "Only the room's owner and admins can invite people to this room."

            public static let roomRemoveNotPermitted = "Only the room's owner and admins can remove people from this room."

            public static let roomOwnerNotRemovable = "The room's owner cannot be removed. Ownership has to be handed over first."

            public static let roomPolicyNotPermitted = "Only the room's owner and admins can change this room's settings."

            public static let roomAdminsOwnerOnly = "Only the room's owner can appoint or demote admins."

            public static let roomTransferOwnerOnly = "Only the room's owner can hand the room over."

            public static let roomTransferNotAMember = "The room can only be handed over to one of its other members."

            public static let roomTransferSuperseded = "The room's settings changed while the hand-over was waiting, so it did not go through. Hand the room over again."

            public static let roomPolicyUnavailable = "This room has no room settings, so there is nothing to change here."

            public static let roomOwnerCannotLeave = "Hand the room over to someone else before you leave — a room always has an owner."

            public static let roomLeaveFailed = "Leaving this room did not go through. Try again."

            public static let attachmentUploadForeignFailed = "The attachment couldn't be uploaded to this conversation's home server. Try sending it again."

            public static func attachmentMissing(filename: String) -> String {
                "Attach \(filename) again — the file is not on this device."
            }
        }
    }

    public enum errors {
        public static let authFailed = "Sign-in failed"

        public static let secretKeyInvalid = "Secret key must be exactly 64 hexadecimal characters"

        public static let nestUnreachable = "Cannot connect to nest at the specified URL"

        public static let nestTimeout = "Nest did not come online within the expected time"

        public static let snapshotDeviceUnknown = "This snapshot's owning device is unknown, so its files cannot be downloaded."

        public static let noIdentity = "No identity configured"

        public static let notConnectedToNest = "Not connected to your nest — no active session"

        public static let nostrNsecRequired = "Enter your nsec"

        public static let nostrBunkerRequired = "Enter bunker URL"

        public static let feedNotReady = "Feed isn't ready yet. Try again in a moment."

        public static let photosAccessDenied = "Photos access denied. Grant access in System Settings > Privacy > Photos."

        public static let blueskyBridgeNotAvailable = "Bluesky bridge not available on this nest."

        public static let photoBackupNotConfigured = "Photo backup engine not configured. Connect to a nest first."

        public static let calendarRequiresMail = "Calendar requires mail to be enabled"

        public static let noCalendarSelected = "No calendar selected"

        public static let eventLoadForInviteFailed = "Could not load the event to invite"

        public static func httpError(detail: String) -> String {
            "HTTP error: \(detail)"
        }

        public static func authError(detail: String) -> String {
            "Authentication error: \(detail)"
        }

        public static func apiError(status: String, message: String) -> String {
            "API error (\(status)): \(message)"
        }

        public static func decodeError(detail: String) -> String {
            "Decode error: \(detail)"
        }

        public static func websocketError(detail: String) -> String {
            "WebSocket error: \(detail)"
        }

        public static func couldNotResolveDomain(domain: String) -> String {
            "Could not resolve domain: \(domain)"
        }

        public static let subprotocolMismatch = "Your app is out of date and can't connect to this nest. Please update to continue."

        public static func nestIdentityChanged(host: String) -> String {
            "The identity of \(host) has changed and could no longer be verified. For your safety, the connection was stopped."
        }

        public static func recoveryIdentityUnreadable(detail: String) -> String {
            "This session's identity secret is unreadable: \(detail)"
        }

        public static let recoverySuperseded = "This identity has already been succeeded by another one. Import your new identity to continue."

        public static let recoveryNoEscrow = "No sealed copy of your identity secret is stored for this recovery kit, so it cannot recover this account right now."

        public static let recoveryNotRegistered = "No recovery key is registered for this identity, so there is nothing to recover from."

        public static let recoveryAlreadySucceeded = "This identity has already been succeeded and cannot be recovered again."

        public static let recoveryInvalidNonce = "That recovery request has expired or was already used. Please try again."

        public static let recoverySignatureFailed = "That recovery kit was refused — it may have already been replaced."

        public static let recoveryKitNotCurrent = "This is not your most recently created recovery kit. Enter your newest kit instead."

        public static let recoverySuccessorExists = "That identity already has an account on this nest."

        public static func recoveryTransport(detail: String) -> String {
            "Connection error: \(detail)"
        }

        public static func recoveryCrypto(detail: String) -> String {
            "A cryptographic step failed: \(detail)"
        }

        public static func recoveryMalformed(detail: String) -> String {
            "The nest sent back something unexpected: \(detail)"
        }

        public static let recoveryPriorKitRequired = "A recovery key is already registered. Enter your current kit to replace it, or use the seed-alone replacement option instead."

        public static let recoveryPriorKitMismatch = "That kit does not match the one currently registered for this identity."

        public static func recoveryPriorEscrowUnreadable(reason: String) -> String {
            "Your existing recovery data could not be read (\(reason)), so replacing it now would destroy it. Try again from a device and connection that can read it."
        }

        public static func recoveryGroupCeremony(detail: String) -> String {
            "Updating your groups failed: \(detail)"
        }
    }

    public enum registrar {
        public enum step {
            public static let title = "Register a new domain"
        }

        public static let domain = "Domain name"

        public static let available = "Available"

        public static let unavailable = "Not available — try a different name"

        public static let priceConfirm = "I accept this price and authorize the charge on my registrar account. Registration starts immediately when I continue, and I understand this means I give up any right to withdraw from this purchase."

        public enum contact {
            public static let firstName = "First name"

            public static let lastName = "Last name"

            public static let email = "Email"

            public static let phone = "Phone (+CC.number)"

            public static let address1 = "Street address"

            public static let city = "City"

            public static let state = "State/province"

            public static let postalCode = "Postal code"

            public static let country = "Country"
        }
    }

    public enum provisioning {
        public static let verifyCredentials = "Verify credentials"

        public enum hostedAuth {
            public static let connect = "Sign in at the provider…"

            public static func pending(code: String) -> String {
                "Finish signing in in your browser — code \(code)"
            }

            public static let connected = "Connected"

            public static func failed(message: String) -> String {
                "Sign-in failed: \(message)"
            }
        }

        public enum bundled {
            public static let name = "Bundled provider (open API)"

            public static let help = "One company that registers your domain, hosts its DNS and rents you the server — you sign up and pay once, there. Paste the address the company gave you, then sign in; it must implement the open Fauna Bundled Provider API (the link above)."

            public static let baseUrl = "Provider address (https://…)"

            public static let account = "Account"

            public static let location = "Datacenter"
        }

        public enum cloudflare {
            public static let name = "Cloudflare"

            public static let help = "DNS and domain registration. Create an API token at dash.cloudflare.com → My Profile → API Tokens with Zone DNS and Registrar permissions; find your account ID on the Account home page's API section."

            public static let apiToken = "API token"

            public static let zone = "DNS zone"

            public static let accountId = "Account ID"

            public static let registrarAccountContactNote = "Cloudflare uses the contact details on your Cloudflare account for domain registration. Make sure they're set at dash.cloudflare.com → Domain Registration → Contacts before you continue."
        }

        public enum porkbun {
            public static let name = "Porkbun"

            public static let help = "Registrar + DNS. Enable API access at porkbun.com → Account → API Access."

            public static let key = "API key"

            public static let secret = "Secret API key"

            public static let domain = "Domain"

            public static let registrarAccountContactNote = "Porkbun uses the contact details on your Porkbun account for domain registration. Make sure they're set at porkbun.com/account/settings before you continue."
        }

        public enum hetzner {
            public static let name = "Hetzner Cloud"

            public static let help = "VPS and DNS. Create one Read & Write project token at console.hetzner.cloud — the same token manages both your server and your DNS."

            public static let apiToken = "Cloud API token"

            public static let location = "Datacenter"
        }

        public enum namecheap {
            public static let name = "Namecheap"

            public static let help = "Enable API access at namecheap.com → Profile → Tools → API Access. Namecheap also requires the calling IP address to be allowlisted there — if it isn't, the error message will name the exact address to add."

            public static let apiUser = "API user"

            public static let apiKey = "API key"

            public static let domain = "Domain"
        }

        public enum gandi {
            public static let name = "Gandi"

            public static let help = "Create a Personal Access Token at account.gandi.net → Security → Personal Access Tokens."

            public static let personalAccessToken = "Personal access token"

            public static let domain = "Domain"
        }

        public enum digitalocean {
            public static let name = "DigitalOcean"

            public static let help = "Create an API token at cloud.digitalocean.com → API → Generate New Token (read and write)."

            public static let apiToken = "API token"

            public static let location = "Region"
        }

        public enum vultr {
            public static let name = "Vultr"

            public static let help = "Create an API key at my.vultr.com → Account → API."

            public static let apiKey = "API key"

            public static let location = "Region"
        }

        public enum ovh {
            public static let name = "OVH Cloud"

            public static let help = "Create app credentials at api.ovh.com/createApp, then generate a consumer key via the OVH token endpoint for your app key and secret."

            public static let appKey = "Application key"

            public static let appSecret = "Application secret"

            public static let consumerKey = "Consumer key"

            public static let project = "Project"
        }

        public enum linode {
            public static let name = "Linode (Akamai)"

            public static let help = "Create a Personal Access Token at cloud.linode.com → Profile → API Tokens."

            public static let apiToken = "API token"

            public static let location = "Region"
        }

        public enum managed {
            public static let disabled = "Managed subdomains on fauna.social are coming soon. For now, pick another option."
        }
    }

    public enum region {
        public static func blockedNotice(region: String, authority: String) -> String {
            "Not shown in \(region) — blocked under the policy of \(authority)"
        }

        public static func collapsedNotice(region: String, authority: String) -> String {
            "Hidden in \(region) under the policy of \(authority) — select to show"
        }

        public static let revealButton = "Show"

        public static let sectionTitle = "Region"

        public static func declared(region: String) -> String {
            "Your region: \(region)"
        }

        public static let noneDeclared = "No region is declared on this device"

        public static let sourceStorefront = "From your app store region — change it in your store account"

        public static let sourceSystemRegion = "From your system region setting — change it in your system settings"

        public static let sourceSystemLocale = "From your system locale — change it in your system settings"

        public static let sourceBrowserLocale = "From your browser language — change it in your browser settings"

        public static let noPolicy = "No regional content policy is in force"

        public static func policyAuthority(region: String, authority: String) -> String {
            "\(region): \(authority)"
        }

        public static func policyVersion(sequence: String, issued: String) -> String {
            "Version \(sequence), issued \(issued)"
        }

        public static func inertNotice(version: String) -> String {
            "Uses a policy format this app does not understand (version \(version)) — nothing is blocked under it"
        }

        public static let malformedNotice = "This policy could not be read — nothing is blocked under it"

        public static func lastChecked(time: String) -> String {
            "Last checked \(time)"
        }

        public static let staleWarning = "Could not check for policy updates recently — the policies above stay in force"
    }

    public enum connectedApps {
        public static let title = "Connected apps"

        public static let description = "Apps and services that act for you from outside Fauna. Each one can only reach what is listed under it, and you can disconnect any of them at any time."

        public static func verbatim(text: String) -> String {
            "\(text)"
        }

        public static let unnamed = "Unnamed app"

        public static let signerPending = "Waiting to connect…"

        public static let scopeNostrSign = "Sign Nostr events with your key"

        public static let scopeMail = "Read and send your mail, and sync your calendar, contacts and files"

        public static let requestsHeading = "Requests"

        public static let consentEndsHolder = "This app now uses a new key. Approving ends the access its earlier key was given."

        public static let consentEndsWriter = "This app now signs with a new key. Approving ends its earlier key's permission to write."

        public static let connectHeading = "Connect an app"

        public static let connectHint = "If an app on another device shows you a code, type it here."

        public static let connectPlaceholder = "Code from the app"

        public static let connectSubmit = "Connect"

        public static let rosterHeading = "Your connected apps"

        public static let empty = "No connected apps yet."

        public static let block = "Never show requests from this app"

        public static let revoke = "Disconnect"

        public static let revokeConfirm = "Disconnect now"

        public static let revokeCancel = "Keep"

        public static func revokePrompt(name: String) -> String {
            "Disconnect \(name)? It will no longer be able to act for you."
        }

        public static func publisher(domain: String) -> String {
            "From \(domain)"
        }

        public static let classRemote = "Website or service"

        public static let classDevice = "App on a device"

        public static let classWasm = "Plugin on your nest"

        public static let classContainer = "Plugin on your nest"

        public static let classAppPassword = "Signed in with an app password"

        public static let classSigner = "Nostr signer app"

        public static let classOauth = "Signed-in app"

        public static let notConnected = "Not connected right now"

        public static func created(time: String) -> String {
            "Added \(time)"
        }

        public static func lastUsed(time: String) -> String {
            "Last used \(time)"
        }

        public static let neverUsed = "Never used"

        public static func lastsUntil(time: String) -> String {
            "Until \(time)"
        }

        public static let openEnded = "Until you disconnect it"

        public static func errorRefresh(message: String) -> String {
            "Failed to load connected apps: \(message)"
        }

        public static func errorRevoke(message: String) -> String {
            "Failed to disconnect the app: \(message)"
        }

        public static func errorResolve(message: String) -> String {
            "Failed to answer the request: \(message)"
        }

        public static let errorRequestGone = "That request is no longer waiting — it was answered or it expired."

        public static let errorCodeExpired = "That code has expired — ask the app for a new one."

        public static let errorHandoffExpired = "That link has expired or was already used — start again from the app."

        public static func errorBlock(message: String) -> String {
            "Failed to block the app: \(message)"
        }

        public static func errorSecret(message: String) -> String {
            "Could not read the secret: \(message)"
        }

        public static let blockedHeading = "Blocked apps"

        public static let blockedHint = "Requests from these apps are never shown to you."

        public static func blockedSince(time: String) -> String {
            "Blocked \(time)"
        }

        public static let unblock = "Allow requests again"
    }

    public enum shareLink {
        public static let button = "Share a link"

        public static func createTitle(name: String) -> String {
            "Share a link to \(name)"
        }

        public static let createBody = "Anyone with the link can open this file until it expires or you revoke it."

        public static let keyNotice = "This file is private, so the link carries the key that unlocks it. Anyone holding the link can open the file — and wherever you paste it, anyone who can read that place can open it too."

        public static let expiryLabel = "Link expires after"

        public static let expiry1d = "1 day"

        public static let expiry7d = "7 days"

        public static let expiry30d = "30 days"

        public static let expiry1y = "1 year"

        public static let create = "Create link"

        public static let creating = "Creating link…"

        public static let cancel = "Cancel"

        public static let close = "Close"

        public static let copy = "Copy link"

        public static let listButton = "Shared links"

        public static let listTitle = "Your shared links"

        public static let listLoading = "Loading your shared links…"

        public static let empty = "You haven't shared any links yet."

        public static func expires(date: String) -> String {
            "Expires \(date)"
        }

        public static let stateActive = "Active"

        public static let stateExpired = "Expired"

        public static let stateRevoked = "Revoked"

        public static let revoke = "Revoke"

        public static let revokeConfirmTitle = "Revoke this link?"

        public static func revokeConfirmBody(name: String) -> String {
            "The link to \(name) stops working for everyone. This cannot be undone — you can make a new link at any time."
        }

        public static let revokeConfirm = "Revoke link"

        public static func errorCreate(message: String) -> String {
            "Couldn't create the link: \(message)"
        }

        public static func errorList(message: String) -> String {
            "Couldn't load your shared links: \(message)"
        }

        public static func errorRevoke(message: String) -> String {
            "Couldn't revoke the link: \(message)"
        }
    }

    public enum shareViewer {
        public static let title = "A file shared with Fauna"

        public static let genericBody = "Someone shared a file with you through Fauna. Open the complete link you were sent to see it."

        public static let loading = "Opening the file…"

        public static let download = "Download"

        public static let keepNote = "A copy you download stays with you, even after the link stops working."

        public static let gone = "This link has expired or was revoked."

        public static let withheld = "This file is not available for legal reasons."

        public static let notFound = "This link was not found. It may be mistyped, or the file is no longer stored."

        public static let damaged = "This link could not be opened. Check that you copied the whole link."

        public static let unavailable = "The file could not be loaded right now. Try again later."
    }

    /// Runtime lookup of an i18n string by dotted key (e.g.
    /// "provisioning.cloudflare.name"). Used when the call site only
    /// has the key as a String, e.g. `ProviderMeta.displayNameKey`.
    /// Returns the key itself when missing — surfaces typos in the UI
    /// rather than silently producing empty strings.
    public static func lookup(_ key: String) -> String {
        _LFlat.table[key] ?? key
    }
}

private enum _LFlat {
    static let table: [String: String] = [
        "admin.actor_id_fallback_label": "actor {short}…",
        "admin.aliases": "Aliases",
        "admin.aliases_page.count": "{count} aliases",
        "admin.aliases_page.create_button": "Create Alias",
        "admin.aliases_page.create_forwarder": "Add Forwarder",
        "admin.aliases_page.created_col": "Created",
        "admin.aliases_page.delete_forwarder": "Delete",
        "admin.aliases_page.domain_optional": "Domain (optional)",
        "admin.aliases_page.forward": "Forward",
        "admin.aliases_page.forwarder_domain": "Domain",
        "admin.aliases_page.forwarder_local_part": "Local part",
        "admin.aliases_page.forwarder_local_part_placeholder": "info",
        "admin.aliases_page.forwarder_row": "{address} → {target}",
        "admin.aliases_page.forwarder_target": "Forwards to",
        "admin.aliases_page.forwarder_target_placeholder": "name@example.com",
        "admin.aliases_page.forwarders_desc": "Map an address on one of your domains to an external destination. Forwarded addresses have no local mailbox.",
        "admin.aliases_page.forwarders_title": "External Forwarders",
        "admin.aliases_page.loading": "Loading aliases...",
        "admin.aliases_page.local_part": "Local part",
        "admin.aliases_page.no_aliases": "No aliases configured yet.",
        "admin.aliases_page.no_forwarders": "No external forwarders configured yet.",
        "admin.aliases_page.target_address": "Target address",
        "admin.aliases_page.target_col": "Target",
        "admin.aliases_page.title": "External Forwarders",
        "admin.bridges_pending.approve": "Approve",
        "admin.bridges_pending.approved_at": "Approved",
        "admin.bridges_pending.approved_empty": "No approved bridges yet.",
        "admin.bridges_pending.approved_section": "Approved bridges",
        "admin.bridges_pending.description": "Mail and calendar bridges awaiting your approval.",
        "admin.bridges_pending.empty": "No bridges awaiting approval.",
        "admin.bridges_pending.empty_desc": "Mail and calendar bridges that connect to this nest appear here for approval.",
        "admin.bridges_pending.first_seen": "First seen",
        "admin.bridges_pending.name_bluesky": "Bluesky bridge",
        "admin.bridges_pending.name_bridge": "Bridge",
        "admin.bridges_pending.name_mail": "Mail bridge",
        "admin.bridges_pending.name_mail_calendar": "Mail & calendar bridge",
        "admin.bridges_pending.pending_section": "Pending approval",
        "admin.bridges_pending.pubkey": "Public key",
        "admin.bridges_pending.reject": "Reject",
        "admin.bridges_pending.role": "Role",
        "admin.bridges_pending.rotate": "Rotate service-user key",
        "admin.bridges_pending.source_ip": "Source IP",
        "admin.bridges_pending.source_ip_unknown": "—",
        "admin.bridges_pending.title": "Bridges",
        "admin.bridges_rotate.cancel": "Cancel",
        "admin.bridges_rotate.confirm": "Rotate key",
        "admin.bridges_rotate.title": "Rotate service-user key?",
        "admin.bridges_rotate.warning": "The bridge will be marked revoked and will exit; the supervisor restarts it with a fresh key. On a mail-enabled box the new key is approved automatically.",
        "admin.calendar_page.caldav_port_desc": "The port the calendar (CalDAV) server listens on for desktop or IP-only nests that have no domain — reach it at https://this-host:port/. Default 8443. On a domain nest this is ignored: CalDAV is served at mail.your-domain on port 443.",
        "admin.calendar_page.caldav_port_invalid": "Enter a port number between 1 and 65535.",
        "admin.calendar_page.caldav_port_label": "CalDAV port",
        "admin.calendar_page.caldav_port_save": "Save port",
        "admin.calendar_page.description": "Calendar (CalDAV) sync for this deployment.",
        "admin.calendar_page.enabled_label": "Enable calendar (CalDAV) on this nest",
        "admin.calendar_page.enabled_subtitle": "Serve calendar sync (CalDAV) for all users. Needs only a real domain with a public certificate — no email infrastructure — so calendar can run with or without email. The shared mail-and-calendar bridge runs whenever this or Enable mail is on.",
        "admin.calendar_page.title": "Calendar",
        "admin.contacts_page.description": "Contact (CardDAV) sync for this deployment.",
        "admin.contacts_page.enabled_label": "Enable contacts (CardDAV) on this nest",
        "admin.contacts_page.enabled_subtitle": "Serve contact sync (CardDAV) for all users. Rides the same server and certificate as calendar — no email infrastructure — so contacts can run with or without email or calendar. The shared bridge runs whenever this, Enable calendar, or Enable mail is on.",
        "admin.contacts_page.title": "Contacts",
        "admin.custody_hosting.active": "Active",
        "admin.custody_hosting.budget": "Budget",
        "admin.custody_hosting.budget_default": "Default",
        "admin.custody_hosting.count": "{count} held for others",
        "admin.custody_hosting.description": "Data this nest holds on behalf of other people's accounts, at the request of an account holder here. Each row makes this nest dial an outside address on a schedule and keep what it serves.",
        "admin.custody_hosting.empty": "No account here has asked this nest to hold data for anyone.",
        "admin.custody_hosting.held": "Now holding",
        "admin.custody_hosting.host": "Requested by",
        "admin.custody_hosting.owner": "Held for",
        "admin.custody_hosting.receipt_fresh": "Confirmed recently",
        "admin.custody_hosting.receipt_none": "Never confirmed",
        "admin.custody_hosting.receipt_stale": "Not confirmed lately",
        "admin.custody_hosting.remove": "Remove",
        "admin.custody_hosting.remove_cancel": "Keep it",
        "admin.custody_hosting.remove_confirm": "Remove it",
        "admin.custody_hosting.remove_confirm_body": "This frees the space now. Pausing only stops the schedule and keeps what is already stored. Removing cannot be undone from here — the account holder would have to ask again.",
        "admin.custody_hosting.remove_confirm_title": "Remove this held custody?",
        "admin.custody_hosting.remove_missing": "That row was already gone.",
        "admin.custody_hosting.removed": "Removed.",
        "admin.custody_hosting.removed_with_store": "Removed, and the stored copy was freed.",
        "admin.custody_hosting.stopped": "Paused",
        "admin.custody_hosting.title": "Held Custody",
        "admin.custody_hosting.url": "Pulled from",
        "admin.dashboard.admin_token": "Admin token",
        "admin.dashboard.connections": "Connections",
        "admin.dashboard.email": "Email",
        "admin.dashboard.load_error": "Failed to load admin stats: {message}",
        "admin.dashboard.loading": "Loading stats...",
        "admin.dashboard.loading_dashboard": "Loading dashboard...",
        "admin.dashboard.mail": "Mail",
        "admin.dashboard.nest_dashboard": "Nest Dashboard",
        "admin.dashboard.nest_domain": "Nest Domain",
        "admin.dashboard.no_users": "No users",
        "admin.dashboard.not_admin": "Not an Admin",
        "admin.dashboard.not_admin_desc": "Enter a valid admin token to access the dashboard.",
        "admin.dashboard.paired_nests": "Paired Nests",
        "admin.dashboard.registration": "Registration",
        "admin.dashboard.suspended": "Suspended",
        "admin.dashboard.title": "Dashboard",
        "admin.dashboard.tls": "TLS",
        "admin.dashboard.token_prompt": "Enter your admin Bearer token to view nest statistics.",
        "admin.dashboard.total_storage": "Total Storage",
        "admin.dashboard.version": "Version",
        "admin.dns.add_credential": "Add credential",
        "admin.dns.add_credential_submit": "Add",
        "admin.dns.add_domain": "Add domain",
        "admin.dns.add_domain_placeholder": "example.com",
        "admin.dns.add_domain_primary_warning": "This becomes your nest's primary domain and can never be removed — undoing it later requires renaming onto a different domain.",
        "admin.dns.add_domain_submit": "Add",
        "admin.dns.catch_all_cleared_by_succession": "A succession cleared this domain's catch-all — unmatched mail now bounces. Re-designate below if you want a new one.",
        "admin.dns.catch_all_label": "Catch-all:",
        "admin.dns.catch_all_none": "None",
        "admin.dns.cert.auto_renew": "Auto-renew",
        "admin.dns.cert.delegate": "Automate renewals",
        "admin.dns.cert.delegate_cancel": "Cancel",
        "admin.dns.cert.delegate_no_zones": "Add a DNS-provider credential first to delegate renewals.",
        "admin.dns.cert.delegate_submit": "Delegate",
        "admin.dns.cert.delegate_zone_label": "Delegate to zone:",
        "admin.dns.cert.expires": "expires {date}",
        "admin.dns.cert.issue": "Get certificate",
        "admin.dns.cert.issue_cancel": "Cancel",
        "admin.dns.cert.issue_complete": "I've added the record",
        "admin.dns.cert.label": "Certificate:",
        "admin.dns.cert.paste_instructions": "Add this DNS record at your registrar, then confirm:",
        "admin.dns.cert.remove_delegation": "Remove delegation",
        "admin.dns.cert.renewals_automated": "Renewals automated",
        "admin.dns.cert.self_signed": "self-signed",
        "admin.dns.cert.status_expiring": "Expiring soon",
        "admin.dns.cert.status_on_floor": "Renew needed",
        "admin.dns.cert.status_valid": "Valid",
        "admin.dns.copy": "Copy",
        "admin.dns.credential_zones": "Zones",
        "admin.dns.credentials_empty": "No DNS-provider credentials yet.",
        "admin.dns.credentials_title": "DNS-provider credentials",
        "admin.dns.description": "Every DNS record each of your domains needs, with the exact value to set and a live check against public DNS.",
        "admin.dns.empty": "No domains yet.",
        "admin.dns.empty_desc": "Add a mail domain and its required DNS records appear here.",
        "admin.dns.field_name": "Name",
        "admin.dns.field_type": "Type",
        "admin.dns.field_value": "Value",
        "admin.dns.manage_all": "Fauna controls all domains",
        "admin.dns.mode_managed": "Fauna-managed",
        "admin.dns.mode_manual": "Manual",
        "admin.dns.primary_badge": "Primary",
        "admin.dns.ptr_provider_note": "Reverse DNS (PTR) is set at your server's IP provider, not published here. Most VPS providers let you set it in their control panel.",
        "admin.dns.refresh": "Refresh",
        "admin.dns.remove": "Remove",
        "admin.dns.removed_desc": "Restorable for 30 days.",
        "admin.dns.removed_title": "Recently removed",
        "admin.dns.rename.abort": "Abort rename",
        "admin.dns.rename.abort_confirm": "Confirm abort",
        "admin.dns.rename.abort_postflip_warning": "Aborting after the anchor flip re-flips the primary and rewrites every domain's DNS — expensive but safe.",
        "admin.dns.rename.banner_title": "Primary-domain rename in progress",
        "admin.dns.rename.button": "Rename primary domain",
        "admin.dns.rename.cancel": "Cancel",
        "admin.dns.rename.complete": "Complete now",
        "admin.dns.rename.complete_confirm": "Confirm complete",
        "admin.dns.rename.complete_force_warning": "Completing before the grace window ends may briefly break mail delivery for peers whose caches have not yet refreshed.",
        "admin.dns.rename.extend": "Extend grace",
        "admin.dns.rename.extend_days_label": "Extend by (days):",
        "admin.dns.rename.grace_days_label": "Grace window (days, default 7)",
        "admin.dns.rename.grace_elapsed": "Grace window elapsed",
        "admin.dns.rename.grace_ends": "Grace ends:",
        "admin.dns.rename.new_primary_label": "New primary domain",
        "admin.dns.rename.promote": "Promote to primary",
        "admin.dns.rename.renaming_to": "Renaming to",
        "admin.dns.rename.sheet_title": "Rename primary domain",
        "admin.dns.rename.state_label": "State:",
        "admin.dns.rename.submit": "Start rename",
        "admin.dns.restore": "Restore",
        "admin.dns.role_address_admin_default": "Admin (default)",
        "admin.dns.role_address_label": "Role addresses:",
        "admin.dns.status_checking": "Checking…",
        "admin.dns.status_mismatch": "Mismatch",
        "admin.dns.status_mismatch_found": "Mismatch — found {found}",
        "admin.dns.status_missing": "Missing",
        "admin.dns.status_ok": "OK",
        "admin.dns.title": "DNS",
        "admin.exit": "Exit admin",
        "admin.files_page.description": "File (WebDAV) sync for this deployment.",
        "admin.files_page.enabled_label": "Enable files (WebDAV) on this nest",
        "admin.files_page.enabled_subtitle": "Serve file access (WebDAV) for all users. Rides the same server and certificate as calendar and contacts — no email infrastructure — so files can run with or without email, calendar, or contacts. Nothing is served until a user flags a folder for WebDAV. The shared bridge runs whenever this, Enable contacts, Enable calendar, or Enable mail is on.",
        "admin.files_page.title": "Files",
        "admin.invite_requests_page.approve": "Approve",
        "admin.invite_requests_page.approve_failed": "Could not approve. Please try again.",
        "admin.invite_requests_page.approving": "Approving...",
        "admin.invite_requests_page.column_actor": "Actor",
        "admin.invite_requests_page.column_handle": "Handle",
        "admin.invite_requests_page.column_message": "Message",
        "admin.invite_requests_page.deny": "Deny",
        "admin.invite_requests_page.deny_failed": "Could not deny. Please try again.",
        "admin.invite_requests_page.deny_reason_placeholder": "Optional reason",
        "admin.invite_requests_page.denying": "Denying...",
        "admin.invite_requests_page.description": "Review and approve or deny invite requests submitted by users.",
        "admin.invite_requests_page.empty": "No pending invite requests.",
        "admin.invite_requests_page.title": "Invite Requests",
        "admin.logs_page.description": "Recent activity recorded on the nest, newest first. No message contents or secrets are logged — only what happened, when, and where.",
        "admin.logs_page.empty": "No nest log entries yet.",
        "admin.logs_page.title": "Nest Logs",
        "admin.mail_page.alias_exact_max_label": "Exact aliases per account (max)",
        "admin.mail_page.alias_exact_max_subtitle": "Cap on user-added exact aliases beyond the signup address.",
        "admin.mail_page.alias_group_desc": "Per-account alias limits and the inbound address-resolution rules.",
        "admin.mail_page.alias_group_title": "Aliases",
        "admin.mail_page.alias_reserved_label": "Reserved local-parts",
        "admin.mail_page.alias_reserved_subtitle": "Role addresses users cannot claim — one local-part per line; empty clears the reservation.",
        "admin.mail_page.alias_save": "Save alias policy",
        "admin.mail_page.alias_subaddressing_label": "Sub-addressing (plus-suffix)",
        "admin.mail_page.alias_subaddressing_subtitle": "Allow plus-tagged aliases that route to the base address.",
        "admin.mail_page.alias_wildcard_prefix_label": "Wildcard-prefix aliases",
        "admin.mail_page.alias_wildcard_prefix_subtitle": "Allow name-prefixed aliases that route to the same user.",
        "admin.mail_page.auth_group_desc": "Which SPF / DKIM / DMARC failures reject inbound mail at delivery.",
        "admin.mail_page.auth_group_title": "Authentication enforcement",
        "admin.mail_page.auth_save": "Save authentication policy",
        "admin.mail_page.auto_enable_new_users_label": "Auto-enable mail for new users",
        "admin.mail_page.auto_enable_new_users_subtitle": "New users automatically get a mailbox at their handle on first sign-in. Each user can still turn their own mail off.",
        "admin.mail_page.bayesian_full_confidence_samples_label": "Bayesian full-confidence samples",
        "admin.mail_page.bayesian_full_confidence_samples_subtitle": "Samples at which a user's model reaches full weight (default 200). Must be above min samples.",
        "admin.mail_page.bayesian_min_samples_label": "Bayesian min samples",
        "admin.mail_page.bayesian_min_samples_subtitle": "Training samples below which a user's own model is ignored (default 50).",
        "admin.mail_page.bayesian_weight_label": "Bayesian weight (milli)",
        "admin.mail_page.bayesian_weight_subtitle": "Weight of each user's own model in the combined spam score, in milli — 700 = 0.7. Range 0–1000.",
        "admin.mail_page.description": "Box-wide mail policy — enable mail and tune the inbound perimeter and authentication enforcement. DKIM, TLS, and DNS records are managed automatically.",
        "admin.mail_page.dnsbl_label": "DNS blocklists",
        "admin.mail_page.dnsbl_subtitle": "One blocklist host per line, queried during delivery.",
        "admin.mail_page.enabled_label": "Enable mail",
        "admin.mail_page.enabled_subtitle": "Run the mail subsystem (SMTP / IMAP / CalDAV) for this nest.",
        "admin.mail_page.enforce_dkim_label": "Enforce DKIM",
        "admin.mail_page.enforce_dmarc_label": "Enforce DMARC reject",
        "admin.mail_page.enforce_dmarc_quarantine_label": "Enforce DMARC quarantine",
        "admin.mail_page.enforce_spf_hardfail_label": "Enforce SPF hardfail",
        "admin.mail_page.fcrdns_enforce": "Enforce",
        "admin.mail_page.fcrdns_mode_label": "Forward-confirmed rDNS",
        "admin.mail_page.fcrdns_off": "Off",
        "admin.mail_page.fcrdns_score_signal": "Score signal",
        "admin.mail_page.greylist_delay_label": "Greylist delay (seconds)",
        "admin.mail_page.greylist_enabled_label": "Greylisting",
        "admin.mail_page.health_check_blocklist": "Blocklist check",
        "admin.mail_page.health_check_bridge": "Mail service connection",
        "admin.mail_page.health_check_fail": "Problem",
        "admin.mail_page.health_check_info": "Info",
        "admin.mail_page.health_check_last_delivered": "Last delivered",
        "admin.mail_page.health_check_last_received": "Last received",
        "admin.mail_page.health_check_pass": "OK",
        "admin.mail_page.health_check_queue": "Outgoing queue",
        "admin.mail_page.health_check_records": "DNS and authentication records",
        "admin.mail_page.health_check_warmup": "Sending warm-up",
        "admin.mail_page.health_check_warn": "Warning",
        "admin.mail_page.health_delist": "Request removal from the blocklist",
        "admin.mail_page.health_never": "Never",
        "admin.mail_page.health_recheck": "Check again",
        "admin.mail_page.health_state_blocklisted": "Mail: server address is blocklisted",
        "admin.mail_page.health_state_bridge_down": "Mail: mail service not connected",
        "admin.mail_page.health_state_delivering": "Mail: delivering",
        "admin.mail_page.health_state_off": "Mail: off",
        "admin.mail_page.health_state_queue_stalled": "Mail: outgoing mail is delayed",
        "admin.mail_page.health_state_records_failing": "Mail: DNS records need attention",
        "admin.mail_page.health_state_unknown": "Mail: needs attention",
        "admin.mail_page.health_state_warming_up": "Mail: warming up",
        "admin.mail_page.health_status_line": "{state} — last delivered: {delivered} · last received: {received}",
        "admin.mail_page.health_title": "Mail health",
        "admin.mail_page.health_warmup_reset": "Restart warm-up",
        "admin.mail_page.health_warmup_reset_confirm": "Restart the sending warm-up at day 1? Do this only after the server's outgoing IP address changed.",
        "admin.mail_page.helo_identity_label": "Require HELO identity",
        "admin.mail_page.imap_bodystructure_cache_label": "BodyStructure cache size (entries)",
        "admin.mail_page.imap_bodystructure_cache_subtitle": "In-memory derivation cache the MDA keeps per session.",
        "admin.mail_page.imap_delete_allowed": "Allowed",
        "admin.mail_page.imap_delete_forbidden": "Forbidden",
        "admin.mail_page.imap_delete_nonempty_label": "Delete non-empty mailbox",
        "admin.mail_page.imap_group_desc": "How the IMAP/MDA serves mailboxes to mail clients.",
        "admin.mail_page.imap_group_title": "IMAP server policy",
        "admin.mail_page.imap_idle_timeout_label": "IDLE timeout (seconds)",
        "admin.mail_page.imap_idle_timeout_subtitle": "How long an idle IMAP session is held before the server ends it.",
        "admin.mail_page.imap_message_count_label": "Message-count quota",
        "admin.mail_page.imap_message_count_subtitle": "Per-actor message-count ceiling across all mailboxes.",
        "admin.mail_page.imap_save": "Save IMAP policy",
        "admin.mail_page.imap_storage_bytes_label": "Storage quota (bytes)",
        "admin.mail_page.imap_storage_bytes_subtitle": "Per-actor storage ceiling across all mailboxes.",
        "admin.mail_page.imap_tombstone_retention_label": "Tombstone retention (days)",
        "admin.mail_page.imap_tombstone_retention_subtitle": "How long expunged-message markers are kept for resync (minimum 7).",
        "admin.mail_page.log_only_label": "Log only (never reject)",
        "admin.mail_page.max_conn_per_ip_label": "Max concurrent connections (per IP)",
        "admin.mail_page.max_conn_per_min_label": "Max connections / minute (per IP)",
        "admin.mail_page.max_failures_label": "AUTH failure limit / minute",
        "admin.mail_page.max_message_bytes_label": "Max message size (bytes)",
        "admin.mail_page.outbound_delay_warning_label": "Delay-warning time (hours)",
        "admin.mail_page.outbound_delay_warning_subtitle": "When a delay-warning notice is sent to the sender.",
        "admin.mail_page.outbound_group_desc": "Retry, bounce, and TLS-reporting behavior for outbound mail.",
        "admin.mail_page.outbound_group_title": "Outbound delivery",
        "admin.mail_page.outbound_ipv6_label": "IPv6 outbound",
        "admin.mail_page.outbound_ndr_rate_limit_label": "Bounce rate-limit window (days)",
        "admin.mail_page.outbound_ndr_rate_limit_subtitle": "Per-recipient window for suppressing repeated bounce notices.",
        "admin.mail_page.outbound_permfail_timeout_label": "Permanent-failure timeout (hours)",
        "admin.mail_page.outbound_permfail_timeout_subtitle": "Total retry budget before a message permanently fails.",
        "admin.mail_page.outbound_postmaster_cc_label": "CC postmaster on bounces",
        "admin.mail_page.outbound_postmaster_cc_subtitle": "Disabled — project policy never copies the postmaster.",
        "admin.mail_page.outbound_retry_schedule_label": "Retry schedule (seconds)",
        "admin.mail_page.outbound_retry_schedule_subtitle": "Delay before each successive attempt — one value per line.",
        "admin.mail_page.outbound_save": "Save outbound policy",
        "admin.mail_page.outbound_suppress_ndr_dmarc_label": "Suppress bounce on DMARC reject",
        "admin.mail_page.outbound_suppress_ndr_spf_label": "Suppress bounce on SPF hardfail",
        "admin.mail_page.outbound_tlsrpt_send_label": "Send TLSRPT reports",
        "admin.mail_page.outbound_treat_5xx_label": "Treat as transient (5xx codes)",
        "admin.mail_page.outbound_treat_5xx_subtitle": "Enhanced-status codes to retry even when the reply is 5xx — one per line.",
        "admin.mail_page.publish_spam_baseline_button": "Publish deployment baseline",
        "admin.mail_page.publish_spam_baseline_subtitle": "Aggregate every opted-in user's spam training into a baseline that new users start from. Never reveals who contributed, and needs at least 3 contributors.",
        "admin.mail_page.reject_fcrdns_fail_label": "Reject on FCrDNS failure",
        "admin.mail_page.reject_no_rdns_label": "Reject senders with no rDNS",
        "admin.mail_page.spam_baseline_published": "Published from {contributors} contributors ({samples} samples).",
        "admin.mail_page.spam_baseline_skipped_contributors": "{count} opted-in contributor(s) could not be merged this run.",
        "admin.mail_page.spam_baseline_standing_label": "Keep a shared spam baseline published",
        "admin.mail_page.spam_baseline_standing_subtitle": "Republishes the baseline every 24 hours while enough users contribute. Turning this off withdraws the published baseline.",
        "admin.mail_page.spam_baseline_state_none": "No baseline published.",
        "admin.mail_page.spam_baseline_state_published": "Published over {contributors} contributors on {date}.",
        "admin.mail_page.spam_baseline_waiting": "Waiting for more contributor activity.",
        "admin.mail_page.spam_baseline_withheld": "Not published — too few contributors ({contributors}); at least 3 must opt in.",
        "admin.mail_page.spam_group_desc": "How inbound mail is scored, rate-limited, and gated before delivery.",
        "admin.mail_page.spam_group_title": "Spam and inbound perimeter",
        "admin.mail_page.spam_save": "Save spam policy",
        "admin.mail_page.submission_group_desc": "Per-actor ceilings on outbound message submission.",
        "admin.mail_page.submission_group_title": "Submission quotas",
        "admin.mail_page.submission_max_per_day_label": "Messages per day (per actor)",
        "admin.mail_page.submission_max_per_day_subtitle": "How many messages each account may submit per day.",
        "admin.mail_page.submission_max_recipients_label": "Recipients per message",
        "admin.mail_page.submission_max_recipients_subtitle": "Maximum recipients allowed on a single submitted message.",
        "admin.mail_page.submission_save": "Save submission policy",
        "admin.mail_page.threshold_junk_label": "Junk threshold",
        "admin.mail_page.threshold_junk_subtitle": "Combined score (0–15) above which mail is delivered to Junk. 0 disables.",
        "admin.mail_page.threshold_reject_label": "Reject threshold",
        "admin.mail_page.threshold_reject_subtitle": "Score above which mail is rejected outright. 0 disables.",
        "admin.mail_page.title": "Mail",
        "admin.mail_page.training_history_retention_label": "Training history retention (days)",
        "admin.mail_page.training_history_retention_subtitle": "How long each user's per-message training-undo history is kept (default 30).",
        "admin.mail_page.unlisted_recipient_penalty_label": "Unlisted-recipient penalty (points)",
        "admin.mail_page.unlisted_recipient_penalty_subtitle": "Extra spam points added when mail arrives at an address that isn't one of a user's aliases (delivered via catch-all). 0 = off; a large value (e.g. 1000) forces such mail to Junk.",
        "admin.nest_page.description": "Nest-wide settings for this deployment.",
        "admin.nest_page.load_serving_port_error": "Failed to load serving port: {message}",
        "admin.nest_page.load_settings_error": "Failed to load nest settings: {message}",
        "admin.nest_page.nat_mode_choosing": "Applies to mail serving immediately; certificates and connectivity re-check at the next restart.",
        "admin.nest_page.nat_mode_error_load": "Couldn't load the current network mode: {cause}. You can still pick and save a mode.",
        "admin.nest_page.nat_mode_error_terminal": "Couldn't save the network mode: {cause}.",
        "admin.nest_page.nat_mode_error_transient": "Couldn't save the network mode: {cause}. Try again.",
        "admin.nest_page.nat_mode_label": "Network mode",
        "admin.nest_page.nat_mode_loading": "Loading network mode…",
        "admin.nest_page.nat_mode_save": "Save mode",
        "admin.nest_page.nat_mode_saved": "Network mode saved. Mail serving updated now; certificates and connectivity re-check at the next restart.",
        "admin.nest_page.nat_mode_submitting": "Saving network mode…",
        "admin.nest_page.oauth_cancel_button": "Cancel",
        "admin.nest_page.oauth_desc": "The keys this nest signs outside apps' sign-in passes with, and the secret behind their saved sign-ins. Replacing them is a response to an exposure, never routine upkeep.",
        "admin.nest_page.oauth_force_rotate_button": "Replace sign-in key at once…",
        "admin.nest_page.oauth_force_rotate_confirm_button": "Replace at once",
        "admin.nest_page.oauth_force_rotate_confirm_many": "Replace the sign-in key at once? All {count} keys accepted now stop being accepted immediately, so every outside app signed in with them must sign in again. Use this when a key is known to have leaked.",
        "admin.nest_page.oauth_force_rotate_confirm_one": "Replace the sign-in key at once? The key in use stops being accepted immediately, so every outside app signed in with it must sign in again. Use this when the key is known to have leaked.",
        "admin.nest_page.oauth_force_rotate_done": "Replaced at once. {kid} is now the only key accepted. Stopped working: {dropped}.",
        "admin.nest_page.oauth_force_rotate_done_none": "Replaced at once. {kid} is now the only key accepted.",
        "admin.nest_page.oauth_key_retired": "{kid} — replaced; no longer accepted",
        "admin.nest_page.oauth_key_retiring": "{kid} — replaced; still accepted for {minutes} min",
        "admin.nest_page.oauth_key_signing": "{kid} — signing now",
        "admin.nest_page.oauth_keys_error": "Couldn't read this nest's sign-in keys: {cause}. Reload this page to try again.",
        "admin.nest_page.oauth_keys_loading": "Checking which sign-in keys this nest uses…",
        "admin.nest_page.oauth_label": "Outside-app sign-in keys",
        "admin.nest_page.oauth_rotate_button": "Replace sign-in key",
        "admin.nest_page.oauth_rotate_desc": "A precaution: the current key stays accepted for {minutes} min after it is replaced, so nobody is signed out. Use this for a suspected exposure.",
        "admin.nest_page.oauth_rotate_done": "Replaced. {kid} signs from now on; the previous key stays accepted until it times out.",
        "admin.nest_page.oauth_rotate_failed": "The nest didn't confirm the change: {cause}. Check the keys listed here before trying again.",
        "admin.nest_page.oauth_secret_force_rotate_button": "End all saved sign-ins…",
        "admin.nest_page.oauth_secret_force_rotate_confirm": "End every saved sign-in? Every connected outside app must be approved again. After a known leak, do this as well as replacing the sign-in key — replacing the key alone leaves saved sign-ins able to get new passes.",
        "admin.nest_page.oauth_secret_force_rotate_confirm_button": "End saved sign-ins",
        "admin.nest_page.oauth_secret_force_rotate_done": "Ended. Every saved sign-in issued since {minted} stopped working, and {apps} outside apps were signed out; each must be approved again the next time it is used.",
        "admin.nest_page.oauth_secret_force_rotate_done_none": "Ended. Every saved sign-in issued since {minted} stopped working. No outside apps were connected here.",
        "admin.nest_page.oauth_secret_force_rotate_done_one": "Ended. Every saved sign-in issued since {minted} stopped working, and one outside app was signed out; it must be approved again the next time it is used.",
        "admin.nest_page.oauth_secret_force_rotate_first": "Done. There were no saved sign-ins to end.",
        "admin.nest_page.oauth_working": "Working…",
        "admin.nest_page.os_restart_now": "Restart now",
        "admin.nest_page.os_restart_now_error": "Failed to request a host restart: {message}",
        "admin.nest_page.os_restart_pending": "Restart pending — will restart automatically when idle",
        "admin.nest_page.os_up_to_date": "OS up to date",
        "admin.nest_page.os_updates_pending": "Security updates pending",
        "admin.nest_page.region_declared": "Declared region: {region}",
        "admin.nest_page.region_desc": "The region whose laws this deployment operates under. You declare it; it is never detected from an address or a network. If that region has an authority publishing feature rules, they apply to accounts hosted here.",
        "admin.nest_page.region_document": "Region rules in force, published by {authority} (version {sequence}).",
        "admin.nest_page.region_enrolled_no_document": "An authority is enrolled for this region; no rules have been published yet.",
        "admin.nest_page.region_invalid": "Enter a 2–8 character region code in capitals, like NO or EU.",
        "admin.nest_page.region_label": "Region",
        "admin.nest_page.region_load_error": "Failed to load the declared region: {message}",
        "admin.nest_page.region_none": "No region declared",
        "admin.nest_page.region_not_enrolled": "No authority is enrolled for this region, so no region rules apply here.",
        "admin.nest_page.region_placeholder": "Country or region code, e.g. NO",
        "admin.nest_page.region_save": "Declare region",
        "admin.nest_page.region_save_error": "Failed to save the declared region: {message}",
        "admin.nest_page.region_stale": "Haven't been able to check for updated region rules recently. The rules already received stay in force.",
        "admin.nest_page.region_unreadable": "The stored region declaration can't be read. Declare the region again (or withdraw it) to fix this; any region rules already received stay in force.",
        "admin.nest_page.region_withdraw": "Withdraw declaration",
        "admin.nest_page.reports_acted": "Mark as acted on",
        "admin.nest_page.reports_desc": "Reports from users of this nest, and reports forwarded from other nests about accounts hosted here. A report is evidence for you to weigh; it removes nothing by itself. Acting means the legal-takedown console or a suspension — resolving a row only records what you decided.",
        "admin.nest_page.reports_dismiss": "Dismiss",
        "admin.nest_page.reports_empty": "No open reports.",
        "admin.nest_page.reports_failed": "Could not update the report: {error}",
        "admin.nest_page.reports_label": "Reports",
        "admin.nest_page.reports_loading": "Loading reports…",
        "admin.nest_page.reports_open_takedown": "Open in takedown console",
        "admin.nest_page.reports_origin_forwarded": "Reported by a user of {nest}",
        "admin.nest_page.reports_origin_local": "Reported by {handle}",
        "admin.nest_page.reports_resolved_acted": "Recorded as acted on. The reporter is told the outcome, nothing more.",
        "admin.nest_page.reports_resolved_dismissed": "Dismissed. The reporter is told the outcome, nothing more.",
        "admin.nest_page.retire_button": "Retire this server…",
        "admin.nest_page.retire_desc": "Delete this server at your cloud provider and remove the DNS records that point at it. Unlike a factory reset, which wipes a server you keep, this destroys the server itself. You'll need your cloud provider's token.",
        "admin.nest_page.retire_title": "Retire this server",
        "admin.nest_page.rotate_seed_button": "Rotate deployment identity",
        "admin.nest_page.rotate_seed_cancel_button": "Cancel",
        "admin.nest_page.rotate_seed_confirm_body": "These admins inherit the new identity and can still recover this nest. Anyone not listed loses that ability. This cannot be undone.",
        "admin.nest_page.rotate_seed_confirm_button": "Rotate now",
        "admin.nest_page.rotate_seed_desc": "Give this nest a brand-new identity. Apps that already trust it re-trust it automatically, and anyone still holding the old identity — a removed admin, a lost device — stops being able to use it. It does not undo anything they already saw. Remove the admin first: everyone on the roster inherits the new identity.",
        "admin.nest_page.rotate_seed_done": "Deployment identity rotated. Apps re-trust this nest automatically.",
        "admin.nest_page.rotate_seed_done_unmarked": "Deployment identity rotated. Your recovery list still shows the old identity — reconnect from this device to clear it.",
        "admin.nest_page.rotate_seed_failed": "Couldn't rotate the deployment identity: {cause}",
        "admin.nest_page.rotate_seed_label": "Deployment identity",
        "admin.nest_page.rotate_seed_mismatch": "This nest reported a different identity than the one that was sent. Nothing further was changed; check the nest before trying again.",
        "admin.nest_page.rotate_seed_roster_empty": "This nest reported no administrators, which can't be right. Nothing was rotated — reload this page and try again.",
        "admin.nest_page.rotate_seed_roster_error": "Couldn't check who currently administers this nest: {cause}. Nothing was rotated — try again.",
        "admin.nest_page.rotate_seed_roster_loading": "Checking who currently administers this nest…",
        "admin.nest_page.rotate_seed_working": "Rotating the deployment identity…",
        "admin.nest_page.serving_port_desc": "The port this nest's client-facing API and web app listen on for desktop or IP-only nests with no router in front — reach it at https://this-host:port/. Default 443. Behind the cloud router this is ignored: the external port is set by the deployment. Takes effect after the nest restarts.",
        "admin.nest_page.serving_port_fronted_hint": "Served on 443 by this deployment.",
        "admin.nest_page.serving_port_invalid": "Enter a port number between 1 and 65535.",
        "admin.nest_page.serving_port_label": "Serving port",
        "admin.nest_page.serving_port_save": "Save port",
        "admin.nest_page.set_serving_port_error": "Failed to set serving port: {message}",
        "admin.nest_page.takedown_arm_restore": "Restore…",
        "admin.nest_page.takedown_arm_takedown": "Take down…",
        "admin.nest_page.takedown_blocked_no_content": "Enter the content id of the item named by the legal obligation.",
        "admin.nest_page.takedown_blocked_no_reference": "A legal reference is required — a takedown without one is refused.",
        "admin.nest_page.takedown_cancel_button": "Cancel",
        "admin.nest_page.takedown_confirm_button_restore": "Confirm restore",
        "admin.nest_page.takedown_confirm_button_takedown": "Confirm takedown",
        "admin.nest_page.takedown_confirm_restore": "Overturn the takedown of {content_type} {content_id}? The content serves again; the takedown record remains as history.",
        "admin.nest_page.takedown_confirm_takedown": "Take down {content_type} {content_id}, citing \"{reference}\"? A visible tombstone will be served in its place, the author can appeal, and a permanent audit row records this action.",
        "admin.nest_page.takedown_content_id_label": "Content id",
        "admin.nest_page.takedown_desc": "The one nest-wide content removal, for legal compulsion only (a court order, a statutory demand). Every takedown serves a visible tombstone in place of the content, can be appealed by the author, and writes a permanent audit record. It is never a policy or opinion lever.",
        "admin.nest_page.takedown_done": "Taken down. A tombstone is served in its place and the author can appeal.",
        "admin.nest_page.takedown_failed": "The nest refused the request: {error}",
        "admin.nest_page.takedown_label": "Legal takedown",
        "admin.nest_page.takedown_reference_label": "Legal reference",
        "admin.nest_page.takedown_restore_label": "Overturn an existing takedown (restore)",
        "admin.nest_page.takedown_restored": "Restored. The content is served again; the takedown stays on record.",
        "admin.nest_page.takedown_type_conversation": "Conversation message",
        "admin.nest_page.takedown_type_post": "Post",
        "admin.nest_page.takedown_working": "Submitting…",
        "admin.nest_page.title": "Nest",
        "admin.nest_page.update_setting_error": "Failed to update nest setting: {message}",
        "admin.nest_page.web_app_origin_bundled": "Serve the app this server ships",
        "admin.nest_page.web_app_origin_central": "Send people to {origin}, with this server filled in",
        "admin.nest_page.web_app_origin_desc": "What this server's own address answers when someone opens the app there.",
        "admin.nest_page.web_app_origin_label": "Web app",
        "admin.nest_page.web_app_origin_loading": "Loading the web app choice…",
        "admin.nest_page.web_app_origin_save": "Save web app choice",
        "admin.nest_page.web_app_origin_scope": "This changes only what this server's own address answers. Anyone who opens {origin} directly loads the app from there either way, and an address someone types or bookmarks always wins.",
        "admin.nest_page.web_app_origin_status_bundled": "This server's address serves the app it ships.",
        "admin.nest_page.web_app_origin_status_central": "People who open this server's address are sent to {target}",
        "admin.nest_page.web_app_origin_status_domainless": "Sending people to {origin} is chosen, but this server has no domain to fill in yet, so its address keeps serving the app it ships.",
        "admin.nest_page.web_app_origin_status_predates": "This server is too old to offer this choice; its address always serves the app it ships. Update the server to change it.",
        "admin.nest_page.web_app_origin_status_unknown_mode": "This server uses a web app choice this app doesn't recognize ({mode}). Update the app to change it.",
        "admin.services_page.bridge": "Email Bridge",
        "admin.services_page.bridge_desc": "IMAP, SMTP, and CalDAV (calendar) access for all users.",
        "admin.services_page.description": "Enable or disable nest sidecar services.",
        "admin.services_page.disabled": "Disabled",
        "admin.services_page.dns": "DNS",
        "admin.services_page.dns_desc": "Automatic DNS record management.",
        "admin.services_page.enabled": "Enabled",
        "admin.services_page.manage_dns": "Manage DNS",
        "admin.services_page.pairing": "Nest Pairing",
        "admin.services_page.pairing_desc": "Let users link their own nests to sync their account (per-user multi-homing).",
        "admin.services_page.title": "Services",
        "admin.settings_page.add_tier": "Add tier",
        "admin.settings_page.add_tier_error": "Failed to add tier: {message}",
        "admin.settings_page.add_tier_error_empty_name": "Can't add this tier — give it a name.",
        "admin.settings_page.add_tier_error_invalid_cap": "Can't add this tier — every limit must be a whole number.",
        "admin.settings_page.add_tier_name": "Tier name",
        "admin.settings_page.add_tier_section": "Define a new tier",
        "admin.settings_page.cap_blob_size": "Blob size (bytes)",
        "admin.settings_page.cap_devices": "Devices",
        "admin.settings_page.cap_feeds": "Feeds",
        "admin.settings_page.cap_inbox_bytes": "Inbox (bytes)",
        "admin.settings_page.cap_storage_bytes": "Storage (bytes)",
        "admin.settings_page.clear_membership_tier_error": "Failed to clear membership designation: {message}",
        "admin.settings_page.create_code": "Create Invite Code",
        "admin.settings_page.created": "created {date}",
        "admin.settings_page.email_domains": "Email Domains",
        "admin.settings_page.factory_reset_button": "Factory Reset…",
        "admin.settings_page.factory_reset_cancel": "Cancel",
        "admin.settings_page.factory_reset_confirm_body": "This permanently deletes all users, mail, and deployment configuration on this nest and returns it to an unclaimed state. The nest identity and TLS certificate are kept, and you will be guided through re-claiming it. This cannot be undone.",
        "admin.settings_page.factory_reset_confirm_button": "Factory Reset",
        "admin.settings_page.factory_reset_confirm_title": "Factory reset this nest?",
        "admin.settings_page.factory_reset_desc": "Wipe all deployment state (users, mail, stored content) and return this nest to a fresh, unclaimed state. The nest identity and TLS certificate are preserved. You will re-claim the nest immediately afterward.",
        "admin.settings_page.factory_reset_failed": "Factory reset failed. The nest is unchanged.",
        "admin.settings_page.factory_reset_persist_failed": "Could not save the new setup code on this device, so the reset was not started and your nest is unchanged. Free up storage and try again.",
        "admin.settings_page.factory_reset_section": "Danger Zone",
        "admin.settings_page.factory_reset_title": "Factory Reset This Nest",
        "admin.settings_page.inbox_limit": "Inbox Limit",
        "admin.settings_page.invite_codes": "Invite Codes",
        "admin.settings_page.load_membership_tiers_error": "Failed to load membership designations: {message}",
        "admin.settings_page.load_tiers_error": "Failed to load tiers: {message}",
        "admin.settings_page.loading_codes": "Loading invite codes...",
        "admin.settings_page.loading_domains": "Loading email domains...",
        "admin.settings_page.loading_membership": "Loading membership designations...",
        "admin.settings_page.loading_tiers": "Loading tiers...",
        "admin.settings_page.max_uses": "Max Uses",
        "admin.settings_page.membership_admits_at": "Admits at",
        "admin.settings_page.membership_clear": "Clear",
        "admin.settings_page.membership_lapses_to": "Lapses to",
        "admin.settings_page.membership_save": "Save",
        "admin.settings_page.membership_section": "Membership",
        "admin.settings_page.no_codes": "No invite codes.",
        "admin.settings_page.no_domains": "No email domains configured.",
        "admin.settings_page.no_membership_tiers": "You have no subscription tiers yet — create one in your Tiers tab, then designate it here for paid nest access.",
        "admin.settings_page.no_tiers": "No tier definitions found.",
        "admin.settings_page.save_membership_tier_error": "Failed to save membership designation: {message}",
        "admin.settings_page.save_membership_tier_error_no_tier": "Can't save this row — pick an \"Admits at\" tier first.",
        "admin.settings_page.save_tier": "Save",
        "admin.settings_page.save_tier_error": "Failed to save tier: {message}",
        "admin.settings_page.save_tier_error_invalid_cap": "Can't save this tier — every limit must be a whole number.",
        "admin.settings_page.storage_limit": "Storage Limit",
        "admin.settings_page.tier_caps": "Inbox {inbox} · Storage {storage} · {devices} devices",
        "admin.settings_page.tiers": "Tiers",
        "admin.settings_page.title": "Tiers",
        "admin.settings_page.unverified": "Unverified",
        "admin.settings_page.uses_left": "{remaining}/{total} uses left",
        "admin.settings_page.uses_left_n": "{count} uses left",
        "admin.users_page.admit_actor_hint": "The account key must be exactly 64 hex characters.",
        "admin.users_page.admit_actor_label": "Their account key (64 characters)",
        "admin.users_page.admit_button": "Admit",
        "admin.users_page.admit_handle_label": "Their handle (blank admits without one — they cannot send email until they have a handle)",
        "admin.users_page.age_verification_required_label": "Accept only signups carrying app age verification",
        "admin.users_page.cancel": "Cancel",
        "admin.users_page.cancel_eviction": "Cancel Eviction",
        "admin.users_page.code_minted": "Code minted —",
        "admin.users_page.copy_code": "Copy code",
        "admin.users_page.delete": "Delete",
        "admin.users_page.evict": "Evict",
        "admin.users_page.evict_confirm": "Start eviction for {id}? The user will be warned and given time to export data.",
        "admin.users_page.evict_default_reason": "Evicted by admin",
        "admin.users_page.guardian_label": "Guardian",
        "admin.users_page.guardian_none": "None",
        "admin.users_page.loading": "Loading users...",
        "admin.users_page.make_admin": "Make Admin",
        "admin.users_page.max_free_users_hint": "Blank for no limit. Counts every free account, including yours.",
        "admin.users_page.max_free_users_label": "Limit free accounts",
        "admin.users_page.minted_code": "New invite code: {code}",
        "admin.users_page.next_page": "Next",
        "admin.users_page.no_handle": "no handle",
        "admin.users_page.no_pending_requests": "No pending requests.",
        "admin.users_page.no_users": "No users.",
        "admin.users_page.page_indicator": "Page {current} of {pages}",
        "admin.users_page.pending_approvals": "{given} of {needed} approvals",
        "admin.users_page.pending_approve": "Approve",
        "admin.users_page.pending_by": "by {who}",
        "admin.users_page.pending_count": "Pending admin actions ({count})",
        "admin.users_page.pending_none": "Nothing is pending.",
        "admin.users_page.prev_page": "Previous",
        "admin.users_page.registration_mode_closed": "Nobody — only I can admit people",
        "admin.users_page.registration_mode_invite_required": "Only people with an invite code",
        "admin.users_page.registration_mode_label": "Who may create an account",
        "admin.users_page.registration_mode_open": "Anyone",
        "admin.users_page.registration_mode_unknown": "This nest uses a registration setting this app version does not recognize ({mode}). Update the app to change it.",
        "admin.users_page.registration_save": "Save",
        "admin.users_page.remove_admin": "Remove Admin",
        "admin.users_page.section_admit": "Admit someone directly",
        "admin.users_page.section_invite": "Invite",
        "admin.users_page.section_pending": "Pending admin actions",
        "admin.users_page.section_registration": "Registration",
        "admin.users_page.section_requests": "Pending requests",
        "admin.users_page.section_users": "Users",
        "admin.users_page.serving_disabled": "Not serving",
        "admin.users_page.serving_here": "Serving here",
        "admin.users_page.suspend": "Suspend",
        "admin.users_page.suspend_default_reason": "Suspended by admin",
        "admin.users_page.title": "Users",
        "admin.users_page.total": "{count} users total",
        "admin.users_page.user_count": "{count} users",
        "admin.view.active_sessions": "Active Sessions",
        "admin.view.nest_statistics": "Nest Statistics",
        "admin.view.no_registered_users": "No registered users.",
        "admin.view.no_user_data": "No user data available.",
        "admin.view.no_users_loaded": "No users loaded yet.",
        "admin.view.recent_users": "Recent Users",
        "admin.view.recent_users_desc": "Last 10 registered users",
        "admin.view.registered_users": "Registered Users",
        "admin.view.server_status": "Server Status",
        "admin.view.total_inbox_messages": "Total Inbox Messages",
        "admin.view.total_storage_used": "Total Storage Used",
        "admin.view.uptime": "Uptime",
        "admin.view.workers": "Workers",
        "admin.web_page.apex_info": "The main address serves at {url}.",
        "admin.web_page.apex_none": "None (info page)",
        "admin.web_page.apex_select_label": "Home page",
        "admin.web_page.apex_select_subtitle": "Choose whose website serves at this deployment's main address. None serves the built-in info page.",
        "admin.web_page.description": "Web-content hosting for this deployment.",
        "admin.web_page.title": "Web",
        "archive_import.archive_open_button": "Open archive",
        "archive_import.archive_path_placeholder": "Path to the export .zip",
        "archive_import.archive_summary_fmt": "{platform} · {owner} · {first} – {last} · {archive_size} archive, {media_size} of photos and videos",
        "archive_import.archive_summary_undated": "{platform} · {owner} · no dated records · {archive_size} archive, {media_size} of photos and videos",
        "archive_import.archive_title": "Step 2 — The archive",
        "archive_import.audience_only_me": "Only me",
        "archive_import.audience_original": "Keep original audiences",
        "archive_import.back": "Back",
        "archive_import.cancel_button": "Cancel",
        "archive_import.category_albums": "Albums",
        "archive_import.category_comments": "Comments",
        "archive_import.category_events": "Events",
        "archive_import.category_friends": "Friends",
        "archive_import.category_groups": "Groups",
        "archive_import.category_messages": "Messages",
        "archive_import.category_posts": "Posts",
        "archive_import.category_profile": "Profile",
        "archive_import.category_reactions": "Reactions",
        "archive_import.category_threads": "Message threads",
        "archive_import.confirm_summary_fmt": "{records} records · about {bytes} to upload",
        "archive_import.confirm_title": "Step 4 — Confirm",
        "archive_import.description": "Bring your posts, photos and events from a Facebook or Instagram export archive into Fauna, at their original dates and audiences. The archive itself stays in a sealed folder on your nest.",
        "archive_import.done_summary_fmt": "{imported} imported · {skipped} skipped",
        "archive_import.done_title": "Step 6 — Done",
        "archive_import.error_log_title": "Skipped records",
        "archive_import.folder_link_fmt": "Archive folder: {folder}",
        "archive_import.next": "Next",
        "archive_import.pause_button": "Pause",
        "archive_import.profile_prefill_button": "Use the archive's profile name and bio",
        "archive_import.progress_row_fmt": "{imported} of {count} · {skipped} skipped",
        "archive_import.progress_summary_fmt": "{state} · {imported} of {total} · {skipped} skipped",
        "archive_import.progress_title": "Step 5 — Importing",
        "archive_import.resume_button": "Resume",
        "archive_import.review_skipped_button": "Review skipped",
        "archive_import.scope_audience_mode_label": "Audience",
        "archive_import.scope_audience_summary_fmt": "{known} posts and albums have a recorded audience and import to it; {unknown} have none recorded and will be visible only to you.",
        "archive_import.scope_audience_summary_only_me": "Everything will be visible only to you.",
        "archive_import.scope_categories_label": "Categories",
        "archive_import.scope_category_kept_fmt": "{category} ({count}) — kept in the archive for later",
        "archive_import.scope_category_row_fmt": "{category} ({count})",
        "archive_import.scope_date_from_placeholder": "From date (optional, YYYY-MM-DD)",
        "archive_import.scope_date_to_placeholder": "To date (optional, YYYY-MM-DD)",
        "archive_import.scope_hidden_tiers_unavailable": "This nest predates hidden tiers, so only public posts can be imported now. Update the nest, then import again to pick up the rest.",
        "archive_import.scope_title": "Step 3 — What to import",
        "archive_import.source_help_facebook": "On Facebook, open Settings & privacy → Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON, any media quality. The download link expires after a few days, so save the zip as soon as it is ready.",
        "archive_import.source_help_instagram": "On Instagram, open Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON. The download link expires after a few days, so save the zip as soon as it is ready.",
        "archive_import.source_title": "Step 1 — Where the archive comes from",
        "archive_import.start_button": "Start import",
        "archive_import.state_cancelled": "Cancelled",
        "archive_import.state_completed": "Done",
        "archive_import.state_errored": "Stopped after an error",
        "archive_import.state_paused": "Paused",
        "archive_import.state_running": "Importing",
        "archive_import.title": "Import from other services",
        "archive_import.unavailable": "Importing needs a session that holds your identity key; this one does not.",
        "archive_import.view_imported_button": "View imported posts",
        "atproto_settings.app_credentials_empty": "No app credentials yet.",
        "atproto_settings.app_credentials_heading": "App credentials",
        "atproto_settings.card_deactivate": "Publishing stops and the network no longer serves your profile or posts. Your identity is kept — re-enabling restores it exactly.",
        "atproto_settings.card_delete_pointer": "\"Delete my Bluesky presence\" below is the separate, stronger action.",
        "atproto_settings.card_dm_honesty": "Bluesky direct messages are not end-to-end encrypted and pass through this nest in transit.",
        "atproto_settings.card_mint": "A new public identity {handle} is created on the Bluesky network.",
        "atproto_settings.card_no_recall": "Copies of already-published posts held by other servers cannot be recalled.",
        "atproto_settings.card_open_plane": "Third-party Bluesky apps will be able to log in as this identity once you create an app credential.",
        "atproto_settings.card_publish_consent": "Your public posts become visible to everyone on the Bluesky network.",
        "atproto_settings.card_reactivate": "Your identity {handle} is restored — the same identity you had before, nothing new is created.",
        "atproto_settings.card_suspend_plane": "Connected apps stop working immediately. Nothing is deleted — your app credentials stay listed, and stepping back up restores them.",
        "atproto_settings.card_unlink": "The link to {account} is removed. The external account itself is untouched — it keeps existing on its own server and is not migrated.",
        "atproto_settings.connected_apps_empty": "No connected apps yet.",
        "atproto_settings.connected_apps_heading": "Connected apps",
        "atproto_settings.consent_approve_button": "Approve",
        "atproto_settings.consent_client": "{name} — {client_id}",
        "atproto_settings.consent_client_unnamed": "{client_id}",
        "atproto_settings.consent_code": "Confirmation code: {code}",
        "atproto_settings.consent_code_hint": "Approve only if your browser is showing this same code.",
        "atproto_settings.consent_deny_button": "Deny",
        "atproto_settings.consent_heading": "An app wants to sign in as you",
        "atproto_settings.consent_scopes_heading": "It is asking to:",
        "atproto_settings.consent_set_heading": "Some of that comes from “{title}” ({nsid}):",
        "atproto_settings.consent_set_heading_unnamed": "Some of that comes from {nsid}:",
        "atproto_settings.contest_button": "Undo this change…",
        "atproto_settings.contest_cancel_button": "Not now",
        "atproto_settings.contest_card_heading": "Your AT Protocol identity may have been taken over",
        "atproto_settings.contest_confirm_button": "Undo it now",
        "atproto_settings.contest_confirm_directory_rules": "The public directory decides whether to accept the undo. If it refuses, nothing about your identity changes and trying again is safe.",
        "atproto_settings.contest_confirm_signs": "This device signs with the recovery key it holds, which puts your own keys back in charge of who can change this identity. Your nest keeps the separate key it publishes your posts with, so this does not stop it posting as you; replacing that key is a separate step.",
        "atproto_settings.contest_confirm_undo": "You are about to undo the change to {handle}, and everything published on top of it, by signing an earlier state of your identity back into place.",
        "atproto_settings.contest_deadline": "About {hours} hours left to undo this.",
        "atproto_settings.contest_deadline_soon": "Less than an hour left to undo this.",
        "atproto_settings.contest_detail_contestable": "A change to your AT Protocol identity {handle} was made with a key this device does not hold. You can undo it: the change and everything built on it are reversed, and afterwards only your own keys can change who controls this identity. Your posts and profile keep working through this nest — which also means it keeps the key it uses to publish them, so it could still post as you. Undoing the change does not take that key away; replacing it is a separate step.",
        "atproto_settings.contest_detail_genesis": "Your AT Protocol identity {handle} was created with a key this device does not hold, so there is no earlier state to return it to. This identity cannot be recovered — create a new one, and contact whoever runs your nest.",
        "atproto_settings.contest_detail_unauthenticated": "The public record of your AT Protocol identity {handle} does not check out: the changes it lists are not signed by keys this identity's own history allows. That points at the record being tampered with, or your connection to it being intercepted — so nothing has been signed or changed from here, and undoing is not offered, because acting on a false record would destroy your real history. Try again from another network, and contact whoever runs your nest.",
        "atproto_settings.contest_detail_window_closed": "A change to your AT Protocol identity {handle} was made with a key this device does not hold, and the time limit for undoing it has passed. The change now stands permanently. Contact whoever runs your nest.",
        "atproto_settings.credential_created_prefix": "Created {date}",
        "atproto_settings.credential_last_used_prefix": "Last used {date}",
        "atproto_settings.credential_never_used": "Never used",
        "atproto_settings.default_credential_label": "App credential {count}",
        "atproto_settings.delegation_authorize_button": "Let other apps post as me",
        "atproto_settings.delegation_capability_post": "post",
        "atproto_settings.delegation_capability_update_profile": "update your profile",
        "atproto_settings.delegation_empty": "Other Bluesky apps can sign in, but cannot post as you yet.",
        "atproto_settings.delegation_heading": "Posting from other apps",
        "atproto_settings.delegation_last_used": "Last reported use: {when}",
        "atproto_settings.delegation_last_used_hint": "Reported by your nest, so treat it as a hint — check your feed for posts marked \"Via connected app\" to see what was actually written.",
        "atproto_settings.delegation_last_used_never": "No use reported yet",
        "atproto_settings.delegation_lasts_until": "Authorized {authorized} · until {expires}",
        "atproto_settings.delegation_lasts_until_no_expiry": "Authorized {authorized} · no expiry",
        "atproto_settings.delegation_reauthorize_button": "Re-authorize",
        "atproto_settings.delegation_revoke_button": "Stop other apps posting as me",
        "atproto_settings.delegation_scope_prefix": "Allowed: {capabilities}",
        "atproto_settings.delegation_status_active": "Active",
        "atproto_settings.delegation_status_expired": "Expired — other apps can no longer post as you",
        "atproto_settings.delegation_status_expiring_soon": "Expiring soon — re-authorize to keep other apps posting",
        "atproto_settings.delegation_status_never_expires": "No expiry",
        "atproto_settings.delete_cancel_button": "Keep my presence",
        "atproto_settings.delete_confirm_apps_disconnected": "Bluesky apps you are signed in to are disconnected, and other apps can no longer post as you.",
        "atproto_settings.delete_confirm_button": "Delete my presence",
        "atproto_settings.delete_confirm_identity_kept": "Your AT Protocol identity @{handle} is kept. This removes what you published, not who you are — turning AT Protocol hosting back on later restores the same identity.",
        "atproto_settings.delete_confirm_identity_retired": "Your AT Protocol identity @{handle} is also permanently retired once the deletion finishes. This cannot be undone: the identity stops existing on the Bluesky network, and no one — not you, not this nest — can ever restore it. Turning AT Protocol hosting back on later creates a new, different identity.",
        "atproto_settings.delete_confirm_level_off": "Your Bluesky setting returns to Off.",
        "atproto_settings.delete_confirm_sweep": "Every post published to Bluesky is deleted, and the network is told to remove them.",
        "atproto_settings.delete_presence_button": "Delete my Bluesky presence",
        "atproto_settings.delete_retire_identity_label": "Also permanently retire my AT Protocol identity — this cannot be undone",
        "atproto_settings.delete_retire_unavailable_unpublished": "This identity has not been published yet, so there is nothing to retire.",
        "atproto_settings.delete_retire_unavailable_web": "This identity is tied to your domain, so there is no separate record to retire — it ends when your domain stops serving it.",
        "atproto_settings.depth_cancel_button": "Cancel",
        "atproto_settings.depth_card_heading": "Confirm this change",
        "atproto_settings.depth_confirm_button": "Confirm",
        "atproto_settings.depth_heading": "Integration depth",
        "atproto_settings.depth_hosted_full_desc": "Additionally, other Bluesky apps can log in as you through this nest.",
        "atproto_settings.depth_hosted_full_title": "Hosted here — full access",
        "atproto_settings.depth_hosted_visible_desc": "This nest holds your identity and publishes your public posts to the Bluesky network.",
        "atproto_settings.depth_hosted_visible_title": "Hosted here — visible",
        "atproto_settings.depth_linked_desc": "Read, interact, and cross-post through an existing Bluesky account.",
        "atproto_settings.depth_linked_title": "Linked account",
        "atproto_settings.depth_off_desc": "No Bluesky presence.",
        "atproto_settings.depth_off_title": "Off",
        "atproto_settings.did_method_heading": "Identity method",
        "atproto_settings.did_method_plc_desc": "A portable identity you can move to another server later.",
        "atproto_settings.did_method_plc_title": "did:plc — recommended",
        "atproto_settings.did_method_web_desc": "Ties your identity to this nest's domain.",
        "atproto_settings.did_method_web_title": "did:web",
        "atproto_settings.error_authorize": "Failed to let external apps post as you: {message}",
        "atproto_settings.error_consent": "Could not send your answer: {message}",
        "atproto_settings.error_consent_gone": "That request is no longer waiting for an answer — it may have expired, or you may have already answered it on another device. Start the sign-in again from the app that asked.",
        "atproto_settings.error_contest_not_contestable": "This change cannot be undone from here.",
        "atproto_settings.error_deauthorize": "Failed to stop external apps posting as you: {message}",
        "atproto_settings.error_delegation_untrusted": "The stored authorization for external apps was not created by this account, so it is not being shown. ({message})",
        "atproto_settings.error_delete_presence": "Could not delete your Bluesky presence: {message}",
        "atproto_settings.error_mint": "Failed to create the app credential: {message}",
        "atproto_settings.error_no_identity": "This app cannot authorize posting from external apps. Use another of your devices to turn it on.",
        "atproto_settings.error_nothing_to_delete": "There is no Bluesky presence left to delete.",
        "atproto_settings.error_refresh": "Failed to load app credentials: {message}",
        "atproto_settings.error_request_contest": "Could not start undoing the change: {message}",
        "atproto_settings.error_revoke": "Failed to revoke the app credential: {message}",
        "atproto_settings.error_revoke_session": "Failed to disconnect the app: {message}",
        "atproto_settings.error_save_local": "Created, but this device could not save a copy — copy the password now, it cannot be shown again later. ({message})",
        "atproto_settings.error_toggle": "Failed to change external app access: {message}",
        "atproto_settings.error_transition": "Failed to change the Bluesky integration level: {message}",
        "atproto_settings.external_apps_toggle": "Allow external apps",
        "atproto_settings.gate_reason": "Hosting an AT Protocol identity needs a public domain — this nest is reachable at \"{domain}\", which the Bluesky network cannot resolve. Claim a real domain to enable these options.",
        "atproto_settings.gate_reason_pending": "Checking whether this nest has a public domain — hosting an AT Protocol identity needs one.",
        "atproto_settings.handle_either_way": "Your Bluesky handle will be @{handle} either way.",
        "atproto_settings.history_backfill_label": "Also publish my existing public posts.",
        "atproto_settings.hosted_handle_prefix": "Your Bluesky handle: @{handle}",
        "atproto_settings.hosted_method_prefix": "Method: {method}",
        "atproto_settings.identity_status_active": "Active",
        "atproto_settings.identity_status_deactivated": "Deactivated",
        "atproto_settings.identity_status_deleted": "Deleted",
        "atproto_settings.identity_status_pending": "Setting up…",
        "atproto_settings.identity_status_tombstoned": "Permanently retired",
        "atproto_settings.mint_button": "New app credential",
        "atproto_settings.reveal_button": "Reveal",
        "atproto_settings.revoke_button": "Revoke",
        "atproto_settings.session_created_prefix": "Connected {date}",
        "atproto_settings.session_expires_prefix": "Expires {date}",
        "atproto_settings.session_last_used_prefix": "Last seen {date}",
        "atproto_settings.session_never_used": "Not seen since connecting",
        "atproto_settings.session_scopes_prefix": "Approved for {scopes}",
        "atproto_settings.session_set_named": "“{title}” ({nsid})",
        "atproto_settings.session_sets_prefix": "Granted via {sets}",
        "atproto_settings.session_status_live": "Working",
        "atproto_settings.session_status_suspended": "Paused — turn Bluesky back on to let this app work again",
        "atproto_settings.title": "AT Protocol",
        "backups.about_restore": "About Restore",
        "backups.all_devices": "All Devices",
        "backups.backup_audit_alert_freshness": "{destination} is {days} days behind your data. Your backup there is not keeping up.",
        "backups.backup_audit_alert_inclusion": "{destination} is missing {missing} of {sampled} records we checked for. Your backup there is incomplete.",
        "backups.backup_audit_alert_overdue": "{destination} has not been checked for {days} days. We cannot confirm your backup there is intact.",
        "backups.backup_audit_alert_self_reported": "{destination} reports that its own copy of your data failed its check. That copy cannot be relied on.",
        "backups.backup_audit_alert_source_regressed": "{destination} still holds data your nest lost when it went back to an older copy. It will be kept there for about {days} more days — ask whoever restored your nest whether a newer copy exists.",
        "backups.backup_audit_alert_source_regressed_until_recovered": "{destination} still holds data your nest lost when it went back to an older copy. It will be kept there until it is recovered — ask whoever restored your nest whether a newer copy exists.",
        "backups.backup_destination_add_button": "Add destination",
        "backups.backup_destination_add_cancel": "Cancel",
        "backups.backup_destination_add_confirm": "Save",
        "backups.backup_destination_backlog": "{count} queued",
        "backups.backup_destination_capacity_invalid": "Enter a storage limit like \"50 GB\" or \"500 MB\".",
        "backups.backup_destination_capacity_placeholder": "Storage limit, e.g. 50 GB",
        "backups.backup_destination_custodian_exposure": "This device will keep a complete offline copy of your data. Anyone who can unlock it can read all of it, not just what is on screen.",
        "backups.backup_destination_edit_button": "Edit",
        "backups.backup_destination_edit_different_nest": "That URL points to a different nest. Remove this destination and add the new one.",
        "backups.backup_destination_form_add_title": "Add a backup destination",
        "backups.backup_destination_form_edit_title": "Edit backup destination",
        "backups.backup_destination_keep_button": "Keep",
        "backups.backup_destination_kind_client_device": "This device",
        "backups.backup_destination_kind_nest": "Another nest",
        "backups.backup_destination_kind_select_label": "Where should the copy live?",
        "backups.backup_destination_kind_unknown": "Unsupported destination ({kind})",
        "backups.backup_destination_last_audit": "Last checked: {when}",
        "backups.backup_destination_last_audit_never": "Last checked: never",
        "backups.backup_destination_last_self_audit": "Self-checked: {when}",
        "backups.backup_destination_last_self_audit_never": "Self-checked: not yet",
        "backups.backup_destination_last_upload": "Last synced: {when}",
        "backups.backup_destination_last_upload_never": "Last synced: never",
        "backups.backup_destination_name_placeholder": "Friendly name (optional)",
        "backups.backup_destination_reclaim_button": "Free up this space",
        "backups.backup_destination_remove_button": "Remove",
        "backups.backup_destination_remove_cancel_button": "Cancel",
        "backups.backup_destination_remove_confirm_button": "Remove",
        "backups.backup_destination_remove_confirm_title": "Remove this backup destination?",
        "backups.backup_destination_remove_reclaim_checkbox": "Also delete this device's copy now",
        "backups.backup_destination_reseed_button": "Restore my data to this nest",
        "backups.backup_destination_resolving": "Resolving destination…",
        "backups.backup_destination_unattested_mark": "Added before you recovered this account — still backing up. Keep it, or remove it if you don't recognise it.",
        "backups.backup_destination_url_placeholder": "Destination nest URL (https://…)",
        "backups.backup_destination_usage": "{held} of {cap} used",
        "backups.backup_destination_usage_cap_reached": "{held} of {cap} used — full, older copies are being dropped",
        "backups.backup_destination_usage_uncapped": "{held} held, no limit set",
        "backups.backup_destination_usage_unknown": "Nothing held yet",
        "backups.backup_destinations_desc": "Replicate your data to another nest you control. Chunks are sealed under your backup key — the destination never reads them.",
        "backups.backup_destinations_empty": "No backup destinations configured.",
        "backups.backup_destinations_full": "This box's backup list is full. Remove a destination or a covered folder, then try again.",
        "backups.backup_destinations_title": "Backup destinations",
        "backups.backup_now": "Backup Now",
        "backups.backup_orphaned_store_row": "This device is still holding {held} of a backup copy. No destination uses it any more.",
        "backups.backup_reclaim_after_remove_failed": "The destination was removed, but this device's copy could not be deleted: {reason}",
        "backups.backup_reclaim_cancel_button": "Keep it",
        "backups.backup_reclaim_confirm_body": "This device can currently restore your data on its own, with no nest reachable. Deleting the copy ends that. You can rebuild it by making this device a backup destination again.",
        "backups.backup_reclaim_confirm_button": "Delete the copy",
        "backups.backup_reclaim_confirm_title": "Delete this device's backup copy?",
        "backups.backup_reclaim_no_agent": "This device is not running a sync agent, so it holds no backup copy to delete.",
        "backups.backup_reclaim_still_hosting": "This device is still backing up right now, so nothing was deleted. Try again in a moment.",
        "backups.backup_reseed_cancel_button": "Not now",
        "backups.backup_reseed_confirm_body": "This copies the backup this device holds onto this nest and makes it live again. Nothing is deleted, here or on the nest. If this nest already holds your data, it stops rather than mixing the two.",
        "backups.backup_reseed_confirm_button": "Restore",
        "backups.backup_reseed_confirm_title": "Restore this device's copy to this nest?",
        "backups.backup_reseed_failed": "The restore stopped before anything was made live: {reason}",
        "backups.backup_reseed_no_agent": "This device runs no backup service, so it holds no copy to restore from.",
        "backups.backup_reseed_reenroll_failed": "Your data is back on this nest, but this device could not sign up again as its backup: {reason}. Add this device as a backup destination to keep a copy here.",
        "backups.backup_reseed_running": "Restoring your data to this nest…",
        "backups.backup_sole_client_destination_warning": "Every backup destination you have is one of your own devices. Devices get lost, wiped and replaced — add a nest destination so a copy lives somewhere else.",
        "backups.busy_check": "Checking integrity…",
        "backups.busy_create": "Creating a snapshot…",
        "backups.busy_delete": "Deleting a snapshot…",
        "backups.busy_immediate_delete": "Deleting a snapshot immediately…",
        "backups.busy_prune": "Applying the retention policy…",
        "backups.busy_refresh": "Loading…",
        "backups.busy_undelete": "Recovering a snapshot…",
        "backups.check_button": "Check integrity",
        "backups.check_integrity": "Check Integrity",
        "backups.check_result_errors": "Integrity check found problems: {missing_manifests} missing manifests, {missing_chunks} missing chunks, {corrupt_manifests} corrupt manifests.",
        "backups.check_result_ok": "Integrity check passed — {snapshots} snapshots, {files} files, {chunks} chunks verified.",
        "backups.choose_destination": "Choose Destination...",
        "backups.create_snapshot": "Create Snapshot",
        "backups.date": "Date",
        "backups.delete_snapshot": "Delete Snapshot",
        "backups.delete_snapshot_confirm": "Delete this snapshot? This action cannot be undone.",
        "backups.detail.created": "Created",
        "backups.detail.device_id": "Device ID",
        "backups.detail.file_count": "File Count",
        "backups.detail.none": "None",
        "backups.detail.parent_id": "Parent ID",
        "backups.detail.snapshot_id": "Snapshot ID",
        "backups.detail.tags": "Tags",
        "backups.detail.total_size": "Total Size",
        "backups.diff.added": "Added",
        "backups.diff.added_count": "+{count} added",
        "backups.diff.compare": "Compare",
        "backups.diff.compare_with": "Compare with:",
        "backups.diff.modified": "Modified",
        "backups.diff.modified_count": "~{count} modified",
        "backups.diff.net": "Net: {size}",
        "backups.diff.no_comparison": "No Comparison",
        "backups.diff.no_comparison_desc": "Select a snapshot and tap Compare to view differences.",
        "backups.diff.removed": "Removed",
        "backups.diff.removed_count": "-{count} removed",
        "backups.diff.select_snapshot": "Select snapshot...",
        "backups.diff.title": "Compare",
        "backups.download": "Download",
        "backups.error_check": "Failed to run the integrity check: {message}",
        "backups.error_create_snapshot": "Failed to create snapshot: {message}",
        "backups.error_delete_snapshot": "Failed to delete snapshot: {message}",
        "backups.error_delete_snapshot_immediate": "Failed to delete snapshot immediately: {message}",
        "backups.error_detail": "Failed to open the snapshot: {message}",
        "backups.error_download_manifest": "This file's content address could not be read, so it cannot be downloaded.",
        "backups.error_prune": "Failed to apply the retention policy: {message}",
        "backups.error_refresh": "Failed to load backups: {message}",
        "backups.error_undelete_snapshot": "Failed to recover snapshot: {message}",
        "backups.file": "File",
        "backups.file_count": "{count} files",
        "backups.folder": "Folder",
        "backups.folder_label": "Folder",
        "backups.immediate_delete_acknowledge_placeholder": "Acknowledgement phrase",
        "backups.immediate_delete_acknowledge_prompt": "Type this exact phrase to confirm:",
        "backups.immediate_delete_button": "Delete now",
        "backups.immediate_delete_cancel_button": "Cancel",
        "backups.immediate_delete_confirm_button": "Delete immediately",
        "backups.immediate_delete_confirm_id_placeholder": "Re-type the snapshot id to confirm",
        "backups.immediate_delete_modal_title": "Delete snapshot #{id} immediately?",
        "backups.immediate_delete_warning": "This permanently deletes the snapshot now, skipping the soft-delete window, and cannot be undone. At least three snapshots are always kept.",
        "backups.integrity_check.all_ok": "All OK",
        "backups.integrity_check.chunks_checked": "Chunks Checked",
        "backups.integrity_check.corrupt_manifests": "Corrupt Manifests",
        "backups.integrity_check.description": "Verify that all snapshots, manifests, and chunks are intact for \"{folder}\".",
        "backups.integrity_check.errors_found": "{count} errors found",
        "backups.integrity_check.files_checked": "Files Checked",
        "backups.integrity_check.issues": "Issues",
        "backups.integrity_check.manifests_checked": "Manifests Checked",
        "backups.integrity_check.missing_chunks": "Missing Chunks",
        "backups.integrity_check.missing_manifests": "Missing Manifests",
        "backups.integrity_check.options": "Options",
        "backups.integrity_check.results": "Results",
        "backups.integrity_check.snapshots_checked": "Snapshots Checked",
        "backups.integrity_check.start_check": "Start Check",
        "backups.integrity_check.title": "Backup Integrity Check",
        "backups.integrity_check.verify_content": "Verify content (slower, more thorough)",
        "backups.integrity_failed": "Integrity check found errors:",
        "backups.integrity_passed": "Integrity check passed.",
        "backups.last_backed_up": "Last backed up:",
        "backups.last_backed_up_at": "Last backed up: {when}",
        "backups.last_backed_up_never": "Last backed up: never",
        "backups.last_seq": "Last Seq: {seq}",
        "backups.loading_snapshots": "Loading snapshots...",
        "backups.no_backups": "No backups yet.",
        "backups.no_destination": "No destination selected",
        "backups.no_files_in_snapshot": "No files in this snapshot.",
        "backups.no_folders": "No folders yet — create one under Settings → Folders.",
        "backups.no_snapshots": "No snapshots yet.",
        "backups.no_snapshots_desc": "Snapshots are created when you back up files",
        "backups.notification.complete_body": "{folder}: {count} files ({size})",
        "backups.notification.complete_title": "Backup Complete",
        "backups.notification.failed_body": "{folder}: {error}",
        "backups.notification.failed_title": "Backup Failed",
        "backups.notification.view_backups": "View Backups",
        "backups.prune_button": "Apply retention policy",
        "backups.prune_cancel_button": "Cancel",
        "backups.prune_confirm": "Prune old snapshots? Only the latest 3 will be kept.",
        "backups.prune_execute_button": "Delete them",
        "backups.prune_policy_not_set": "No retention policy configured for this set. Set one on the Folders page.",
        "backups.prune_policy_unparseable": "This set's retention policy could not be read, so nothing was pruned. Re-set it on the Folders page.",
        "backups.prune_preview_candidate": "#{id} · {when}",
        "backups.prune_preview_counts": "{would_prune} would be deleted, {remaining} kept.",
        "backups.prune_preview_nothing": "Nothing to prune — every snapshot is within this set's retention policy.",
        "backups.prune_preview_title": "Retention policy preview",
        "backups.prune_result": "Pruned {count} snapshot(s).",
        "backups.prune_snapshots": "Prune Old Snapshots",
        "backups.repo_stats.chunk_sizes": "512 KB - 16 MB avg 4 MB",
        "backups.repo_stats.compression": "zstd level 3",
        "backups.repo_stats.dedup_ratio": "Dedup Ratio",
        "backups.repo_stats.encryption": "Encryption & Compression",
        "backups.repo_stats.encryption_algo": "ChaCha20-Poly1305",
        "backups.repo_stats.loading_stats": "Loading statistics...",
        "backups.repo_stats.raw_size": "Raw Size",
        "backups.repo_stats.storage_backend": "Storage Backend",
        "backups.repo_stats.stored_size": "Stored Size",
        "backups.repo_stats.title": "Repository Statistics",
        "backups.repo_stats.total_files": "Total Files",
        "backups.reseed_gap_sidecarless_segments": "{count} parts of your mail arrived without their index, so some mail is still missing. Run the restore again after this device's next backup.",
        "backups.reseed_gap_unnamed_files": "{count} files arrived without their names, so some files are still missing.",
        "backups.reseed_refused_custody_incomplete": "The copy on this nest is not complete yet. Run the restore again.",
        "backups.reseed_refused_custody_unsealed": "Some files arrived without their names. Run the restore again after this device's next backup.",
        "backups.reseed_refused_folder_unnamed": "This device does not know the folder's name, so it was left on the nest without being restored.",
        "backups.reseed_refused_not_enrolled": "This nest does not hold your backup key. Run the restore again.",
        "backups.reseed_refused_other": "The nest refused it.",
        "backups.reseed_refused_quota_exceeded": "This nest does not have enough storage for the copy. An admin can raise the limit in the admin app.",
        "backups.reseed_refused_rehome_unsigned": "This device could not sign the folder's files for the restore, so it was left on the nest without being restored. Sign in on this device again, then run the restore again.",
        "backups.reseed_refused_target_missing": "The folder to restore into could not be created on this nest. Run the restore again.",
        "backups.reseed_refused_target_not_fresh": "A folder with this name is already shared or published. Rename it or clear that setting, then run the restore again.",
        "backups.reseed_result_incomplete": "The restore is not complete yet. Run it again to finish what is missing.",
        "backups.reseed_result_whole": "Your data is back on this nest.",
        "backups.reseed_set_already_restored": "{set}: already restored",
        "backups.reseed_set_mail": "Mail",
        "backups.reseed_set_refused": "{set}: not restored. {remedy}",
        "backups.reseed_set_restored": "{set}: {count} restored",
        "backups.restore": "Restore",
        "backups.restore_about_detail": "Fetches every file in snapshot #{id}, decrypts it on this device, and writes it into the chosen directory.",
        "backups.restore_complete": "Restore Complete",
        "backups.restore_confirm_button": "Restore",
        "backups.restore_confirm_placeholder": "Re-type the snapshot id to confirm",
        "backups.restore_directory_panel_message": "Choose a directory to restore this snapshot into",
        "backups.restore_divergence_banner": "{count} MUAs reconnected with newer state",
        "backups.restore_divergence_close": "Close",
        "backups.restore_divergence_detail_row": "{collection} · {mua} · client modseq {client} / server modseq {server} · ~{lost} writes lost",
        "backups.restore_divergence_footer": "Lost writes cannot be recovered — they died with the source nest. This list is forensic.",
        "backups.restore_divergence_modal_title": "Restore divergence (forensic)",
        "backups.restore_divergence_unknown_mua": "(unknown)",
        "backups.restore_failed": "Restore Failed",
        "backups.restore_files_written": "Restored {count} files to {path}",
        "backups.restore_history_row": "{kinds} from {source} — {when}",
        "backups.restore_into_directory_desc": "Restores every file in this snapshot into the chosen directory.",
        "backups.restore_kinds_calendar": "calendar",
        "backups.restore_kinds_mail": "mail",
        "backups.restore_local_title": "Restore from a local snapshot",
        "backups.restore_no_destinations": "No backup destinations yet — add one under \"Backup destinations\" above.",
        "backups.restore_no_snapshots": "No local snapshots available to restore.",
        "backups.restore_progress_done": "Done — restart the bridge.",
        "backups.restore_progress_idle": "Select a snapshot and re-type its id to restore.",
        "backups.restore_progress_running": "Restoring…",
        "backups.restore_section_title": "Restore history",
        "backups.restore_snapshot_label": "Snapshot",
        "backups.restore_source_label": "Backup destination",
        "backups.restore_source_local": "local snapshot",
        "backups.restore_warning_config_absent": "Restored, but the bridge's sign-in keys were not part of the restore — the bridge can't sign in after it restarts until they are restored too.",
        "backups.retention.keep_daily": "Keep Daily",
        "backups.retention.keep_daily_count": "Keep Daily: {count}",
        "backups.retention.keep_last": "Keep Last",
        "backups.retention.keep_last_count": "Keep Last: {count}",
        "backups.retention.keep_monthly": "Keep Monthly",
        "backups.retention.keep_monthly_count": "Keep Monthly: {count}",
        "backups.retention.keep_weekly": "Keep Weekly",
        "backups.retention.keep_weekly_count": "Keep Weekly: {count}",
        "backups.retention.keep_yearly": "Keep Yearly",
        "backups.retention.keep_yearly_count": "Keep Yearly: {count}",
        "backups.retention.preview": "Preview",
        "backups.retention.prune_now": "Prune Now",
        "backups.retention.prune_preview": "Prune Preview",
        "backups.retention.would_keep": "Would Keep",
        "backups.retention.would_prune": "Would Prune",
        "backups.retention_and_prune": "Retention & Prune",
        "backups.retention_title": "Retention Policy — {name}",
        "backups.reveal_in_finder": "Reveal in Finder",
        "backups.select_snapshot": "Select a snapshot to view files.",
        "backups.snapshot": "Snapshot #{id}",
        "backups.snapshot_count": "{count} snapshots",
        "backups.snapshot_delete_button": "Delete",
        "backups.snapshot_integrity_implicated": "integrity problem found in this snapshot",
        "backups.snapshot_integrity_ok": "integrity verified",
        "backups.snapshot_row": "#{id} · {when} · {files} · {size}",
        "backups.snapshot_state_deletion_pending": "Deletion scheduled — cancel before {when}",
        "backups.snapshot_state_deletion_pending_undated": "Deletion scheduled",
        "backups.snapshot_state_soft_deleted": "Deleted — recoverable until {when}",
        "backups.snapshot_state_soft_deleted_undated": "Deleted — still recoverable",
        "backups.snapshot_title": "Snapshot",
        "backups.snapshot_undelete_button": "Recover",
        "backups.snapshots_title": "Snapshots",
        "backups.start_restore": "Start Restore",
        "backups.statistics": "Statistics",
        "backups.status_title": "Backup Status",
        "backups.sync_conflicts": "Sync Conflicts",
        "backups.title": "Backups",
        "backups.verify_integrity": "Verify Integrity",
        "bridges.add_follow": "Add Follow",
        "bridges.bridge_settings_desc": "Configure bridge-specific settings.",
        "bridges.detail_title": "Bridge",
        "bridges.follows": "Follows",
        "bridges.friendly_name_placeholder": "Friendly name",
        "bridges.handle": "Handle",
        "bridges.id": "ID",
        "bridges.id_to_follow": "ID to follow",
        "bridges.id_to_follow_placeholder": "e.g. did:plc:... or user.bsky.social",
        "bridges.link": "Link {name}",
        "bridges.link_action": "Link",
        "bridges.link_bridge": "Link Bridge",
        "bridges.link_method": "Link method",
        "bridges.link_status": "Link Status",
        "bridges.loading_bridges": "Loading bridges...",
        "bridges.mark_all_read": "Mark All Read",
        "bridges.no_bridges": "No bridges available.",
        "bridges.no_bridges_desc": "Bridges connect your nest to other networks.",
        "bridges.no_follows": "No follows yet.",
        "bridges.no_follows_configured": "No follows configured.",
        "bridges.no_link_method": "This bridge has no link method available right now.",
        "bridges.no_notifications": "No notifications",
        "bridges.no_settings": "No settings available.",
        "bridges.not_available": "{name} not available on this nest.",
        "bridges.not_available_node": "Not available on this node",
        "bridges.or": "or",
        "bridges.password": "Password",
        "bridges.petname": "Petname",
        "bridges.petname_optional": "Petname (optional)",
        "bridges.port": "Port",
        "bridges.refresh_list": "Refresh bridge list",
        "bridges.remove_follow": "Remove Follow",
        "bridges.select_bridge": "Select a bridge",
        "bridges.select_bridge_desc": "Select a bridge to view details.",
        "bridges.source_blocked": "This account can only add sources your guardian approves.",
        "bridges.source_request_approved": "Approved — try again",
        "bridges.source_request_button": "Ask your guardian",
        "bridges.source_request_pending": "Asked — waiting for your guardian",
        "bridges.status_unavailable": "Unavailable",
        "bridges.subscribe": "Subscribe",
        "bridges.title": "Bridges",
        "bridges.unlink": "Unlink {name}",
        "bridges.unlink_action": "Unlink",
        "bridges.unlink_bridge": "Unlink Bridge",
        "bridges.unlink_confirm_msg": "This will disconnect the bridge. You can re-link it later.",
        "bridges.unsafe_redirect": "This nest returned an unsafe link (links must be https). Not following it.",
        "c2pa.badge_label": "C2PA",
        "c2pa.image_viewer": "Image viewer",
        "c2pa.invalid_title": "C2PA provenance (validation issue)",
        "c2pa.provenance": "Content Provenance",
        "c2pa.signer": "Signer",
        "c2pa.tool": "Tool",
        "c2pa.valid": "Valid",
        "c2pa.validation_issue": "Validation issue",
        "c2pa.verified_title": "C2PA verified provenance",
        "c2pa.view_label": "View content provenance",
        "common.accept": "Accept",
        "common.accepted": "Accepted",
        "common.account": "Account",
        "common.actions": "Actions",
        "common.active": "Active",
        "common.actor_id": "Actor ID",
        "common.add": "Add",
        "common.admin": "Admin",
        "common.app_name": "Fauna",
        "common.apply": "Apply",
        "common.archive": "Archive",
        "common.available": "Available",
        "common.back": "Back",
        "common.block": "Block",
        "common.blocked": "Blocked",
        "common.bridges": "Bridges",
        "common.cancel": "Cancel",
        "common.cannot_connect": "Cannot connect",
        "common.change": "Change",
        "common.check": "Check",
        "common.checking": "Checking...",
        "common.clear_search": "Clear search",
        "common.close": "Close",
        "common.closed": "Closed",
        "common.confirm": "Confirm",
        "common.confirm_q": "Confirm?",
        "common.confirmed": "Confirmed",
        "common.connect": "Connect",
        "common.connected": "Connected",
        "common.connected_realtime": "Connected (real-time)",
        "common.connecting": "Connecting...",
        "common.contacts": "Contacts",
        "common.continue": "Continue",
        "common.copied": "Copied!",
        "common.copy": "Copy",
        "common.create": "Create",
        "common.creating": "Creating...",
        "common.danger_zone": "Danger Zone",
        "common.decline": "Decline",
        "common.delete": "Delete",
        "common.delete_folder": "Delete Folder",
        "common.device_id": "Device ID",
        "common.devices": "Devices",
        "common.dialog_already_open": "Close the open dialog first.",
        "common.disable": "Disable",
        "common.disabled": "Disabled",
        "common.disconnected": "Disconnected",
        "common.dismiss": "Dismiss",
        "common.domain": "Domain",
        "common.done": "Done",
        "common.download": "Download",
        "common.edit": "Edit",
        "common.enable": "Enable",
        "common.enabled": "Enabled",
        "common.error": "Error",
        "common.exit_fauna": "Exit Fauna",
        "common.export_action": "Export",
        "common.feed": "Feed",
        "common.files": "Files",
        "common.fmt_ellipsis": "{text}...",
        "common.fmt_exclamation": "{text}!",
        "common.fmt_question": "{text}?",
        "common.follow": "Follow",
        "common.handle": "Handle",
        "common.identity": "Identity",
        "common.identity_required": "Set up your identity in the Status tab first.",
        "common.inactive": "Inactive",
        "common.inbox": "Inbox",
        "common.leave": "Leave",
        "common.linked": "Linked",
        "common.linking": "Linking...",
        "common.load_failed": "Failed to load",
        "common.load_more": "Load More",
        "common.loading": "Loading...",
        "common.mark_all_read": "Mark All Read",
        "common.messages": "Messages",
        "common.mode": "Mode",
        "common.moderation": "Moderation",
        "common.more": "More",
        "common.mute": "Mute",
        "common.name": "Name",
        "common.navigation": "Navigation",
        "common.needs_nest": "Needs a connection to your nest",
        "common.needs_other_device": "Waiting for another of your devices: open the app on it, or remove it under Devices if it's gone",
        "common.never": "Never",
        "common.next": "Next",
        "common.no": "No",
        "common.no_folders_configured": "No folders configured.",
        "common.no_messages_yet": "No messages yet.",
        "common.no_notifications": "No notifications",
        "common.node_url": "Node URL",
        "common.not_connected": "Not connected",
        "common.not_found": "Not found",
        "common.not_linked": "Not linked",
        "common.notifications": "Notifications",
        "common.ok": "OK",
        "common.open": "Open",
        "common.path": "Path",
        "common.peers": "Peers",
        "common.pending": "Pending",
        "common.post": "Post",
        "common.posts": "Posts",
        "common.previous": "Previous",
        "common.provider": "Provider",
        "common.prune": "Prune",
        "common.quit": "Quit",
        "common.refresh": "Refresh",
        "common.refreshing": "Refreshing...",
        "common.remove": "Remove",
        "common.reply": "Reply",
        "common.reply_all": "Reply All",
        "common.replying_to": "Replying to",
        "common.retention_policy": "Retention Policy",
        "common.retry": "Retry",
        "common.save": "Save",
        "common.save_preferences": "Save Preferences",
        "common.saved": "Saved!",
        "common.saving": "Saving...",
        "common.search": "Search",
        "common.searching": "Searching...",
        "common.send": "Send",
        "common.sending": "Sending...",
        "common.settings": "Settings",
        "common.sign_in_required": "Sign in to access this feature.",
        "common.size": "Size",
        "common.snapshots": "Snapshots",
        "common.sort": "Sort",
        "common.starting": "Starting...",
        "common.status": "Status",
        "common.still_loading": "Still loading — try again in a moment",
        "common.storage": "Storage",
        "common.success_detail": "Success: {detail}",
        "common.sync": "Sync",
        "common.this_nest": "this nest",
        "common.tier": "Tier",
        "common.today": "Today",
        "common.toggle_sidebar": "Toggle sidebar",
        "common.type": "Type",
        "common.unfollow": "Unfollow",
        "common.unknown": "Unknown",
        "common.unlinking": "Unlinking...",
        "common.unread_count": "{count} unread",
        "common.upcoming": "Upcoming",
        "common.users": "Users",
        "common.users_by_tier": "Users by Tier",
        "common.value": "Value",
        "common.verified": "Verified",
        "common.verify": "Verify",
        "common.verifying": "Verifying...",
        "composer.new_post": "New Post",
        "composer.quote": "Quote",
        "composer.quote_post": "Quote Post",
        "conflicts.local": "Local",
        "conflicts.no_conflicts": "No sync conflicts.",
        "conflicts.resolve": "Resolve",
        "connected_apps.block": "Never show requests from this app",
        "connected_apps.blocked_heading": "Blocked apps",
        "connected_apps.blocked_hint": "Requests from these apps are never shown to you.",
        "connected_apps.blocked_since": "Blocked {time}",
        "connected_apps.class_app_password": "Signed in with an app password",
        "connected_apps.class_container": "Plugin on your nest",
        "connected_apps.class_device": "App on a device",
        "connected_apps.class_oauth": "Signed-in app",
        "connected_apps.class_remote": "Website or service",
        "connected_apps.class_signer": "Nostr signer app",
        "connected_apps.class_wasm": "Plugin on your nest",
        "connected_apps.connect_heading": "Connect an app",
        "connected_apps.connect_hint": "If an app on another device shows you a code, type it here.",
        "connected_apps.connect_placeholder": "Code from the app",
        "connected_apps.connect_submit": "Connect",
        "connected_apps.consent_ends_holder": "This app now uses a new key. Approving ends the access its earlier key was given.",
        "connected_apps.consent_ends_writer": "This app now signs with a new key. Approving ends its earlier key's permission to write.",
        "connected_apps.created": "Added {time}",
        "connected_apps.description": "Apps and services that act for you from outside Fauna. Each one can only reach what is listed under it, and you can disconnect any of them at any time.",
        "connected_apps.empty": "No connected apps yet.",
        "connected_apps.error_block": "Failed to block the app: {message}",
        "connected_apps.error_code_expired": "That code has expired — ask the app for a new one.",
        "connected_apps.error_handoff_expired": "That link has expired or was already used — start again from the app.",
        "connected_apps.error_refresh": "Failed to load connected apps: {message}",
        "connected_apps.error_request_gone": "That request is no longer waiting — it was answered or it expired.",
        "connected_apps.error_resolve": "Failed to answer the request: {message}",
        "connected_apps.error_revoke": "Failed to disconnect the app: {message}",
        "connected_apps.error_secret": "Could not read the secret: {message}",
        "connected_apps.last_used": "Last used {time}",
        "connected_apps.lasts_until": "Until {time}",
        "connected_apps.never_used": "Never used",
        "connected_apps.not_connected": "Not connected right now",
        "connected_apps.open_ended": "Until you disconnect it",
        "connected_apps.publisher": "From {domain}",
        "connected_apps.requests_heading": "Requests",
        "connected_apps.revoke": "Disconnect",
        "connected_apps.revoke_cancel": "Keep",
        "connected_apps.revoke_confirm": "Disconnect now",
        "connected_apps.revoke_prompt": "Disconnect {name}? It will no longer be able to act for you.",
        "connected_apps.roster_heading": "Your connected apps",
        "connected_apps.scope_mail": "Read and send your mail, and sync your calendar, contacts and files",
        "connected_apps.scope_nostr_sign": "Sign Nostr events with your key",
        "connected_apps.signer_pending": "Waiting to connect…",
        "connected_apps.title": "Connected apps",
        "connected_apps.unblock": "Allow requests again",
        "connected_apps.unnamed": "Unnamed app",
        "connected_apps.verbatim": "{text}",
        "contacts.add_contact": "Add Contact",
        "contacts.address_book.address": "Address",
        "contacts.address_book.card_not_found": "That contact is no longer in your address books.",
        "contacts.address_book.email": "Email",
        "contacts.address_book.no_addressbooks": "No address books yet.",
        "contacts.address_book.no_cards": "No contacts yet.",
        "contacts.address_book.note": "Note",
        "contacts.address_book.organization": "Organization",
        "contacts.address_book.phone": "Phone",
        "contacts.address_book.select_card": "Select a contact to view details.",
        "contacts.address_book.title": "Address Book",
        "contacts.ask_guardian": "Ask your guardian",
        "contacts.contact_request_pending": "Asked — waiting for your guardian",
        "contacts.detail_title": "Contact",
        "contacts.filter_placeholder": "Filter contacts...",
        "contacts.find_placeholder": "Find by handle...",
        "contacts.find_user.description": "Enter a handle (e.g. alice@fauna.social) or actor ID hex to find a user.",
        "contacts.find_user.find": "Find",
        "contacts.find_user.placeholder": "alice@fauna.social or actor ID hex",
        "contacts.find_user.title": "Find User",
        "contacts.guardian_approval_required": "This account can only message approved contacts.",
        "contacts.handle_not_found": "Handle not found",
        "contacts.handle_or_actor_id": "Handle, handle@domain, or actor ID...",
        "contacts.knock": "Knock",
        "contacts.knocks": "Knocks",
        "contacts.looking_up": "Looking up...",
        "contacts.looking_up_handle": "Looking up @{handle}@{domain}…",
        "contacts.message": "Message",
        "contacts.message_requests.count": "Message Requests ({count})",
        "contacts.message_requests.none": "No pending message requests.",
        "contacts.message_requests.title": "Message Requests",
        "contacts.no_contact_selected": "Select a contact to view details.",
        "contacts.no_contacts": "No contacts yet.",
        "contacts.no_matching_contacts": "No matching contacts.",
        "contacts.no_pending": "No pending requests.",
        "contacts.no_pending_knocks": "No pending knocks.",
        "contacts.pending_requests": "Pending Requests",
        "contacts.request_sent": "Contact request sent!",
        "contacts.search_results": "Search Results",
        "contacts.sent": "Sent",
        "contacts.sign_in_prompt": "Sign in to view your contacts.",
        "contacts.title": "Contacts",
        "contacts.unattested_mark": "Not reviewed since you recovered your account",
        "contacts.wants_to_connect": "wants to connect",
        "conversations.compose.body": "Body",
        "conversations.compose.encrypted": "Encrypted",
        "conversations.compose.new_message": "New Message",
        "conversations.compose.resolve": "Resolve",
        "conversations.compose.subject": "Subject",
        "conversations.compose.title": "Compose",
        "conversations.compose.to": "To",
        "conversations.compose.write_message": "Write your message...",
        "conversations.detail.add_reaction": "Add reaction",
        "conversations.detail.badge_c2pa": "C2PA content credentials present",
        "conversations.detail.badge_encrypted": "End-to-end encrypted",
        "conversations.detail.badge_signed": "Cryptographically signed",
        "conversations.detail.badge_verified": "Verified sender",
        "conversations.detail.delete_message": "Delete message",
        "conversations.detail.delete_message_confirm": "Delete",
        "conversations.detail.delete_message_confirm_title": "Delete message?",
        "conversations.detail.load_remote_content": "Load remote images",
        "conversations.detail.mailbox": "Mailbox",
        "conversations.detail.mark_as_spam": "Mark as spam",
        "conversations.detail.member_keep": "Keep",
        "conversations.detail.member_unattested_mark": "Was in this group before you recovered your account. Keep them, or remove them if you don't recognise them.",
        "conversations.detail.message_actions": "More actions",
        "conversations.detail.message_deleted": "This message was deleted",
        "conversations.detail.more_reactions": "More reactions",
        "conversations.detail.muted_reveal": "Show anyway",
        "conversations.detail.muted_word": "Muted word",
        "conversations.detail.no_messages": "No messages in this conversation.",
        "conversations.detail.no_subject": "(no subject)",
        "conversations.detail.remote_image_blocked": "Remote image blocked",
        "conversations.detail.report_message": "Report message",
        "conversations.detail.selected_message": "Your search result",
        "conversations.detail.title": "Conversation",
        "conversations.errors.mail_unopenable": "{count} received messages could not be opened on this device. They were sealed to mail keys this account no longer holds, and were skipped.",
        "conversations.errors.receive_stopped": "New messages stopped arriving because of an internal error. Restart the app (or reload the page) to receive them again.",
        "conversations.errors.served_elsewhere": "Conversations are open in another instance of this app. Use them there — everything else works here.",
        "conversations.list.new_conversation": "New Conversation",
        "conversations.list.no_conversations": "No conversations yet.",
        "conversations.list.search_placeholder": "Search conversations...",
        "conversations.list.select_conversation": "Select a conversation to view.",
        "conversations.list.select_conversation_short": "Select a conversation",
        "conversations.list.sort": "Sort",
        "conversations.list.title": "Conversations",
        "conversations.message.signed": "Signed",
        "conversations.unified.attachment_button": "Attach file",
        "conversations.unified.attachment_remove": "Remove attachment",
        "conversations.unified.bridged_no_recipient_key": "This account has no mail key yet, so a copy of the message cannot be kept. Set up mail, then send again.",
        "conversations.unified.bridged_one_recipient": "A conversation over this bridge is with one person. Remove the other recipients and send again.",
        "conversations.unified.error_add_participant": "Could not add them to this conversation: {message}",
        "conversations.unified.error_add_participant_after_heal": "their undelivered earlier invitation was removed first, so they are no longer in the group — adding them again starts cleanly. The re-invitation failed: {reason}",
        "conversations.unified.error_attachment_no_composer": "Open a message composer before attaching a file.",
        "conversations.unified.error_leave_room": "Could not leave this conversation: {message}",
        "conversations.unified.error_remove_participant": "Could not remove them from this conversation: {message}",
        "conversations.unified.error_rename_thread": "Could not rename this conversation: {message}",
        "conversations.unified.error_room_invitation": "Could not answer this invitation: {message}",
        "conversations.unified.error_send": "Could not send this message: {message}",
        "conversations.unified.error_set_room_policy": "Could not change this room's settings: {message}",
        "conversations.unified.error_withdraw_room_invite": "Could not withdraw this invitation: {message}",
        "conversations.unified.group_conversation_hint": "This will start a group conversation.",
        "conversations.unified.guardian_state_blocked": "Blocked by your guardian",
        "conversations.unified.guardian_state_held": "Waiting for your guardian",
        "conversations.unified.list_quota_approaching": "Approaching daily limit — {remaining} more recipients today",
        "conversations.unified.list_quota_over": "Sending to {count} recipients would pass today's limit of {limit} ({remaining} left). Try again tomorrow.",
        "conversations.unified.list_send_progress": "Sent to {list}: {delivered} of {count} recipients delivered",
        "conversations.unified.list_send_warning": "This will send to {count} subscribed recipients on {list}. Today's quota: {used} / {limit}.",
        "conversations.unified.list_send_warning_no_limit": "This will send to {count} subscribed recipients on {list}.",
        "conversations.unified.recipient_picker_bridges": "Also reaches people on: {bridges}",
        "conversations.unified.recipient_picker_placeholder": "Type a handle, email, npub, DID, or @user@instance",
        "conversations.unified.recipient_resolve_error": "Lookup failed — try again",
        "conversations.unified.recipient_resolve_not_found": "Not found — check the address",
        "conversations.unified.recipient_resolve_resolved": "Resolved",
        "conversations.unified.recipient_resolve_resolving": "Resolving…",
        "conversations.unified.reply_all": "Reply all",
        "conversations.unified.reply_recipient_add_placeholder": "Add recipient…",
        "conversations.unified.room_admin_no": "admin: no",
        "conversations.unified.room_admin_yes": "admin: yes",
        "conversations.unified.room_class_community": "Community — searched and labelled by the home nest",
        "conversations.unified.room_class_end_to_end": "End-to-end encrypted",
        "conversations.unified.room_class_transport_only": "Transport-only",
        "conversations.unified.room_history_policy_full": "The whole conversation",
        "conversations.unified.room_history_policy_label": "History for new members",
        "conversations.unified.room_history_policy_none": "Nothing before they join",
        "conversations.unified.room_home_nest_no": "Home nest joins: no",
        "conversations.unified.room_home_nest_yes": "Home nest joins: yes",
        "conversations.unified.room_invitation_admin": "{inviter} invited you to a room as an admin",
        "conversations.unified.room_invitation_member": "{inviter} invited you to a room",
        "conversations.unified.room_join_rule_invite": "Owner and admins",
        "conversations.unified.room_join_rule_label": "Who can invite",
        "conversations.unified.room_join_rule_member_invite": "Any member",
        "conversations.unified.room_labeler_off": "labels: off",
        "conversations.unified.room_labeler_on": "labels: on",
        "conversations.unified.room_labelers_label": "Labels the home nest adds to every message",
        "conversations.unified.room_leave": "Leave room",
        "conversations.unified.room_nest_read_no": "Home nest reads this room: no",
        "conversations.unified.room_nest_read_yes": "Home nest reads this room: yes",
        "conversations.unified.room_notice_awaiting_key": "Waiting for a room key — messages will appear once an owner or admin keys you in.",
        "conversations.unified.room_notice_moderation_unverified": "Some moderation in this room couldn't be verified on this device, so the affected messages are still shown.",
        "conversations.unified.room_pending_invite_admin": "{invitee} — invited by {inviter} as an admin",
        "conversations.unified.room_pending_invite_lapsed": "{sentence} (can no longer be accepted)",
        "conversations.unified.room_pending_invite_member": "{invitee} — invited by {inviter}",
        "conversations.unified.room_pending_invite_withdraw": "Withdraw",
        "conversations.unified.room_pending_invites_label": "Pending invitations",
        "conversations.unified.room_role_admin": "admin",
        "conversations.unified.room_role_owner": "owner",
        "conversations.unified.room_transfer_mark": "make owner",
        "conversations.unified.room_transfer_staged": "new owner",
        "conversations.unified.show_full_headers": "Show full headers",
        "conversations.unified.thread_add_participant": "Add someone…",
        "conversations.unified.thread_rename": "Rename",
        "conversations.unified.thread_rename_placeholder": "New name",
        "conversations.unified.thread_room_settings": "Room settings",
        "conversations.unified.to_line_label": "To:",
        "conversations.unified.topic_input_placeholder": "Topic (optional)",
        "conversations.unified.topic_toggle_add": "+ topic",
        "credential_store.confirm_label": "Confirm new passphrase",
        "credential_store.current_label": "Current passphrase",
        "credential_store.error_empty": "Enter your current passphrase and choose a new one",
        "credential_store.error_failed": "Could not change the passphrase: {message}",
        "credential_store.error_mismatch": "The two new entries don't match",
        "credential_store.error_wrong": "Wrong passphrase, or the store file is corrupt",
        "credential_store.new_label": "New passphrase",
        "credential_store.rekey_button": "Change passphrase…",
        "credential_store.rekey_title": "Change passphrase",
        "credential_store.section_title": "Credential store",
        "credential_store.seed_nudge": "Back up your recovery seed first (Identity export, above). The sealed store has no recovery path of its own — if the new passphrase is forgotten, the seed is the only way back into your account.",
        "credential_store.status_sealed": "Your sign-in keys rest in a file on this device, sealed under your passphrase.",
        "credential_store.submit": "Change passphrase",
        "credential_store.success": "Passphrase changed.",
        "critical_alerts.atproto_custody_mismatch": "Security alert: the published record of your AT Protocol identity {handle} names a recovery key this device does not hold. Your identity may not be under your control — do not trust it for anything sensitive. You may be able to undo this yourself from Settings → AT Protocol, and there is a time limit; you can also contact whoever runs your nest.",
        "critical_alerts.atproto_handle_unbound": "Security alert: the published record of your AT Protocol identity does not name a handle at {domain}. It is published as {published} instead — people looking for you may be finding someone else. Do not trust this identity for anything sensitive, and contact whoever runs your nest.",
        "critical_alerts.atproto_handle_unbound_none": "Security alert: the published record of your AT Protocol identity names no handle at all, so nobody can find you at {domain}. Do not trust this identity for anything sensitive, and contact whoever runs your nest.",
        "critical_alerts.domain_expired_admin": "Urgent: {domain} — the name this deployment runs on — has expired. Whoever registers it next receives your mail, including password resets for accounts tied to those addresses. Renew or redeem it at your registrar immediately; if it is gone, move the deployment to a new domain.",
        "critical_alerts.domain_expired_resident": "Urgent: {domain} — the name this deployment runs on — has expired. Mail sent to your address there may now reach someone else, and recovering your account from your recovery phrase alone will not work. Contact whoever runs this nest, and make sure you know its direct address.",
        "critical_alerts.domain_expiring_admin": "Urgent: {domain} — the name this deployment runs on — expires in {days} days. If it lapses, whoever registers it next receives your mail (including password resets for accounts tied to those addresses), and anyone recovering an account from their recovery phrase alone will no longer be able to find this nest. Renew it at your registrar now.",
        "critical_alerts.domain_expiring_resident": "Urgent: {domain} — the name this deployment runs on — expires in {days} days. If it lapses, mail sent to your address there will go to whoever registers the name next, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address.",
        "critical_alerts.domain_lapsing_admin": "Urgent: the registration for {domain} — the name this deployment runs on — is in a hold or deletion state ({status}) and is being withdrawn from DNS. Whoever registers it next receives your mail. Contact your registrar now; renewal is usually still possible at this stage.",
        "critical_alerts.domain_lapsing_resident": "Urgent: the registration for {domain} — the name this deployment runs on — is being withdrawn ({status}). Mail to your address there will stop arriving, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address.",
        "critical_alerts.recovery_replacement_pending": "Security alert: someone used your identity secret to request a replacement of your account recovery key. If that was not you, someone else has your identity secret. Cancel it in Settings, under Recovery kit.",
        "critical_alerts.recovery_replacement_pending_detail": "The replacement takes effect in {days} days unless cancelled. The pending recovery key begins {fingerprint} — if you hold a recovery kit and it does not begin with those characters, the request was not made with your kit.",
        "devices.access_revoked_warning": "The owner removed your permission to make changes, so this location is no longer syncing. Your local files are untouched.",
        "devices.add_folder": "Add Folder",
        "devices.audience_public_blocked_by_paywall": "Remove the paywall first — a paywalled folder cannot also be public to everyone.",
        "devices.audience_public_blocked_by_webdav": "Turn off WebDAV serving first — WebDAV needs the folder encrypted, and a public folder is not.",
        "devices.col_changes": "Changes",
        "devices.col_role": "Role",
        "devices.col_size": "Size",
        "devices.col_snapshots": "Snapshots",
        "devices.conflict_policy": "Conflict policy",
        "devices.conflict_policy_auto": "Auto (merge text, else latest wins)",
        "devices.conflict_policy_latest_wins": "Latest edit wins",
        "devices.conflicts.awaiting_device": "Awaiting device",
        "devices.conflicts.candidate_detail": "{device} · {size}",
        "devices.conflicts.col_file": "File",
        "devices.conflicts.col_time": "Time",
        "devices.conflicts.col_type": "Type",
        "devices.conflicts.delete_declined": "Delete declined",
        "devices.conflicts.device": "Device",
        "devices.conflicts.folder": "Folder",
        "devices.conflicts.keep_version": "Keep this version",
        "devices.conflicts.resolve": "Resolve",
        "devices.conflicts.resolved_latest_wins": "Latest kept",
        "devices.conflicts.resolved_merged": "Merged",
        "devices.conflicts.section_title": "Sync Conflicts",
        "devices.conflicts.title": "sync conflicts need attention",
        "devices.conflicts.type_binary": "Binary",
        "devices.conflicts.type_catchup_failed": "Not applied",
        "devices.conflicts.type_concurrent": "Concurrent edits",
        "devices.conflicts.type_merge": "Merge",
        "devices.conflicts.type_other": "Conflict",
        "devices.conflicts.unreadable_path": "(unreadable file name)",
        "devices.conflicts.use_other_version": "Use the other version",
        "devices.copy_actor_id": "Copy ID",
        "devices.custody_budget_label": "Keep at most (bytes)",
        "devices.custody_degraded_badge": "Degraded — some copies were dropped under the budget",
        "devices.custody_held_bytes": "Holding {held} of {cap}",
        "devices.custody_held_owner": "Holding for {owner}",
        "devices.custody_held_scope_account": "Their account's sealed planes — unreadable on this device",
        "devices.custody_held_section": "Held for others — sealed copies this device keeps",
        "devices.custody_holder_scope": "Trusted to hold sealed copies — cannot read them",
        "devices.custody_holder_section": "Custodians — who holds sealed copies of your data",
        "devices.custody_mint_button": "Ask a friend to hold sealed copies",
        "devices.custody_mint_confirm": "Send the request",
        "devices.custody_mint_floor": "Their device will store sealed copies it cannot read. They will see the shape of your data — which scopes exist, how much is stored, and when it changes — never the content. Choosing custodians is choosing who sees that shape.",
        "devices.custody_mint_host_label": "Who to ask",
        "devices.custody_mint_host_placeholder": "Choose a contact",
        "devices.custody_mint_no_contacts": "Start a conversation with them first — the request travels over it.",
        "devices.custody_offer_accept": "Hold for them",
        "devices.custody_offer_decline": "Decline",
        "devices.custody_offer_floor": "This device would store sealed data it cannot read. It would see only the shape: which scopes exist, how much is stored, and when it changes — never the content.",
        "devices.custody_offer_target_device": "This device",
        "devices.custody_offer_target_label": "Where to hold",
        "devices.custody_offer_target_nest": "My nest",
        "devices.custody_offer_title": "{owner} asks this device to hold sealed copies",
        "devices.custody_receipt_fresh": "Last confirmed {when}",
        "devices.custody_receipt_none": "No confirmation yet",
        "devices.custody_receipt_stale": "Stale — last confirmed {when}. Treat this copy as degraded.",
        "devices.custody_remove": "Remove and free the space",
        "devices.custody_remove_done": "Removed — the space is free.",
        "devices.custody_revoke": "Stop trusting this custodian",
        "devices.custody_revoke_bound_note": "Stops future copies and serving on honest devices. Copies already held stay held — and stay sealed forever.",
        "devices.custody_stop": "Stop holding",
        "devices.custody_stopped_bytes_remain": "Stopped — stored bytes remain until removed",
        "devices.declassify_body": "Anyone on the web will be able to read this folder's files, and its file and folder names too — they become part of the address of each file.",
        "devices.declassify_confirm": "Make public",
        "devices.declassify_irreversible": "Making it private again protects only files you add afterwards. Anything published while the folder is public should be treated as public for good.",
        "devices.declassify_title": "Make this folder public?",
        "devices.default_conflict_policy": "Default conflict policy for new sets",
        "devices.delete_confirm_body": "Delete folder \"{name}\"? All data including snapshots will be permanently removed.",
        "devices.delete_confirm_title": "Delete folder?",
        "devices.delete_folder": "Delete Folder",
        "devices.detail.capabilities": "Capabilities",
        "devices.detail.device_id": "Device ID",
        "devices.detail.device_info": "Device Info",
        "devices.detail.no_folders": "No folders assigned to this device.",
        "devices.detail.registered": "Registered",
        "devices.detail.remove_confirm_text": "Are you sure you want to remove \"{label}\"? This device will lose access to all folders.",
        "devices.detail.remove_device": "Remove Device",
        "devices.device_activity": "Device activity",
        "devices.enrolled_devices": "Enrolled Devices",
        "devices.enrolled_devices_count": "Enrolled Devices ({count})",
        "devices.error_accept_share": "Failed to accept share: {message}",
        "devices.error_bind_location": "Failed to sync this location: {message}",
        "devices.error_decline_share": "Failed to decline share: {message}",
        "devices.error_delete_folder": "Failed to delete folder: {message}",
        "devices.error_folder_destination": "Failed to change the folder's destination places: {message}",
        "devices.error_follow_failed": "Couldn't follow that folder: {message}",
        "devices.error_follow_not_found": "No public folder by that name for that person. Check the handle and the folder name.",
        "devices.error_leave_share": "Failed to leave share: {message}",
        "devices.error_other_version_needs_history": "That version was saved under a previous identity of this account. Restore it from the file's version history instead.",
        "devices.error_other_version_unverified": "That version is no longer in this file's verified history, so nothing was changed.",
        "devices.error_p2p_remote_enable": "Peer transfers can only be turned on from that device itself. From here you can only turn them off.",
        "devices.error_paywall_set": "Failed to paywall the folder: {message}",
        "devices.error_refresh": "Failed to load devices: {message}",
        "devices.error_remove_device": "Failed to remove device: {message}",
        "devices.error_remove_fleet_device": "Removed the device, but couldn't finish cleanup: {message}",
        "devices.error_remove_member": "Failed to remove member: {message}",
        "devices.error_remove_own_device": "This is the device you're using, so it wasn't removed. To remove it, sign out on this device.",
        "devices.error_remove_row_mismatch": "This entry doesn't match what the device itself reports, so nothing was removed. Trying again won't change that. If the device is not one you hold, remove it by its key from the signed-in devices without a matching entry, below the list.",
        "devices.error_remove_unverified_device": "Couldn't confirm which of your devices this is, so nothing was removed. Try again in a moment.",
        "devices.error_resolve_conflict": "Failed to resolve conflict: {message}",
        "devices.error_revoke_custody": "Couldn't stop trusting this custodian: {message}",
        "devices.error_save_paths": "Failed to save paths: {message}",
        "devices.error_serve_webdav": "Failed to change WebDAV serving: {message}",
        "devices.error_serve_website": "Failed to change website serving: {message}",
        "devices.error_set_audience": "Failed to change who can see this folder: {message}",
        "devices.error_set_conflict_policy": "Failed to set conflict policy: {message}",
        "devices.error_set_default_conflict_policy": "Failed to set the default conflict policy: {message}",
        "devices.error_set_exclusive_editing": "Failed to change exclusive editing for this folder: {message}",
        "devices.error_set_member_access": "Failed to change member access: {message}",
        "devices.error_set_nest_place": "Failed to save the snapshot settings: {message}",
        "devices.error_set_p2p_participation": "Couldn't change peer transfers: {message}",
        "devices.error_set_place": "Failed to change the device's place: {message}",
        "devices.error_set_residency": "Failed to change where this folder's content is kept: {message}",
        "devices.error_share_set": "Failed to share folder: {message}",
        "devices.error_unfollow_failed": "Couldn't remove that folder: {message}",
        "devices.error_use_other_version": "Failed to use the other version: {message}",
        "devices.exclude_paths": "Exclude Paths (comma-separated)",
        "devices.exclude_placeholder": "e.g. node_modules, .git",
        "devices.folder_audience": "Who can see this folder",
        "devices.folder_audience_hint": "Private folders are encrypted so only you can read them. Public folders are readable by anyone on the web.",
        "devices.folder_audience_private": "Private",
        "devices.folder_audience_public": "Public",
        "devices.folder_audience_public_bound_hint": "This folder is shared with other people and currently public. Pick Shared to re-seal it so only those people can read it. Anything published while it was public should still be treated as public.",
        "devices.folder_audience_reconfirm": "Confirm public",
        "devices.folder_audience_shared": "Shared",
        "devices.folder_audience_shared_hint": "This folder is shared with other people, so it can't be made private while it's shared. To make it private, remove the sharing in the section below first — or change who it is shared with there.",
        "devices.folder_audience_unattested": "This folder is public, but this app can't confirm that you made it public. Until you confirm again, up-to-date apps may keep its files encrypted, and your website may not show them.",
        "devices.folder_destination_attach": "Attach",
        "devices.folder_destination_attach_label": "Add a destination place",
        "devices.folder_destination_detach": "Detach",
        "devices.folder_destinations_title": "Destination places",
        "devices.folder_detail.delete_confirm_text": "Are you sure you want to delete \"{name}\"? All snapshots and membership data will be removed.",
        "devices.folder_detail.info": "Folder Info",
        "devices.folder_detail.rescan_interval": "Rescan Interval",
        "devices.folder_detail.total_size": "Total Size",
        "devices.folder_detail.update_schedule": "Update Schedule",
        "devices.folder_exclusive_editing": "One device at a time may edit this folder",
        "devices.folder_lease_free": "No device is editing this folder right now.",
        "devices.folder_lease_held_by": "{device} is editing this folder right now. Changes you make here are kept on this device and upload when it finishes.",
        "devices.folder_lease_held_elsewhere": "Another device is editing this folder right now. Changes you make here are kept on this device and upload when it finishes.",
        "devices.folder_lease_held_here": "This device is editing this folder right now. Your other devices keep their own changes and upload them when it finishes.",
        "devices.folder_places_title": "Device places",
        "devices.folder_residency": "Content kept on the nest",
        "devices.folder_residency_full": "Full — the nest keeps this folder's content",
        "devices.folder_residency_hint": "The nest keeps a copy of this folder's content, so a device can catch up while your other devices are offline.",
        "devices.folder_residency_metadata_only": "Metadata only — content stays on my devices",
        "devices.folder_residency_metadata_only_hint": "This folder's content stays on your devices only. It moves between them while one of them holding it is online, and the nest cannot restore it. File names, changes and snapshots still sync through the nest.",
        "devices.folders": "Folders",
        "devices.folders_count": "Folders ({count})",
        "devices.follow_confirm": "Follow",
        "devices.follow_folder_name": "Folder name",
        "devices.follow_folder_name_hint": "Public folders are named in the clear — type the name exactly as its owner published it.",
        "devices.follow_public_folder": "Follow a public folder",
        "devices.follow_public_folder_hint": "Read someone else's public folder from your own app. You need their handle and the folder's name.",
        "devices.followed_folders_section": "Folders you follow",
        "devices.followed_owner": "By {owner}",
        "devices.followed_public_badge": "Public",
        "devices.followed_status_following": "Following",
        "devices.followed_status_unavailable": "No longer available",
        "devices.followed_unavailable_hint": "Its owner stopped sharing it publicly, or removed it. If they publish it again, it will start working here.",
        "devices.guardian_marked_badge": "Guardian device",
        "devices.include_paths": "Include Paths (comma-separated)",
        "devices.keyless_posture_badge": "Relay only — holds no keys",
        "devices.last_seen": "Last seen",
        "devices.loading_members": "Loading members...",
        "devices.member_access": "Access",
        "devices.member_access_reader": "Reader",
        "devices.member_access_writer": "Writer",
        "devices.member_byte_cap": "Storage cap",
        "devices.member_byte_cap_placeholder": "No cap",
        "devices.member_enrolled_at": "Says it signed in {when}",
        "devices.member_fingerprint": "Device {fingerprint}",
        "devices.member_note": "Removing a device here is permanent: there is no undo, and it must sign in again from scratch. A listed device is not necessarily a problem — one that has signed in but not yet registered with the server appears here until it does. Before removing one, compare its fingerprint with the devices you still have: each shows its own on its Devices page, and the one to remove matches none of them. If two appear where you expected one, one of them is a device you hold.",
        "devices.member_remove_confirm": "Yes, remove it permanently",
        "devices.members_title": "Signed-in devices without a matching entry",
        "devices.my_devices": "My Devices",
        "devices.nest_place_blank_hint": "Leave a box empty to use the default. Empty is a choice — saving applies every box together.",
        "devices.nest_place_section": "What your nest keeps",
        "devices.nest_quiet": "Wait for quiet (seconds)",
        "devices.nest_retention_days": "Keep for at most (days)",
        "devices.nest_retention_snapshots": "Keep at most (snapshots)",
        "devices.nest_save": "Save Nest Settings",
        "devices.nest_snapshots": "Keep snapshots",
        "devices.nest_snapshots_default_label": "Use the default",
        "devices.nest_snapshots_off_label": "Don't keep snapshots",
        "devices.nest_snapshots_on_label": "Keep snapshots",
        "devices.no_device_activity": "No recorded activity yet.",
        "devices.no_devices": "No devices registered. Sign in to Fauna on a device to register it.",
        "devices.no_devices_enrolled": "No devices enrolled.",
        "devices.no_folders": "No folders created.",
        "devices.not_shared_yet": "Not shared with anyone yet.",
        "devices.offline": "Offline",
        "devices.on_demand_bound_hint": "This set syncs to its bound location; remove the binding to show it on demand instead.",
        "devices.online": "Online",
        "devices.own_fingerprint": "Key {fingerprint}",
        "devices.p2p_participation": "Peer transfers",
        "devices.p2p_participation_off_requested": "Peer transfers (turning off)",
        "devices.p2p_participation_own": "Peer transfers on this device",
        "devices.p2p_participation_unreported": "Peer transfers (not reported yet)",
        "devices.paths_placeholder": "e.g. Documents, Photos",
        "devices.paywall_blocked_by_public": "This folder is public, so a paywall would not restrict anyone. Make it private to paywall it.",
        "devices.paywall_tier": "Paywall to tier",
        "devices.paywall_tier_hint": "Only subscribers to the chosen tier can view this set's files; other visitors see a teaser.",
        "devices.paywall_tier_needs_tier": "Create a subscription tier first to paywall this set.",
        "devices.paywall_tier_none": "Not paywalled (public)",
        "devices.peers.connection": "Connection",
        "devices.peers.display_name": "Display Name",
        "devices.peers.lan_endpoints": "LAN Endpoints",
        "devices.peers.last_connected": "Last Connected",
        "devices.peers.last_endpoint": "Last Endpoint",
        "devices.peers.latency": "Latency",
        "devices.peers.no_peers": "No peers yet",
        "devices.peers.no_peers_desc": "Exchange QR codes to add P2P contacts.",
        "devices.peers.not_set": "Not set",
        "devices.peers.path_lan": "LAN",
        "devices.peers.path_relay": "Relay",
        "devices.peers.path_type": "Path Type",
        "devices.peers.path_wan_direct": "WAN Direct",
        "devices.peers.remove_peer": "Remove Peer",
        "devices.peers.select_peer": "Select a peer",
        "devices.peers.select_peer_desc": "Select a peer to view details.",
        "devices.peers.success_rate": "Success Rate",
        "devices.place_none": "Doesn't send or receive changes",
        "devices.place_three": "{first} · {second} · {third}",
        "devices.place_two": "{first} · {second}",
        "devices.remove_member": "Remove",
        "devices.residency_blocked_by_serving": "Turn off website serving, WebDAV serving and any paywall first — those serve this folder's content from the nest, which needs a copy of it.",
        "devices.residency_confirm": "Delete the nest's copy",
        "devices.residency_confirm_body": "The nest's copy of this folder's content is deleted now, and your devices become the only holders. Content moves between your devices only while one of them holding it is online. If your devices lose it, the nest cannot restore it.",
        "devices.residency_confirm_title": "Stop keeping this folder's content on the nest?",
        "devices.retention": "Retention",
        "devices.save_paths": "Save Paths",
        "devices.selective_sync": "Selective Sync",
        "devices.serve_webdav": "Serve over WebDAV",
        "devices.serve_webdav_blocked_by_public": "This folder is public, so there is nothing for WebDAV to encrypt. Make it private to serve it over WebDAV.",
        "devices.serve_webdav_hint": "Browse and edit this set from any WebDAV client (Finder, GNOME Files, rclone).",
        "devices.serve_webdav_needs_mail": "Set up mail first — serving over WebDAV uses your mail encryption key.",
        "devices.serve_website": "Serve this folder as your website",
        "devices.serve_website_address_off": "Your site is published from this folder, but your web address is switched off, so nobody can reach it yet. Switch it on in Settings → Web.",
        "devices.serve_website_hint": "Your site is published from this folder — an index.html here becomes your home page. Switch on your web address in Settings → Web to make it reachable.",
        "devices.serve_website_live": "Your site is served from this folder at your web address — an index.html here becomes your home page.",
        "devices.serve_website_needs_audience": "Make this folder public, or paywall it to a tier, for the site to be visible to visitors.",
        "devices.share_button": "Share…",
        "devices.shared_badge": "Shared · {count}",
        "devices.shared_by": "Shared by {who}",
        "devices.shared_with": "Shared with",
        "devices.shared_with_you": "Shared with you",
        "devices.show_on_demand_files": "Show in Files",
        "devices.show_on_demand_finder": "Show in Finder",
        "devices.show_on_demand_hint": "Browse this set on demand — files download when opened and can be freed again.",
        "devices.sync_defaults": "Sync defaults",
        "devices.sync_locations.add_location": "Add Location",
        "devices.sync_locations.browse": "Browse...",
        "devices.sync_locations.folder_placeholder": "Folder name",
        "devices.sync_locations.helper_unavailable": "Sync service is not running. Start it and try again.",
        "devices.sync_locations.location_placeholder": "Location path (or use Browse...)",
        "devices.sync_locations.no_locations": "No sync locations configured.",
        "devices.sync_locations.on_demand_label": "On-demand",
        "devices.sync_locations.on_demand_mount_failed": "On-demand couldn't start for this folder, so only files already on this device are kept in sync. Turn on-demand off to keep every file here.",
        "devices.sync_locations.on_demand_mount_refused": "On-demand can't be used in this location, so only files already on this device are kept in sync. Choose a folder inside your home folder, or turn on-demand off.",
        "devices.sync_locations.on_demand_needs_fuse3": "On-demand needs the fuse3 package. Install it, then restart Fauna.",
        "devices.sync_locations.on_demand_no_fuse_device": "On-demand isn't available on this system.",
        "devices.sync_locations.on_demand_unavailable": "On-demand isn't available here.",
        "devices.sync_locations.remove": "Remove",
        "devices.sync_locations.title": "Sync Locations",
        "devices.sync_locations.unbound": "(unbound)",
        "devices.this_device_badge": "This device",
        "devices.title": "Devices",
        "devices.unfollow_folder": "Remove",
        "devices.version_retention_count": "Keep at most (versions per file)",
        "devices.version_retention_days": "Keep versions for at most (days)",
        "devices.wizard.back": "Back",
        "devices.wizard.create": "Create",
        "devices.wizard.create_error": "Failed to create folder: {message}",
        "devices.wizard.create_member_error": "Folder created, but some devices could not be enrolled: {message}",
        "devices.wizard.creating_folder": "Creating folder...",
        "devices.wizard.failed_create": "Failed to create folder.",
        "devices.wizard.name_label": "Name",
        "devices.wizard.name_placeholder": "my-photos",
        "devices.wizard.name_required": "Enter a name to continue.",
        "devices.wizard.new_folder": "New Folder",
        "devices.wizard.next": "Next",
        "devices.wizard.no_devices_available": "No devices available. Register a device first.",
        "devices.wizard.place_accepts": "Receives changes from elsewhere",
        "devices.wizard.place_accepts_desc": "Changes made on your other devices land on this one.",
        "devices.wizard.place_applies_deletes": "Applies deletions",
        "devices.wizard.place_applies_deletes_desc": "When a file is deleted somewhere else, delete it here too. Leave this off and the device keeps every file forever — an archive.",
        "devices.wizard.place_originates": "Uploads what I change here",
        "devices.wizard.place_originates_desc": "Files you add or edit on this device are sent to the rest of the folder.",
        "devices.wizard.retention_days": "Keep days",
        "devices.wizard.retention_snapshots": "Keep snapshots",
        "devices.wizard.retention_summary": "{snapshots} snapshots, {days} days",
        "devices.wizard.review": "Review",
        "devices.wizard.review_devices": "Devices",
        "devices.wizard.review_name": "Name",
        "devices.wizard.review_no_devices": "None selected",
        "devices.wizard.review_retention": "Retention",
        "devices.wizard.schedule": "Schedule",
        "devices.wizard.select_devices": "Select Devices",
        "devices.wizard.select_devices_roles": "Select devices and choose what each one does (optional — skip to just store files here):",
        "devices.writer_paywalled_warning": "This folder is sold to subscribers, so this member can change what they see.",
        "devices.writer_public_warning": "This folder is public, so this member can change what anyone can see.",
        "devices.writer_uncapped_warning": "Without a cap, this member can use your entire storage quota.",
        "error.activitypub.not_linked": "This post came from the fediverse, and ActivityPub isn't enabled for your account yet. Enable it on the Bridges page first.",
        "error.activitypub.switched_off": "Your ActivityPub federation is switched off, so this can't be sent to the fediverse. Turn it back on from the Bridges page.",
        "error.authorization": "You do not have permission to do this.",
        "error.bluesky.not_linked": "This post came from Bluesky, and no Bluesky account is linked for you. Link one on the AT Protocol page first.",
        "error.bridges.address_refused": "None of your connected bridges can reach that address. Check how it is written.",
        "error.bridges.conversation_store_full": "This bridge has too many messages waiting, so this wasn't sent. Try again once it catches up.",
        "error.bridges.forward_target_on_local_domain": "That address is on your own server, so mail can't be forwarded to it. Add it as an alias instead.",
        "error.bridges.guardian_approval_required": "This account can only message approved contacts.",
        "error.bridges.over_quota": "Your storage is full, so this wasn't saved. Delete something to make room, or ask your nest admin for more space.",
        "error.bridges.recipient_on_local_domain": "That address is on your own server, so it can't be a list member. Add it as an alias instead.",
        "error.conversations.forbidden": "This person isn't accepting new conversations right now. You can send them a contact request instead.",
        "error.conversations.rate_limited": "This is happening too quickly. Please wait a moment and try again.",
        "error.email.no_handle": "You need to set a handle for your account before you can send email.",
        "error.email.permission_denied": "You can only send email from your own address. Check the account you are sending from.",
        "error.email.rate_limited": "You have reached your sending limit for now. Try again later.",
        "error.email.too_large": "This message is too large to send. Remove attachments or shorten it and try again.",
        "error.federation.peer_nest_outdated": "The other side's nest is running an outdated version and does not support this yet.",
        "error.nest.outdated": "This nest is running an outdated version and must be updated before you can connect.",
        "error.nest.schema_mismatch": "This nest's database does not match its software version and must be updated.",
        "error.nostr.no_custodial_key": "Your nest doesn't hold your Nostr key, so it can't sign this for you. Switch to a generated or imported key on the Nostr page.",
        "error.nostr.no_relays_configured": "You've removed every Nostr relay, so there is nowhere to publish this. Add a relay on the Nostr page first.",
        "error.nostr.not_linked": "This post came from Nostr, and no Nostr key is linked for you. Link one on the Nostr page first.",
        "error.nostr.replies_off": "Replies to Nostr are switched off, so this wasn't sent. Turn on Publish replies on the Nostr page.",
        "error.profile.handle_cooldown": "That handle was released recently and can't be taken yet. Choose a different one, or try again later.",
        "error.profile.handle_taken": "That handle already belongs to someone else on your nest. Choose a different one.",
        "error.protocol.cancelled": "The request was cancelled.",
        "error.protocol.disconnected": "The connection to the nest was lost.",
        "error.protocol.encode": "The nest sent a response that could not be read.",
        "error.protocol.internal": "The nest ran into an unexpected error. Please try again.",
        "error.protocol.malformed": "The request could not be processed.",
        "error.protocol.replay_too_large": "There is too much to catch up on at once. Please try again.",
        "error.protocol.timeout": "The nest took too long to respond. Please try again.",
        "error.protocol.unknown_kind": "This nest does not support that request. It may need to be updated.",
        "error.rate_limited": "This is happening too quickly. Please wait a moment and try again.",
        "error.send.attachment_missing": "Attach {filename} again — the file is not on this device.",
        "error.send.attachment_upload_foreign_failed": "The attachment couldn't be uploaded to this conversation's home server. Try sending it again.",
        "error.send.auth_required": "Sign in again to send this message.",
        "error.send.generic": "Something went wrong. Please try again.",
        "error.send.no_recipients": "There are no recipients to send this message to.",
        "error.send.not_supported": "This action is not available for this conversation.",
        "error.send.room_admins_owner_only": "Only the room's owner can appoint or demote admins.",
        "error.send.room_invite_not_permitted": "Only the room's owner and admins can invite people to this room.",
        "error.send.room_leave_failed": "Leaving this room did not go through. Try again.",
        "error.send.room_owner_cannot_leave": "Hand the room over to someone else before you leave — a room always has an owner.",
        "error.send.room_owner_not_removable": "The room's owner cannot be removed. Ownership has to be handed over first.",
        "error.send.room_policy_not_permitted": "Only the room's owner and admins can change this room's settings.",
        "error.send.room_policy_unavailable": "This room has no room settings, so there is nothing to change here.",
        "error.send.room_remove_not_permitted": "Only the room's owner and admins can remove people from this room.",
        "error.send.room_transfer_not_a_member": "The room can only be handed over to one of its other members.",
        "error.send.room_transfer_owner_only": "Only the room's owner can hand the room over.",
        "error.send.room_transfer_superseded": "The room's settings changed while the hand-over was waiting, so it did not go through. Hand the room over again.",
        "error.sync.device_limit_exceeded": "This account already has as many devices as its plan allows, so this device could not be added. Remove a device you no longer use under Settings → Devices, or ask your admin for a bigger tier.",
        "error.unexpected": "Something went wrong talking to the nest. Please try again.",
        "errors.api_error": "API error ({status}): {message}",
        "errors.auth_error": "Authentication error: {detail}",
        "errors.auth_failed": "Sign-in failed",
        "errors.bluesky_bridge_not_available": "Bluesky bridge not available on this nest.",
        "errors.calendar_requires_mail": "Calendar requires mail to be enabled",
        "errors.could_not_resolve_domain": "Could not resolve domain: {domain}",
        "errors.decode_error": "Decode error: {detail}",
        "errors.event_load_for_invite_failed": "Could not load the event to invite",
        "errors.feed_not_ready": "Feed isn't ready yet. Try again in a moment.",
        "errors.http_error": "HTTP error: {detail}",
        "errors.nest_identity_changed": "The identity of {host} has changed and could no longer be verified. For your safety, the connection was stopped.",
        "errors.nest_timeout": "Nest did not come online within the expected time",
        "errors.nest_unreachable": "Cannot connect to nest at the specified URL",
        "errors.no_calendar_selected": "No calendar selected",
        "errors.no_identity": "No identity configured",
        "errors.nostr_bunker_required": "Enter bunker URL",
        "errors.nostr_nsec_required": "Enter your nsec",
        "errors.not_connected_to_nest": "Not connected to your nest — no active session",
        "errors.photo_backup_not_configured": "Photo backup engine not configured. Connect to a nest first.",
        "errors.photos_access_denied": "Photos access denied. Grant access in System Settings > Privacy > Photos.",
        "errors.recovery_already_succeeded": "This identity has already been succeeded and cannot be recovered again.",
        "errors.recovery_crypto": "A cryptographic step failed: {detail}",
        "errors.recovery_group_ceremony": "Updating your groups failed: {detail}",
        "errors.recovery_identity_unreadable": "This session's identity secret is unreadable: {detail}",
        "errors.recovery_invalid_nonce": "That recovery request has expired or was already used. Please try again.",
        "errors.recovery_kit_not_current": "This is not your most recently created recovery kit. Enter your newest kit instead.",
        "errors.recovery_malformed": "The nest sent back something unexpected: {detail}",
        "errors.recovery_no_escrow": "No sealed copy of your identity secret is stored for this recovery kit, so it cannot recover this account right now.",
        "errors.recovery_not_registered": "No recovery key is registered for this identity, so there is nothing to recover from.",
        "errors.recovery_prior_escrow_unreadable": "Your existing recovery data could not be read ({reason}), so replacing it now would destroy it. Try again from a device and connection that can read it.",
        "errors.recovery_prior_kit_mismatch": "That kit does not match the one currently registered for this identity.",
        "errors.recovery_prior_kit_required": "A recovery key is already registered. Enter your current kit to replace it, or use the seed-alone replacement option instead.",
        "errors.recovery_signature_failed": "That recovery kit was refused — it may have already been replaced.",
        "errors.recovery_successor_exists": "That identity already has an account on this nest.",
        "errors.recovery_superseded": "This identity has already been succeeded by another one. Import your new identity to continue.",
        "errors.recovery_transport": "Connection error: {detail}",
        "errors.secret_key_invalid": "Secret key must be exactly 64 hexadecimal characters",
        "errors.snapshot_device_unknown": "This snapshot's owning device is unknown, so its files cannot be downloaded.",
        "errors.subprotocol_mismatch": "Your app is out of date and can't connect to this nest. Please update to continue.",
        "errors.websocket_error": "WebSocket error: {detail}",
        "events.all_day": "All day",
        "events.attendance": "Attendance",
        "events.attendance_mode": "Attendance Mode",
        "events.attendees": "Attendees",
        "events.attendees_count": "Attendees ({count})",
        "events.calendar": "Calendar",
        "events.calendar_exported": "Calendar exported to {path}",
        "events.calendar_name": "Calendar name",
        "events.calendars": "Calendars",
        "events.capacity": "Capacity (0 = unlimited)",
        "events.create_event": "Create Event",
        "events.delete_event": "Delete Event",
        "events.delete_event_confirm": "Are you sure you want to delete this event?",
        "events.deleting": "Deleting...",
        "events.description": "Description",
        "events.detail_title": "Event",
        "events.end": "End",
        "events.end_date": "End Date",
        "events.end_optional": "End (optional)",
        "events.error.create_calendar": "Failed to create calendar",
        "events.error.create_event": "Failed to create event",
        "events.error.delete_event": "Failed to delete event",
        "events.error.export": "Export failed",
        "events.error.import": "Import failed",
        "events.error.invite": "Failed to invite",
        "events.error.load_calendars": "Failed to load calendars",
        "events.error.load_events": "Failed to load events",
        "events.error.remove_reminder": "Failed to remove reminder",
        "events.error.rsvp": "RSVP failed",
        "events.error.set_reminder": "Failed to set reminder",
        "events.event_count": "{count} events",
        "events.event_count_one": "1 event",
        "events.event_not_found": "Event not found",
        "events.export_calendar_title": "Export Calendar as ICS",
        "events.export_ics": "Export .ics",
        "events.export_ics_tooltip": "Export a calendar as .ics file",
        "events.exporting": "Exporting...",
        "events.group_only": "Group Only",
        "events.ics_file_required": "Choose an .ics file first, then press Import.",
        "events.ics_path_required": "Type the path to an .ics file first, then press Import.",
        "events.import_calendar_title": "Import ICS Calendar",
        "events.import_ics": "Import .ics",
        "events.import_ics_tooltip": "Import events from .ics file",
        "events.import_result": "Imported: {imported}, Skipped: {skipped}, Total: {total}",
        "events.importing": "Importing...",
        "events.invalid_datetime": "Enter a date and time as YYYY-MM-DDTHH:MM.",
        "events.invite.button": "Invite",
        "events.invite.email_label": "Email",
        "events.invite.email_placeholder": "Attendee email",
        "events.invite.inviting": "Inviting...",
        "events.invite.title": "Invite Attendee",
        "events.invite_only": "Invite Only",
        "events.invited_events": "Invited Events",
        "events.link_code": "Link Code",
        "events.loading_events": "Loading events...",
        "events.location": "Location",
        "events.location_placeholder": "Add a location...",
        "events.mail_required": "Calendars and events require mail to be enabled. Enable mail in Settings to use the calendar.",
        "events.more_options": "More options...",
        "events.new_calendar": "New Calendar",
        "events.new_event": "New Event",
        "events.no_attendees": "No attendees yet.",
        "events.no_calendars": "No calendars yet.",
        "events.no_event_selected": "No event selected",
        "events.no_events": "No events",
        "events.no_events_in_calendar": "No events in this calendar.",
        "events.no_events_yet": "No events yet",
        "events.no_upcoming_events": "No upcoming events",
        "events.no_upcoming_events_desc": "Events from selected calendars will appear here.",
        "events.refused_changes.attempts": "Tried {count} times",
        "events.refused_changes.cancel_attempt": "Someone tried to cancel \"{title}\"",
        "events.refused_changes.dismiss": "Dismiss",
        "events.refused_changes.other_attempt": "Someone tried to change an event on your calendar",
        "events.refused_changes.reason.attendee_unresolvable": "The guest they answered for could not be confirmed.",
        "events.refused_changes.reason.no_attested_author": "Your nest could not confirm who sent the message.",
        "events.refused_changes.reason.not_the_attendee": "They are not the guest they answered for.",
        "events.refused_changes.reason.not_the_organizer": "They are not the organizer of this event.",
        "events.refused_changes.reason.organizer_changed": "The message tried to change who organizes the event.",
        "events.refused_changes.reason.organizer_unresolvable": "No one could be confirmed as this event's organizer.",
        "events.refused_changes.reason.other": "The change was refused.",
        "events.refused_changes.reason.sender_unauthenticated": "The message did not come from a confirmed sender.",
        "events.refused_changes.reason.spoofed_organizer": "The sender was not the organizer they claimed to be.",
        "events.refused_changes.reply_attempt": "Someone tried to answer for \"{title}\"",
        "events.refused_changes.sender": "Sent by {who}",
        "events.refused_changes.sender_via_nest": "Sent by {who}, according to {nest}",
        "events.refused_changes.title": "Refused changes",
        "events.refused_changes.unknown_sender": "Sender could not be identified",
        "events.refused_changes.update_attempt": "Someone tried to change \"{title}\"",
        "events.remind_me": "Remind me",
        "events.reminder.current": "Current:",
        "events.reminder.day_1": "1 day before",
        "events.reminder.hour_1": "1 hour before",
        "events.reminder.min_15": "15 min before",
        "events.reminder.select_placeholder": "Select…",
        "events.reminder.set": "Set",
        "events.reminder.title": "Reminder",
        "events.remove_reminder": "Remove Reminder",
        "events.rsvp.decline": "Decline",
        "events.rsvp.declined": "Declined",
        "events.rsvp.going": "Going",
        "events.rsvp.interested": "Interested",
        "events.rsvp.invited": "Invited",
        "events.rsvp.tentative": "Tentative",
        "events.rsvp.title": "RSVP",
        "events.rsvp.waitlisted": "Waitlisted",
        "events.select_calendar": "Select a calendar",
        "events.select_calendar_event_hint": "Select a calendar and event to view details.",
        "events.select_event_hint": "Select an event to see its details.",
        "events.send_invite": "Send Invite",
        "events.set_reminder": "Set Reminder",
        "events.setting": "Setting...",
        "events.start": "Start",
        "events.start_date": "Start Date",
        "events.summary": "Summary",
        "events.summary_placeholder": "New event",
        "events.time": "Time",
        "events.title": "Events",
        "events.view_agenda": "Agenda",
        "events.view_day": "Day",
        "events.view_month": "Month",
        "events.view_week": "Week",
        "events.your_status": "Your status:",
        "family.age_band.adult": "18+",
        "family.age_band.claim_line": "Age {band} · {provenance}",
        "family.age_band.claim_none": "No app age verification",
        "family.age_band.label": "Age band",
        "family.age_band.not_set": "Not set",
        "family.age_band.notice_attested": "Your age range ({band}) from {store} will be shared with this nest's admin, verified by {verifier}",
        "family.age_band.notice_declared": "Your age range ({band}) will be shared with this nest's admin as declared, not verified",
        "family.age_band.own_summary": "Your age band: {band} · {provenance}, set at admission",
        "family.age_band.own_summary_band_only": "Your age band: {band}, set at admission",
        "family.age_band.provenance_attested_android": "verified on Android",
        "family.age_band.provenance_attested_ios": "verified on iOS",
        "family.age_band.provenance_guardian_asserted": "set by guardian",
        "family.age_band.provenance_none": "declared, not verified",
        "family.age_band.store_android": "Google Play",
        "family.age_band.store_ios": "the App Store",
        "family.age_band.teen_13_15": "13–15",
        "family.age_band.teen_16_17": "16–17",
        "family.age_band.u13": "Under 13",
        "family.age_band.verifier_android": "Google",
        "family.age_band.verifier_ios": "Apple",
        "family.age_band.ward_line": "Age band: {band} · {provenance}",
        "family.age_band.ward_line_band_only": "Age band: {band}",
        "family.approval_no_sender": "No sender (delivery notice)",
        "family.approvals_heading": "Approvals",
        "family.approve": "Approve",
        "family.blocked_peer_allow": "Allow again",
        "family.blocked_peers_heading": "Denied message senders",
        "family.blocked_peers_hint": "People you denied for {handle}. Their new messages are refused; anything already delivered stays readable. Allowing lets them message again.",
        "family.contact_add_button": "Pre-approve contact",
        "family.contact_add_invalid_actor_id": "Not a valid actor ID (expected hex)",
        "family.contact_add_placeholder": "Actor ID (hex)",
        "family.content_blocked_notice": "Hidden by your family policy",
        "family.content_collapsed_notice": "Flagged content",
        "family.content_reveal_button": "Show anyway",
        "family.deny": "Deny",
        "family.device_mark_label": "Guardian device",
        "family.graduate_button": "Graduate to full account",
        "family.graduate_confirm_button": "Yes, graduate {handle}",
        "family.guardian_label": "Supervised by {guardian}",
        "family.incoming_transfer_accept_button": "Accept guardianship",
        "family.incoming_transfer_decline_button": "Decline",
        "family.incoming_transfer_text": "{guardian} asks you to take over supervision of {ward}",
        "family.incoming_transfers_heading": "Guardianship requests",
        "family.no_approvals": "No pending approvals.",
        "family.no_blocked_peers": "Nobody denied.",
        "family.no_ward_devices": "No devices registered yet.",
        "family.no_wards": "You are not supervising any accounts.",
        "family.policy_contact_approval_label": "Require my approval for new contacts",
        "family.policy_content_commercial_label": "Ads and promotions",
        "family.policy_content_notify_label": "Notify me about flagged content",
        "family.policy_content_nsfw_label": "Adult content",
        "family.policy_content_phishing_label": "Phishing and scams",
        "family.policy_content_spam_label": "Spam",
        "family.policy_federation_label": "Allow contact from other nests",
        "family.policy_feed_sources_caveat": "Blocks new external accounts and follows. Messages arriving through an already-connected account are governed by \"Unknown message senders\".",
        "family.policy_feed_sources_label": "New feed sources",
        "family.policy_save_button": "Save policy",
        "family.policy_screen_caveat": "Enforced by the apps on your child's devices. Leave a field empty to remove that limit.",
        "family.policy_screen_daily_minutes_label": "Daily limit (minutes, all devices)",
        "family.policy_screen_heading": "Screen time",
        "family.policy_screen_window_end_label": "Usable until (HH:MM)",
        "family.policy_screen_window_start_label": "Usable from (HH:MM)",
        "family.policy_summary_heading": "Current policy",
        "family.policy_unknown_peer_dm_label": "Unknown message senders",
        "family.policy_unknown_sender_label": "Unknown email senders",
        "family.screen_lock_budget": "You have used today's {minutes} minutes. Set by {guardian}.",
        "family.screen_lock_family_hint": "You can still open Family to see your settings.",
        "family.screen_lock_title": "Screen time is off",
        "family.screen_lock_window": "Your screen time starts again at {resumes}. Set by {guardian}.",
        "family.supervised_indicator": "This account is supervised by {guardian}",
        "family.supervised_notice_onboarding": "This account will be supervised by {guardian}",
        "family.title": "Family",
        "family.transfer_button": "Propose new guardian",
        "family.transfer_cancel_button": "Cancel proposal",
        "family.transfer_pending": "Waiting for {handle} to accept guardianship",
        "family.transfer_placeholder": "New guardian actor ID (hex)",
        "family.value_allow": "Allow",
        "family.value_block": "Block",
        "family.value_collapse": "Collapse",
        "family.value_hold": "Hold for review",
        "family.value_inherit": "Use my settings",
        "family.value_reject": "Reject",
        "family.ward_content_notice_count": "{count} flagged today",
        "family.ward_devices_heading": "Devices",
        "family.ward_devices_hint": "Mark the device you enrolled into this account. {handle} cannot remove a marked device, and graduating un-enrolls it automatically.",
        "family.ward_usage_today": "{minutes} minutes",
        "family.ward_usage_today_label": "Screen time today",
        "family.ward_usage_today_of_budget": "{used} of {budget} minutes",
        "family.wards_heading": "Accounts you supervise",
        "features.admin_edit": "Edit",
        "features.admin_section_desc": "Limits set here apply to every account on this nest. They can only tighten what Fauna and your region already allow.",
        "features.admin_section_title": "Feature limits for everyone",
        "features.authored_limited": "On, {count} limits",
        "features.authored_limited_one": "On, 1 limit",
        "features.authored_none": "No limit set",
        "features.authored_off": "Turned off",
        "features.authored_on": "On, no limits",
        "features.authored_unreadable": "This limit can't be read, so the feature is off until it's set again or removed.",
        "features.denied_by_admin": "Turned off by your nest admin.",
        "features.denied_by_guardian": "Turned off by your guardian.",
        "features.denied_by_other": "Turned off by another rule-setter.",
        "features.denied_by_region": "Turned off by your region's rules.",
        "features.denied_by_self": "You turned this off.",
        "features.denied_by_structural": "Turned off by Fauna's built-in limits.",
        "features.dimension_counterparties": "People",
        "features.dimension_operations": "Uses",
        "features.dimension_volume": "Amount",
        "features.editor_cancel": "Cancel",
        "features.editor_hint": "Leave a box empty for no limit. Zero is a limit.",
        "features.editor_invalid_count": "\"{value}\" isn't a whole number. Type one, or leave the box empty.",
        "features.editor_invalid_sats": "\"{value}\" isn't a whole number of sats. Type one, or leave the box empty.",
        "features.editor_invalid_size": "\"{value}\" isn't a size. Try something like 50 GB, or leave the box empty.",
        "features.editor_load_failed": "Could not load the limits: {error}",
        "features.editor_off": "Off",
        "features.editor_on": "On",
        "features.editor_per_operation_bytes": "Largest single item (for example 2 GB)",
        "features.editor_per_operation_sats": "Largest single payment, in sats",
        "features.editor_remove": "Remove limit",
        "features.editor_removed": "Limit removed.",
        "features.editor_save": "Save",
        "features.editor_save_failed": "Could not save the limit: {error}",
        "features.editor_saved": "Saved.",
        "features.editor_title_admin": "{feature}: limits for everyone on this nest",
        "features.editor_title_guardian": "{feature}: limits only for {ward}",
        "features.editor_title_self": "{feature}: limits only for you",
        "features.editor_volume_label_bytes": "Amount per {window} (for example 50 GB)",
        "features.editor_volume_label_sats": "Amount per {window}, in sats",
        "features.editor_ward_missing": "This account is no longer in your family, so the limit was not saved.",
        "features.empty": "No feature limits apply on this nest.",
        "features.exhausted_admin": "You've used up the limit your nest admin set for this {window}.",
        "features.exhausted_guardian": "You've used up the limit your guardian set for this {window}.",
        "features.exhausted_other": "You've used up the limit another rule-setter set for this {window}.",
        "features.exhausted_region": "You've used up the limit your region's rules set for this {window}.",
        "features.exhausted_self": "You've used up the limit you set for this {window}.",
        "features.exhausted_structural": "You've used up Fauna's built-in limit for this {window}.",
        "features.guardian_section_desc": "Limits set here apply only to this child. They can only tighten what already applies.",
        "features.guardian_section_title": "Feature limits",
        "features.magnitude_sats": "{value} sats",
        "features.name_other": "A feature this app does not know yet",
        "features.name_p2p_share": "File sharing",
        "features.name_payments": "Payments",
        "features.name_zaps": "Zaps",
        "features.no_effect_admin": "No effect: your nest admin already limits this to {limit}.",
        "features.no_effect_guardian": "No effect: your guardian already limits this to {limit}.",
        "features.no_effect_other": "No effect: another rule-setter already limits this to {limit}.",
        "features.no_effect_region": "No effect: your region's rules already limit this to {limit}.",
        "features.no_effect_self": "No effect: your own setting already limits this to {limit}.",
        "features.no_effect_structural": "No effect: Fauna's built-in limit is already {limit}.",
        "features.own_edit": "Set your own limit",
        "features.own_label": "Your own limit",
        "features.quota_label": "{dimension} per {window}",
        "features.quota_value": "{remaining} left of {limit}",
        "features.quota_value_exhausted": "none left of {limit}",
        "features.section_title": "Feature limits",
        "features.status_available": "Available",
        "features.status_restricted": "Restricted",
        "features.tier_admin": "Your nest admin",
        "features.tier_guardian": "Your guardian",
        "features.tier_other": "Another rule-setter",
        "features.tier_region": "Your region's rules",
        "features.tier_self": "Your own setting",
        "features.tier_structural": "Fauna's built-in limits",
        "features.window_day": "day",
        "features.window_month": "month",
        "features.window_week": "week",
        "feed.bridge_form.kind": "Bridge",
        "feed.bridge_form.name": "Display Name",
        "feed.bridge_form.uri": "Feed URI",
        "feed.compose_attachment_missing": "Attach {filename} again — the file is not on this device.",
        "feed.compose_attachment_stale": "Attach the file again — the audience changed after it was prepared",
        "feed.compose_empty": "Post cannot be empty",
        "feed.compose_gate_no_key": "This device does not hold the key for tier {tier}",
        "feed.compose_gate_preview_empty": "Add a public teaser for a gated post",
        "feed.compose_room_no_key": "This device does not hold the key for that room yet",
        "feed.compose_sell_price_invalid": "Enter a smaller price",
        "feed.compose_sell_rank_unavailable": "Could not check your tiers to price this post — try again",
        "feed.create.add_factor": "Add Factor",
        "feed.create.add_rule": "Add Rule",
        "feed.create.combination": "Combination",
        "feed.create.factor_engagement": "Engagement",
        "feed.create.factor_global_toggle": "Apply to all feeds",
        "feed.create.factor_trending": "Trending",
        "feed.create.factor_weight_placeholder": "1.0",
        "feed.create.factors": "Factors",
        "feed.create.feed_name": "Feed Name",
        "feed.create.filter_rules": "Filter Rules",
        "feed.create.mode_all": "All match",
        "feed.create.mode_any": "Any match",
        "feed.create.name_placeholder": "My Feed",
        "feed.create.rule_category": "Category",
        "feed.create.rule_excluded": "Excluded",
        "feed.create.rule_required": "Required",
        "feed.create.rule_threshold": "Threshold (0-10)",
        "feed.create.rule_threshold_short": "0-10",
        "feed.create.rule_value_placeholder": "tag1, tag2",
        "feed.create.title": "New Feed",
        "feed.delegated_origin": "Via connected app",
        "feed.delegated_origin_tooltip": "A connected app wrote this post as you, using the access you granted it. Manage or revoke that access on the AT Protocol settings page.",
        "feed.delete_post": "Delete post",
        "feed.delete_post_confirm": "Delete",
        "feed.delete_post_confirm_title": "Delete post?",
        "feed.error_buy_unlock": "Could not buy this post: {message}",
        "feed.error_delete": "Failed to delete post: {message}",
        "feed.error_feeds": "Failed to load feeds: {message}",
        "feed.error_gated_unlock": "Could not unseal this post: {message}",
        "feed.error_load": "Failed to load posts: {message}",
        "feed.error_muted_keywords": "Your muted words are not being applied: {message}",
        "feed.error_submit": "Failed to post: {message}",
        "feed.error_subscribe": "Failed to subscribe: {message}",
        "feed.error_subscribed_model": "A subscribed community model could not be applied: {message}",
        "feed.error_train": "Could not train on this post: {message}",
        "feed.error_trained_factor": "This feed's trained topic could not be applied: {message}",
        "feed.less_like_this": "Less like this",
        "feed.like_tooltip": "Like",
        "feed.list.bridge_feeds": "Bridge Feeds",
        "feed.list.delete_feed": "Delete feed",
        "feed.list.end_of_feed": "End of feed",
        "feed.list.no_matching_posts": "No matching posts.",
        "feed.list.no_posts": "No posts yet.",
        "feed.list.subscribe_bridge": "Subscribe to Bridge Feed",
        "feed.list.title": "Feeds",
        "feed.list.trending": "Trending",
        "feed.list.unsubscribe": "Unsubscribe",
        "feed.more_like_this": "More like this",
        "feed.post.add_comment": "Add your comment...",
        "feed.post.attach_image": "Attach Image",
        "feed.post.buy_button": "Buy",
        "feed.post.compose": "Compose",
        "feed.post.compose_drop_hint": "Compose post — drop files here to attach",
        "feed.post.compose_post": "Compose Post",
        "feed.post.create_tooltip": "Create Feed",
        "feed.post.gate_audience": "Audience",
        "feed.post.gate_preview_placeholder": "Public teaser shown to non-subscribers...",
        "feed.post.gate_public": "Public",
        "feed.post.gate_room": "Room: {room}",
        "feed.post.gate_sell": "Sell this post…",
        "feed.post.gated_badge_room_tooltip": "Room members only: {room}",
        "feed.post.gated_badge_tooltip": "Subscribers only: {tier}",
        "feed.post.has_media": "[media]",
        "feed.post.no_bridge_feeds": "No bridge feeds available.",
        "feed.post.no_feeds_configured": "No feeds configured.",
        "feed.post.no_thread_data": "No thread data",
        "feed.post.no_thread_desc": "Could not load thread data.",
        "feed.post.open_rich_compose": "Open rich compose dialog",
        "feed.post.post_detail": "Post detail",
        "feed.post.post_not_found": "Post not found",
        "feed.post.posting": "Posting…",
        "feed.post.reply_audience_public": "This post is for a smaller audience — a reply from you would be public",
        "feed.post.reply_audience_room": "Reply goes to Room: {room}",
        "feed.post.reply_audience_tier": "Reply goes to your tier {tier}",
        "feed.post.reply_public_confirm": "Post my reply publicly",
        "feed.post.replying_to_user": "Replying to {user}",
        "feed.post.repost": "Repost",
        "feed.post.reposted_marker": "reposted",
        "feed.post.search_placeholder": "Search this feed...",
        "feed.post.sell_asking_price_placeholder": "Machine price in sats (optional)",
        "feed.post.sell_price_placeholder": "Price, e.g. $3",
        "feed.post.sell_subscribers_free": "Subscribers get it free",
        "feed.post.tags_placeholder": "Tags (comma-separated)",
        "feed.post.thread": "Thread",
        "feed.post.view_thread": "View Thread",
        "feed.post.whats_on_your_mind": "What's on your mind?",
        "feed.post.write_post": "Write a post...",
        "feed.post.write_reply": "Write a reply...",
        "feed.post_actions_tooltip": "More actions",
        "feed.post_muted_placeholder": "Muted word",
        "feed.post_muted_reveal": "Show anyway",
        "feed.post_type.classified": "Listing",
        "feed.post_type.community": "Community",
        "feed.quote": "Quote",
        "feed.reference_restricted": "This post is for a smaller audience, and your reply would be public, so it was not sent",
        "feed.report_post": "Report post",
        "feed.rule_chip.body_contains": "contains: {value}",
        "feed.rule_chip.body_excludes": "excludes: {value}",
        "feed.rule_chip.created_after": "age < {hours}h",
        "feed.rule_chip.has_hashtag": "{tags}",
        "feed.rule_chip.has_media_no": "media: no",
        "feed.rule_chip.has_media_yes": "media: yes",
        "feed.rule_chip.is_reply_no": "reply: no",
        "feed.rule_chip.is_reply_yes": "reply: yes",
        "feed.rule_chip.label_above": "label above: {value}",
        "feed.rule_chip.label_below": "label below: {value}",
        "feed.rule_chip.min_replies": "replies >= {count}",
        "feed.rule_chip.min_reposts": "reposts >= {count}",
        "feed.rule_chip.source": "source: {value}",
        "feed.rule_types.body_contains": "Body contains",
        "feed.rule_types.body_excludes": "Body Excludes",
        "feed.rule_types.created_after": "Created After",
        "feed.rule_types.has_hashtag": "Has Hashtag",
        "feed.rule_types.has_media": "Has Media",
        "feed.rule_types.is_reply": "Is Reply",
        "feed.rule_types.label_above": "Label Above (show only)",
        "feed.rule_types.label_below": "Label Below (exclude spam)",
        "feed.rule_types.min_replies": "Min Replies",
        "feed.rule_types.min_reposts": "Min Reposts",
        "feed.rule_types.source": "Protocol Source",
        "feed.train_target_title": "Train which topic?",
        "feed.unverified_source": "Unverified",
        "feed.unverified_source_tooltip": "This device could not verify the author's signature on this post.",
        "feed.watch": "Watch",
        "file_context_menu.devices_count": "Synced to {count} devices",
        "file_context_menu.devices_none": "Not synced to any device",
        "file_context_menu.devices_one": "On this device only",
        "file_context_menu.info_unavailable": "File info unavailable",
        "file_context_menu.keep_on_device": "Always keep on this device",
        "file_context_menu.make_on_demand": "Make available on-demand",
        "file_context_menu.share": "Share",
        "file_context_menu.share_not_available": "This item can't be shared from here",
        "file_context_menu.share_open_failed": "Couldn't open Fauna to share this item",
        "file_context_menu.version_history": "Version history",
        "file_context_menu.version_restore_failed": "Could not restore version.",
        "file_context_menu.version_restore_failed_detail": "Could not restore version: {message}",
        "file_context_menu.version_restored": "Version restored.",
        "file_context_menu.versions_count": "{count} saved versions",
        "file_context_menu.versions_count_dated": "{count} saved versions, latest {date}",
        "file_context_menu.versions_none": "No saved versions",
        "file_context_menu.versions_one": "1 saved version",
        "file_context_menu.versions_one_dated": "1 saved version ({date})",
        "file_sync.info": "Info",
        "file_sync.new_folder": "New Folder",
        "file_sync.saf_root_summary": "Synced files",
        "folders.apply_deletes": "Apply held deletions ({count})",
        "folders.bind_location": "Bind Location",
        "folders.choose": "Choose…",
        "folders.deletes_held": "This folder looks empty. Deletions held: {count}. Reconnect the folder, or apply them to your nest.",
        "folders.error_offline_share": "Sharing did not finish: {message}",
        "folders.keep_syncing_help_off": "The sync agent only runs while you are logged in. Turn this on to keep it syncing on this machine after you disconnect.",
        "folders.keep_syncing_help_on": "The sync agent keeps running on this machine after you log out.",
        "folders.keep_syncing_when_logged_out": "Keep syncing when logged out",
        "folders.location_path_placeholder": "Location path",
        "folders.no_locations_bound": "No locations bound to this set.",
        "folders.offline_share_begin": "Begin sharing",
        "folders.offline_share_code_malformed": "That does not look like a code. It should be 64 letters and numbers.",
        "folders.offline_share_code_own": "That is this device's own code — type the other person's.",
        "folders.offline_share_expect": "Ready to receive",
        "folders.offline_share_from": "{who} wants to share a folder with you ({code})",
        "folders.offline_share_own_code_help": "Give this to the person next to you — read it out, or let them read it off your screen — and check that what they type back matches. It ends with where your device can be reached, so copy all of it. Never send it in a message: exchanging it in person is what makes it safe.",
        "folders.offline_share_own_code_label": "Your code",
        "folders.offline_share_peer_code_label": "Their code",
        "folders.offline_share_receive": "Receive a folder",
        "folders.offline_share_section": "Share with someone next to you",
        "folders.offline_share_set": "Shared folder {code}",
        "folders.offline_share_start": "Share a folder",
        "folders.offline_share_status_admitted": "Joined",
        "folders.offline_share_status_awaiting_consent": "Waiting for them to accept…",
        "folders.offline_share_status_delivered": "Shared",
        "folders.offline_share_status_delivering": "Setting up the shared folder…",
        "folders.offline_share_status_expecting": "Waiting for their invitation…",
        "folders.offline_share_status_failed": "Did not finish",
        "folders.offline_share_status_idle": "Not started",
        "folders.offline_share_status_offer_sent": "Invitation sent",
        "folders.photo_library_section": "Photo Library",
        "folders.share_serve_status_no_sets": "No shared folders to serve",
        "folders.share_serve_status_off": "Peer transfers are off — this nest does not enable them",
        "folders.share_serve_status_participation_off": "Peer transfers are off on this device",
        "folders.share_serve_status_serving": "Serving {count} shared folder(s) to members",
        "folders.share_transfer_peer_row": "{folder} — {who}",
        "folders.share_transfer_progress": "{files} file(s), {rows} change(s) this pass",
        "folders.share_transfer_section": "Peer transfers",
        "folders.share_transfer_source_free_space": "free space on this device",
        "folders.share_transfer_state_admission_pending": "Waiting to be admitted",
        "folders.share_transfer_state_limited": "Limited by {source}",
        "folders.share_transfer_state_pulling": "Receiving",
        "folders.share_transfer_state_up_to_date": "Up to date",
        "folders.synced_locations": "Synced Locations",
        "folders.title": "Folders",
        "folders.unreadable": "Fauna couldn't read {count} items in this folder, so it has stopped syncing them. Check that the drive is connected and that Fauna can open the folder.",
        "groups.cancel_reply": "Cancel reply",
        "groups.create_group": "Create Group",
        "groups.create_group_hint": "Create a group to start messaging.",
        "groups.demote": "Demote",
        "groups.group": "Group",
        "groups.group_chat": "Group Chat",
        "groups.group_members": "Group Members",
        "groups.group_name": "Group name",
        "groups.invite": "Invite",
        "groups.invite_member": "Invite Member",
        "groups.invite_member_title": "Invite Member — {name}",
        "groups.invite_nest_url_placeholder": "Nest URL (optional, for cross-nest)",
        "groups.invite_placeholder": "alice@fauna.social or actor ID",
        "groups.invite_to_group": "Invite to Group",
        "groups.invitee": "Invitee",
        "groups.loading_group": "Loading group...",
        "groups.make_admin": "Make Admin",
        "groups.mark_spam": "Mark Spam",
        "groups.member_role": "Member",
        "groups.members": "Members",
        "groups.members_title": "Members — {name}",
        "groups.message.encrypted": "Encrypted",
        "groups.message.encrypted_title": "End-to-end encrypted via MLS",
        "groups.message.signed": "Signed",
        "groups.message.signed_title": "Sender signature verified — content is plaintext in the nest",
        "groups.message_placeholder": "Type a message...",
        "groups.mute_group": "Mute Group",
        "groups.my_groups": "My Groups",
        "groups.new_group": "New Group",
        "groups.no_channel": "No encrypted channel for this group",
        "groups.no_groups": "No groups yet.",
        "groups.no_members": "No members yet.",
        "groups.node_url_placeholder": "Node URL (for cross-nest invites)",
        "groups.not_spam": "Not Spam",
        "groups.owner": "Owner",
        "groups.react": "React",
        "groups.replying_to": "Replying to",
        "groups.replying_to_user": "Replying to {user}",
        "groups.role": "Role",
        "groups.select_group": "Select a group to view.",
        "groups.select_prompt": "Select or create a group to get started.",
        "groups.title": "Groups",
        "groups.view_threaded": "Threaded",
        "labeler_catalog.close_inspect": "Close",
        "labeler_catalog.empty": "No community labelers published yet.",
        "labeler_catalog.error_inspect": "Failed to inspect this labeler: {message}",
        "labeler_catalog.error_refresh": "Failed to load community labelers: {message}",
        "labeler_catalog.error_subscribe": "Failed to subscribe: {message}",
        "labeler_catalog.error_unsubscribe": "Failed to unsubscribe: {message}",
        "labeler_catalog.inspect": "Inspect",
        "labeler_catalog.kind_needs_newer_app": "needs a newer app",
        "labeler_catalog.list_entry_count": "{count} entries",
        "labeler_catalog.list_name": "List name: {name}",
        "labeler_catalog.model_name": "Model name: {name}",
        "labeler_catalog.model_ngram_count": "{count} word patterns",
        "labeler_catalog.subscribe": "Subscribe",
        "labeler_catalog.subscribed_without_mail": "Subscribed, but this labeler cannot run over your mail until mail is set up for your account.",
        "labeler_catalog.subscribed_without_mail_holder": "Subscribed, but this labeler cannot run over your mail yet: this nest has no mail service to trust with it.",
        "labeler_catalog.title": "Community labelers",
        "labeler_catalog.unnamed_list": "Unnamed list",
        "labeler_catalog.unnamed_model": "Unnamed model",
        "labeler_catalog.unsubscribe": "Unsubscribe",
        "launch.identity_changed_title": "This nest's identity changed",
        "launch.needs_update_title": "Update this app to continue",
        "launch.recovery_custody_failed": "Off-box recovery custody wasn't saved — couldn't reach the box to confirm. Your nest still works, but it isn't protected against total box loss yet.",
        "launch.recovery_custody_mismatch": "Off-box recovery isn't protected: this box handed off an inconsistent recovery key.",
        "launch.retry_button": "Try again",
        "launch.retry_title": "Couldn't reach your nest",
        "launch.signing_in": "Signing you in…",
        "launch.use_different_nest": "Use a different nest",
        "linked_nests.title": "Linked nests",
        "linked_nests.unlink": "Unlink",
        "logs.clear_button": "Clear",
        "logs.copy_button": "Copy",
        "logs.description": "Recent activity recorded on this device, newest first. No message contents or secrets are ever logged — only what happened, when, and where.",
        "logs.empty": "No log entries yet.",
        "logs.filter_all": "All",
        "logs.filter_label": "Severity",
        "logs.level_debug": "Debug",
        "logs.level_error": "Error",
        "logs.level_info": "Info",
        "logs.level_trace": "Trace",
        "logs.level_warn": "Warn",
        "logs.title": "Logs",
        "mail_aliases.active_toggle_label": "Active",
        "mail_aliases.active_toggle_tooltip": "When on, this address receives mail. Turn off to bounce mail to it without deleting the address — you can turn it back on anytime.",
        "mail_aliases.add_button": "Add alias",
        "mail_aliases.cancel": "Cancel",
        "mail_aliases.copied": "Copied address",
        "mail_aliases.delete": "Delete",
        "mail_aliases.description": "Extra mail addresses that all deliver to you — share a different one with each service so you can see who leaked your address and turn any of them off.",
        "mail_aliases.disabled_badge": "disabled",
        "mail_aliases.edit": "Edit",
        "mail_aliases.empty": "No aliases yet",
        "mail_aliases.form_title": "Add alias",
        "mail_aliases.generate_button": "Generate disposable",
        "mail_aliases.hits": "{count} hits",
        "mail_aliases.hits_with_last": "{count} hits · last {date}",
        "mail_aliases.import_button": "Import addresses",
        "mail_aliases.import_cancel": "Cancel",
        "mail_aliases.import_invalid_line": "{address} — {reason}",
        "mail_aliases.import_placeholder": "One address per line",
        "mail_aliases.import_result": "{created} created · {existed} already existed · {invalid} invalid",
        "mail_aliases.import_submit": "Import",
        "mail_aliases.import_subtitle": "Paste one address per line. Each becomes an exact alias that delivers to you; addresses you already have are skipped.",
        "mail_aliases.import_title": "Import addresses",
        "mail_aliases.kind_catchall": "Catch-all",
        "mail_aliases.kind_disposable": "Disposable",
        "mail_aliases.kind_exact": "Exact",
        "mail_aliases.kind_forwarder": "Forwarder",
        "mail_aliases.kind_other": "Alias",
        "mail_aliases.kind_subaddress": "+suffix",
        "mail_aliases.kind_wildcard": "Wildcard",
        "mail_aliases.kind_wildcard_label": "Wildcard prefix (matches anything starting with it)",
        "mail_aliases.label_placeholder": "Label (optional)",
        "mail_aliases.loading": "Loading your aliases…",
        "mail_aliases.no_default_domain": "Enable mail before adding aliases.",
        "mail_aliases.pattern_placeholder": "Address (e.g. shop, news-)",
        "mail_aliases.primary_address_badge": "Primary address",
        "mail_aliases.primary_address_tooltip": "Your main address and sign-in identity. It can't be disabled, renamed, or deleted so your mail and login always work.",
        "mail_aliases.rate_per_hour_placeholder": "Rate limit per hour (optional)",
        "mail_aliases.revoke": "Revoke",
        "mail_aliases.show_audit": "Show audit",
        "mail_aliases.spam_threshold_placeholder": "Spam threshold override (0–15, optional)",
        "mail_aliases.submit": "Add",
        "mail_aliases.title": "Aliases",
        "mail_aliases.ttl_placeholder": "Disposable lifetime in days (optional)",
        "mail_aliases.uses_placeholder": "Disposable max uses (optional)",
        "mail_export.back": "Back",
        "mail_export.backend_unbuilt": "Mailbox export is not available on this nest yet.",
        "mail_export.cancel_button": "Cancel",
        "mail_export.confirm_pending": "Estimate unavailable until the export backend is ready.",
        "mail_export.confirm_summary_fmt": "{format} · {mailboxes} mailbox(es)",
        "mail_export.confirm_title": "Step 3 — Confirm",
        "mail_export.description": "Download your whole mailbox in a standard format you can import into another mail app. The export is encrypted until you download it.",
        "mail_export.discard_button": "Discard now",
        "mail_export.done_summary_fmt": "{format} · {bytes} bytes",
        "mail_export.done_title": "Step 5 — Done",
        "mail_export.download_button": "Download export",
        "mail_export.download_url_label": "Download link (for another device)",
        "mail_export.error_log_title": "Skipped / errored messages",
        "mail_export.format_eml": "EML zip (one .eml per message + manifest)",
        "mail_export.format_maildir": "Maildir++ (one file per message; preserves flags)",
        "mail_export.format_mbox": "mbox (one file per mailbox — broadest support)",
        "mail_export.format_title": "Step 1 — Format",
        "mail_export.next": "Next",
        "mail_export.pause_button": "Pause",
        "mail_export.progress_summary_fmt": "{exported} of {total} · {skipped} skipped · {errored} errored",
        "mail_export.progress_title": "Step 4 — Exporting",
        "mail_export.resume_button": "Resume",
        "mail_export.saved_summary_fmt": "{format} · {bytes} bytes · saved to {path}",
        "mail_export.scope_date_from_placeholder": "From date (optional, YYYY-MM-DD)",
        "mail_export.scope_date_to_placeholder": "To date (optional, YYYY-MM-DD)",
        "mail_export.scope_mailboxes_empty": "No mailboxes to export.",
        "mail_export.scope_mailboxes_label": "Mailboxes",
        "mail_export.scope_strip_headers_label": "Strip transit headers",
        "mail_export.scope_strip_headers_subtitle": "Removes the headers mail servers add in transit — relay hops, server names and IP addresses. Off keeps full forensic fidelity.",
        "mail_export.scope_title": "Step 2 — What to include",
        "mail_export.start_button": "Start export",
        "mail_export.title": "Export mailbox",
        "mail_import.back": "Back",
        "mail_import.cancel_button": "Cancel",
        "mail_import.confirm_summary_fmt": "{source} · {mailboxes} mailbox(es) · {messages} messages",
        "mail_import.confirm_title": "Step 3 — Confirm",
        "mail_import.connect_button": "Connect",
        "mail_import.description": "Pull your existing mail from Gmail, Outlook, iCloud, or any IMAP server into your Fauna mailbox. Your credentials never leave this device.",
        "mail_import.done_summary_fmt": "{imported} imported · {skipped} skipped · {errored} errored",
        "mail_import.done_title": "Step 5 — Done",
        "mail_import.error_log_title": "Skipped / errored messages",
        "mail_import.next": "Next",
        "mail_import.pause_button": "Pause",
        "mail_import.progress_row_fmt": "{count} messages",
        "mail_import.progress_summary_fmt": "{imported} of {total} · {skipped} skipped · {errored} errored",
        "mail_import.progress_title": "Step 4 — Importing",
        "mail_import.resume_button": "Resume",
        "mail_import.review_skipped_button": "Review skipped",
        "mail_import.scope_date_from_placeholder": "From date (optional, YYYY-MM-DD)",
        "mail_import.scope_mailbox_mapping_label": "Mailboxes map 1:1 by name — INBOX to INBOX, Sent to Sent, and so on. Mailboxes with no matching Fauna mailbox are created, named after the source.",
        "mail_import.scope_mailboxes_empty": "No mailboxes found on the source server.",
        "mail_import.scope_mailboxes_label": "Mailboxes",
        "mail_import.scope_max_size_label": "Max message size (MB)",
        "mail_import.scope_title": "Step 2 — What to import",
        "mail_import.source_app_password_help_gmail": "Requires 2FA on your Google account. Generate one at myaccount.google.com/apppasswords and paste it here.",
        "mail_import.source_app_password_help_icloud": "Requires 2FA on your Apple ID. Generate one at appleid.apple.com and paste it here.",
        "mail_import.source_app_password_label": "App password",
        "mail_import.source_generic": "Generic IMAP",
        "mail_import.source_gmail": "Gmail",
        "mail_import.source_host_placeholder": "Server hostname",
        "mail_import.source_icloud": "iCloud",
        "mail_import.source_oauth_button": "Connect with Microsoft",
        "mail_import.source_outlook": "Outlook / Hotmail / Office365",
        "mail_import.source_password_placeholder": "Password",
        "mail_import.source_port_placeholder": "Port (default 993)",
        "mail_import.source_title": "Step 1 — Source",
        "mail_import.source_unavailable": "Importing from another mail server isn't available in the browser yet. You can start an import from the desktop or terminal app, and watch or pause it here.",
        "mail_import.source_username_placeholder": "Username",
        "mail_import.start_button": "Start import",
        "mail_import.title": "Import mailbox",
        "mail_import.tls_implicit": "Implicit TLS (993)",
        "mail_import.tls_starttls": "STARTTLS (143)",
        "mail_import.view_imported_button": "View imported messages",
        "mail_lists.add_button": "Add list",
        "mail_lists.add_member_button": "Add member",
        "mail_lists.add_member_cancel": "Cancel",
        "mail_lists.add_member_placeholder": "Email address",
        "mail_lists.add_member_submit": "Add",
        "mail_lists.archive_off_server_confirm": "This archive link is not on your server and goes out with every message. Save anyway?",
        "mail_lists.backend_unbuilt": "Mailing lists are not available on this nest yet.",
        "mail_lists.cancel": "Cancel",
        "mail_lists.delete": "Delete",
        "mail_lists.delete_confirm": "Delete the list and all its members?",
        "mail_lists.description": "Run a newsletter or mailing list from your own address, with one-click unsubscribe built in.",
        "mail_lists.description_placeholder": "Description (optional)",
        "mail_lists.domain_label": "Domain",
        "mail_lists.edit": "Edit",
        "mail_lists.empty": "No lists yet",
        "mail_lists.form_title": "Add list",
        "mail_lists.import_button": "Import",
        "mail_lists.import_cancel": "Cancel",
        "mail_lists.import_placeholder": "One email address per line",
        "mail_lists.import_result": "{added} added · {existed} already subscribed · {invalid} invalid",
        "mail_lists.import_submit": "Import",
        "mail_lists.list_archive_placeholder": "List-Archive URL (optional)",
        "mail_lists.list_help_placeholder": "List-Help URL (optional)",
        "mail_lists.loading": "Loading your lists…",
        "mail_lists.local_part_placeholder": "Address (e.g. newsletter)",
        "mail_lists.members": "Members",
        "mail_lists.members_loading": "Loading members…",
        "mail_lists.members_no_list": "Open a list from the Lists page to manage its members.",
        "mail_lists.members_title": "Members",
        "mail_lists.name_placeholder": "List name (e.g. Bob's Weekly)",
        "mail_lists.no_domain": "Add a mail domain before creating lists.",
        "mail_lists.per_send_placeholder": "Recipients per send (optional)",
        "mail_lists.resubscribe": "Resubscribe",
        "mail_lists.status_subscribed": "Subscribed",
        "mail_lists.status_unsubscribed": "Unsubscribed",
        "mail_lists.submit": "Add",
        "mail_lists.summary_fmt": "{subscribed} subscribed · {unsubscribed} unsubscribed",
        "mail_lists.title": "Lists",
        "mail_lists.unsubscribe": "Unsubscribe",
        "mail_settings.credentials_on_connected_apps": "Your app passwords are listed under Settings → Connected apps — copy, reveal or disconnect each one there.",
        "mail_settings.forward_all_to_invalid": "That doesn't look like an email address.",
        "mail_settings.forward_all_to_label": "Forward all incoming mail to",
        "mail_settings.forward_all_to_subtitle": "Every message you receive is also sent on to this address, and you keep your own copy. Clear the field to stop forwarding.",
        "mail_settings.forward_per_hour_above_ceiling": "The hourly forwarding limit can't be more than {ceiling}.",
        "mail_settings.forward_per_hour_label": "Hourly forwarding limit",
        "mail_settings.forward_per_hour_not_a_number": "Enter the hourly forwarding limit as a whole number.",
        "mail_settings.forward_per_hour_subtitle": "The most messages forwarded for you in one hour, up to {ceiling}. Past the limit, forwards wait for the next hour.",
        "mail_settings.forward_per_hour_zero": "The hourly forwarding limit must be at least 1.",
        "mail_settings.forwarding_title": "Forwarding",
        "mail_settings.keys_info": "Your mail is protected by an encryption key held on your Fauna devices. Each app password or token you add unlocks that same key for one email app (Thunderbird, Apple Mail, …). Rotate your keys if a password or token may have leaked, or if a device that had your mail set up was lost or stolen — rotating replaces the key so the exposed credential can no longer read your mail (messages you've already received stay readable). If you're just retiring an app you no longer use, revoke that one credential instead — you don't need to rotate.",
        "mail_settings.serve_here_label": "Serve my mail & calendar over IMAP/CalDAV on this nest",
        "mail_settings.serve_here_subtitle": "When on, this nest answers IMAP and CalDAV for your mailbox so email and calendar apps can connect here. Turn it off if you read your mail on a different nest — your own Fauna apps are unaffected either way.",
        "mail_settings.title": "Mail & Calendar",
        "mail_spam.backend_unbuilt": "Spam-classifier training is not available on this nest yet.",
        "mail_spam.contribute_baseline_label": "Contribute to deployment spam baseline",
        "mail_spam.contribute_baseline_subtitle": "Off by default. When on, your training helps seed the shared filter new accounts start from — your individual messages are never shared.",
        "mail_spam.description": "Your spam filter learns from what you mark as spam or not-spam. Reset its training, choose whether to help the deployment's shared filter, and undo any past training here.",
        "mail_spam.empty": "No training history yet",
        "mail_spam.history_title": "Training history",
        "mail_spam.label_ham": "Not spam",
        "mail_spam.label_spam": "Spam",
        "mail_spam.label_unknown": "Other",
        "mail_spam.published_description": "The anonymized report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 reporters.",
        "mail_spam.published_empty": "This nest publishes no report aggregates yet",
        "mail_spam.published_reporters": "reporters",
        "mail_spam.published_title": "What this nest publishes",
        "mail_spam.reset_button": "Reset spam classifier",
        "mail_spam.reset_confirm": "Reset for good? This cannot be undone.",
        "mail_spam.reset_subtitle": "Deletes your spam-training model and history. Future mail starts from scratch. This cannot be undone.",
        "mail_spam.share_reports_label": "Share spam reports (anonymized)",
        "mail_spam.share_reports_subtitle": "Off by default. When on, the fact that you flagged a message as spam joins an anonymized count your nest shares — but only once at least 3 people here have flagged the same content, and never your identity or the message itself.",
        "mail_spam.source_explicit_button": "Fauna app",
        "mail_spam.source_imap_junk_flag": "Junk flag",
        "mail_spam.source_imap_junk_move": "Junk move",
        "mail_spam.source_unknown": "Other",
        "mail_spam.threshold_override_label": "Spam-folder threshold override (0–15, optional)",
        "mail_spam.threshold_override_subtitle": "Messages scoring at or above this override are filed to Junk, instead of the deployment default. 0 turns automatic filing off for this account; leave blank to follow the default.",
        "mail_spam.title": "Spam",
        "mail_spam.undo": "Undo",
        "markdown.bold": "Bold",
        "markdown.code": "Code",
        "markdown.heading": "Heading",
        "markdown.italic": "Italic",
        "markdown.link": "Link",
        "markdown.list": "List",
        "markdown.list_item": "List item",
        "markdown.toggle_markers": "Show/hide markdown markers",
        "media.add_file": "Add File",
        "media.choose_file": "Choose file…",
        "media.detail_close": "Close",
        "media.download": "Download",
        "media.error_delete": "Failed to delete: {message}",
        "media.error_download": "Failed to download: {message}",
        "media.error_external_open": "Failed to open externally: {message}",
        "media.error_followed_fetch": "Couldn't read that followed folder: {message}",
        "media.error_followed_read_only": "You follow this folder — it's read-only here. Switch to one of your own folders to upload.",
        "media.error_followed_unavailable": "This folder is no longer shared publicly. Its owner may have stopped sharing it, or removed it.",
        "media.error_metadata_only_folder": "This folder's content stays on your devices, so it can't be uploaded here. Put the file in the folder on a device that syncs it.",
        "media.error_no_set": "You don't have a folder to upload into yet. Create a folder under Settings → Folders first.",
        "media.error_refresh": "Failed to load media: {message}",
        "media.error_restore": "Failed to restore version: {message}",
        "media.error_undelete": "Failed to recover version: {message}",
        "media.error_upload": "Failed to upload: {message}",
        "media.external_open": "Open externally",
        "media.external_open_cancel": "Cancel",
        "media.external_open_confirm": "Open",
        "media.external_open_confirm_body": "The clip is decrypted to a private temporary file and handed to your system's media player.",
        "media.external_open_confirm_title": "Open \"{name}\" externally?",
        "media.file_detail.delete_confirm": "Are you sure you want to delete \"{name}\"? This cannot be undone.",
        "media.file_detail.delete_confirm_button": "Delete",
        "media.file_detail.delete_confirm_title": "Delete this file?",
        "media.file_detail.delete_file": "Delete File",
        "media.file_not_found": "That file is no longer in your folders.",
        "media.file_path_required": "Type the path to a file first, then press Upload.",
        "media.file_required": "Choose a file first, then press Upload.",
        "media.filter_all": "All media",
        "media.no_folders": "No folders available. Configure a sync source first.",
        "media.no_media_yet": "No media yet",
        "media.restore_cancel": "Cancel",
        "media.restore_confirm": "Restore",
        "media.restore_confirm_body": "The file will return to this version on all your devices. The current version stays in the history.",
        "media.restore_confirm_title": "Restore this version?",
        "media.sort_ascending": "Ascending",
        "media.sort_date": "Date",
        "media.sort_descending": "Descending",
        "media.sort_name": "Name",
        "media.sort_size": "Size",
        "media.source_offline": "Files unreachable",
        "media.source_offline_notice": "Files unreachable — no device holding them is connected",
        "media.source_online": "Files reachable",
        "media.status_label.conflict": "Conflict",
        "media.status_label.downloading": "Downloading",
        "media.status_label.local_only": "Local Only",
        "media.status_label.remote_only": "Remote Only",
        "media.status_label.synced": "Synced",
        "media.status_label.uploading": "Uploading",
        "media.title": "Media",
        "media.type_file_path": "Type a file path…",
        "media.upload": "Upload",
        "media.upload_failed": "Upload failed",
        "media.uploading": "Uploading... {progress}%",
        "media.version_author": "Edited by {author}",
        "media.version_pruned_badge": "Pruned",
        "media.version_restore": "Restore",
        "media.version_undelete": "Recover",
        "media.versions_error": "Failed to load versions: {message}",
        "media.versions_loading": "Loading versions…",
        "media.versions_show_pruned": "Show recently pruned",
        "media.versions_title": "Version history",
        "media.view_grid": "Grid",
        "media.view_list": "List",
        "media.watched.add_directory": "Add Watched Directory",
        "media.watched.directory_label": "Directory",
        "media.watched.error_folders": "Failed to load your folders: {message}",
        "media.watched.error_read": "Failed to read {directory}: {message}",
        "media.watched.error_upload": "Failed to upload {directory}: {message}",
        "media.watched.no_folders": "You don't have a folder yet. Create one under Settings → Folders first.",
        "media.watched.no_watched": "No watched directories",
        "media.watched.remove_directory": "Remove directory",
        "media.watched.scan_now": "Scan Now",
        "media.watched.scanning": "Scanning...",
        "media.watched.target_folder": "Back up into",
        "media.watched_directories": "Watched Directories",
        "moderation.action.flagged": "Flagged",
        "moderation.action.labeled": "Labeled",
        "moderation.action.logged": "Logged",
        "moderation.action.quarantined": "Quarantined",
        "moderation.action.rate_limited": "Rate limited",
        "moderation.action.rejected": "Rejected",
        "moderation.action.suppressed": "Hidden from feeds",
        "moderation.action.taken_down": "Removed under legal obligation",
        "moderation.appeal": "Appeal",
        "moderation.appeal_blocked_no_content": "No content selected to appeal.",
        "moderation.appeal_blocked_no_reason": "Enter a reason before submitting the appeal.",
        "moderation.appeal_blocked_reason_too_long": "The reason is too long. Shorten it before submitting the appeal.",
        "moderation.appeal_cancel": "Cancel",
        "moderation.appeal_failed": "Appeal failed: {error}",
        "moderation.appeal_reason_label": "Why should this decision be reviewed?",
        "moderation.appeal_recorded": "Appeal recorded. An administrator will review it.",
        "moderation.appeal_submit": "Submit appeal",
        "moderation.appeal_summary": "Appealing the enforcement action on {content_id}.",
        "moderation.avg_confidence": "Avg spam confidence:",
        "moderation.category.commercial": "Commercial",
        "moderation.category.nsfw": "NSFW",
        "moderation.category.phishing": "Phishing",
        "moderation.category.spam": "Spam",
        "moderation.category.trusted": "Trusted",
        "moderation.confidence": "confidence",
        "moderation.correct": "Correct",
        "moderation.enforcement_title": "Enforcement Actions",
        "moderation.flagged_count": "{count} flagged",
        "moderation.legal_takedown.tombstone": "Removed under legal obligation ({reference})",
        "moderation.no_actions": "No enforcement actions on your content.",
        "moderation.phishing_hint": "Content scoring above this threshold is flagged as phishing.",
        "moderation.report.block_author_label": "Also block this person",
        "moderation.report.blocked_no_reason": "Choose a reason before sending the report.",
        "moderation.report.blocked_note_too_long": "The note is too long. Shorten it before sending the report.",
        "moderation.report.cancel": "Cancel",
        "moderation.report.failed": "The report could not be sent: {error}",
        "moderation.report.hidden_placeholder": "You reported this",
        "moderation.report.include_text_label": "Include the text of this message — the admins will be able to read it",
        "moderation.report.ledger_empty": "You have not reported anything.",
        "moderation.report.ledger_routed_to": "Sent to {destinations}",
        "moderation.report.ledger_title": "Your reports",
        "moderation.report.note_label": "Anything the admins should know? (optional)",
        "moderation.report.outcome_acted": "Acted on",
        "moderation.report.outcome_dismissed": "Dismissed",
        "moderation.report.reason.harassment": "Harassment",
        "moderation.report.reason.hate": "Hateful content",
        "moderation.report.reason.illegal": "Illegal content",
        "moderation.report.reason.impersonation": "Impersonation",
        "moderation.report.reason.other": "Something else",
        "moderation.report.reason.sexual": "Sexual content",
        "moderation.report.reason.spam": "Spam",
        "moderation.report.reason.violence": "Violence or threats",
        "moderation.report.reason_label": "Why are you reporting this?",
        "moderation.report.sent_forwarded": "Report sent to the admins of {nest} and forwarded, without your name, to the admins of {home_nest}.",
        "moderation.report.sent_local": "Report sent to the admins of {nest}.",
        "moderation.report.status_open": "Open",
        "moderation.report.status_resolved": "Resolved",
        "moderation.report.status_withdrawn": "Withdrawn",
        "moderation.report.submit": "Send report",
        "moderation.report.title": "Report",
        "moderation.report.withdraw": "Withdraw",
        "moderation.report.withdrawn": "Report withdrawn. Your note and any attached text were deleted everywhere they went.",
        "moderation.spam_detected": "Spam detected:",
        "moderation.spam_hint": "Content scoring above this threshold is filtered as spam.",
        "moderation.spam_protection": "Spam Protection",
        "moderation.stats_title": "Moderation Stats",
        "moderation.total_labels": "Total labels:",
        "muted_words.add": "Mute word",
        "muted_words.description": "Conversation messages containing one of these words are collapsed behind a “Show anyway” button. This list stays on your devices — the server never sees it.",
        "muted_words.empty": "You haven’t muted any words yet.",
        "muted_words.input_placeholder": "Add a word to mute",
        "muted_words.remove": "Un-mute",
        "muted_words.title": "Muted words",
        "navigation.category_action": "Go to",
        "navigation.category_conversation": "Conversation",
        "navigation.category_file": "File",
        "navigation.category_group": "Group",
        "navigation.members": "{count} members",
        "navigation.navigate": "Navigate",
        "navigation.no_matches": "No matches",
        "navigation.quick_switcher": "Quick Switcher",
        "navigation.quick_switcher_placeholder": "Search conversations, groups…",
        "navigation.section": "Section",
        "nests.add_button": "Link a nest",
        "nests.add_cancel": "Cancel",
        "nests.add_input_placeholder": "Nest address (https://…) — links both ends — or a 64-hex identity",
        "nests.add_submit": "Link",
        "nests.authorize_subtitle": "Authorize one of your nests to sync this account",
        "nests.backup_bound_note_seal": "Stopping this means your nest can no longer make new backups of your messages. Backups it already made stay where they are until the box holding them clears them out.",
        "nests.backup_bound_note_writer": "Stopping this means your nest can no longer send new backups to this destination. Backups already stored there stay until that box clears them out. You can stop it here even if your own nest is misbehaving.",
        "nests.backup_revoke": "Stop backing up",
        "nests.backup_scope_seal": "Backs up your messages for you",
        "nests.backup_scope_writer": "Writes your backups to {destination}",
        "nests.backup_since": "Trusted since:",
        "nests.backup_status_active": "Active",
        "nests.backup_status_missing": "Not set up to accept your backups",
        "nests.backup_status_unreachable": "Could not reach this destination",
        "nests.blessed_toggle": "Keep this box's trust renewed",
        "nests.bound_note_bounded_mail": "This trust is cryptographically time-boxed: once its window ends (accurate to within about a week), this nest can no longer read new mail at all — not even by re-acquiring a key. Within the window it can only open mail sealed under that period's rotating keys, so content sealed before the schedule caught up may still be unreadable to it.",
        "nests.bound_note_standing": "On an honest nest, revoking stops future access and re-acquisition. It cannot un-see what was already read, and does not yet block content arriving after revocation.",
        "nests.capabilities_label": "Syncs:",
        "nests.capability_account_replica": "sealed copy of your account settings",
        "nests.custody_nest_label": "{host}'s nest — trusted to hold sealed copies",
        "nests.description": "Nests you have linked to sync your account's content.",
        "nests.empty": "No linked nests yet.",
        "nests.escrow_holder_badge": "Holds your recovery escrow",
        "nests.expiry_label": "Expires:",
        "nests.expiry_never": "Never expires",
        "nests.forward_discard": "Stop forwarding these",
        "nests.forward_queue_last_error": "Last attempt failed: {error}",
        "nests.forward_queue_stuck": "{count} of them have been refused for more than eight hours. Check that this nest is allowed to forward your posts on the relay — linking the relay again from here grants that — or stop forwarding them below.",
        "nests.forward_queue_summary": "{count} of your posts are waiting to reach your relay.",
        "nests.forward_retry": "Retry now",
        "nests.generation_expires": "Can be restored until {when}, and counts against your storage until then",
        "nests.generation_past_window": "That version is past the recovery window, so it can no longer be restored.",
        "nests.generation_path": "Backup of {path}",
        "nests.generation_path_unknown": "Backup {hash}",
        "nests.generation_restore": "Restore this version",
        "nests.generation_restored": "That version has been restored.",
        "nests.generation_status_listed": "Can be restored",
        "nests.generation_status_unreachable": "Could not reach this destination",
        "nests.generation_superseded": "Replaced:",
        "nests.grant_keep_button": "Keep",
        "nests.grant_unattested_mark": "Given before you recovered this account — still active. Keep it, or revoke it if you don't recognise it.",
        "nests.history_minted": "Trusted to read {scope} · {when}",
        "nests.history_renewed": "Trust renewed: {scope} · {when}",
        "nests.history_revoked": "Trust revoked: {scope} · {when}",
        "nests.lasts_until": "Trusted until:",
        "nests.link_recovery_keys_differ": "These two nests hold different recovery keys for this account, so they can't be linked.",
        "nests.list_title": "Your linked nests",
        "nests.mint_button": "Add trust…",
        "nests.mint_confirm": "Trust",
        "nests.mint_duration_one_off": "For a few hours",
        "nests.mint_duration_standard": "For 90 days",
        "nests.mint_holder_placeholder": "Which service on this nest?",
        "nests.mint_option_calendar": "Read my calendar",
        "nests.mint_option_mail": "Read and filter my mail — and the spam-filter model and training history derived from it",
        "nests.mint_option_paywalled": "Serve paywalled posts — {tier}",
        "nests.mint_scope_placeholder": "Choose what to trust it with…",
        "nests.nest_to_link": "Nest to link",
        "nests.not_trusted": "This nest is not trusted to read any of your content.",
        "nests.renew": "Renew",
        "nests.revoke": "Revoke",
        "nests.scope_calendar": "Calendar",
        "nests.scope_folder": "Your folder \"{folder}\"",
        "nests.scope_folder_deleted": "A folder you have since deleted",
        "nests.scope_labeler_labels": "Write labels for community labeler {labeler}",
        "nests.scope_mail": "Mail — and the spam-filter model and training history derived from it",
        "nests.scope_mail_labeler": "Mail — only to run community labeler {labeler} over it",
        "nests.scope_posts": "Posts",
        "nests.scope_posts_tier": "Posts — {tier}",
        "nests.scope_spam_labels": "Write spam labels",
        "nests.scope_spam_model": "Your spam-filter training, for the shared spam baseline",
        "nests.status_active": "Active",
        "nests.status_auto_renewing": "Auto-renewing",
        "nests.status_expired": "Paused — renew to resume",
        "nests.status_expiring": "Expiring soon",
        "nests.title": "Nests",
        "nests.trusted_to_read": "Trusted to read:",
        "nests.unlink": "Unlink",
        "nests.view_history": "History",
        "nests.view_now": "Now",
        "nostr.account.description": "Link a Nostr identity to your Fauna account",
        "nostr.account.generate_key_button": "Generate Key",
        "nostr.account.link_subtitle": "Generate a new Nostr keypair on the nest",
        "nostr.account.mode_generated": "Generated keypair",
        "nostr.account.mode_imported": "Imported nsec",
        "nostr.account.mode_nip07": "NIP-07 extension",
        "nostr.account.mode_proxied": "Proxied (paired nest signs)",
        "nostr.account.mode_remote": "NIP-46 bunker",
        "nostr.account.public_key": "Public Key",
        "nostr.account.signing_mode": "Signing Mode",
        "nostr.account.status_no_client": "No client",
        "nostr.account.status_unavailable": "Nostr unavailable on this nest",
        "nostr.account.title": "Nostr Account",
        "nostr.account.unlink": "Unlink Account",
        "nostr.account.unlink_button": "Unlink",
        "nostr.account.unlink_subtitle": "Remove Nostr identity from your nest account",
        "nostr.connected_apps.connect_button": "Connect an app",
        "nostr.connected_apps.description": "Sign in to Nostr apps with your nest using Nostr Connect. Your key stays on the nest — apps only ask it to sign.",
        "nostr.connected_apps.disconnect": "Disconnect",
        "nostr.connected_apps.expires": "Expires {time}",
        "nostr.connected_apps.last_used": "Last used {time}",
        "nostr.connected_apps.never_used": "Never used",
        "nostr.connected_apps.none": "No connected apps yet.",
        "nostr.connected_apps.pending": "Waiting to connect…",
        "nostr.connected_apps.qr_alt": "Nostr Connect QR code",
        "nostr.connected_apps.reveal_hint": "Shown once — copy it now.",
        "nostr.connected_apps.reveal_title": "Scan or paste this in your Nostr app",
        "nostr.connected_apps.title": "Connected apps",
        "nostr.connected_apps.unnamed": "Unnamed app",
        "nostr.follows.add": "Add",
        "nostr.follows.none": "No Nostr follows yet.",
        "nostr.follows.petname_placeholder": "Petname",
        "nostr.follows.pubkey_placeholder": "npub1... or hex pubkey",
        "nostr.follows.title": "Follows",
        "nostr.link_account.description": "Connect a Nostr identity to your Fauna account.",
        "nostr.link_account.enter_nsec": "Please enter an nsec key",
        "nostr.link_account.generate": "Generate new keypair",
        "nostr.link_account.import_nsec": "Import nsec",
        "nostr.link_account.link_button": "Link Account",
        "nostr.link_account.mode_label": "Link mode",
        "nostr.link_account.nip07": "NIP-07 browser extension",
        "nostr.link_account.nip07_prompt": "Your browser extension will be prompted for the public key.",
        "nostr.link_account.no_nip07": "No NIP-07 browser extension detected",
        "nostr.link_account.nsec_label": "nsec key",
        "nostr.link_account.nsec_placeholder": "nsec1...",
        "nostr.link_account.title": "Link Nostr Account",
        "nostr.npub_confirm.banner": "A recent account recovery changed your Nostr key. Please confirm the public key shown above ({npub}) is yours.",
        "nostr.npub_confirm.no_button": "No / nothing is linked",
        "nostr.npub_confirm.yes_button": "Yes, that's my npub",
        "nostr.relays.add": "Add Relay",
        "nostr.relays.invalid_url": "Relay URL must start with wss:// or ws://",
        "nostr.relays.none": "No relays configured. Default relays will be used.",
        "nostr.relays.placeholder": "wss://relay.example.com",
        "nostr.relays.private_address": "Relays on a private network or on this device can't be used. Use a public relay address.",
        "nostr.relays.title": "Relays",
        "nostr.settings.auto_publish": "Auto-publish posts",
        "nostr.settings.auto_publish_subtitle": "Automatically publish Fauna posts to Nostr relays",
        "nostr.settings.description": "Configure Nostr publishing behavior",
        "nostr.settings.expose_subtitle": "Allow Nostr users to see your Fauna content",
        "nostr.settings.expose_title": "Expose content",
        "nostr.settings.inbound_subtitle": "Show events from followed Nostr users in your feed",
        "nostr.settings.inbound_title": "Inbound to feed",
        "nostr.settings.publish_reactions": "Publish reactions",
        "nostr.settings.publish_replies": "Publish replies",
        "nostr.settings.title": "Content Settings",
        "nostr.title": "Nostr",
        "nostr.unavailable": "Nostr isn't available on this nest — it was built without Nostr support.",
        "nostr.zap_signers.add": "Designate signer",
        "nostr.zap_signers.description": "Zaps are Lightning tips. A zap receipt is signed by the wallet provider that received the payment — not by the sender — so your nest only believes receipts from signers you name here.",
        "nostr.zap_signers.invalid_pubkey": "A signer pubkey must be 64 hexadecimal characters.",
        "nostr.zap_signers.label_placeholder": "Label (optional)",
        "nostr.zap_signers.none": "You have not designated any signer, so no zap is counted as paid. Add your wallet provider's signer key to start believing its receipts.",
        "nostr.zap_signers.pubkey_placeholder": "64-character hex signer pubkey",
        "nostr.zap_signers.remove": "Stop trusting",
        "nostr.zap_signers.title": "Zap signers",
        "nostr.zap_signers.unnamed": "Unnamed signer",
        "notifications.default_body": "New notification",
        "notifications.event_reminder_body": "{name} in {minutes} minutes",
        "notifications.event_reminder_title": "Upcoming Event",
        "notifications.group_invite_body": "{name} invited you to {group}",
        "notifications.group_invite_title": "Group Invite",
        "notifications.group_message": "{sender} in {group}",
        "notifications.knock_body": "{name} wants to connect",
        "notifications.knock_title": "New Contact Request",
        "notifications.message_from": "Message from {sender}",
        "notifications.row_abuse_report_received": "A report is waiting in the reports queue. Open Admin → Nest to review it.",
        "notifications.row_abuse_report_resolved": "Your report was reviewed: {outcome}.",
        "notifications.row_family_contact_request": "An account you supervise asked to add a contact. Open Family to decide.",
        "notifications.row_family_content_notice": "Filtered content was seen on an account you supervise. Open Family to review.",
        "notifications.row_family_feed_source_approved": "Your guardian approved the source you asked for. Try adding it again.",
        "notifications.row_family_feed_source_request": "An account you supervise asked to add a source. Open Family to decide.",
        "notifications.row_follow": "{sender} followed you",
        "notifications.row_forward_queue_evicted": "Forward to {dest} dropped because your forward queue is full. Configured rate: {cap}/hour. Reduce inbound or increase the cap.",
        "notifications.row_interaction": "{sender} interacted with your content",
        "notifications.row_knock": "{sender} wants to connect: {message}",
        "notifications.row_like": "{sender} liked your post",
        "notifications.row_mention": "{sender} mentioned you",
        "notifications.row_quote": "{sender} quoted your post",
        "notifications.row_reply": "{sender} replied to your post",
        "notifications.row_repost": "{sender} reposted your post",
        "notifications.row_security_action_cancelled": "Security: a pending action ({action_type}) on your account was cancelled by {cancelled_by}.",
        "notifications.row_security_action_executed": "Security: a pending action ({action_type}) was executed on your account. If you did not authorize it, contact your nest administrator.",
        "notifications.row_security_action_expired": "Security: a pending action ({action_type}, #{action_id}) on your account expired without the approvals it needed. Nothing changed.",
        "notifications.row_security_admin_action_cancelled": "Security: the pending admin action {action_type} (#{action_id}) on {target} was cancelled by {cancelled_by}.",
        "notifications.row_security_admin_action_expired": "Security: the pending admin action {action_type} (#{action_id}) on {target} expired without the approvals it needed. Nothing changed.",
        "notifications.row_security_admin_action_pending": "Security: admin {by} scheduled {action_type} (#{action_id}) on {target}; it runs at {execute_after} and still needs {approvals_needed} approval(s). Approve or cancel it under Admin → Users → Pending admin actions.",
        "notifications.row_security_admin_change": "Security: an administrative change ({change_type}) was made to {target}. If you did not request it, contact your nest administrator.",
        "notifications.row_security_archive_exported": "Security: your full account archive was downloaded from {ip}. If this was not you, change your keys and contact your nest administrator.",
        "notifications.row_security_identity_succeeded": "Security: this identity was succeeded. Your account moved to a new key ({new_actor_id}).",
        "notifications.row_security_mailbox_export_downloaded": "Security: a mailbox export ({format}) was downloaded from {ip}. If this was not you, change your keys and contact your nest administrator.",
        "notifications.row_security_new_token": "Security: new sign-in from a different IP address ({ip}). If this was not you, change your keys and contact your nest administrator.",
        "notifications.row_security_pending_action_against_you": "Security: administrator {by} scheduled {action_type} (#{action_id}) against your account; it runs at {execute_after}. You can cancel it under Settings → Pending actions.",
        "notifications.row_security_pending_action_queued": "Security: a pending action ({action_type}, #{action_id}) was queued by your account and runs at {execute_after}. If this was not you, cancel it under Settings → Pending actions.",
        "notifications.row_security_recovery_replacement_cancelled": "Security: the pending recovery key replacement was cancelled by {cancelled_by}.",
        "notifications.row_security_recovery_replacement_landed": "Security: your recovery key was replaced (new key {new_key}).",
        "notifications.row_security_recovery_replacement_pending": "Security: a recovery key replacement was requested (new key {new_key}) and lands at {lands_at}. If this was not you, veto it within 30 days.",
        "notifications.sync_complete_body": "{filename} uploaded",
        "notifications.sync_complete_title": "Sync Complete",
        "notifications.type_default": "Notification",
        "notifications.type_event_invite": "Event invite",
        "notifications.type_follow": "Follow",
        "notifications.type_group_invite": "Group invite",
        "notifications.type_knock": "Contact request",
        "notifications.type_like": "Like",
        "notifications.type_mention": "Mention",
        "notifications.type_message": "Message",
        "notifications.type_quote": "Quote",
        "notifications.type_reply": "Reply",
        "notifications.type_report": "Report",
        "notifications.type_repost": "Repost",
        "notifications.update_available_body": "Version {version} is available. Visit fauna.social to download.",
        "notifications.update_available_summary": "Fauna update available",
        "onboarding.add_account.cancel": "Cancel",
        "onboarding.awaiting_dns.checking": "Checking whether your nest is online…",
        "onboarding.awaiting_dns.claimed": "All set. Continuing…",
        "onboarding.awaiting_dns.claiming": "Your nest is online — finishing setup…",
        "onboarding.awaiting_dns.copy_button": "Copy all",
        "onboarding.awaiting_dns.error": "Couldn't finish setting up your nest: {cause}",
        "onboarding.awaiting_dns.pending": "Add the DNS records below at your registrar. We'll bring your nest online automatically once they take effect — you can leave this screen open.",
        "onboarding.awaiting_dns.recheck_button": "Check now",
        "onboarding.awaiting_dns.server_starting": "Your server is starting — we'll sign you in the moment it answers. You can close the app and come back.",
        "onboarding.awaiting_dns.title": "Almost ready",
        "onboarding.bridges.link_mode": "Link mode",
        "onboarding.bridges.no_link_options": "No link options available.",
        "onboarding.bridges.not_available_on_nest": "{name} not available on this nest.",
        "onboarding.claim_code.claimed": "Welcome, admin. Continuing…",
        "onboarding.claim_code.description": "No one has claimed this nest yet. Paste the one-time claim code printed by your nest server to become its admin.",
        "onboarding.claim_code.error.already_claimed": "This nest has already been claimed.",
        "onboarding.claim_code.error.claim_code_unreadable": "The nest can't read its claim code — a server setup problem, not a wrong code. Restart the nest and try again.",
        "onboarding.claim_code.error.terminal": "Couldn't claim: {cause}.",
        "onboarding.claim_code.error.transient": "Couldn't reach the nest. Try again.",
        "onboarding.claim_code.idle": "Paste the claim code from your server.",
        "onboarding.claim_code.invalid": "Code not accepted: {reason}",
        "onboarding.claim_code.label": "Claim code",
        "onboarding.claim_code.placeholder": "Claim code from server bootstrap",
        "onboarding.claim_code.submit_button": "Claim",
        "onboarding.claim_code.submitting": "Claiming nest…",
        "onboarding.claim_code.title": "Claim this nest",
        "onboarding.complete.nest_details": "Nest Details",
        "onboarding.complete.title": "Your nest is ready!",
        "onboarding.dns_config.buy_domain_checkbox": "Buy domain on Continue (this page)",
        "onboarding.dns_config.contact_fields.address1": "Address",
        "onboarding.dns_config.contact_fields.city": "City",
        "onboarding.dns_config.contact_fields.country": "Country (ISO 3166-1 alpha-2, e.g. US)",
        "onboarding.dns_config.contact_fields.email": "Email",
        "onboarding.dns_config.contact_fields.first_name": "First name",
        "onboarding.dns_config.contact_fields.last_name": "Last name",
        "onboarding.dns_config.contact_fields.phone": "Phone (E.164: +12025550100)",
        "onboarding.dns_config.contact_fields.postal_code": "Postal code",
        "onboarding.dns_config.contact_fields.state": "State / region",
        "onboarding.dns_config.contact_form_heading": "WHOIS registration contact",
        "onboarding.dns_config.ineligible_needs_registrar": "Can't register domains — untick the buy-domain box above to pick it.",
        "onboarding.dns_config.ineligible_needs_registrar_and_vps": "Can't register domains or sell VPS servers — untick both boxes above to pick it.",
        "onboarding.dns_config.ineligible_needs_vps": "Doesn't sell VPS servers — untick the same-provider box above to pick it.",
        "onboarding.dns_config.no_provider_carries_tld": "None of our supported registrars carry .{tld} domains. You'll need to buy this domain elsewhere, then either transfer it to a supported registrar or set up DNS manually.",
        "onboarding.dns_config.open_in_browser": "Open in browser",
        "onboarding.dns_config.same_provider_checkbox": "Buy VPS with same provider (next page)",
        "onboarding.dns_config.set_up_later": "Set up later",
        "onboarding.dns_config.set_up_later_warning": "Your handle will not work until DNS is configured. We'll show instructions after VPS purchase.",
        "onboarding.dns_config.status_buyable": "{provider} will register this domain for {price}. Tick the confirm box and press Continue to buy.",
        "onboarding.dns_config.status_not_buyable": "{provider} can't sell this domain. Buy it elsewhere first, then transfer it or use manual DNS.",
        "onboarding.dns_config.status_owned": "You own this domain at {provider}.",
        "onboarding.dns_config.status_pick_provider": "Choose where your domain's DNS lives to continue.",
        "onboarding.dns_config.status_registered_elsewhere": "This domain is already registered. Transfer it to {provider} (or pick another provider) before continuing.",
        "onboarding.dns_config.status_verify_credentials": "Enter this provider's credentials and press Verify to continue.",
        "onboarding.dns_config.title": "Configure DNS",
        "onboarding.dns_post_instructions.copy_button": "Copy all",
        "onboarding.dns_post_instructions.description": "Add these records at your DNS provider so your handle starts working.",
        "onboarding.dns_post_instructions.records_pending": "(no records yet — try again in a moment)",
        "onboarding.dns_post_instructions.title": "DNS setup instructions",
        "onboarding.done.finished": "Onboarding finished.",
        "onboarding.handle.control_checkbox": "I control DNS for this domain",
        "onboarding.handle.examples_help": "Example: alice@example.com or alice@bsky.social. Format: user@domain.",
        "onboarding.handle.localhost_hint": "test@localhost is allowed for trying out the app.",
        "onboarding.handle.prompt": "Enter your handle",
        "onboarding.handle_check.error.challenge_failed": "Couldn't verify your identity with the nest. This may indicate a key mismatch.",
        "onboarding.handle_check.error.challenge_temp": "The nest's challenge service is temporarily unavailable. Try again.",
        "onboarding.handle_check.error.nest_misbehaving": "{domain} responded with an unexpected error. Try again later.",
        "onboarding.handle_check.error.nest_protocol_mismatch": "{domain} returned a malformed response. The nest version may be incompatible.",
        "onboarding.handle_check.error.nest_unreachable": "{domain} is registered but the nest didn't respond. Try again, or check the domain.",
        "onboarding.handle_check.error.no_network": "Couldn't reach the network. Check your connection and try again.",
        "onboarding.handle_check.error.terminal": "Error: {cause}.",
        "onboarding.handle_check.error.transient": "Temporary error: {cause}. Try again.",
        "onboarding.handle_check.idle": "Enter your handle, then press Check to continue.",
        "onboarding.handle_check.outcome.already_on_nest_handle_differs": "You're already registered on {domain} as {old_handle}. Continue to log in as that handle (you can change it after).",
        "onboarding.handle_check.outcome.already_on_nest_handle_matches": "Welcome back, {handle}.",
        "onboarding.handle_check.outcome.domain_available_inside_zone": "Nothing is set up at {domain} yet. It sits inside {zone}: if you hold {zone}, continue and pick the DNS provider that hosts it. If not, you can register {domain} on the next page.",
        "onboarding.handle_check.outcome.domain_available_not_buyable_via_provider": "{domain} appears to be available, but none of our supported registrars carry .{tld}. You'll need to buy it elsewhere.",
        "onboarding.handle_check.outcome.domain_available_priced": "{domain} is available — registration about {price}.",
        "onboarding.handle_check.outcome.domain_available_unpriced": "{domain} appears to be available for purchase.",
        "onboarding.handle_check.outcome.format_invalid": "Handle format must be user@domain or user.domain (with localhost or IP also accepted).",
        "onboarding.handle_check.outcome.registered_no_nest": "{domain} resolves but no nest is running. To set one up, confirm you control DNS for this domain.",
        "onboarding.handle_check.outcome.tld_invalid": "{tld} is not a TLD that can be registered.",
        "onboarding.handle_check.outcome.unregistered_unclaimed_nest": "There's a nest at {domain} but no one has claimed it yet. Continue to claim it as your own.",
        "onboarding.handle_check.outcome.user_unregistered": "There's a nest at {domain} but you're not registered. Request an invite or paste a code below.",
        "onboarding.handle_check.phase.challenge_response": "Checking your account…",
        "onboarding.handle_check.phase.dns_lookup": "Checking domain availability…",
        "onboarding.handle_check.phase.nest_probe": "Looking for a nest at {domain}…",
        "onboarding.handle_check.phase.parsing": "Checking format…",
        "onboarding.handle_check.phase.price_lookup": "Looking up registration price…",
        "onboarding.identity_choice.create_new": "Create New Identity",
        "onboarding.identity_choice.import_existing": "Import from Another Device",
        "onboarding.identity_choice.recover_lost_box": "Recover a lost box",
        "onboarding.identity_choice.restore_from_recovery_kit": "Restore my account from a recovery phrase",
        "onboarding.identity_choice.subtitle": "Your identity is an encryption key that belongs only to you.",
        "onboarding.identity_choice.title": "Set Up Your Identity",
        "onboarding.identity_created.continue": "Continue",
        "onboarding.identity_created.desc": "Your new identity has been generated. Save this secret key — it's your only way to recover your account.",
        "onboarding.identity_created.not_generated": "Identity not generated yet",
        "onboarding.identity_created.secret_key_label": "Your secret key:",
        "onboarding.identity_created.title": "Identity Created",
        "onboarding.identity_created.warning": "Write this down or save it in a password manager. If you lose it, your account cannot be recovered.",
        "onboarding.identity_import.camera_unavailable": "Camera Not Available",
        "onboarding.identity_import.camera_unavailable_hint": "Use the paste tab instead.",
        "onboarding.identity_import.import": "Import",
        "onboarding.identity_import.invalid_qr": "Not a valid Fauna identity QR code.",
        "onboarding.identity_import.invalid_secret": "Secret key must be 64 hex characters.",
        "onboarding.identity_import.paste_label": "Secret key",
        "onboarding.identity_import.paste_placeholder": "64-character hex secret key",
        "onboarding.identity_import.paste_subtitle": "Paste the secret key from your other device.",
        "onboarding.identity_import.paste_tab": "Paste Secret Key",
        "onboarding.identity_import.scan_subtitle": "Scan the QR code shown on your other device.",
        "onboarding.identity_import.scan_tab": "Scan QR Code",
        "onboarding.identity_import.title": "Import Identity",
        "onboarding.instance_chooser.account_taken": "That account was just opened in another window. Pick another.",
        "onboarding.instance_chooser.add_account": "Log in as a new user",
        "onboarding.instance_chooser.choose_account": "Open a different account",
        "onboarding.instance_chooser.focus_existing": "Switch to the open window",
        "onboarding.instance_chooser.no_running_instance": "Couldn't switch to the window running this account. Close it and launch Fauna again.",
        "onboarding.instance_chooser.none_available": "Every account you've added is already open in another window.",
        "onboarding.instance_chooser.subtitle": "This window can't open {account} — it's already running. Choose another account, or switch to the open window.",
        "onboarding.instance_chooser.title": "Fauna is already open",
        "onboarding.invite.denied": "Request denied: {reason}",
        "onboarding.invite.error.already_registered": "This nest already has an account for you, so it won't take a new invite request. Its admin may have suspended your account — contact the admin; if they restore it, sign in again.",
        "onboarding.invite.error.closed": "This nest is not currently accepting invite requests.",
        "onboarding.invite.error.not_found": "This invite request was not found. It may have been removed by the admin.",
        "onboarding.invite.error.rate_limited": "Too many requests. Please try again later.",
        "onboarding.invite.error.terminal": "Error: {cause}.",
        "onboarding.invite.error.transient": "{cause}. Try again.",
        "onboarding.invite.idle": "Request an invite or paste a code below.",
        "onboarding.invite.pending_review": "Submitted. The admin will review — you'll continue automatically once they respond.",
        "onboarding.invite.recheck_button": "Recheck",
        "onboarding.invite.rechecking": "Checking status…",
        "onboarding.invite.request_button": "Request invite",
        "onboarding.invite.submitting": "Submitting request…",
        "onboarding.invite_request.code_label": "Invite code",
        "onboarding.invite_request.code_placeholder": "Paste invite code",
        "onboarding.invite_request.code_section_title": "Have an invite code?",
        "onboarding.invite_request.handle_label": "Your requested handle",
        "onboarding.invite_request.handle_placeholder": "alice",
        "onboarding.invite_request.message_label": "Message to the admin (optional)",
        "onboarding.invite_request.message_placeholder": "Hi, I'd like to join this nest because...",
        "onboarding.invite_request.oob_error": "Could not reach the nest to check this code, so it has not been rejected. Check your connection, then press Check again. ({cause})",
        "onboarding.invite_request.oob_idle": "Paste an invite code, then press Check.",
        "onboarding.invite_request.oob_invalid": "This nest did not accept that code: {reason}. Check it for typos and press Check again, or use Request invite above.",
        "onboarding.invite_request.oob_valid": "Code accepted",
        "onboarding.invite_request.recheck_button": "Check again",
        "onboarding.invite_request.request_button": "Request invite",
        "onboarding.invite_request.status_idle": "Ask for an invite above, or paste a code you already have, to continue.",
        "onboarding.invite_request.submit": "Send request",
        "onboarding.invite_request.submit_failed": "Could not send the request. Please try again.",
        "onboarding.invite_request.submitting": "Sending...",
        "onboarding.invite_request.subtitle": "Ask the admin of this nest to let you in.",
        "onboarding.invite_request.title": "Request an invite",
        "onboarding.launch.account_locked": "This account is locked and nobody can sign in until the lock ends.",
        "onboarding.launch.account_locked_not_yours": "If you did not lock it, somebody else holds your secret key, and they can lock it again. Your recovery kit moves the account to a new key they do not have — the lock does not stop that.",
        "onboarding.launch.account_locked_title": "Account locked",
        "onboarding.launch.account_locked_until": "The lock ends {time}.",
        "onboarding.launch.identity_changed_trust": "Trust this nest and continue",
        "onboarding.launch.identity_changed_warning": "This nest's identity has changed, or it can no longer prove the identity you previously trusted. It may have been re-deployed or had its key rotated — or someone may be impersonating it. Don't continue unless you were expecting this change.",
        "onboarding.launch.identity_superseded": "This identity was succeeded — import the new identity to continue. Your account now belongs to a new identity, and this one can no longer sign in.",
        "onboarding.launch.identity_superseded_verified": "This identity was succeeded — import the new identity to continue. Your account now belongs to {successor}.",
        "onboarding.launch.index_malformed": "Your saved accounts can't be read, and updating the app won't help. Nothing has been changed or deleted. You can start over on this device, or move this device's data somewhere safe first.",
        "onboarding.launch.index_malformed_reset": "Start over on this device",
        "onboarding.launch.index_malformed_reset_blocked_other_window": "Nothing was removed — another Fauna window is using an account on this device. Close it, then start over again.",
        "onboarding.launch.index_malformed_reset_confirm": "Remove everything and start over",
        "onboarding.launch.index_malformed_reset_residual": "Starting over removes every account from this device and returns the app to a fresh install. Your accounts still exist on your nest, but you will need your secret key or recovery kit to sign back in — this device's copy is removed. Some saved data on this device cannot be reached to clear it.",
        "onboarding.launch.index_malformed_title": "Your saved accounts can't be read",
        "onboarding.launch.index_newer_build": "Your accounts were saved by a newer version of this app, so this version can't read them. Nothing has been lost — update the app and they'll be here.",
        "onboarding.launch.index_newer_build_title": "Update needed",
        "onboarding.launch.launch_failed": "Startup hit a problem. Your data is safe — you can retry, or continue setup below.",
        "onboarding.launch.recover_lost_box": "Recover a lost box",
        "onboarding.launch.retire_server": "Retire a server",
        "onboarding.launch.sign_in_refused": "This nest no longer signs you in. Its admin may have suspended or removed your account — nothing on this device has changed. Contact the admin; if they restore your account, try again.",
        "onboarding.launch.sign_in_refused_title": "Can't sign in here",
        "onboarding.launch.transient_error": "Couldn't reach your nest. Check your connection and try again.",
        "onboarding.launch.use_different_nest": "Use a different nest",
        "onboarding.nat_mode.choosing": "Confirm how this nest connects, or decide later.",
        "onboarding.nat_mode.confirm_button": "Confirm",
        "onboarding.nat_mode.defer_button": "Decide later",
        "onboarding.nat_mode.description": "This sets how your nest connects to the world. The pre-selected option matches how it was installed, so you can usually just confirm — and you can change it anytime in Admin → Nest.",
        "onboarding.nat_mode.done": "Connection mode saved.",
        "onboarding.nat_mode.error.terminal": "Couldn't save the connection mode: {cause}.",
        "onboarding.nat_mode.error.transient": "Couldn't save the connection mode: {cause}. Try again.",
        "onboarding.nat_mode.private_desc": "This box sits behind a home router and isn't reachable from the internet: it runs no mail receiver, keeps calendar and mail sync on your local network, and pairs with a public nest that relays for it.",
        "onboarding.nat_mode.private_hint": "This looks like a home-network address, so Private is pre-selected.",
        "onboarding.nat_mode.private_label": "Private (home network)",
        "onboarding.nat_mode.public_desc": "This box has a public address: it can receive email, federate directly with other nests, get automatic certificates, and relay for private nests. The usual choice for a hosted server.",
        "onboarding.nat_mode.public_label": "Public (internet-facing)",
        "onboarding.nat_mode.submitting": "Saving the connection mode...",
        "onboarding.nat_mode.title": "Is this nest reachable from the internet?",
        "onboarding.nest_provisioning.bom_line": "{label}: {price}",
        "onboarding.nest_provisioning.bom_line_domain": "{label}: {price} for the first year, then {renewal}/year",
        "onboarding.nest_provisioning.bom_line_recurring": "{label}: {price}/month",
        "onboarding.nest_provisioning.cancel_button": "Cancel",
        "onboarding.nest_provisioning.continue_blocked_cancelled": "Setup was cancelled — retry it to continue.",
        "onboarding.nest_provisioning.continue_blocked_failed": "Setup did not finish — retry it to continue.",
        "onboarding.nest_provisioning.continue_blocked_idle": "Set up your nest first — choose \"Buy and set up\" above.",
        "onboarding.nest_provisioning.continue_blocked_running": "Continue unlocks once setup finishes.",
        "onboarding.nest_provisioning.elapsed_template": "{seconds}s elapsed",
        "onboarding.nest_provisioning.retry_button": "Retry",
        "onboarding.nest_provisioning.start_button": "Buy and set up",
        "onboarding.nest_provisioning.title": "Setting up your nest…",
        "onboarding.oob_code.error": "Couldn't verify code: {cause}",
        "onboarding.oob_code.idle": "Have an invite code? Paste it here.",
        "onboarding.oob_code.invalid": "Code not recognized: {reason}",
        "onboarding.oob_code.placeholder": "Invite code from an admin",
        "onboarding.oob_code.valid": "Code accepted. Click Continue to log in.",
        "onboarding.oob_code.verifying": "Verifying code…",
        "onboarding.provision.adding_dkim_record": "Adding email DNS record",
        "onboarding.provision.complete": "Your nest is online!",
        "onboarding.provision.configuring_dns": "Configuring DNS...",
        "onboarding.provision.creating_dkim": "Creating DKIM record...",
        "onboarding.provision.creating_dns_records": "Creating DNS records",
        "onboarding.provision.creating_server": "Creating server...",
        "onboarding.provision.fetching_dkim": "Fetching DKIM key...",
        "onboarding.provision.polling_health": "Polling for nest to come online",
        "onboarding.provision.provisioning_complete": "Provisioning complete",
        "onboarding.provision.registering_domain": "Registering domain...",
        "onboarding.provision.registering_domain_details": "Registering domain with registrar",
        "onboarding.provision.retrieving_dkim": "Retrieving email signing key",
        "onboarding.provision.server": "Provision Server",
        "onboarding.provision.setting_up_vps": "Setting up VPS instance",
        "onboarding.provision.start_over": "Start over",
        "onboarding.provision.step.dns": "DNS",
        "onboarding.provision.step.domain": "Domain",
        "onboarding.provision.step.online": "Online",
        "onboarding.provision.step.server": "Server",
        "onboarding.provision.step_attempt_template": " (attempt {attempt} of {max_attempts})",
        "onboarding.provision.step_failed": "Step {step} failed: {cause}",
        "onboarding.provision.substep.dns_adding_domain_records": "Adding domain records",
        "onboarding.provision.substep.dns_adding_email_records": "Adding email records",
        "onboarding.provision.substep.dns_setting_reverse_dns": "Setting reverse DNS",
        "onboarding.provision.substep.domain_checking_availability": "Checking domain availability",
        "onboarding.provision.substep.domain_registering": "Registering domain",
        "onboarding.provision.substep.domain_verifying_zone": "Verifying DNS zone",
        "onboarding.provision.substep.online_claiming": "Signing you in to your new nest",
        "onboarding.provision.substep.online_waiting": "Waiting for your nest to start",
        "onboarding.provision.substep.server_creating": "Creating server",
        "onboarding.provision.substep.server_generating_dkim": "Generating DKIM keys",
        "onboarding.provision.substep.status_cancelled": "Cancelled",
        "onboarding.provision.substep.status_cancelling": "Cancelling…",
        "onboarding.provision.substep.status_retrying": "Retrying after error: {cause}",
        "onboarding.provision.substep.status_skipped": "Already configured — skipped",
        "onboarding.provision.time": "This usually takes 2-3 minutes.",
        "onboarding.recovery.box_item_hint": "Recovery key custodied",
        "onboarding.recovery.box_list_label": "Your boxes",
        "onboarding.recovery.empty_message": "No recovery keys are available yet. Connect to one of your other nests first — its synced configuration holds the recovery keys for every box you administer.",
        "onboarding.recovery.method_cloud": "Re-provision on a cloud host",
        "onboarding.recovery.method_selfhosted": "Install on my own server",
        "onboarding.recovery.restore_cta": "Restore your data",
        "onboarding.recovery.selfhosted_command_pending": "The installer command with your recovery seed will appear here.",
        "onboarding.recovery.selfhosted_continue": "Done",
        "onboarding.recovery.selfhosted_desc": "Run the installer below on a fresh box. It carries your saved deployment identity, so the rebuilt box re-presents the same nest identity and your pinned devices reconnect automatically once its DNS is re-pointed.",
        "onboarding.recovery.selfhosted_title": "Install on your own server",
        "onboarding.recovery.subtitle": "Choose the box you lost. Fauna re-provisions a fresh box with its saved identity, so your pinned devices reconnect automatically once it's back online.",
        "onboarding.recovery.title": "Recover a lost box",
        "onboarding.recovery_entry.account_hint": "If your phrase doesn't name your account, enter your handle (user@domain) so your nest can be found. If that domain is gone, put your nest's own address after the @ instead — like alice@192.0.2.10 or alice@nest.local.",
        "onboarding.recovery_entry.account_malformed": "Enter the full handle including the domain, like alice@fauna.social — or, if that domain is gone, your handle followed by your nest's address, like alice@192.0.2.10.",
        "onboarding.recovery_entry.account_needed": "Enter the handle of the account you're restoring (user@domain) — this phrase doesn't say which account it belongs to.",
        "onboarding.recovery_entry.account_unknown": "No account named {account} exists on that nest.",
        "onboarding.recovery_entry.desc": "Paste your recovery phrase (the fauna://recovery link or the 64-character code). Your identity will be restored from your nest's sealed escrow.",
        "onboarding.recovery_entry.invalid_kit": "That isn't a recovery phrase. Paste the fauna://recovery link or the 64-character recovery code — not your secret key.",
        "onboarding.recovery_entry.no_escrow": "This account has no sealed backup to restore from. Create a new recovery kit from a device that's still signed in.",
        "onboarding.recovery_entry.phrase_label": "Recovery phrase",
        "onboarding.recovery_entry.refused": "That recovery phrase was refused — it may have been replaced by a newer kit, or belong to another account. ({reason})",
        "onboarding.recovery_entry.restored_predecessors_lost": "Your account was restored. But part of the sealed backup — material from a previous identity of yours — could not be opened, so content still encrypted under that older identity may be unreadable. If another of your devices still has that identity, sign in there to finish moving your content over. ({reason})",
        "onboarding.recovery_entry.submit": "Restore",
        "onboarding.recovery_entry.superseded": "This identity was replaced after it was compromised. Import the new identity to continue.",
        "onboarding.recovery_entry.title": "Restore from Recovery Kit",
        "onboarding.recovery_entry.unreachable": "Could not reach that account's nest. Check the handle and your connection, then try again. If the domain itself is gone, enter your handle with your nest's address instead, like alice@192.0.2.10. ({reason})",
        "onboarding.recovery_kit.confirm": "I've saved it — continue",
        "onboarding.recovery_kit.desc": "This recovery phrase outranks your secret key — it is the only way to get your account back if your secret key is ever stolen or lost. Keep it offline; it is shown exactly once and never stored on any device.",
        "onboarding.recovery_kit.escrow_deferred": "Your kit activates when your account comes online at the end of setup — until then, keep the phrase safe.",
        "onboarding.recovery_kit.not_minted": "Recovery kit not minted yet",
        "onboarding.recovery_kit.skip": "Skip for now",
        "onboarding.recovery_kit.title": "Your Recovery Kit",
        "onboarding.retire.address_label": "Address",
        "onboarding.retire.cancel_button": "Cancel",
        "onboarding.retire.confirm_button": "Delete the server and clean up DNS",
        "onboarding.retire.confirm_by_hand": "To remove by hand afterwards: {records}",
        "onboarding.retire.confirm_current": "This is the server you're signed into. This session's nest will stop existing.",
        "onboarding.retire.confirm_destroyed": "Everything on this server — its disk and every account's data on it — is destroyed and cannot be recovered except from a backup.",
        "onboarding.retire.confirm_dns_none": "No DNS records will be removed automatically. The records to remove by hand are listed below.",
        "onboarding.retire.confirm_dns_removals": "Before the server is deleted, these DNS records pointing at it are removed: {records}",
        "onboarding.retire.confirm_domain": "Domain: {domain}",
        "onboarding.retire.confirm_name_label": "Type the server's name to confirm",
        "onboarding.retire.confirm_secondary_domains": "Also serves: {domains}",
        "onboarding.retire.confirm_what": "You are about to permanently delete {name} at {provider} ({address}).",
        "onboarding.retire.current_badge": "The server you're signed into",
        "onboarding.retire.delete_button": "Delete this server…",
        "onboarding.retire.domain_label": "Domain",
        "onboarding.retire.done_button": "Done",
        "onboarding.retire.done_deleted": "{name} has been deleted.",
        "onboarding.retire.empty_message": "No servers created by Fauna were found in this account. Servers set up some other way carry no Fauna marker and are not listed — retire those from your provider's dashboard.",
        "onboarding.retire.force_server_button": "Delete the server anyway",
        "onboarding.retire.force_server_warning": "Deleting the server now leaves DNS records pointing at an address your provider can hand to someone else. Remove them by hand right away — they are listed once the server is gone.",
        "onboarding.retire.leftover_copy": "Copy the list",
        "onboarding.retire.leftover_none": "Nothing is left to remove by hand.",
        "onboarding.retire.leftover_points_at_box": "Remove now — still points at the deleted server: {record}",
        "onboarding.retire.leftover_shared_name": "Stale, remove when convenient: {record}",
        "onboarding.retire.list_label": "Servers Fauna created in this account",
        "onboarding.retire.retry_button": "Try again",
        "onboarding.retire.secondary_domains_label": "Also serves",
        "onboarding.retire.step_dns": "DNS records",
        "onboarding.retire.step_server": "Server",
        "onboarding.retire.step_skipped_dns_step_forced_past": "Skipped — the server was deleted despite the failed DNS step.",
        "onboarding.retire.step_skipped_no_dns_credential": "No DNS credential reaches this domain's zone, so nothing was removed automatically — see the list to remove by hand.",
        "onboarding.retire.step_skipped_no_verified_domain": "No domain was verified for this server, so no DNS records were touched.",
        "onboarding.retire.step_skipped_no_zone_for_domain": "None of your DNS credentials holds this domain's zone, so nothing was removed automatically — see the list to remove by hand.",
        "onboarding.retire.subtitle": "Delete a server Fauna created in your cloud account and clean up the DNS records that pointed at it, or get your domain's transfer code. Your cloud token is used for this visit only and is never saved.",
        "onboarding.retire.title": "Retire a server",
        "onboarding.retire.transfer_code_available_after": "The registry locks this domain against transfer until {when}. Ask again after that — the code is never refused, only delayed.",
        "onboarding.retire.transfer_code_button": "Get the domain transfer code",
        "onboarding.retire.transfer_code_copy": "Copy code",
        "onboarding.retire.transfer_code_fetching": "Asking the registrar…",
        "onboarding.retire.transfer_code_from_registrar": "Get this domain's transfer code from your registrar's own dashboard.",
        "onboarding.retire.transfer_code_label": "Transfer code",
        "onboarding.retire.unmarked_note": "Not marked as created by Fauna — check the name carefully before deleting it",
        "onboarding.session_error.invalid_secret": "This device's saved secret key isn't valid: {message}",
        "onboarding.session_error.no_active_account": "Couldn't find the account that was just added.",
        "onboarding.session_error.persist_account": "Couldn't save your new account: {message}",
        "onboarding.session_error.persist_successor": "Couldn't save your new identity: {message}",
        "onboarding.session_error.remove_account": "Couldn't remove that account: {message}",
        "onboarding.session_error.switch_appended": "Couldn't switch to the new account: {message}",
        "onboarding.session_error.switch_successor": "Couldn't switch to your new identity: {message}",
        "onboarding.trust_prompt.grant_button": "Yes, trust this box",
        "onboarding.trust_prompt.nothing_to_grant": "There's nothing to decide yet — this box isn't running anything that would read your content. You can trust it later from Settings → Nests.",
        "onboarding.trust_prompt.skip_button": "Not now",
        "onboarding.trust_prompt.summary": "This box can do more for you if you let it read the things it looks after: filtering your mail as it arrives, and serving your calendar to your other devices. Your trust renews itself while you use Fauna, you can take it back at any time in Settings → Nests, and every time you give or withdraw trust it is written down for you there.",
        "onboarding.trust_prompt.title": "Trust this box?",
        "onboarding.vps_config.location_heading": "Location",
        "onboarding.vps_config.mail_mode_desc": "A mail box runs email (SMTP, IMAP) and calendar, which needs the spam and virus scanners and at least 2 GB of RAM. Turn this off for a social-only box: it runs lean and works on the cheapest 1 GB plan, but cannot add mail later without resizing the VPS.",
        "onboarding.vps_config.mail_mode_label": "Run mail on this box",
        "onboarding.vps_config.server_type_radio_legend": "Choose a VPS plan",
        "onboarding.vps_config.status_pick_location": "Choose where in the world your server runs to continue.",
        "onboarding.vps_config.status_pick_provider": "Choose who hosts your server to continue.",
        "onboarding.vps_config.status_pick_server_type": "Choose a plan for your server to continue.",
        "onboarding.vps_config.status_verify_credentials": "Enter this provider's credentials and press Verify to continue.",
        "onboarding.vps_config.title": "Configure VPS",
        "onboarding.vps_config.update_channel_dev_desc": "The newest development builds, before any checking. Expect breakage.",
        "onboarding.vps_config.update_channel_dev_label": "Dev",
        "onboarding.vps_config.update_channel_heading": "Updates",
        "onboarding.vps_config.update_channel_stable_desc": "Released versions. Recommended.",
        "onboarding.vps_config.update_channel_stable_label": "Stable",
        "onboarding.vps_config.update_channel_test_desc": "Release candidates that are still being checked.",
        "onboarding.vps_config.update_channel_test_label": "Test",
        "onboarding.welcome.subtitle": "Your personal, private communications nest.",
        "onboarding.welcome.tagline": "Private messaging and file sync,\nbuilt on your own server.",
        "onboarding.welcome.title": "Fauna",
        "p2p.copy_to_clipboard": "Copy to Clipboard",
        "p2p.delete_contact": "Delete Contact",
        "p2p.no_contacts": "No P2P contacts yet",
        "p2p.title": "P2P Contacts",
        "personalization.browse_catalog": "Browse labeler catalog",
        "personalization.clear_engagement_data": "Clear activity data",
        "personalization.feeds_link": "Feeds",
        "personalization.labelers_empty": "You haven’t subscribed to any community labelers yet.",
        "personalization.muted_words_link": "Muted words",
        "personalization.publish_corpus_size": "Built from {included} public examples of your {marked} marked posts.",
        "personalization.publish_exemplar_include": "Include",
        "personalization.publish_exemplars_empty": "This topic hasn’t scored any of the posts loaded so far. Open a feed it ranks, then try again.",
        "personalization.publish_exemplars_title": "Posts to include",
        "personalization.publish_kind_label": "Share as",
        "personalization.publish_kind_list": "List of posts",
        "personalization.publish_kind_model": "Word-pattern model",
        "personalization.publish_kind_unknown": "{kind}",
        "personalization.publish_limitation_note": "You’re sharing the posts below — not the topic itself or anything it learned about you. Only posts this device has already loaded and seen can be included, so the list won’t cover posts you never saw. It’s published anonymously: nothing links the list back to you or your account.",
        "personalization.publish_limitation_note_model": "You’re sharing the word patterns this topic learned — not the topic itself, and none of the posts. Unlike a list, a model also matches posts nobody here has seen yet. Only patterns that appear in at least 3 of your marked public posts are included, and that covers both what you marked as more like this and what you marked as less like this — the direction column below shows which is which. It’s published anonymously, and it’s built only from posts you marked by hand: nothing you merely read or watched goes into it.",
        "personalization.publish_name_blank": "A published name can’t be blank.",
        "personalization.publish_name_label": "Public name for this list",
        "personalization.publish_name_label_model": "Public name for this model",
        "personalization.publish_name_placeholder": "e.g. Small orange cats",
        "personalization.publish_name_too_long": "A published name is at most {max} characters.",
        "personalization.publish_ngram_count": "In {count} posts",
        "personalization.publish_ngram_direction_both": "Both",
        "personalization.publish_ngram_direction_less": "Less like this",
        "personalization.publish_ngram_direction_more": "More like this",
        "personalization.publish_ngrams_empty": "No word pattern appears in at least 3 of this topic’s public examples yet, so there’s nothing that can be shared without quoting a single post. Mark a few more public posts for this topic, then try again.",
        "personalization.publish_ngrams_title": "Word patterns to include",
        "personalization.publish_score": "Score {score}",
        "personalization.publish_sheet_title": "Publish this topic",
        "personalization.publish_submit": "Publish",
        "personalization.publish_vocabulary_empty": "This topic needs more public examples before it can be shared as a model.",
        "personalization.share_signals_label": "Share anonymous engagement signals",
        "personalization.share_signals_subtitle": "Off by default. When on, whether you watched or skipped a public post joins an anonymized count your nest shares — but only once at least 3 people here have the same verdict on the same post, and never your identity or your activity.",
        "personalization.share_signals_title": "Anonymous signal sharing",
        "personalization.signal_published_contributors": "contributors",
        "personalization.signal_published_description": "The anonymized signal and report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 contributors.",
        "personalization.signal_published_empty": "This nest publishes no signal aggregates yet",
        "personalization.signal_published_title": "What this nest publishes",
        "personalization.title": "Personalization",
        "personalization.trained_factor_blank_name": "A trained topic needs a name.",
        "personalization.trained_factor_cap": "You already have {max} trained topics — delete one to create another.",
        "personalization.trained_factor_create": "New trained topic",
        "personalization.trained_factor_engagement_toggle": "Learn from my activity",
        "personalization.trained_factor_examples": "{count} examples",
        "personalization.trained_factor_placeholder": "Topic name",
        "personalization.trained_factor_publish": "Publish…",
        "personalization.trained_factor_rename": "Rename",
        "personalization.trained_factor_save": "Save name",
        "personalization.trained_topics_empty": "You haven’t created any trained topics yet.",
        "personalization.trained_topics_title": "Trained topics",
        "photo_backup.auto_upload_desc": "New photos and videos will be automatically uploaded to your nest.",
        "photo_backup.back_up_photos": "Back up Photos library",
        "photo_backup.backed_up": "Backed up",
        "photo_backup.backup_in_progress": "Backup in progress...",
        "photo_backup.enable": "Enable Photo Backup",
        "photo_backup.error_prepare_set": "Could not prepare the photo library folder: {message}",
        "photo_backup.error_upload_item": "Failed to upload {uri}: {message}",
        "photo_backup.error_wifi_lost": "WiFi lost, sync paused",
        "photo_backup.last_backup": "Last backup",
        "photo_backup.last_sync": "Last Sync",
        "photo_backup.notification_complete": "Backup complete",
        "photo_backup.notification_progress": "Backing up photos... {uploaded}/{total}",
        "photo_backup.notification_starting": "Starting backup...",
        "photo_backup.pending_count": "{count} pending",
        "photo_backup.photo_access_required": "Photo library access required. Grant access in Settings.",
        "photo_backup.photos_access_granted": "Photos access granted",
        "photo_backup.photos_count": "{count} photos",
        "photo_backup.remaining_count": "{count} remaining",
        "photo_backup.sync_now": "Sync Now",
        "photo_backup.syncing": "Syncing {uploaded} of {total}...",
        "photo_backup.title": "Photo Backup",
        "photo_backup.wifi_only": "WiFi Only",
        "profile.block": "Block",
        "profile.blocked": "Blocked",
        "profile.copy_id": "Copy ID",
        "profile.edit": "Edit Profile",
        "profile.edit_add_link": "Add link",
        "profile.edit_avatar": "Avatar image path",
        "profile.edit_banner": "Banner image path",
        "profile.edit_bio": "Bio",
        "profile.edit_cancel": "Cancel",
        "profile.edit_display_name": "Display name",
        "profile.edit_link_label": "Label",
        "profile.edit_link_url": "URL",
        "profile.edit_remove_avatar": "Remove avatar",
        "profile.edit_remove_banner": "Remove banner",
        "profile.edit_remove_link": "Remove",
        "profile.edit_save": "Save",
        "profile.follow": "Follow",
        "profile.followers": "Followers",
        "profile.following": "Following",
        "profile.no_posts": "No posts",
        "profile.posts": "Posts",
        "profile.private_label_add": "Add label",
        "profile.private_label_empty": "Type a label before adding it",
        "profile.private_label_history_full": "This person has had too many labels — this one cannot be added",
        "profile.private_label_remove": "Remove",
        "profile.private_label_too_long": "Label is too long — keep it to {max} characters",
        "profile.private_labels": "Labels",
        "profile.private_nickname": "Nickname",
        "profile.private_nickname_too_long": "Nickname is too long — keep it to {max} characters",
        "profile.private_notes": "Notes",
        "profile.private_notes_too_long": "Notes are too long — keep them under {max} KB",
        "profile.private_save": "Save",
        "profile.private_save_failed": "Could not save your private notes: {reason}",
        "profile.private_title": "Only you can see this",
        "profile.private_too_many_labels": "This person already has {max} labels — remove one first",
        "profile.report": "Report account",
        "profile.request_contact": "Request contact",
        "profile.request_contact_sent": "Request sent",
        "profile.start_dm": "Message",
        "profile.tiers": "Tiers",
        "profile.title": "Profile",
        "profile.unblock": "Unblock",
        "provisioning.bundled.account": "Account",
        "provisioning.bundled.base_url": "Provider address (https://…)",
        "provisioning.bundled.help": "One company that registers your domain, hosts its DNS and rents you the server — you sign up and pay once, there. Paste the address the company gave you, then sign in; it must implement the open Fauna Bundled Provider API (the link above).",
        "provisioning.bundled.location": "Datacenter",
        "provisioning.bundled.name": "Bundled provider (open API)",
        "provisioning.cloudflare.account_id": "Account ID",
        "provisioning.cloudflare.api_token": "API token",
        "provisioning.cloudflare.help": "DNS and domain registration. Create an API token at dash.cloudflare.com → My Profile → API Tokens with Zone DNS and Registrar permissions; find your account ID on the Account home page's API section.",
        "provisioning.cloudflare.name": "Cloudflare",
        "provisioning.cloudflare.registrar_account_contact_note": "Cloudflare uses the contact details on your Cloudflare account for domain registration. Make sure they're set at dash.cloudflare.com → Domain Registration → Contacts before you continue.",
        "provisioning.cloudflare.zone": "DNS zone",
        "provisioning.digitalocean.api_token": "API token",
        "provisioning.digitalocean.help": "Create an API token at cloud.digitalocean.com → API → Generate New Token (read and write).",
        "provisioning.digitalocean.location": "Region",
        "provisioning.digitalocean.name": "DigitalOcean",
        "provisioning.gandi.domain": "Domain",
        "provisioning.gandi.help": "Create a Personal Access Token at account.gandi.net → Security → Personal Access Tokens.",
        "provisioning.gandi.name": "Gandi",
        "provisioning.gandi.personal_access_token": "Personal access token",
        "provisioning.hetzner.api_token": "Cloud API token",
        "provisioning.hetzner.help": "VPS and DNS. Create one Read & Write project token at console.hetzner.cloud — the same token manages both your server and your DNS.",
        "provisioning.hetzner.location": "Datacenter",
        "provisioning.hetzner.name": "Hetzner Cloud",
        "provisioning.hosted_auth.connect": "Sign in at the provider…",
        "provisioning.hosted_auth.connected": "Connected",
        "provisioning.hosted_auth.failed": "Sign-in failed: {message}",
        "provisioning.hosted_auth.pending": "Finish signing in in your browser — code {code}",
        "provisioning.linode.api_token": "API token",
        "provisioning.linode.help": "Create a Personal Access Token at cloud.linode.com → Profile → API Tokens.",
        "provisioning.linode.location": "Region",
        "provisioning.linode.name": "Linode (Akamai)",
        "provisioning.managed.disabled": "Managed subdomains on fauna.social are coming soon. For now, pick another option.",
        "provisioning.namecheap.api_key": "API key",
        "provisioning.namecheap.api_user": "API user",
        "provisioning.namecheap.domain": "Domain",
        "provisioning.namecheap.help": "Enable API access at namecheap.com → Profile → Tools → API Access. Namecheap also requires the calling IP address to be allowlisted there — if it isn't, the error message will name the exact address to add.",
        "provisioning.namecheap.name": "Namecheap",
        "provisioning.ovh.app_key": "Application key",
        "provisioning.ovh.app_secret": "Application secret",
        "provisioning.ovh.consumer_key": "Consumer key",
        "provisioning.ovh.help": "Create app credentials at api.ovh.com/createApp, then generate a consumer key via the OVH token endpoint for your app key and secret.",
        "provisioning.ovh.name": "OVH Cloud",
        "provisioning.ovh.project": "Project",
        "provisioning.porkbun.domain": "Domain",
        "provisioning.porkbun.help": "Registrar + DNS. Enable API access at porkbun.com → Account → API Access.",
        "provisioning.porkbun.key": "API key",
        "provisioning.porkbun.name": "Porkbun",
        "provisioning.porkbun.registrar_account_contact_note": "Porkbun uses the contact details on your Porkbun account for domain registration. Make sure they're set at porkbun.com/account/settings before you continue.",
        "provisioning.porkbun.secret": "Secret API key",
        "provisioning.verify_credentials": "Verify credentials",
        "provisioning.vultr.api_key": "API key",
        "provisioning.vultr.help": "Create an API key at my.vultr.com → Account → API.",
        "provisioning.vultr.location": "Region",
        "provisioning.vultr.name": "Vultr",
        "region.blocked_notice": "Not shown in {region} — blocked under the policy of {authority}",
        "region.collapsed_notice": "Hidden in {region} under the policy of {authority} — select to show",
        "region.declared": "Your region: {region}",
        "region.inert_notice": "Uses a policy format this app does not understand (version {version}) — nothing is blocked under it",
        "region.last_checked": "Last checked {time}",
        "region.malformed_notice": "This policy could not be read — nothing is blocked under it",
        "region.no_policy": "No regional content policy is in force",
        "region.none_declared": "No region is declared on this device",
        "region.policy_authority": "{region}: {authority}",
        "region.policy_version": "Version {sequence}, issued {issued}",
        "region.reveal_button": "Show",
        "region.section_title": "Region",
        "region.source_browser_locale": "From your browser language — change it in your browser settings",
        "region.source_storefront": "From your app store region — change it in your store account",
        "region.source_system_locale": "From your system locale — change it in your system settings",
        "region.source_system_region": "From your system region setting — change it in your system settings",
        "region.stale_warning": "Could not check for policy updates recently — the policies above stay in force",
        "registrar.available": "Available",
        "registrar.contact.address1": "Street address",
        "registrar.contact.city": "City",
        "registrar.contact.country": "Country",
        "registrar.contact.email": "Email",
        "registrar.contact.first_name": "First name",
        "registrar.contact.last_name": "Last name",
        "registrar.contact.phone": "Phone (+CC.number)",
        "registrar.contact.postal_code": "Postal code",
        "registrar.contact.state": "State/province",
        "registrar.domain": "Domain name",
        "registrar.price_confirm": "I accept this price and authorize the charge on my registrar account. Registration starts immediately when I continue, and I understand this means I give up any right to withdraw from this purchase.",
        "registrar.step.title": "Register a new domain",
        "registrar.unavailable": "Not available — try a different name",
        "search_page.all": "All",
        "search_page.badge_contact": "Contact",
        "search_page.badge_draft": "Draft",
        "search_page.badge_email": "Email",
        "search_page.badge_event": "Event",
        "search_page.badge_file": "File",
        "search_page.badge_media": "Media",
        "search_page.badge_message": "Message",
        "search_page.badge_post": "Post",
        "search_page.badge_profile": "Profile",
        "search_page.clear": "Clear",
        "search_page.hide_search_bar": "Hide search bar",
        "search_page.load_more_failed": "Load more failed",
        "search_page.no_results": "No results for",
        "search_page.no_results_short": "No results",
        "search_page.placeholder": "Search posts, profiles, email...",
        "search_page.search_failed": "Search failed",
        "search_page.search_failed_reason": "Search failed: {reason}",
        "search_page.search_messages": "Search messages...",
        "search_page.show_search_bar": "Show search bar",
        "search_page.sign_in_prompt": "Sign in to search.",
        "search_page.title": "Search Results",
        "sessions.act_failed": "That did not go through: {error}",
        "sessions.address_not_recorded": "address not recorded",
        "sessions.detail": "Signed in {created} · last active {last_used} · expires {expires} · {address}",
        "sessions.empty": "No sessions to show.",
        "sessions.kind_app": "App sign-in",
        "sessions.kind_device": "Device: {name}",
        "sessions.kind_unknown_device": "A device key not in your device list",
        "sessions.load_failed": "Could not load your sessions: {error}",
        "sessions.lockout_button": "Lock for 24 Hours",
        "sessions.lockout_confirm_placeholder": "Type LOCK to confirm",
        "sessions.lockout_warning": "Lock this account for 24 hours. Every device is signed out, this one too. Nobody can sign in for 24 hours, you included, and there is no unlock. It does not remove somebody who holds your secret key — they come back when the lock ends. They can do the same to you: anyone with your secret key can sign you out and lock the account, again every 24 hours. Your recovery kit still works while the account is locked and moves it to a new key the thief cannot use (Settings → Account → Recovery Kit → My Identity Was Stolen) — a lock you did not set is itself the sign to use it.",
        "sessions.lockout_wrong_word": "Type {word} exactly to lock the account.",
        "sessions.mark_this_app": "This app",
        "sessions.mark_this_device": "This device",
        "sessions.no_own_session": "This app does not know its own sign-in yet, so it cannot tell which one to keep. Try again in a moment.",
        "sessions.revoke": "Revoke",
        "sessions.revoke_note": "Revoking ends a sign-in and makes whoever held it prove themselves again. It does not sign a device out — a device that holds your secret key or a device grant signs itself straight back in. To end a device for good, remove it under Settings → Devices. If someone else holds your secret key, only your recovery kit ends them (Settings → Account → Recovery Kit → My Identity Was Stolen).",
        "sessions.revoke_others": "Sign Out Everywhere Else",
        "sessions.revoke_others_cancel": "Cancel",
        "sessions.revoke_others_confirm": "Yes, Sign Out Everywhere Else",
        "sessions.title": "Sessions",
        "settings.about": "About",
        "settings.about_desc": "Encrypted messaging, contacts, and file sync.",
        "settings.about_name": "Fauna for Windows",
        "settings.account_list_full": "Not added — this device's account list is full. Remove an account this device no longer uses, then try again.",
        "settings.account_page.accounts": "Accounts",
        "settings.account_page.accounts_subtitle": "Switch between the identities on this device, or add another.",
        "settings.account_page.actor_id": "Actor ID",
        "settings.account_page.add_account": "Add account",
        "settings.account_page.bluesky": "Bluesky",
        "settings.account_page.bluesky_handle_placeholder": "yourname.bsky.social",
        "settings.account_page.bridge_management": "Bridge Management",
        "settings.account_page.bridge_management_subtitle": "Open the Bridges section in the sidebar to link or manage accounts on other networks (Bluesky, ActivityPub, Nostr, Email)",
        "settings.account_page.bridges_description": "Connect to other social networks",
        "settings.account_page.change_handle": "Change Handle",
        "settings.account_page.connected_services": "Connected Services",
        "settings.account_page.copied_clipboard": "Copied to clipboard",
        "settings.account_page.data": "Data",
        "settings.account_page.data_export": "Data Export",
        "settings.account_page.delete_account": "Delete Account",
        "settings.account_page.delete_confirm_text": "This permanently removes your handle and data from this node. Your key is not affected.",
        "settings.account_page.delete_requested": "Account deletion scheduled",
        "settings.account_page.delete_subtitle": "Permanently delete your account and all data",
        "settings.account_page.export_dialog_title": "Export Account Data",
        "settings.account_page.export_my_data": "Export My Data",
        "settings.account_page.export_subtitle": "Download a copy of all your data, content included — it may be large",
        "settings.account_page.handle_changed": "Handle changed",
        "settings.account_page.identity": "Identity",
        "settings.account_page.link": "Link",
        "settings.account_page.new_handle": "New handle",
        "settings.account_page.new_handle_placeholder": "Enter new handle",
        "settings.account_page.node": "Node",
        "settings.account_page.open_new_instance": "Open in new window",
        "settings.account_page.open_new_instance_copied": "Copied: {command} — paste it into a new terminal window to open {account}",
        "settings.account_page.reauth_confirm": "Switch",
        "settings.account_page.reauth_prompt_body": "This account asks for confirmation before you switch to it. Switch to {account} now?",
        "settings.account_page.reauth_prompt_title": "Confirm account switch",
        "settings.account_page.reauth_reason": "switch to this account",
        "settings.account_page.require_confirm_toggle": "Require confirmation to switch",
        "settings.account_page.session": "Session",
        "settings.account_page.sign_out_subtitle": "Remove local credentials and return to onboarding. Your secret key is still required to sign back in.",
        "settings.account_page.title": "Account",
        "settings.account_page.unlink": "Unlink",
        "settings.account_page.usage": "Usage",
        "settings.admin_page.overview": "Overview",
        "settings.admin_page.total": "Total",
        "settings.appearance": "Appearance",
        "settings.autostart_header": "Start Fauna when you sign in",
        "settings.check_failed": "Check failed",
        "settings.check_for_updates": "Check for Updates",
        "settings.close_to_tray_header": "Close to tray",
        "settings.close_to_tray_subtitle": "Keep Fauna running in the system tray when the window is closed",
        "settings.configuration": "Configuration",
        "settings.configure_nest": "Configure Nest",
        "settings.configure_nest_desc": "Change the nest URL this app connects to.",
        "settings.danger_zone_desc": "Permanently delete your account and all associated data. This action cannot be undone.",
        "settings.data": "Data",
        "settings.delete_confirm_placeholder": "Type DELETE to confirm",
        "settings.encryption_page.available_key_packages": "Available key packages",
        "settings.encryption_page.error_key_count": "Could not load key count: {message}",
        "settings.encryption_page.error_refresh_keys": "Failed to refresh keys: {message}",
        "settings.encryption_page.low_key_warning_body": "Only {count} key package(s) remaining. Refresh to generate more and maintain end-to-end encryption availability.",
        "settings.encryption_page.low_key_warning_subtitle": "Generate new key packages to ensure uninterrupted encrypted messaging",
        "settings.encryption_page.low_key_warning_title": "Low Key Packages",
        "settings.encryption_page.mls_description": "Key material used for end-to-end encrypted group messaging",
        "settings.encryption_page.mls_key_packages": "MLS Key Packages",
        "settings.encryption_page.refresh_keys": "Refresh Keys",
        "settings.encryption_page.refresh_keys_description": "Generate and upload new key packages to your nest",
        "settings.encryption_page.title": "Encryption",
        "settings.errors.change_handle": "Failed to change handle",
        "settings.errors.create_filter": "Failed to create filter",
        "settings.errors.delete_account": "Deletion failed",
        "settings.errors.delete_filter": "Failed to delete filter",
        "settings.errors.disable_notifications": "Failed to disable notifications.",
        "settings.errors.enable_notifications": "Failed to enable notifications.",
        "settings.errors.export": "Export failed",
        "settings.errors.export_status": "Export failed: {status}",
        "settings.errors.keep_filter": "Failed to record that you recognise this rule",
        "settings.errors.publish_keys": "Failed to publish keys",
        "settings.errors.save_prefs": "Failed to save preferences",
        "settings.errors.task_assignment_stale": "That option is no longer available — reopen the page and try again",
        "settings.errors.train": "Training failed",
        "settings.errors.update_filter": "Failed to save filter",
        "settings.errors.update_inbox": "Failed to update inbox mode",
        "settings.exit_settings": "Exit settings",
        "settings.filter_name": "Filter name",
        "settings.general": "General",
        "settings.general_page.appearance_note": "Fauna follows your terminal's own color scheme — there is no separate theme picker here.",
        "settings.general_page.behaviour": "Behaviour",
        "settings.general_page.keyboard_shortcuts": "Keyboard Shortcuts",
        "settings.general_page.keyboard_shortcuts_description": "Shortcuts available while the application window is focused",
        "settings.general_page.launch_at_login": "Launch at login",
        "settings.general_page.launch_at_login_subtitle": "Start Fauna automatically when you log in",
        "settings.general_page.no_tray_subtitle": "No system tray detected — closing the window will quit Fauna",
        "settings.general_page.notification_sound": "Notification sound",
        "settings.general_page.notification_sound_subtitle": "Play a sound when a new message arrives",
        "settings.general_page.raise_window": "Raise window from tray",
        "settings.general_page.raise_window_subtitle": "Click the system tray icon to bring the window back",
        "settings.general_page.shortcut_close_window": "Close window / hide to tray",
        "settings.general_page.shortcut_compose_email": "Compose email (SMTP)",
        "settings.general_page.shortcut_hide_to_tray": "Hide to system tray",
        "settings.general_page.shortcut_minimize_to_tray": "Minimize to tray (when enabled)",
        "settings.general_page.shortcut_new_group": "New group",
        "settings.general_page.shortcut_new_message": "New message",
        "settings.general_page.shortcut_preferences": "Preferences",
        "settings.general_page.shortcut_quick_switcher": "Quick switcher",
        "settings.general_page.shortcut_quit": "Quit application",
        "settings.general_page.shortcut_switch_section": "Switch sidebar section",
        "settings.general_page.theme": "Theme",
        "settings.general_page.theme_dark": "Dark",
        "settings.general_page.theme_follow_system": "Follow System",
        "settings.general_page.theme_light": "Light",
        "settings.general_page.theme_subtitle": "Choose the application colour scheme",
        "settings.general_page.update_available": "{version} available",
        "settings.general_page.version": "Version",
        "settings.icloud_backup.footer": "When off (the default), your identity stays on this device and never syncs to iCloud. Turn it on to let iCloud Keychain restore your identity on a new device. Fauna's own device-add and recovery flows are the primary way to use multiple devices.",
        "settings.icloud_backup.title": "iCloud Backup",
        "settings.icloud_backup.toggle": "Back up identity to iCloud Keychain",
        "settings.identity_export.desc": "Show a QR code that another device can scan to import your identity.",
        "settings.identity_export.hide_qr": "Hide QR Code",
        "settings.identity_export.show_qr": "Show QR Code",
        "settings.identity_export.title": "Export Identity",
        "settings.identity_export.warning": "Anyone who scans this QR code gets full access to your identity. Only show it in a trusted environment.",
        "settings.inbox_mode": "Inbox Mode",
        "settings.mail.add_credential": "Add credential",
        "settings.mail.add_title": "Add mail credential",
        "settings.mail.autogenerate": "Auto-generate a strong password",
        "settings.mail.banner_subtitle": "Resume to complete it.",
        "settings.mail.banner_title": "A previous mail-credential rotation didn't finish.",
        "settings.mail.cancel": "Cancel",
        "settings.mail.copied": "Copied",
        "settings.mail.copy_secret": "Copy secret",
        "settings.mail.copy_secret_failed": "Could not copy secret: {error}",
        "settings.mail.copy_token": "Copy token",
        "settings.mail.copy_username": "Copy address",
        "settings.mail.created_prefix": "created",
        "settings.mail.credential_revoked": "Access revoked — set this mail app up again with a new password",
        "settings.mail.credentials_description": "Mail credentials you've created for your mail apps",
        "settings.mail.credentials_empty": "No mail credentials yet",
        "settings.mail.credentials_empty_subtitle": "Add a credential to connect a mail app",
        "settings.mail.credentials_title": "Credentials",
        "settings.mail.disable_confirm": "Disable mail",
        "settings.mail.disable_title": "Disable mail?",
        "settings.mail.disable_warning": "This revokes all your mail credentials and clears your mail encryption key. Third-party mail apps (IMAP/CalDAV) will stop working until you re-enable mail.",
        "settings.mail.done": "Done",
        "settings.mail.enable_subtitle": "Allow a third-party mail app to connect over IMAP and SMTP",
        "settings.mail.enable_title": "Enable mail",
        "settings.mail.hide": "Hide",
        "settings.mail.hide_secret": "Hide secret",
        "settings.mail.keys_title": "Mail encryption keys",
        "settings.mail.kind_bearer": "Bearer token",
        "settings.mail.kind_password": "Password",
        "settings.mail.last_used_never": "Never",
        "settings.mail.mua_auth": "Authentication",
        "settings.mail.mua_caldav_host": "CalDAV host",
        "settings.mail.mua_caldav_port": "CalDAV port",
        "settings.mail.mua_description": "Connection details to enter in your mail, calendar and file apps",
        "settings.mail.mua_imap_host": "IMAP host",
        "settings.mail.mua_imap_port": "IMAP port",
        "settings.mail.mua_smtp_host": "SMTP host",
        "settings.mail.mua_smtp_port": "SMTP port",
        "settings.mail.mua_title": "Mail, calendar & files app setup",
        "settings.mail.mua_username": "Username",
        "settings.mail.mua_webdav_url": "WebDAV URL",
        "settings.mail.name_placeholder": "Credential name (e.g. iPhone Mail)",
        "settings.mail.password_placeholder": "Password",
        "settings.mail.password_required": "Enter a password for this credential.",
        "settings.mail.resume": "Resume",
        "settings.mail.reveal_secret": "Reveal secret",
        "settings.mail.reveal_secret_failed": "Could not reveal secret: {error}",
        "settings.mail.revoke": "Revoke",
        "settings.mail.revoke_confirm": "Confirm?",
        "settings.mail.rotate_confirm": "Rotate keys",
        "settings.mail.rotate_exclude_caption": "Exclude compromised credentials (they lose access):",
        "settings.mail.rotate_keys": "Rotate mail keys",
        "settings.mail.rotate_title": "Rotate mail keys",
        "settings.mail.rotate_warning": "Rotating replaces your mail encryption key and re-wraps it under every surviving credential. Already-received mail stays readable. Every connected mail app must re-authenticate, and any credential you mark below as compromised loses access. This is safe to interrupt — it resumes automatically.",
        "settings.mail.secret_label": "Secret",
        "settings.mail.section_description": "Enable to set up third-party mail and calendar apps like Apple Mail, Thunderbird, and Apple Calendar.",
        "settings.mail.section_title": "Mail & Calendar",
        "settings.mail.show": "Show",
        "settings.mail.status_disabled": "Mail is disabled",
        "settings.mail.status_enabled": "All up to date",
        "settings.mail.status_rotation": "Rotation in progress ({count} remaining)",
        "settings.mail.status_syncing": "Syncing mail credentials…",
        "settings.mail.strength_fair": "Fair",
        "settings.mail.strength_strong": "Strong",
        "settings.mail.strength_weak": "Weak",
        "settings.mail.submit_add": "Add",
        "settings.mail.submit_enable": "Enable mail",
        "settings.mail.token_warning": "Copy this token into your mail app now — it is shown only once.",
        "settings.mail.type_selector": "Use a password (PLAIN) instead of a bearer token",
        "settings.mail.weak_password_warning": "A password you choose yourself limits how strongly your stored mail is protected at rest on an encrypted nest. Letting Fauna generate one is recommended.",
        "settings.member_review_page.empty": "There is nobody waiting for you to review.",
        "settings.member_review_page.intro": "These are the people you have not decided about since you recovered your account. Keep the ones you recognise, and remove anyone you do not from your groups.",
        "settings.member_review_page.title": "Members To Review",
        "settings.mls_available": "MLS encryption: Available",
        "settings.mls_not_available": "MLS encryption: Not available",
        "settings.moderation": "Moderation",
        "settings.moderation_page.title": "Content Moderation",
        "settings.nest_admin": "Nest Admin",
        "settings.nest_url": "Nest URL",
        "settings.new_nest_url": "New Nest URL",
        "settings.new_nest_url_placeholder": "https://nest.fauna.social",
        "settings.open_bridges": "Open Bridges",
        "settings.open_devices": "Open Devices",
        "settings.p2p_page.connection_group_description": "Network information for P2P connectivity",
        "settings.p2p_page.connection_group_title": "Connection",
        "settings.p2p_page.contacts_group_description": "P2P peers you can reach directly (local to this device)",
        "settings.p2p_page.contacts_group_title": "Contacts",
        "settings.p2p_page.lan_addresses": "LAN Addresses",
        "settings.p2p_page.lan_none": "(no active interfaces detected)",
        "settings.p2p_page.node_id": "Node ID",
        "settings.p2p_page.start": "Start",
        "settings.p2p_page.stop": "Stop",
        "settings.p2p_page.title": "P2P",
        "settings.p2p_page.tunnel_group_description": "Direct peer-to-peer connections between your devices",
        "settings.p2p_page.tunnel_group_title": "P2P Tunnel",
        "settings.p2p_page.tunnel_row_subtitle": "Start or stop the P2P tunnel",
        "settings.p2p_page.tunnel_row_title": "Tunnel",
        "settings.p2p_redirect": "Peer connections and bridges are managed on the Devices and Bridges pages.",
        "settings.pending_actions.admin_add": "Grant {target} the admin role",
        "settings.pending_actions.admin_change_role": "Change the admin role of {target}",
        "settings.pending_actions.admin_delete_user": "Delete the account {target}",
        "settings.pending_actions.admin_remove": "Revoke the admin role of {target}",
        "settings.pending_actions.applies": "Applies {time}",
        "settings.pending_actions.cancel": "Cancel",
        "settings.pending_actions.change_handle_to": "Change handle to {handle}",
        "settings.pending_actions.delete_account": "Delete this account",
        "settings.pending_actions.delete_snapshot": "Delete snapshot {snapshot}",
        "settings.pending_actions.none_scheduled": "Nothing is scheduled.",
        "settings.pending_actions.title": "Pending actions",
        "settings.pending_actions.title_count": "Pending actions ({count})",
        "settings.privacy": "Privacy",
        "settings.privacy_desc": "Control who can contact you.",
        "settings.privacy_page.action": "Action",
        "settings.privacy_page.add_filter": "Add Filter",
        "settings.privacy_page.apply_changes": "Apply changes",
        "settings.privacy_page.delete_filter": "Delete filter",
        "settings.privacy_page.edit_filter": "Edit filter",
        "settings.privacy_page.email_filters": "Email Filters",
        "settings.privacy_page.email_filters_description": "Rules applied to incoming messages",
        "settings.privacy_page.filter_inherited": "From before your account recovery — check you recognise this rule",
        "settings.privacy_page.filter_keep": "I recognise this",
        "settings.privacy_page.forward_address": "Forward to",
        "settings.privacy_page.inbox_mode_description": "Control who can send you messages",
        "settings.privacy_page.inbox_mode_subtitle": "Who can message you directly",
        "settings.privacy_page.inbox_mode_unknown": "Your current inbox mode has not loaded, so none of the four below is marked. Your setting is unchanged — reopen this page once your nest is reachable to see and change it.",
        "settings.privacy_page.keep_local_copy": "Keep a local copy",
        "settings.privacy_page.match_value": "Match value",
        "settings.privacy_page.new_filter": "New Filter",
        "settings.privacy_page.no_filters": "No filters",
        "settings.privacy_page.no_filters_configured": "No filters configured",
        "settings.privacy_page.no_filters_subtitle": "Add a filter to automatically sort or reject messages",
        "settings.privacy_page.phishing_threshold_subtitle": "Messages above this score are flagged as phishing",
        "settings.privacy_page.spam_preferences": "Spam Preferences",
        "settings.privacy_page.spam_protection": "Spam Protection",
        "settings.privacy_page.spam_protection_description": "Threshold scores for automatic filtering",
        "settings.privacy_page.spam_threshold_subtitle": "Messages above this score are marked as spam",
        "settings.privacy_page.title": "Privacy",
        "settings.publishing_key_packages": "Publishing key packages...",
        "settings.push_notifications.agent_unreachable": "The Fauna sync agent is not running on this computer, so it cannot show notifications while Fauna is closed.",
        "settings.push_notifications.description": "Receive notifications in this browser even when the app is not open.",
        "settings.push_notifications.device_description": "Get a notification for new messages, knocks and invites on this device, even when Fauna is closed.",
        "settings.push_notifications.no_sink": "This computer has no desktop notification service, so notifications cannot be shown here.",
        "settings.push_notifications.opt_in_label": "Notify me on this device",
        "settings.push_notifications.title": "Push Notifications",
        "settings.push_notifications.update_failed": "Failed to update push notification settings.",
        "settings.recovery_kit.action_failed": "Could not update your recovery kit: {message}",
        "settings.recovery_kit.backup_regrant_done": "Your backups are running again under your new identity.",
        "settings.recovery_kit.backup_regrant_failed": "Your backups could not be restarted under your new identity yet: {reason}. This will be retried the next time you sign in.",
        "settings.recovery_kit.backup_regrant_running": "Restarting your backups under your new identity…",
        "settings.recovery_kit.corpus_reseal_done": "Your files are now held under your new identity.",
        "settings.recovery_kit.corpus_reseal_failed": "Your files could not be moved to your new identity yet: {reason}. This will be retried the next time you sign in.",
        "settings.recovery_kit.corpus_reseal_owed_elsewhere": "Your files are still held under your previous identity, and this device does not have the key to move them. Sign in on the device you used to take back your account, and it will finish there.",
        "settings.recovery_kit.corpus_reseal_partly_owed": "Moving your files across to your new identity: {done} done, {remaining} still to go. This continues on its own, and picks up where it left off each time you sign in.",
        "settings.recovery_kit.corpus_reseal_running": "Moving your files across to your new identity…",
        "settings.recovery_kit.create": "Create Recovery Kit",
        "settings.recovery_kit.desc": "An offline key that can recover your account if you lose your identity secret — or take it back if someone steals it. It is shown once and never stored on this device.",
        "settings.recovery_kit.drafts_reseal_done": "Your unsent drafts are available again under your new identity.",
        "settings.recovery_kit.drafts_reseal_failed": "Your unsent drafts could not be recovered yet: {reason}. They are safe, and this will be retried the next time you sign in.",
        "settings.recovery_kit.drafts_reseal_owed_elsewhere": "Your unsent drafts are still held under your previous identity, and this device does not have the key to unlock them. They are safe. Sign in on the device you used to take back your account, and it will finish there.",
        "settings.recovery_kit.drafts_reseal_partly_owed_elsewhere": "Some of your unsent drafts are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there.",
        "settings.recovery_kit.drafts_reseal_running": "Recovering your unsent drafts under your new identity…",
        "settings.recovery_kit.escrow_reseal": "Restore Phrase Recovery",
        "settings.recovery_kit.grant_remint_done": "The access you had granted to services (mail filtering, search and similar) is restored under your new identity. Review each one on the Nests page — if your identity was stolen, the thief could have granted access you never did.",
        "settings.recovery_kit.grant_remint_failed": "The access you had granted to services could not be restored under your new identity yet: {reason}. This will be retried the next time you sign in.",
        "settings.recovery_kit.grant_remint_partial": "Some of the access you had granted to services could not be restored yet. This will be retried the next time you sign in — anything already restored is listed on the Nests page for review.",
        "settings.recovery_kit.grant_remint_running": "Restoring the access you had granted to services, under your new identity…",
        "settings.recovery_kit.inherited_filters": "{count} of your email filter rule(s) were set up before your account recovery and are still unchecked. A filter can silently bin or redirect incoming mail, so it is worth confirming you recognise each one — Settings ▸ Privacy ▸ Email Filters.",
        "settings.recovery_kit.kit_phrase_placeholder": "Paste your recovery phrase (fauna://recovery link or 64-character code)",
        "settings.recovery_kit.kit_phrase_required": "Paste the recovery phrase for this account first.",
        "settings.recovery_kit.let_go": "Let Go Of Unreadable Data",
        "settings.recovery_kit.let_go_confirm_placeholder": "Type LET GO to confirm",
        "settings.recovery_kit.let_go_done": "{retired} unreadable items were let go.",
        "settings.recovery_kit.let_go_failed": "Couldn't let the data go: {message}",
        "settings.recovery_kit.let_go_kept": "Some of this data became readable again and was kept.",
        "settings.recovery_kit.lost": "I Lost My Kit",
        "settings.recovery_kit.mail_burn_done": "Your mail encryption key was replaced and your {count} mail app password(s) were revoked — whoever held your previous identity could have used them to read your mail. Your mailbox keeps receiving as normal, but each mail app needs setting up again with a new password from this page.",
        "settings.recovery_kit.mail_burn_failed": "Your mail keys could not be replaced yet: {reason}. Until this finishes, anyone who had your previous identity can still read new mail. This will be retried the next time you sign in.",
        "settings.recovery_kit.mail_burn_running": "Replacing your mail keys, because your previous identity's passwords could open your mailbox…",
        "settings.recovery_kit.mls_reseal_done": "Your conversations are now held under your new identity.",
        "settings.recovery_kit.mls_reseal_failed": "Your conversations could not be moved to your new identity yet: {reason}. This will be retried the next time you sign in.",
        "settings.recovery_kit.mls_reseal_owed_elsewhere": "Your conversations are still held under your previous identity, and this device does not have the key to unlock them. Sign in on the device you used to take back your account, and it will finish there.",
        "settings.recovery_kit.mls_reseal_partly_owed_elsewhere": "Some of your conversations are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there.",
        "settings.recovery_kit.mls_reseal_running": "Unlocking your conversations under your new identity…",
        "settings.recovery_kit.replace": "Replace Using My Kit",
        "settings.recovery_kit.review_defer": "Review The Rest Later",
        "settings.recovery_kit.review_intro": "Keep the people you recognise. Anyone you don't, you can remove from your groups. You do not have to finish now.",
        "settings.recovery_kit.review_keep": "Keep",
        "settings.recovery_kit.review_reason_compromise": "was in your groups before you recovered your account",
        "settings.recovery_kit.review_reason_other": "this could not confirm their identity",
        "settings.recovery_kit.review_remove": "Remove From My Groups",
        "settings.recovery_kit.review_remove_done_here": "Removed {who} from {removed} of your group conversations.",
        "settings.recovery_kit.review_remove_folder_seats": "They are also in {seats} shared folder(s). Remove them in each folder's sharing settings — or leave the set, if it is not yours — then choose Remove again.",
        "settings.recovery_kit.review_remove_none_here": "{who} is not in any of your group conversations on this device.",
        "settings.recovery_kit.review_remove_partial": "Removed {who} from {removed} of {groups} of your group conversations. The rest still include them — try again, or ask another member of those groups to remove them.",
        "settings.recovery_kit.review_remove_unsynced_seats": "They are also in {seats} group chat(s) not yet synced to this device. Choose Remove again after syncing completes, or from a device that has those chats.",
        "settings.recovery_kit.review_row": "{who} — {reason}",
        "settings.recovery_kit.review_unknown_person": "Someone no longer in any of your groups",
        "settings.recovery_kit.review_verdict_failed": "Removed {who} from your group conversations, but your review list could not be updated yet: {reason}. They may still be listed here until it succeeds.",
        "settings.recovery_kit.status_failed": "Could not check your recovery kit: {message}",
        "settings.recovery_kit.status_loading": "Checking your recovery kit…",
        "settings.recovery_kit.status_never_created": "No recovery kit. If you lose your identity secret, your account cannot be recovered, and if someone steals it, you cannot take it back.",
        "settings.recovery_kit.status_registered": "Your recovery kit is active, and a sealed copy of your identity secret is stored for it.",
        "settings.recovery_kit.status_registered_no_escrow": "Your recovery kit is active, but no sealed copy of your identity secret is stored — your recovery phrase cannot recover this account right now. Enter your kit below and press Restore Phrase Recovery to fix this.",
        "settings.recovery_kit.status_replacement_pending": "A replacement of your recovery kit was requested with your identity secret alone, and takes effect in {days} days. Cancel it below if it was not you.",
        "settings.recovery_kit.stolen": "My Identity Was Stolen",
        "settings.recovery_kit.stolen_ceremony_failed": "Couldn't recover your account: {message}",
        "settings.recovery_kit.stolen_confirm_placeholder": "Type SUCCEED to confirm",
        "settings.recovery_kit.stolen_landed_for_another": "Your account has already been moved to a different identity ({actor}) — another device with the same recovery kit got there first. Import that identity to get back into your account.",
        "settings.recovery_kit.stolen_outcome_unknown_saved": "Couldn't confirm whether your account was recovered ({cause}). Your new identity is saved on this device — reopen the app to sign in with it. Details: {reported}",
        "settings.recovery_kit.stolen_outcome_unknown_unsaved": "Couldn't confirm whether your account was recovered ({cause}), and this device couldn't save your new identity. Write down this secret key now and import it — it is the only way back into your account: {secret}. Details: {reported}",
        "settings.recovery_kit.stolen_persist_failed": "Your account was re-pointed to a new identity, but saving it on this device failed. Write this secret key down NOW and import it — it is the only way back into your account: {secret}",
        "settings.recovery_kit.stolen_warning": "This mints a new identity and re-points your account to it. It cannot be undone, your old identity stops working, and you will need to re-add your devices. Your handle stays yours. Afterwards, create a new recovery kit — this one retires with the old identity.",
        "settings.recovery_kit.sweep_all_removed": "Your old identity was removed from all {groups} of your group conversations.",
        "settings.recovery_kit.sweep_failed": "Your account moved to your new identity, but your groups could not be updated: {reason}. Your old identity may still be able to read them.",
        "settings.recovery_kit.sweep_none": "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Press Finish Moving Your Groups below to complete it now, or ask another member of each group to remove the old identity and add your new one.",
        "settings.recovery_kit.sweep_none_no_retry": "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Ask another member of each group to remove the old identity and add your new one.",
        "settings.recovery_kit.sweep_partial": "Your old identity was removed from {removed} of your {groups} group conversations. It can still read the rest — press Finish Moving Your Groups below to finish, or ask another member of those to remove it.",
        "settings.recovery_kit.sweep_partial_no_retry": "Your old identity was removed from {removed} of your {groups} group conversations. It can still read the rest — ask another member of those to remove it.",
        "settings.recovery_kit.sweep_retry": "Finish Moving Your Groups",
        "settings.recovery_kit.sweep_retry_failed": "Your groups could not be updated: {reason}. Nothing was lost — press Finish Moving Your Groups to try once more.",
        "settings.recovery_kit.sweep_retry_landed_for_another": "This account was moved to a different identity than the one signed in here. Sign in as your current identity to finish moving your groups.",
        "settings.recovery_kit.sweep_retry_no_old_state": "This device does not have the conversation history from your previous identity, so it cannot finish the move. If another of your devices still has those conversations, press Finish Moving Your Groups there; otherwise ask another member of each group to remove the old identity and add your new one.",
        "settings.recovery_kit.sweep_retry_not_landed": "No move of this account has been recorded, so there is nothing to finish. If you have just taken your account back, wait a moment and try once more.",
        "settings.recovery_kit.sweep_unattested": "There are {count} other members across your groups that this cannot confirm you added yourself. If your identity was stolen, one of them could be the thief under another name.",
        "settings.recovery_kit.tier_period_rotation_done": "New posts to your {count} subscriber tier(s) are now locked with keys your previous identity never had. Your subscribers keep their access, and posts you published before taking your account back stay readable to them — and to anyone who had the old keys, which is why only new posts are covered.",
        "settings.recovery_kit.tier_period_rotation_failed": "The keys for your subscriber-only posts could not be replaced yet: {reason}. Until this finishes, anyone who had your previous identity can read the subscriber-only posts you publish from now on. This will be retried the next time you sign in.",
        "settings.recovery_kit.tier_period_rotation_nest_too_old": "The keys for your subscriber-only posts could not be replaced: this nest is too old to accept them. Until it is updated, anyone who had your previous identity can read the subscriber-only posts you publish from now on.",
        "settings.recovery_kit.tier_period_rotation_partial": "Keys were replaced for {count} of your subscriber tier(s), but {failed} could not be finished. Until they are, anyone who had your previous identity can read new posts to those tiers. This will be retried the next time you sign in.",
        "settings.recovery_kit.tier_period_rotation_running": "Replacing the keys your subscriber-only posts are locked with, because your previous identity could open them…",
        "settings.recovery_kit.title": "Recovery Kit",
        "settings.recovery_kit.unreadable_status": "{rows} items of your account data, saved since {since}, cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in since {since}, it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take.",
        "settings.recovery_kit.unreadable_status_undated": "{rows} items of your account data cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in for a while, it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take.",
        "settings.recovery_kit.veto": "Cancel The Pending Replacement",
        "settings.recovery_kit.veto_failed": "Could not cancel the pending replacement: {message}",
        "settings.remove_account_blocked_other_window": "Not removed — another Fauna window is using that account on this device. Close it, then remove the account again.",
        "settings.remove_account_blocked_this_window": "Not removed — this window is using that account. Close this window, then remove the account from another one.",
        "settings.rule_type": "Rule type",
        "settings.rule_value": "Rule value",
        "settings.show_in_dock": "Show in Dock",
        "settings.sign_out": "Sign Out",
        "settings.sign_out_blocked_other_window": "Still signed in — another Fauna window is using this account on this device. Close it, then sign out again.",
        "settings.sign_out_confirm": "Are you sure you want to sign out? You will need your secret key to sign back in.",
        "settings.sign_out_residue": "Signed out, but {count} item(s) of your data could not be removed from this device — another program may still be using them. Press Remove Again to try once more.",
        "settings.sign_out_residue_credentials": "Signed out, but your sign-in credentials could not be removed from this device — its secure storage may be locked or unavailable. Press Remove Again to try once more.",
        "settings.sign_out_residue_retry": "Remove Again",
        "settings.sign_out_residue_retry_blocked_other_window": "Not removed — another Fauna window is using some of this data. Close it, then press Remove Again.",
        "settings.sign_out_residue_with_credentials": "Signed out, but your sign-in credentials and {count} item(s) of your data could not be removed from this device. Press Remove Again to try once more.",
        "settings.spam_threshold": "Spam threshold",
        "settings.startup": "Startup",
        "settings.storage_desc": "Account storage usage.",
        "settings.switch_refused": "Not switched to {account} — you are still on the identity you were using.",
        "settings.switch_refused_no_secret": "Not switched — this device can no longer sign in as {account}: its secret key is missing here. You are still on the identity you were using. To use {account} here again, add it back with its secret key or recovery kit.",
        "settings.sync_page.add_location": "Add location",
        "settings.sync_page.add_location_subtitle": "Add the typed location path, bound to the named folder",
        "settings.sync_page.location_path": "Location path",
        "settings.sync_page.no_locations_synced": "No locations synced",
        "settings.sync_page.open_directory": "Open directory",
        "settings.sync_page.select_directory_dialog": "Select Directory to Sync",
        "settings.sync_page.synced_locations": "Synced Locations",
        "settings.sync_page.title": "Sync Settings",
        "settings.title": "Settings",
        "settings.up_to_date": "Up to date",
        "settings.update_available_notice": "Version {version} is available. Get it at {url}",
        "settings.update_button": "Update",
        "setup.back": "Back",
        "setup.byo.title": "Run Fauna on your server",
        "setup.byo_status.title": "Connecting to your nest",
        "setup.dns.description": "We need API access to your DNS provider to create records for {domain}.",
        "setup.dns.provision": "Provision",
        "setup.dns.title": "Configure DNS",
        "setup.dns.verified": "Token verified.",
        "setup.server.title": "Choose a server provider",
        "share_link.button": "Share a link",
        "share_link.cancel": "Cancel",
        "share_link.close": "Close",
        "share_link.copy": "Copy link",
        "share_link.create": "Create link",
        "share_link.create_body": "Anyone with the link can open this file until it expires or you revoke it.",
        "share_link.create_title": "Share a link to {name}",
        "share_link.creating": "Creating link…",
        "share_link.empty": "You haven't shared any links yet.",
        "share_link.error_create": "Couldn't create the link: {message}",
        "share_link.error_list": "Couldn't load your shared links: {message}",
        "share_link.error_revoke": "Couldn't revoke the link: {message}",
        "share_link.expires": "Expires {date}",
        "share_link.expiry_1d": "1 day",
        "share_link.expiry_1y": "1 year",
        "share_link.expiry_30d": "30 days",
        "share_link.expiry_7d": "7 days",
        "share_link.expiry_label": "Link expires after",
        "share_link.key_notice": "This file is private, so the link carries the key that unlocks it. Anyone holding the link can open the file — and wherever you paste it, anyone who can read that place can open it too.",
        "share_link.list_button": "Shared links",
        "share_link.list_loading": "Loading your shared links…",
        "share_link.list_title": "Your shared links",
        "share_link.revoke": "Revoke",
        "share_link.revoke_confirm": "Revoke link",
        "share_link.revoke_confirm_body": "The link to {name} stops working for everyone. This cannot be undone — you can make a new link at any time.",
        "share_link.revoke_confirm_title": "Revoke this link?",
        "share_link.state_active": "Active",
        "share_link.state_expired": "Expired",
        "share_link.state_revoked": "Revoked",
        "share_viewer.damaged": "This link could not be opened. Check that you copied the whole link.",
        "share_viewer.download": "Download",
        "share_viewer.generic_body": "Someone shared a file with you through Fauna. Open the complete link you were sent to see it.",
        "share_viewer.gone": "This link has expired or was revoked.",
        "share_viewer.keep_note": "A copy you download stays with you, even after the link stops working.",
        "share_viewer.loading": "Opening the file…",
        "share_viewer.not_found": "This link was not found. It may be mistyped, or the file is no longer stored.",
        "share_viewer.title": "A file shared with Fauna",
        "share_viewer.unavailable": "The file could not be loaded right now. Try again later.",
        "share_viewer.withheld": "This file is not available for legal reasons.",
        "size.bytes": "{value} B",
        "size.gb": "{value} GB",
        "size.kb": "{value} KB",
        "size.mb": "{value} MB",
        "size.tb": "{value} TB",
        "status.actions.clear_cache": "Clear Cache",
        "status.admin_section.dashboard": "Admin Dashboard",
        "status.admin_section.description": "You are an admin of this nest.",
        "status.admin_section.title": "Nest Administration",
        "status.build.commit": "Commit",
        "status.build.title": "Build",
        "status.build.verify_hint": "Verify this build: clone the repo at this commit, build locally, and compare file hashes.",
        "status.change_handle.changing": "Changing...",
        "status.change_handle.placeholder": "new-handle",
        "status.connection.service": "Service",
        "status.danger_zone.delete_hint": "Permanently removes your handle and data from this node. Your key is not affected.",
        "status.data_export.description": "Download all your data, content included, as a zip archive. It may be large.",
        "status.data_export.exported_to": "Data exported to {path}",
        "status.email_filters.action_add_label": "Label",
        "status.email_filters.action_allow": "Allow",
        "status.email_filters.action_auto_reply": "Auto-reply",
        "status.email_filters.action_discard": "Discard",
        "status.email_filters.action_file_into": "File",
        "status.email_filters.action_forward": "Forward",
        "status.email_filters.action_reject": "Reject",
        "status.email_filters.body_contains": "Body contains",
        "status.email_filters.header_exists": "Header exists",
        "status.email_filters.none": "No email filters configured.",
        "status.email_filters.sender_domain": "Sender domain",
        "status.email_filters.sender_is": "Sender is",
        "status.email_filters.subject_contains": "Subject contains",
        "status.encryption.available": "{count} available",
        "status.encryption.checking": "Checking encryption status...",
        "status.encryption.dm_channels": "DM Channels",
        "status.encryption.key_packages": "Key Packages",
        "status.encryption.low_keys": "Low key packages. Publishing more...",
        "status.encryption.mls_engine": "MLS Engine",
        "status.encryption.publishing": "Publishing...",
        "status.identity.not_configured": "Not configured",
        "status.inbox_privacy.allow_knock": "Allow Knocks",
        "status.inbox_privacy.allow_knock_desc": "New contacts must send a knock request first.",
        "status.inbox_privacy.closed": "Closed",
        "status.inbox_privacy.closed_desc": "No new messages accepted.",
        "status.inbox_privacy.contacts_only": "Contacts Only",
        "status.inbox_privacy.contacts_only_desc": "Only confirmed contacts can message you.",
        "status.inbox_privacy.description": "Control who can send you messages.",
        "status.inbox_privacy.open": "Open",
        "status.inbox_privacy.open_desc": "Anyone can message you directly.",
        "status.inbox_privacy.title": "Inbox Privacy",
        "status.node.title": "Node",
        "status.notifications.content_encrypted": "Notification content is encrypted end-to-end.",
        "status.notifications.denied_hint": "Enable notifications in system Settings to receive alerts.",
        "status.notifications.denied_title": "Notifications Disabled",
        "status.notifications.description": "Get notified when messages arrive, even when Fauna is closed.",
        "status.notifications.disable": "Disable Notifications",
        "status.notifications.disabling": "Disabling...",
        "status.notifications.enable": "Enable Notifications",
        "status.notifications.enabled": "Push notifications are enabled.",
        "status.notifications.enabling": "Enabling...",
        "status.notifications.open_settings": "Open Settings",
        "status.notifications.permission_denied": "Notification permission was denied.",
        "status.notifications.registered": "Registered with server",
        "status.notifications.title": "Push Notifications",
        "status.notifications.unavailable": "Push notifications are not available in this app build.",
        "status.p2p.title": "P2P",
        "status.p2p.tunnel": "Tunnel",
        "status.p2p.tunnel_active_with_address": "Active ({address})",
        "status.quota.inbox_usage": "Inbox Usage",
        "status.quota.storage_usage": "Storage Usage",
        "status.quota.title": "Quota",
        "status.self_host.title": "Run your own nest",
        "status.spam.aggressive": "Aggressive",
        "status.spam.description": "Adjust how aggressively spam is filtered from your inbox and feeds.",
        "status.spam.moderate": "Moderate",
        "status.spam.permissive": "Permissive",
        "status.spam.phishing_threshold": "Phishing threshold",
        "status.spam.save": "Save Spam Preferences",
        "status.spam.spam_threshold": "Spam threshold",
        "status.spam.title": "Spam Filtering",
        "status.sync.bytes_pending": "Bytes Pending",
        "status.sync.files_pending": "Files Pending",
        "status.sync.files_synced": "Files Synced",
        "status.sync.last_sync": "Last Sync",
        "status.sync.menu_status": "Sync: {status}",
        "status.sync.pending_summary": "{files} files, {bytes}",
        "status.sync.stopped": "Stopped",
        "status.sync.syncing": "Syncing",
        "status.sync_agent.keys_pending": "Keys pending",
        "status.sync_agent.not_enrolled": "Not enrolled",
        "status.sync_agent.not_running": "Not running",
        "status.sync_agent.restart_pending": "Restart pending",
        "status.sync_agent.running": "Running",
        "subscriptions.add_provider": "Add Provider",
        "subscriptions.approve": "Approve",
        "subscriptions.approving": "Approving — minting keys…",
        "subscriptions.asking_price": "Asking price (sats, optional)",
        "subscriptions.auto_approve": "Auto-approve",
        "subscriptions.cancel": "Cancel",
        "subscriptions.claim_code": "Claim code",
        "subscriptions.claim_status_redeemed": "Redeemed",
        "subscriptions.claim_status_unredeemed": "Unredeemed",
        "subscriptions.claim_status_voided": "Voided",
        "subscriptions.create_tier": "Create Tier",
        "subscriptions.delete": "Delete",
        "subscriptions.description": "Description",
        "subscriptions.edit": "Edit",
        "subscriptions.manual_claims": "Manual Claim Codes",
        "subscriptions.mint_claim": "Mint Code",
        "subscriptions.my_subscriptions": "My Subscriptions",
        "subscriptions.my_tiers": "My Tiers",
        "subscriptions.no_claims": "No claim codes yet",
        "subscriptions.no_offers": "This creator offers no subscription tiers yet",
        "subscriptions.no_providers": "No payment providers yet",
        "subscriptions.no_requests": "No pending requests",
        "subscriptions.no_subscribers": "No subscribers",
        "subscriptions.no_subscriptions": "You have no subscriptions yet",
        "subscriptions.no_tiers": "No tiers yet",
        "subscriptions.offer_status_active": "Subscribed",
        "subscriptions.offer_status_none": "Not subscribed",
        "subscriptions.offer_status_pending": "Pending approval",
        "subscriptions.offers": "Subscription Tiers",
        "subscriptions.paid": "Paid",
        "subscriptions.payment_providers": "Payment Providers",
        "subscriptions.payment_url": "Payment Link",
        "subscriptions.pending_requests": "Pending Requests",
        "subscriptions.price_hint": "Price",
        "subscriptions.provider_kind_label": "Provider:",
        "subscriptions.provider_status_configured": "Configured",
        "subscriptions.provider_status_error": "Error",
        "subscriptions.provider_status_verified": "Verified",
        "subscriptions.provider_tier_label": "Tier:",
        "subscriptions.rank": "Rank",
        "subscriptions.redeem": "Redeem",
        "subscriptions.redeem_claim_title": "Redeem a claim code",
        "subscriptions.reject": "Reject",
        "subscriptions.remove": "Remove",
        "subscriptions.save": "Save",
        "subscriptions.subscribe": "Subscribe",
        "subscriptions.subscribers": "Subscribers",
        "subscriptions.tier_name": "Name",
        "subscriptions.tier_select_label": "Tier:",
        "subscriptions.title": "Subscriptions",
        "subscriptions.unsafe_payment_url": "This payment link is unsafe (links must be https). Not opening it.",
        "subscriptions.unsubscribe": "Unsubscribe",
        "subscriptions.webhook_secret": "Webhook secret",
        "subscriptions.webhook_url_label": "Webhook URL:",
        "task_delegation.assignment_automatic": "Automatic",
        "task_delegation.assignment_other_name": "{name}",
        "task_delegation.assignment_this_device": "This device",
        "task_delegation.description": "Heavy background tasks run on one capable, always-on device — a nest or a plugged-in computer — and stay off battery phones. Each task picks its device automatically; pin one if you prefer.",
        "task_delegation.error_device_id": "This device could not load its own identity, so task assignments cannot be shown or changed here. Restart the app; if that does not help, its local data directory may not be writable.",
        "task_delegation.kind_backup_upload": "Backup uploads",
        "task_delegation.kind_content_rescore": "Content re-scoring",
        "task_delegation.kind_index": "Search indexing",
        "task_delegation.runner_other_device": "Running on {device}",
        "task_delegation.runner_this_device": "Running on this device",
        "task_delegation.runner_waiting": "Waiting for an eligible device",
        "task_delegation.title": "Task delegation",
        "time.countdown_dh": "{days}d {hours}h",
        "time.countdown_h": "{hours}h",
        "time.days_ago": "{count}d ago",
        "time.hours_ago": "{count}h ago",
        "time.just_now": "just now",
        "time.minutes_ago": "{count}m ago",
        "time.month_apr": "Apr",
        "time.month_aug": "Aug",
        "time.month_dec": "Dec",
        "time.month_feb": "Feb",
        "time.month_full_apr": "April",
        "time.month_full_aug": "August",
        "time.month_full_dec": "December",
        "time.month_full_feb": "February",
        "time.month_full_jan": "January",
        "time.month_full_jul": "July",
        "time.month_full_jun": "June",
        "time.month_full_mar": "March",
        "time.month_full_may": "May",
        "time.month_full_nov": "November",
        "time.month_full_oct": "October",
        "time.month_full_sep": "September",
        "time.month_jan": "Jan",
        "time.month_jul": "Jul",
        "time.month_jun": "Jun",
        "time.month_mar": "Mar",
        "time.month_may": "May",
        "time.month_nov": "Nov",
        "time.month_oct": "Oct",
        "time.month_sep": "Sep",
        "time.uptime_dhm": "{days}d {hours}h {mins}m",
        "time.uptime_hm": "{hours}h {mins}m",
        "time.uptime_m": "{mins}m",
        "time.weekday_fri": "Fri",
        "time.weekday_full_fri": "Friday",
        "time.weekday_full_mon": "Monday",
        "time.weekday_full_sat": "Saturday",
        "time.weekday_full_sun": "Sunday",
        "time.weekday_full_thu": "Thursday",
        "time.weekday_full_tue": "Tuesday",
        "time.weekday_full_wed": "Wednesday",
        "time.weekday_mon": "Mon",
        "time.weekday_sat": "Sat",
        "time.weekday_sun": "Sun",
        "time.weekday_thu": "Thu",
        "time.weekday_tue": "Tue",
        "time.weekday_wed": "Wed",
        "time.yesterday": "Yesterday",
        "tips.amount_unknown": "Amount not reported",
        "tips.count": "{count} tips",
        "tips.count_one": "1 tip",
        "tips.list_open": "Who tipped",
        "tips.list_title": "Tips",
        "tips.more": "and {count} more",
        "tips.msats": "{value} msats",
        "tips.sats": "{value} sats",
        "tips.sender_unknown": "Someone",
        "tui_nav_hints.move_focus": "{keys} move",
        "tui_nav_hints.next": "{keys} next",
        "tui_nav_hints.open": "{keys} open",
        "tui_nav_hints.pane": "{keys} pane",
        "tui_nav_hints.quit": "{keys} quit",
        "tui_settings.external_media_always": "Always open",
        "tui_settings.external_media_ask": "Ask each time",
        "tui_settings.external_media_label": "Play audio & video externally",
        "tui_settings.external_media_never": "Never open (show details only)",
        "tui_settings.external_media_subtitle": "This terminal can't play audio or video inline, so it can hand a clip to your system's default player. Choose whether it asks first, always opens, or never opens.",
        "tui_settings.title": "Terminal",
        "tui_unlock.confirm_label": "Confirm passphrase",
        "tui_unlock.create_button": "Set passphrase",
        "tui_unlock.create_prompt": "No secure OS key store is reachable here, so your sign-in keys will rest in a file protected by a passphrase. Choose one to continue — you'll need it at every launch.",
        "tui_unlock.create_title": "Protect your credentials",
        "tui_unlock.error_empty": "Enter a passphrase",
        "tui_unlock.error_failed": "Could not open the credential store: {message}",
        "tui_unlock.error_mismatch": "The two entries don't match",
        "tui_unlock.error_wrong": "Wrong passphrase, or the store file is corrupt",
        "tui_unlock.passphrase_label": "Passphrase",
        "tui_unlock.unlock_button": "Unlock",
        "tui_unlock.unlock_prompt": "Your credentials are protected by a passphrase on this machine. Enter it to sign in.",
        "tui_unlock.unlock_title": "Unlock your credentials",
        "web_publish.copied_link": "Copied: {url}",
        "web_publish.copied_paywall_link": "Copied, works for about 10 minutes: {url}",
        "web_publish.copy_paywall_link": "Copy paywall link",
        "web_publish.copy_web_link": "Copy web link",
        "web_publish.error_paywall_link": "Failed to create paywall link: {message}",
        "web_publish.error_publish": "Failed to publish post: {message}",
        "web_publish.error_unpublish": "Failed to unpublish post: {message}",
        "web_publish.menu_no_link_reason": "These links need a web address. Turn on your website in Settings → Web.",
        "web_publish.paywall_link_note": "A paywall link opens the full post for anyone, but only for about 10 minutes — it's for a quick preview, not for giving lasting free access. For that, send a claim code instead.",
        "web_publish.publish_to_web": "Publish to web",
        "web_publish.unpublish": "Unpublish",
        "web_settings.content_info": "Add content by turning on a folder's website toggle under Settings → Folders, or by publishing individual posts to the web.",
        "web_settings.link_disabled_no_handle": "Set a handle first — your published posts need a web address before they can be linked to.",
        "web_settings.link_disabled_reserved": "Your handle is a reserved name, so your published posts have no public address to link to.",
        "web_settings.link_disabled_subdomain_off": "Turn on \"Publish my website\" above to get links you can share.",
        "web_settings.published_post_gated_badge": "Paid: {tier}",
        "web_settings.published_posts_empty": "No published posts yet",
        "web_settings.published_posts_title": "Published posts",
        "web_settings.render_status_down": "Your published pages are temporarily unavailable. This nest is restoring them by itself — there is nothing you need to do. Synced files are not affected.",
        "web_settings.subdomain_no_handle": "Set a handle first to get a personal web address.",
        "web_settings.subdomain_no_serving_domain": "This nest has no web address yet, so it can't serve websites. An admin needs to give it a domain first.",
        "web_settings.subdomain_reserved": "Your handle is a reserved name and can't host a website.",
        "web_settings.subdomain_toggle_label": "Publish my website",
        "web_settings.subdomain_toggle_subtitle": "When on, this nest serves your Web files and web-published posts at your personal address. Off by default.",
        "web_settings.subdomain_url_label": "Your site",
        "web_settings.title": "Web",
        "widget.description": "Shows unread message count and quick compose",
        "widget.unread_label": "unread",
    ]
}
