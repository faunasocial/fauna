# AUTO-GENERATED from i18n/strings/en.yaml — do not edit


class _Common:
    cancel = "Cancel"
    save = "Save"
    delete = "Delete"
    accepted = "Accepted"
    pending = "Pending"
    confirmed = "Confirmed"
    blocked = "Blocked"
    unknown = "Unknown"
    loading = "Loading..."
    still_loading = "Still loading — try again in a moment"
    retry = "Retry"
    check = "Check"
    verify = "Verify"
    enable = "Enable"
    disable = "Disable"
    load_failed = "Failed to load"
    not_found = "Not found"
    search = "Search"
    clear_search = "Clear search"
    sort = "Sort"
    create = "Create"
    edit = "Edit"
    close = "Close"
    dialog_already_open = "Close the open dialog first."
    quit = "Quit"
    exit_fauna = "Exit Fauna"
    app_name = "Fauna"
    back = "Back"
    next = "Next"
    done = "Done"
    no = "No"
    ok = "OK"
    error = "Error"
    refresh = "Refresh"
    settings = "Settings"
    confirm = "Confirm"
    confirm_q = "Confirm?"
    send = "Send"
    sending = "Sending..."
    dismiss = "Dismiss"
    remove = "Remove"
    follow = "Follow"
    unfollow = "Unfollow"
    copy = "Copy"
    copied = "Copied!"
    archive = "Archive"
    today = "Today"
    upcoming = "Upcoming"
    post = "Post"
    mute = "Mute"
    add = "Add"
    change = "Change"
    apply = "Apply"
    reply = "Reply"
    reply_all = "Reply All"
    replying_to = "Replying to"
    name = "Name"
    mode = "Mode"
    value = "Value"
    type = "Type"
    verified = "Verified"
    sign_in_required = "Sign in to access this feature."
    identity_required = "Set up your identity in the Status tab first."
    enabled = "Enabled"
    disabled = "Disabled"
    creating = "Creating..."
    saving = "Saving..."
    saved = "Saved!"
    previous = "Previous"
    prune = "Prune"
    continue_ = "Continue"
    this_nest = "this nest"
    export_action = "Export"
    accept = "Accept"
    decline = "Decline"
    leave = "Leave"
    block = "Block"
    connect = "Connect"
    connected = "Connected"
    connected_realtime = "Connected (real-time)"
    disconnected = "Disconnected"
    cannot_connect = "Cannot connect"
    needs_nest = "Needs a connection to your nest"
    needs_other_device = "Waiting for another of your devices: open the app on it, or remove it under Devices if it's gone"
    inactive = "Inactive"
    active = "Active"
    starting = "Starting..."
    linking = "Linking..."
    unlinking = "Unlinking..."
    linked = "Linked"
    not_linked = "Not linked"
    not_connected = "Not connected"
    refreshing = "Refreshing..."
    checking = "Checking..."
    verifying = "Verifying..."
    actor_id = "Actor ID"
    device_id = "Device ID"
    danger_zone = "Danger Zone"
    node_url = "Node URL"
    no_messages_yet = "No messages yet."
    @staticmethod
    def fmt_question(*, text: str) -> str:
        return f"{text}?"
    @staticmethod
    def fmt_exclamation(*, text: str) -> str:
        return f"{text}!"
    @staticmethod
    def fmt_ellipsis(*, text: str) -> str:
        return f"{text}..."
    available = "Available"
    searching = "Searching..."
    domain = "Domain"
    provider = "Provider"
    connecting = "Connecting..."
    feed = "Feed"
    posts = "Posts"
    load_more = "Load More"
    messages = "Messages"
    size = "Size"
    contacts = "Contacts"
    open = "Open"
    closed = "Closed"
    admin = "Admin"
    bridges = "Bridges"
    moderation = "Moderation"
    navigation = "Navigation"
    more = "More"
    notifications = "Notifications"
    no_notifications = "No notifications"
    mark_all_read = "Mark All Read"
    @staticmethod
    def unread_count(*, count: str) -> str:
        return f"{count} unread"
    identity = "Identity"
    files = "Files"
    path = "Path"
    status = "Status"
    no_folders_configured = "No folders configured."
    never = "Never"
    retention_policy = "Retention Policy"
    snapshots = "Snapshots"
    devices = "Devices"
    delete_folder = "Delete Folder"
    sync = "Sync"
    peers = "Peers"
    account = "Account"
    actions = "Actions"
    storage = "Storage"
    tier = "Tier"
    save_preferences = "Save Preferences"
    users = "Users"
    users_by_tier = "Users by Tier"
    inbox = "Inbox"
    handle = "Handle"
    download = "Download"
    toggle_sidebar = "Toggle sidebar"
    @staticmethod
    def success_detail(*, detail: str) -> str:
        return f"Success: {detail}"


class _Time:
    just_now = "just now"
    @staticmethod
    def minutes_ago(*, count: str) -> str:
        return f"{count}m ago"
    @staticmethod
    def hours_ago(*, count: str) -> str:
        return f"{count}h ago"
    @staticmethod
    def days_ago(*, count: str) -> str:
        return f"{count}d ago"
    yesterday = "Yesterday"
    weekday_mon = "Mon"
    weekday_tue = "Tue"
    weekday_wed = "Wed"
    weekday_thu = "Thu"
    weekday_fri = "Fri"
    weekday_sat = "Sat"
    weekday_sun = "Sun"
    weekday_full_mon = "Monday"
    weekday_full_tue = "Tuesday"
    weekday_full_wed = "Wednesday"
    weekday_full_thu = "Thursday"
    weekday_full_fri = "Friday"
    weekday_full_sat = "Saturday"
    weekday_full_sun = "Sunday"
    month_jan = "Jan"
    month_feb = "Feb"
    month_mar = "Mar"
    month_apr = "Apr"
    month_may = "May"
    month_jun = "Jun"
    month_jul = "Jul"
    month_aug = "Aug"
    month_sep = "Sep"
    month_oct = "Oct"
    month_nov = "Nov"
    month_dec = "Dec"
    month_full_jan = "January"
    month_full_feb = "February"
    month_full_mar = "March"
    month_full_apr = "April"
    month_full_may = "May"
    month_full_jun = "June"
    month_full_jul = "July"
    month_full_aug = "August"
    month_full_sep = "September"
    month_full_oct = "October"
    month_full_nov = "November"
    month_full_dec = "December"
    @staticmethod
    def uptime_dhm(*, days: str, hours: str, mins: str) -> str:
        return f"{days}d {hours}h {mins}m"
    @staticmethod
    def uptime_hm(*, hours: str, mins: str) -> str:
        return f"{hours}h {mins}m"
    @staticmethod
    def uptime_m(*, mins: str) -> str:
        return f"{mins}m"
    @staticmethod
    def countdown_dh(*, days: str, hours: str) -> str:
        return f"{days}d {hours}h"
    @staticmethod
    def countdown_h(*, hours: str) -> str:
        return f"{hours}h"


class _Size:
    @staticmethod
    def bytes(*, value: str) -> str:
        return f"{value} B"
    @staticmethod
    def kb(*, value: str) -> str:
        return f"{value} KB"
    @staticmethod
    def mb(*, value: str) -> str:
        return f"{value} MB"
    @staticmethod
    def gb(*, value: str) -> str:
        return f"{value} GB"
    @staticmethod
    def tb(*, value: str) -> str:
        return f"{value} TB"


class _Tips:
    @staticmethod
    def sats(*, value: str) -> str:
        return f"{value} sats"
    @staticmethod
    def msats(*, value: str) -> str:
        return f"{value} msats"
    @staticmethod
    def count(*, count: str) -> str:
        return f"{count} tips"
    count_one = "1 tip"
    list_title = "Tips"
    list_open = "Who tipped"
    sender_unknown = "Someone"
    amount_unknown = "Amount not reported"
    @staticmethod
    def more(*, count: str) -> str:
        return f"and {count} more"


class _OnboardingWelcome:
    title = "Fauna"
    subtitle = "Your personal, private communications nest."
    tagline = "Private messaging and file sync,\nbuilt on your own server."


class _OnboardingLaunch:
    transient_error = "Couldn't reach your nest. Check your connection and try again."
    use_different_nest = "Use a different nest"
    launch_failed = "Startup hit a problem. Your data is safe — you can retry, or continue setup below."
    identity_changed_warning = "This nest's identity has changed, or it can no longer prove the identity you previously trusted. It may have been re-deployed or had its key rotated — or someone may be impersonating it. Don't continue unless you were expecting this change."
    identity_changed_trust = "Trust this nest and continue"
    identity_superseded = "This identity was succeeded — import the new identity to continue. Your account now belongs to a new identity, and this one can no longer sign in."
    @staticmethod
    def identity_superseded_verified(*, successor: str) -> str:
        return f"This identity was succeeded — import the new identity to continue. Your account now belongs to {successor}."
    recover_lost_box = "Recover a lost box"
    retire_server = "Retire a server"
    index_newer_build = "Your accounts were saved by a newer version of this app, so this version can't read them. Nothing has been lost — update the app and they'll be here."
    index_malformed = "Your saved accounts can't be read, and updating the app won't help. Nothing has been changed or deleted. You can start over on this device, or move this device's data somewhere safe first."
    index_malformed_reset_residual = "Starting over removes every account from this device and returns the app to a fresh install. Your accounts still exist on your nest, but you will need your secret key or recovery kit to sign back in — this device's copy is removed. Some saved data on this device cannot be reached to clear it."
    index_newer_build_title = "Update needed"
    index_malformed_title = "Your saved accounts can't be read"
    index_malformed_reset = "Start over on this device"
    index_malformed_reset_confirm = "Remove everything and start over"
    index_malformed_reset_blocked_other_window = "Nothing was removed — another Fauna window is using an account on this device. Close it, then start over again."
    sign_in_refused = "This nest no longer signs you in. Its admin may have suspended or removed your account — nothing on this device has changed. Contact the admin; if they restore your account, try again."
    sign_in_refused_title = "Can't sign in here"
    account_locked = "This account is locked and nobody can sign in until the lock ends."
    @staticmethod
    def account_locked_until(*, time: str) -> str:
        return f"The lock ends {time}."
    account_locked_not_yours = "If you did not lock it, somebody else holds your secret key, and they can lock it again. Your recovery kit moves the account to a new key they do not have — the lock does not stop that."
    account_locked_title = "Account locked"


class _OnboardingInstance_chooser:
    title = "Fauna is already open"
    @staticmethod
    def subtitle(*, account: str) -> str:
        return f"This window can't open {account} — it's already running. Choose another account, or switch to the open window."
    choose_account = "Open a different account"
    none_available = "Every account you've added is already open in another window."
    focus_existing = "Switch to the open window"
    add_account = "Log in as a new user"
    account_taken = "That account was just opened in another window. Pick another."
    no_running_instance = "Couldn't switch to the window running this account. Close it and launch Fauna again."


class _OnboardingIdentity_choice:
    title = "Set Up Your Identity"
    subtitle = "Your identity is an encryption key that belongs only to you."
    create_new = "Create New Identity"
    import_existing = "Import from Another Device"
    recover_lost_box = "Recover a lost box"
    restore_from_recovery_kit = "Restore my account from a recovery phrase"


class _OnboardingAdd_account:
    cancel = "Cancel"


class _OnboardingIdentity_created:
    title = "Identity Created"
    desc = "Your new identity has been generated. Save this secret key — it's your only way to recover your account."
    secret_key_label = "Your secret key:"
    warning = "Write this down or save it in a password manager. If you lose it, your account cannot be recovered."
    continue_ = "Continue"
    not_generated = "Identity not generated yet"


class _OnboardingRecovery_kit:
    title = "Your Recovery Kit"
    desc = "This recovery phrase outranks your secret key — it is the only way to get your account back if your secret key is ever stolen or lost. Keep it offline; it is shown exactly once and never stored on any device."
    escrow_deferred = "Your kit activates when your account comes online at the end of setup — until then, keep the phrase safe."
    confirm = "I've saved it — continue"
    skip = "Skip for now"
    not_minted = "Recovery kit not minted yet"


class _OnboardingRecovery_entry:
    title = "Restore from Recovery Kit"
    phrase_label = "Recovery phrase"
    desc = "Paste your recovery phrase (the fauna://recovery link or the 64-character code). Your identity will be restored from your nest's sealed escrow."
    account_hint = "If your phrase doesn't name your account, enter your handle (user@domain) so your nest can be found. If that domain is gone, put your nest's own address after the @ instead — like alice@192.0.2.10 or alice@nest.local."
    submit = "Restore"
    invalid_kit = "That isn't a recovery phrase. Paste the fauna://recovery link or the 64-character recovery code — not your secret key."
    account_needed = "Enter the handle of the account you're restoring (user@domain) — this phrase doesn't say which account it belongs to."
    account_malformed = "Enter the full handle including the domain, like alice@fauna.social — or, if that domain is gone, your handle followed by your nest's address, like alice@192.0.2.10."
    @staticmethod
    def account_unknown(*, account: str) -> str:
        return f"No account named {account} exists on that nest."
    no_escrow = "This account has no sealed backup to restore from. Create a new recovery kit from a device that's still signed in."
    superseded = "This identity was replaced after it was compromised. Import the new identity to continue."
    @staticmethod
    def refused(*, reason: str) -> str:
        return f"That recovery phrase was refused — it may have been replaced by a newer kit, or belong to another account. ({reason})"
    @staticmethod
    def unreachable(*, reason: str) -> str:
        return f"Could not reach that account's nest. Check the handle and your connection, then try again. If the domain itself is gone, enter your handle with your nest's address instead, like alice@192.0.2.10. ({reason})"
    @staticmethod
    def restored_predecessors_lost(*, reason: str) -> str:
        return f"Your account was restored. But part of the sealed backup — material from a previous identity of yours — could not be opened, so content still encrypted under that older identity may be unreadable. If another of your devices still has that identity, sign in there to finish moving your content over. ({reason})"


class _OnboardingIdentity_import:
    title = "Import Identity"
    scan_subtitle = "Scan the QR code shown on your other device."
    paste_subtitle = "Paste the secret key from your other device."
    paste_label = "Secret key"
    paste_placeholder = "64-character hex secret key"
    import_ = "Import"
    scan_tab = "Scan QR Code"
    paste_tab = "Paste Secret Key"
    invalid_qr = "Not a valid Fauna identity QR code."
    invalid_secret = "Secret key must be 64 hex characters."
    camera_unavailable = "Camera Not Available"
    camera_unavailable_hint = "Use the paste tab instead."


class _OnboardingRecovery:
    title = "Recover a lost box"
    subtitle = "Choose the box you lost. Fauna re-provisions a fresh box with its saved identity, so your pinned devices reconnect automatically once it's back online."
    box_list_label = "Your boxes"
    box_item_hint = "Recovery key custodied"
    empty_message = "No recovery keys are available yet. Connect to one of your other nests first — its synced configuration holds the recovery keys for every box you administer."
    method_cloud = "Re-provision on a cloud host"
    method_selfhosted = "Install on my own server"
    selfhosted_title = "Install on your own server"
    selfhosted_desc = "Run the installer below on a fresh box. It carries your saved deployment identity, so the rebuilt box re-presents the same nest identity and your pinned devices reconnect automatically once its DNS is re-pointed."
    selfhosted_command_pending = "The installer command with your recovery seed will appear here."
    selfhosted_continue = "Done"
    restore_cta = "Restore your data"


class _OnboardingRetire:
    title = "Retire a server"
    subtitle = "Delete a server Fauna created in your cloud account and clean up the DNS records that pointed at it, or get your domain's transfer code. Your cloud token is used for this visit only and is never saved."
    list_label = "Servers Fauna created in this account"
    empty_message = "No servers created by Fauna were found in this account. Servers set up some other way carry no Fauna marker and are not listed — retire those from your provider's dashboard."
    domain_label = "Domain"
    secondary_domains_label = "Also serves"
    address_label = "Address"
    current_badge = "The server you're signed into"
    unmarked_note = "Not marked as created by Fauna — check the name carefully before deleting it"
    transfer_code_button = "Get the domain transfer code"
    transfer_code_fetching = "Asking the registrar…"
    transfer_code_label = "Transfer code"
    transfer_code_copy = "Copy code"
    @staticmethod
    def transfer_code_available_after(*, when: str) -> str:
        return f"The registry locks this domain against transfer until {when}. Ask again after that — the code is never refused, only delayed."
    transfer_code_from_registrar = "Get this domain's transfer code from your registrar's own dashboard."
    delete_button = "Delete this server…"
    @staticmethod
    def confirm_what(*, name: str, provider: str, address: str) -> str:
        return f"You are about to permanently delete {name} at {provider} ({address})."
    @staticmethod
    def confirm_domain(*, domain: str) -> str:
        return f"Domain: {domain}"
    @staticmethod
    def confirm_secondary_domains(*, domains: str) -> str:
        return f"Also serves: {domains}"
    confirm_destroyed = "Everything on this server — its disk and every account's data on it — is destroyed and cannot be recovered except from a backup."
    confirm_current = "This is the server you're signed into. This session's nest will stop existing."
    @staticmethod
    def confirm_dns_removals(*, records: str) -> str:
        return f"Before the server is deleted, these DNS records pointing at it are removed: {records}"
    confirm_dns_none = "No DNS records will be removed automatically. The records to remove by hand are listed below."
    @staticmethod
    def confirm_by_hand(*, records: str) -> str:
        return f"To remove by hand afterwards: {records}"
    confirm_name_label = "Type the server's name to confirm"
    confirm_button = "Delete the server and clean up DNS"
    cancel_button = "Cancel"
    step_dns = "DNS records"
    step_server = "Server"
    step_skipped_no_verified_domain = "No domain was verified for this server, so no DNS records were touched."
    step_skipped_no_dns_credential = "No DNS credential reaches this domain's zone, so nothing was removed automatically — see the list to remove by hand."
    step_skipped_no_zone_for_domain = "None of your DNS credentials holds this domain's zone, so nothing was removed automatically — see the list to remove by hand."
    step_skipped_dns_step_forced_past = "Skipped — the server was deleted despite the failed DNS step."
    retry_button = "Try again"
    force_server_button = "Delete the server anyway"
    force_server_warning = "Deleting the server now leaves DNS records pointing at an address your provider can hand to someone else. Remove them by hand right away — they are listed once the server is gone."
    @staticmethod
    def done_deleted(*, name: str) -> str:
        return f"{name} has been deleted."
    @staticmethod
    def leftover_points_at_box(*, record: str) -> str:
        return f"Remove now — still points at the deleted server: {record}"
    @staticmethod
    def leftover_shared_name(*, record: str) -> str:
        return f"Stale, remove when convenient: {record}"
    leftover_none = "Nothing is left to remove by hand."
    leftover_copy = "Copy the list"
    done_button = "Done"


class _OnboardingInvite_request:
    title = "Request an invite"
    subtitle = "Ask the admin of this nest to let you in."
    handle_label = "Your requested handle"
    handle_placeholder = "alice"
    message_label = "Message to the admin (optional)"
    message_placeholder = "Hi, I'd like to join this nest because..."
    submit = "Send request"
    submitting = "Sending..."
    submit_failed = "Could not send the request. Please try again."
    request_button = "Request invite"
    recheck_button = "Check again"
    code_section_title = "Have an invite code?"
    code_label = "Invite code"
    code_placeholder = "Paste invite code"
    status_idle = "Ask for an invite above, or paste a code you already have, to continue."
    oob_idle = "Paste an invite code, then press Check."
    oob_valid = "Code accepted"
    @staticmethod
    def oob_invalid(*, reason: str) -> str:
        return f"This nest did not accept that code: {reason}. Check it for typos and press Check again, or use Request invite above."
    @staticmethod
    def oob_error(*, cause: str) -> str:
        return f"Could not reach the nest to check this code, so it has not been rejected. Check your connection, then press Check again. ({cause})"


class _OnboardingProvisionStep:
    domain = "Domain"
    server = "Server"
    dns = "DNS"
    online = "Online"


class _OnboardingProvisionSubstep:
    domain_checking_availability = "Checking domain availability"
    domain_registering = "Registering domain"
    domain_verifying_zone = "Verifying DNS zone"
    server_generating_dkim = "Generating DKIM keys"
    server_creating = "Creating server"
    dns_adding_domain_records = "Adding domain records"
    dns_adding_email_records = "Adding email records"
    dns_setting_reverse_dns = "Setting reverse DNS"
    online_waiting = "Waiting for your nest to start"
    online_claiming = "Signing you in to your new nest"
    status_skipped = "Already configured — skipped"
    @staticmethod
    def status_retrying(*, cause: str) -> str:
        return f"Retrying after error: {cause}"
    status_cancelling = "Cancelling…"
    status_cancelled = "Cancelled"


class _OnboardingProvision:
    complete = "Your nest is online!"
    start_over = "Start over"
    registering_domain = "Registering domain..."
    registering_domain_details = "Registering domain with registrar"
    creating_server = "Creating server..."
    configuring_dns = "Configuring DNS..."
    fetching_dkim = "Fetching DKIM key..."
    creating_dkim = "Creating DKIM record..."
    setting_up_vps = "Setting up VPS instance"
    creating_dns_records = "Creating DNS records"
    polling_health = "Polling for nest to come online"
    retrieving_dkim = "Retrieving email signing key"
    adding_dkim_record = "Adding email DNS record"
    provisioning_complete = "Provisioning complete"
    time = "This usually takes 2-3 minutes."
    server = "Provision Server"
    @staticmethod
    def step_failed(*, step: str, cause: str) -> str:
        return f"Step {step} failed: {cause}"
    @staticmethod
    def step_attempt_template(*, attempt: str, max_attempts: str) -> str:
        return f" (attempt {attempt} of {max_attempts})"
    step = _OnboardingProvisionStep
    substep = _OnboardingProvisionSubstep


class _OnboardingComplete:
    title = "Your nest is ready!"
    nest_details = "Nest Details"


class _OnboardingBridges:
    no_link_options = "No link options available."
    link_mode = "Link mode"
    @staticmethod
    def not_available_on_nest(*, name: str) -> str:
        return f"{name} not available on this nest."


class _OnboardingHandle:
    prompt = "Enter your handle"
    examples_help = "Example: alice@example.com or alice@bsky.social. Format: user@domain."
    localhost_hint = "test@localhost is allowed for trying out the app."
    control_checkbox = "I control DNS for this domain"


class _OnboardingDns_configContact_fields:
    first_name = "First name"
    last_name = "Last name"
    email = "Email"
    phone = "Phone (E.164: +12025550100)"
    address1 = "Address"
    city = "City"
    state = "State / region"
    postal_code = "Postal code"
    country = "Country (ISO 3166-1 alpha-2, e.g. US)"


class _OnboardingDns_config:
    title = "Configure DNS"
    buy_domain_checkbox = "Buy domain on Continue (this page)"
    same_provider_checkbox = "Buy VPS with same provider (next page)"
    ineligible_needs_registrar = "Can't register domains — untick the buy-domain box above to pick it."
    ineligible_needs_vps = "Doesn't sell VPS servers — untick the same-provider box above to pick it."
    ineligible_needs_registrar_and_vps = "Can't register domains or sell VPS servers — untick both boxes above to pick it."
    set_up_later = "Set up later"
    set_up_later_warning = "Your handle will not work until DNS is configured. We'll show instructions after VPS purchase."
    status_pick_provider = "Choose where your domain's DNS lives to continue."
    status_verify_credentials = "Enter this provider's credentials and press Verify to continue."
    @staticmethod
    def status_owned(*, provider: str) -> str:
        return f"You own this domain at {provider}."
    @staticmethod
    def status_registered_elsewhere(*, provider: str) -> str:
        return f"This domain is already registered. Transfer it to {provider} (or pick another provider) before continuing."
    @staticmethod
    def status_buyable(*, provider: str, price: str) -> str:
        return f"{provider} will register this domain for {price}. Tick the confirm box and press Continue to buy."
    @staticmethod
    def status_not_buyable(*, provider: str) -> str:
        return f"{provider} can't sell this domain. Buy it elsewhere first, then transfer it or use manual DNS."
    open_in_browser = "Open in browser"
    @staticmethod
    def no_provider_carries_tld(*, tld: str) -> str:
        return f"None of our supported registrars carry .{tld} domains. You'll need to buy this domain elsewhere, then either transfer it to a supported registrar or set up DNS manually."
    contact_form_heading = "WHOIS registration contact"
    contact_fields = _OnboardingDns_configContact_fields


class _OnboardingVps_config:
    title = "Configure VPS"
    server_type_radio_legend = "Choose a VPS plan"
    status_pick_provider = "Choose who hosts your server to continue."
    status_verify_credentials = "Enter this provider's credentials and press Verify to continue."
    status_pick_location = "Choose where in the world your server runs to continue."
    status_pick_server_type = "Choose a plan for your server to continue."
    location_heading = "Location"
    update_channel_heading = "Updates"
    update_channel_stable_label = "Stable"
    update_channel_stable_desc = "Released versions. Recommended."
    update_channel_test_label = "Test"
    update_channel_test_desc = "Release candidates that are still being checked."
    update_channel_dev_label = "Dev"
    update_channel_dev_desc = "The newest development builds, before any checking. Expect breakage."
    mail_mode_label = "Run mail on this box"
    mail_mode_desc = "A mail box runs email (SMTP, IMAP) and calendar, which needs the spam and virus scanners and at least 2 GB of RAM. Turn this off for a social-only box: it runs lean and works on the cheapest 1 GB plan, but cannot add mail later without resizing the VPS."


class _OnboardingNest_provisioning:
    title = "Setting up your nest…"
    start_button = "Buy and set up"
    @staticmethod
    def elapsed_template(*, seconds: str) -> str:
        return f"{seconds}s elapsed"
    cancel_button = "Cancel"
    retry_button = "Retry"
    continue_blocked_idle = "Set up your nest first — choose \"Buy and set up\" above."
    continue_blocked_running = "Continue unlocks once setup finishes."
    continue_blocked_failed = "Setup did not finish — retry it to continue."
    continue_blocked_cancelled = "Setup was cancelled — retry it to continue."
    @staticmethod
    def bom_line(*, label: str, price: str) -> str:
        return f"{label}: {price}"
    @staticmethod
    def bom_line_recurring(*, label: str, price: str) -> str:
        return f"{label}: {price}/month"
    @staticmethod
    def bom_line_domain(*, label: str, price: str, renewal: str) -> str:
        return f"{label}: {price} for the first year, then {renewal}/year"


class _OnboardingDns_post_instructions:
    title = "DNS setup instructions"
    description = "Add these records at your DNS provider so your handle starts working."
    copy_button = "Copy all"
    records_pending = "(no records yet — try again in a moment)"


class _OnboardingHandle_checkPhase:
    parsing = "Checking format…"
    dns_lookup = "Checking domain availability…"
    @staticmethod
    def nest_probe(*, domain: str) -> str:
        return f"Looking for a nest at {domain}…"
    challenge_response = "Checking your account…"
    price_lookup = "Looking up registration price…"


class _OnboardingHandle_checkOutcome:
    format_invalid = "Handle format must be user@domain or user.domain (with localhost or IP also accepted)."
    @staticmethod
    def tld_invalid(*, tld: str) -> str:
        return f"{tld} is not a TLD that can be registered."
    @staticmethod
    def domain_available_priced(*, domain: str, price: str) -> str:
        return f"{domain} is available — registration about {price}."
    @staticmethod
    def domain_available_unpriced(*, domain: str) -> str:
        return f"{domain} appears to be available for purchase."
    @staticmethod
    def domain_available_not_buyable_via_provider(*, domain: str, tld: str) -> str:
        return f"{domain} appears to be available, but none of our supported registrars carry .{tld}. You'll need to buy it elsewhere."
    @staticmethod
    def domain_available_inside_zone(*, domain: str, zone: str) -> str:
        return f"Nothing is set up at {domain} yet. It sits inside {zone}: if you hold {zone}, continue and pick the DNS provider that hosts it. If not, you can register {domain} on the next page."
    @staticmethod
    def registered_no_nest(*, domain: str) -> str:
        return f"{domain} resolves but no nest is running. To set one up, confirm you control DNS for this domain."
    @staticmethod
    def already_on_nest_handle_matches(*, handle: str) -> str:
        return f"Welcome back, {handle}."
    @staticmethod
    def already_on_nest_handle_differs(*, domain: str, old_handle: str) -> str:
        return f"You're already registered on {domain} as {old_handle}. Continue to log in as that handle (you can change it after)."
    @staticmethod
    def user_unregistered(*, domain: str) -> str:
        return f"There's a nest at {domain} but you're not registered. Request an invite or paste a code below."
    @staticmethod
    def unregistered_unclaimed_nest(*, domain: str) -> str:
        return f"There's a nest at {domain} but no one has claimed it yet. Continue to claim it as your own."


class _OnboardingHandle_checkError:
    no_network = "Couldn't reach the network. Check your connection and try again."
    @staticmethod
    def nest_unreachable(*, domain: str) -> str:
        return f"{domain} is registered but the nest didn't respond. Try again, or check the domain."
    @staticmethod
    def nest_misbehaving(*, domain: str) -> str:
        return f"{domain} responded with an unexpected error. Try again later."
    @staticmethod
    def nest_protocol_mismatch(*, domain: str) -> str:
        return f"{domain} returned a malformed response. The nest version may be incompatible."
    challenge_temp = "The nest's challenge service is temporarily unavailable. Try again."
    challenge_failed = "Couldn't verify your identity with the nest. This may indicate a key mismatch."
    @staticmethod
    def transient(*, cause: str) -> str:
        return f"Temporary error: {cause}. Try again."
    @staticmethod
    def terminal(*, cause: str) -> str:
        return f"Error: {cause}."


class _OnboardingHandle_check:
    idle = "Enter your handle, then press Check to continue."
    phase = _OnboardingHandle_checkPhase
    outcome = _OnboardingHandle_checkOutcome
    error = _OnboardingHandle_checkError


class _OnboardingInviteError:
    closed = "This nest is not currently accepting invite requests."
    rate_limited = "Too many requests. Please try again later."
    not_found = "This invite request was not found. It may have been removed by the admin."
    @staticmethod
    def transient(*, cause: str) -> str:
        return f"{cause}. Try again."
    already_registered = "This nest already has an account for you, so it won't take a new invite request. Its admin may have suspended your account — contact the admin; if they restore it, sign in again."
    @staticmethod
    def terminal(*, cause: str) -> str:
        return f"Error: {cause}."


class _OnboardingInvite:
    idle = "Request an invite or paste a code below."
    submitting = "Submitting request…"
    rechecking = "Checking status…"
    request_button = "Request invite"
    recheck_button = "Recheck"
    @staticmethod
    def denied(*, reason: str) -> str:
        return f"Request denied: {reason}"
    pending_review = "Submitted. The admin will review — you'll continue automatically once they respond."
    error = _OnboardingInviteError


class _OnboardingOob_code:
    idle = "Have an invite code? Paste it here."
    placeholder = "Invite code from an admin"
    verifying = "Verifying code…"
    valid = "Code accepted. Click Continue to log in."
    @staticmethod
    def invalid(*, reason: str) -> str:
        return f"Code not recognized: {reason}"
    @staticmethod
    def error(*, cause: str) -> str:
        return f"Couldn't verify code: {cause}"


class _OnboardingClaim_codeError:
    already_claimed = "This nest has already been claimed."
    transient = "Couldn't reach the nest. Try again."
    @staticmethod
    def terminal(*, cause: str) -> str:
        return f"Couldn't claim: {cause}."
    claim_code_unreadable = "The nest can't read its claim code — a server setup problem, not a wrong code. Restart the nest and try again."


class _OnboardingClaim_code:
    title = "Claim this nest"
    description = "No one has claimed this nest yet. Paste the one-time claim code printed by your nest server to become its admin."
    label = "Claim code"
    placeholder = "Claim code from server bootstrap"
    submit_button = "Claim"
    idle = "Paste the claim code from your server."
    submitting = "Claiming nest…"
    claimed = "Welcome, admin. Continuing…"
    @staticmethod
    def invalid(*, reason: str) -> str:
        return f"Code not accepted: {reason}"
    error = _OnboardingClaim_codeError


class _OnboardingNat_modeError:
    @staticmethod
    def transient(*, cause: str) -> str:
        return f"Couldn't save the connection mode: {cause}. Try again."
    @staticmethod
    def terminal(*, cause: str) -> str:
        return f"Couldn't save the connection mode: {cause}."


class _OnboardingNat_mode:
    title = "Is this nest reachable from the internet?"
    description = "This sets how your nest connects to the world. The pre-selected option matches how it was installed, so you can usually just confirm — and you can change it anytime in Admin → Nest."
    public_label = "Public (internet-facing)"
    public_desc = "This box has a public address: it can receive email, federate directly with other nests, get automatic certificates, and relay for private nests. The usual choice for a hosted server."
    private_label = "Private (home network)"
    private_desc = "This box sits behind a home router and isn't reachable from the internet: it runs no mail receiver, keeps calendar and mail sync on your local network, and pairs with a public nest that relays for it."
    confirm_button = "Confirm"
    defer_button = "Decide later"
    choosing = "Confirm how this nest connects, or decide later."
    private_hint = "This looks like a home-network address, so Private is pre-selected."
    submitting = "Saving the connection mode..."
    done = "Connection mode saved."
    error = _OnboardingNat_modeError


class _OnboardingTrust_prompt:
    title = "Trust this box?"
    summary = "This box can do more for you if you let it read the things it looks after: filtering your mail as it arrives, and serving your calendar to your other devices. Your trust renews itself while you use Fauna, you can take it back at any time in Settings → Nests, and every time you give or withdraw trust it is written down for you there."
    grant_button = "Yes, trust this box"
    skip_button = "Not now"
    nothing_to_grant = "There's nothing to decide yet — this box isn't running anything that would read your content. You can trust it later from Settings → Nests."


class _OnboardingAwaiting_dns:
    title = "Almost ready"
    pending = "Add the DNS records below at your registrar. We'll bring your nest online automatically once they take effect — you can leave this screen open."
    server_starting = "Your server is starting — we'll sign you in the moment it answers. You can close the app and come back."
    checking = "Checking whether your nest is online…"
    claiming = "Your nest is online — finishing setup…"
    claimed = "All set. Continuing…"
    @staticmethod
    def error(*, cause: str) -> str:
        return f"Couldn't finish setting up your nest: {cause}"
    recheck_button = "Check now"
    copy_button = "Copy all"


class _OnboardingDone:
    finished = "Onboarding finished."


class _OnboardingSession_error:
    @staticmethod
    def persist_account(*, message: str) -> str:
        return f"Couldn't save your new account: {message}"
    @staticmethod
    def persist_successor(*, message: str) -> str:
        return f"Couldn't save your new identity: {message}"
    @staticmethod
    def switch_successor(*, message: str) -> str:
        return f"Couldn't switch to your new identity: {message}"
    @staticmethod
    def switch_appended(*, message: str) -> str:
        return f"Couldn't switch to the new account: {message}"
    no_active_account = "Couldn't find the account that was just added."
    @staticmethod
    def invalid_secret(*, message: str) -> str:
        return f"This device's saved secret key isn't valid: {message}"
    @staticmethod
    def remove_account(*, message: str) -> str:
        return f"Couldn't remove that account: {message}"


class _Onboarding:
    welcome = _OnboardingWelcome
    launch = _OnboardingLaunch
    instance_chooser = _OnboardingInstance_chooser
    identity_choice = _OnboardingIdentity_choice
    add_account = _OnboardingAdd_account
    identity_created = _OnboardingIdentity_created
    recovery_kit = _OnboardingRecovery_kit
    recovery_entry = _OnboardingRecovery_entry
    identity_import = _OnboardingIdentity_import
    recovery = _OnboardingRecovery
    retire = _OnboardingRetire
    invite_request = _OnboardingInvite_request
    provision = _OnboardingProvision
    complete = _OnboardingComplete
    bridges = _OnboardingBridges
    handle = _OnboardingHandle
    dns_config = _OnboardingDns_config
    vps_config = _OnboardingVps_config
    nest_provisioning = _OnboardingNest_provisioning
    dns_post_instructions = _OnboardingDns_post_instructions
    handle_check = _OnboardingHandle_check
    invite = _OnboardingInvite
    oob_code = _OnboardingOob_code
    claim_code = _OnboardingClaim_code
    nat_mode = _OnboardingNat_mode
    trust_prompt = _OnboardingTrust_prompt
    awaiting_dns = _OnboardingAwaiting_dns
    done = _OnboardingDone
    session_error = _OnboardingSession_error


class _Launch:
    signing_in = "Signing you in…"
    retry_title = "Couldn't reach your nest"
    needs_update_title = "Update this app to continue"
    identity_changed_title = "This nest's identity changed"
    retry_button = "Try again"
    use_different_nest = "Use a different nest"
    recovery_custody_mismatch = "Off-box recovery isn't protected: this box handed off an inconsistent recovery key."
    recovery_custody_failed = "Off-box recovery custody wasn't saved — couldn't reach the box to confirm. Your nest still works, but it isn't protected against total box loss yet."


class _Credential_store:
    section_title = "Credential store"
    status_sealed = "Your sign-in keys rest in a file on this device, sealed under your passphrase."
    rekey_button = "Change passphrase…"
    rekey_title = "Change passphrase"
    seed_nudge = "Back up your recovery seed first (Identity export, above). The sealed store has no recovery path of its own — if the new passphrase is forgotten, the seed is the only way back into your account."
    current_label = "Current passphrase"
    new_label = "New passphrase"
    confirm_label = "Confirm new passphrase"
    submit = "Change passphrase"
    success = "Passphrase changed."
    error_empty = "Enter your current passphrase and choose a new one"
    error_mismatch = "The two new entries don't match"
    error_wrong = "Wrong passphrase, or the store file is corrupt"
    @staticmethod
    def error_failed(*, message: str) -> str:
        return f"Could not change the passphrase: {message}"


class _Tui_unlock:
    unlock_title = "Unlock your credentials"
    unlock_prompt = "Your credentials are protected by a passphrase on this machine. Enter it to sign in."
    create_title = "Protect your credentials"
    create_prompt = "No secure OS key store is reachable here, so your sign-in keys will rest in a file protected by a passphrase. Choose one to continue — you'll need it at every launch."
    passphrase_label = "Passphrase"
    confirm_label = "Confirm passphrase"
    unlock_button = "Unlock"
    create_button = "Set passphrase"
    error_empty = "Enter a passphrase"
    error_mismatch = "The two entries don't match"
    error_wrong = "Wrong passphrase, or the store file is corrupt"
    @staticmethod
    def error_failed(*, message: str) -> str:
        return f"Could not open the credential store: {message}"


class _Tui_nav_hints:
    @staticmethod
    def pane(*, keys: str) -> str:
        return f"{keys} pane"
    @staticmethod
    def move_focus(*, keys: str) -> str:
        return f"{keys} move"
    @staticmethod
    def open(*, keys: str) -> str:
        return f"{keys} open"
    @staticmethod
    def next(*, keys: str) -> str:
        return f"{keys} next"
    @staticmethod
    def quit(*, keys: str) -> str:
        return f"{keys} quit"


class _Tui_settings:
    title = "Terminal"
    external_media_label = "Play audio & video externally"
    external_media_subtitle = "This terminal can't play audio or video inline, so it can hand a clip to your system's default player. Choose whether it asks first, always opens, or never opens."
    external_media_ask = "Ask each time"
    external_media_always = "Always open"
    external_media_never = "Never open (show details only)"


class _FeedPost_type:
    community = "Community"
    classified = "Listing"


class _FeedList:
    title = "Feeds"
    trending = "Trending"
    no_posts = "No posts yet."
    no_matching_posts = "No matching posts."
    end_of_feed = "End of feed"
    bridge_feeds = "Bridge Feeds"
    subscribe_bridge = "Subscribe to Bridge Feed"
    delete_feed = "Delete feed"
    unsubscribe = "Unsubscribe"


class _FeedBridge_form:
    kind = "Bridge"
    uri = "Feed URI"
    name = "Display Name"


class _FeedCreate:
    title = "New Feed"
    name_placeholder = "My Feed"
    add_rule = "Add Rule"
    mode_all = "All match"
    mode_any = "Any match"
    filter_rules = "Filter Rules"
    combination = "Combination"
    feed_name = "Feed Name"
    rule_required = "Required"
    rule_excluded = "Excluded"
    rule_category = "Category"
    rule_threshold = "Threshold (0-10)"
    rule_threshold_short = "0-10"
    rule_value_placeholder = "tag1, tag2"
    factors = "Factors"
    factor_engagement = "Engagement"
    factor_trending = "Trending"
    factor_weight_placeholder = "1.0"
    factor_global_toggle = "Apply to all feeds"
    add_factor = "Add Factor"


class _FeedPost:
    post_not_found = "Post not found"
    @staticmethod
    def replying_to_user(*, user: str) -> str:
        return f"Replying to {user}"
    reposted_marker = "reposted"
    write_reply = "Write a reply..."
    repost = "Repost"
    add_comment = "Add your comment..."
    whats_on_your_mind = "What's on your mind?"
    has_media = "[media]"
    view_thread = "View Thread"
    thread = "Thread"
    no_thread_data = "No thread data"
    no_thread_desc = "Could not load thread data."
    attach_image = "Attach Image"
    no_feeds_configured = "No feeds configured."
    no_bridge_feeds = "No bridge feeds available."
    create_tooltip = "Create Feed"
    write_post = "Write a post..."
    tags_placeholder = "Tags (comma-separated)"
    search_placeholder = "Search this feed..."
    compose = "Compose"
    compose_post = "Compose Post"
    compose_drop_hint = "Compose post — drop files here to attach"
    gate_audience = "Audience"
    gate_public = "Public"
    gate_preview_placeholder = "Public teaser shown to non-subscribers..."
    @staticmethod
    def gated_badge_tooltip(*, tier: str) -> str:
        return f"Subscribers only: {tier}"
    @staticmethod
    def gated_badge_room_tooltip(*, room: str) -> str:
        return f"Room members only: {room}"
    gate_sell = "Sell this post…"
    @staticmethod
    def gate_room(*, room: str) -> str:
        return f"Room: {room}"
    @staticmethod
    def reply_audience_room(*, room: str) -> str:
        return f"Reply goes to Room: {room}"
    @staticmethod
    def reply_audience_tier(*, tier: str) -> str:
        return f"Reply goes to your tier {tier}"
    reply_audience_public = "This post is for a smaller audience — a reply from you would be public"
    reply_public_confirm = "Post my reply publicly"
    sell_price_placeholder = "Price, e.g. $3"
    sell_asking_price_placeholder = "Machine price in sats (optional)"
    sell_subscribers_free = "Subscribers get it free"
    buy_button = "Buy"
    open_rich_compose = "Open rich compose dialog"
    posting = "Posting…"
    post_detail = "Post detail"


class _FeedRule_types:
    has_hashtag = "Has Hashtag"
    source = "Protocol Source"
    has_media = "Has Media"
    is_reply = "Is Reply"
    min_replies = "Min Replies"
    min_reposts = "Min Reposts"
    created_after = "Created After"
    body_contains = "Body contains"
    body_excludes = "Body Excludes"
    label_below = "Label Below (exclude spam)"
    label_above = "Label Above (show only)"


class _FeedRule_chip:
    @staticmethod
    def has_hashtag(*, tags: str) -> str:
        return f"{tags}"
    @staticmethod
    def source(*, value: str) -> str:
        return f"source: {value}"
    has_media_yes = "media: yes"
    has_media_no = "media: no"
    is_reply_yes = "reply: yes"
    is_reply_no = "reply: no"
    @staticmethod
    def min_replies(*, count: str) -> str:
        return f"replies >= {count}"
    @staticmethod
    def min_reposts(*, count: str) -> str:
        return f"reposts >= {count}"
    @staticmethod
    def created_after(*, hours: str) -> str:
        return f"age < {hours}h"
    @staticmethod
    def body_contains(*, value: str) -> str:
        return f"contains: {value}"
    @staticmethod
    def body_excludes(*, value: str) -> str:
        return f"excludes: {value}"
    @staticmethod
    def label_below(*, value: str) -> str:
        return f"label below: {value}"
    @staticmethod
    def label_above(*, value: str) -> str:
        return f"label above: {value}"


class _Feed:
    compose_empty = "Post cannot be empty"
    compose_gate_preview_empty = "Add a public teaser for a gated post"
    @staticmethod
    def compose_gate_no_key(*, tier: str) -> str:
        return f"This device does not hold the key for tier {tier}"
    compose_room_no_key = "This device does not hold the key for that room yet"
    reference_restricted = "This post is for a smaller audience, and your reply would be public, so it was not sent"
    compose_attachment_stale = "Attach the file again — the audience changed after it was prepared"
    @staticmethod
    def compose_attachment_missing(*, filename: str) -> str:
        return f"Attach {filename} again — the file is not on this device."
    compose_sell_price_invalid = "Enter a smaller price"
    compose_sell_rank_unavailable = "Could not check your tiers to price this post — try again"
    @staticmethod
    def error_load(*, message: str) -> str:
        return f"Failed to load posts: {message}"
    @staticmethod
    def error_feeds(*, message: str) -> str:
        return f"Failed to load feeds: {message}"
    @staticmethod
    def error_submit(*, message: str) -> str:
        return f"Failed to post: {message}"
    @staticmethod
    def error_subscribe(*, message: str) -> str:
        return f"Failed to subscribe: {message}"
    @staticmethod
    def error_gated_unlock(*, message: str) -> str:
        return f"Could not unseal this post: {message}"
    @staticmethod
    def error_trained_factor(*, message: str) -> str:
        return f"This feed's trained topic could not be applied: {message}"
    @staticmethod
    def error_subscribed_model(*, message: str) -> str:
        return f"A subscribed community model could not be applied: {message}"
    @staticmethod
    def error_train(*, message: str) -> str:
        return f"Could not train on this post: {message}"
    @staticmethod
    def error_buy_unlock(*, message: str) -> str:
        return f"Could not buy this post: {message}"
    post_actions_tooltip = "More actions"
    more_like_this = "More like this"
    less_like_this = "Less like this"
    train_target_title = "Train which topic?"
    delete_post = "Delete post"
    delete_post_confirm = "Delete"
    delete_post_confirm_title = "Delete post?"
    @staticmethod
    def error_delete(*, message: str) -> str:
        return f"Failed to delete post: {message}"
    report_post = "Report post"
    post_muted_placeholder = "Muted word"
    post_muted_reveal = "Show anyway"
    @staticmethod
    def error_muted_keywords(*, message: str) -> str:
        return f"Your muted words are not being applied: {message}"
    unverified_source = "Unverified"
    unverified_source_tooltip = "This device could not verify the author's signature on this post."
    delegated_origin = "Via connected app"
    delegated_origin_tooltip = "A connected app wrote this post as you, using the access you granted it. Manage or revoke that access on the AT Protocol settings page."
    like_tooltip = "Like"
    quote = "Quote"
    watch = "Watch"
    post_type = _FeedPost_type
    list = _FeedList
    bridge_form = _FeedBridge_form
    create = _FeedCreate
    post = _FeedPost
    rule_types = _FeedRule_types
    rule_chip = _FeedRule_chip


class _ConversationsErrors:
    served_elsewhere = "Conversations are open in another instance of this app. Use them there — everything else works here."
    receive_stopped = "New messages stopped arriving because of an internal error. Restart the app (or reload the page) to receive them again."
    @staticmethod
    def mail_unopenable(*, count: str) -> str:
        return f"{count} received messages could not be opened on this device. They were sealed to mail keys this account no longer holds, and were skipped."


class _ConversationsList:
    title = "Conversations"
    new_conversation = "New Conversation"
    sort = "Sort"
    search_placeholder = "Search conversations..."
    select_conversation = "Select a conversation to view."
    select_conversation_short = "Select a conversation"
    no_conversations = "No conversations yet."


class _ConversationsCompose:
    title = "Compose"
    new_message = "New Message"
    to = "To"
    subject = "Subject"
    body = "Body"
    resolve = "Resolve"
    write_message = "Write your message..."
    encrypted = "Encrypted"


class _ConversationsDetail:
    badge_encrypted = "End-to-end encrypted"
    badge_signed = "Cryptographically signed"
    badge_verified = "Verified sender"
    badge_c2pa = "C2PA content credentials present"
    delete_message = "Delete message"
    delete_message_confirm = "Delete"
    delete_message_confirm_title = "Delete message?"
    message_deleted = "This message was deleted"
    selected_message = "Your search result"
    message_actions = "More actions"
    add_reaction = "Add reaction"
    more_reactions = "More reactions"
    mailbox = "Mailbox"
    no_subject = "(no subject)"
    title = "Conversation"
    no_messages = "No messages in this conversation."
    load_remote_content = "Load remote images"
    remote_image_blocked = "Remote image blocked"
    member_unattested_mark = "Was in this group before you recovered your account. Keep them, or remove them if you don't recognise them."
    member_keep = "Keep"
    muted_word = "Muted word"
    muted_reveal = "Show anyway"
    mark_as_spam = "Mark as spam"
    report_message = "Report message"


class _ConversationsMessage:
    signed = "Signed"


class _ConversationsUnified:
    attachment_button = "Attach file"
    attachment_remove = "Remove attachment"
    @staticmethod
    def error_add_participant(*, message: str) -> str:
        return f"Could not add them to this conversation: {message}"
    @staticmethod
    def error_add_participant_after_heal(*, reason: str) -> str:
        return f"their undelivered earlier invitation was removed first, so they are no longer in the group — adding them again starts cleanly. The re-invitation failed: {reason}"
    @staticmethod
    def error_remove_participant(*, message: str) -> str:
        return f"Could not remove them from this conversation: {message}"
    @staticmethod
    def error_rename_thread(*, message: str) -> str:
        return f"Could not rename this conversation: {message}"
    @staticmethod
    def error_leave_room(*, message: str) -> str:
        return f"Could not leave this conversation: {message}"
    @staticmethod
    def error_set_room_policy(*, message: str) -> str:
        return f"Could not change this room's settings: {message}"
    @staticmethod
    def error_room_invitation(*, message: str) -> str:
        return f"Could not answer this invitation: {message}"
    @staticmethod
    def error_withdraw_room_invite(*, message: str) -> str:
        return f"Could not withdraw this invitation: {message}"
    room_class_end_to_end = "End-to-end encrypted"
    room_class_community = "Community — searched and labelled by the home nest"
    room_class_transport_only = "Transport-only"
    guardian_state_held = "Waiting for your guardian"
    guardian_state_blocked = "Blocked by your guardian"
    bridged_one_recipient = "A conversation over this bridge is with one person. Remove the other recipients and send again."
    bridged_no_recipient_key = "This account has no mail key yet, so a copy of the message cannot be kept. Set up mail, then send again."
    room_notice_moderation_unverified = "Some moderation in this room couldn't be verified on this device, so the affected messages are still shown."
    room_notice_awaiting_key = "Waiting for a room key — messages will appear once an owner or admin keys you in."
    room_role_owner = "owner"
    room_role_admin = "admin"
    room_join_rule_label = "Who can invite"
    room_join_rule_invite = "Owner and admins"
    room_join_rule_member_invite = "Any member"
    room_history_policy_label = "History for new members"
    room_history_policy_none = "Nothing before they join"
    room_history_policy_full = "The whole conversation"
    thread_room_settings = "Room settings"
    room_leave = "Leave room"
    room_home_nest_yes = "Home nest joins: yes"
    room_home_nest_no = "Home nest joins: no"
    room_nest_read_yes = "Home nest reads this room: yes"
    room_nest_read_no = "Home nest reads this room: no"
    @staticmethod
    def room_invitation_member(*, inviter: str) -> str:
        return f"{inviter} invited you to a room"
    @staticmethod
    def room_invitation_admin(*, inviter: str) -> str:
        return f"{inviter} invited you to a room as an admin"
    room_pending_invites_label = "Pending invitations"
    @staticmethod
    def room_pending_invite_member(*, invitee: str, inviter: str) -> str:
        return f"{invitee} — invited by {inviter}"
    @staticmethod
    def room_pending_invite_admin(*, invitee: str, inviter: str) -> str:
        return f"{invitee} — invited by {inviter} as an admin"
    @staticmethod
    def room_pending_invite_lapsed(*, sentence: str) -> str:
        return f"{sentence} (can no longer be accepted)"
    room_pending_invite_withdraw = "Withdraw"
    room_admin_yes = "admin: yes"
    room_admin_no = "admin: no"
    room_transfer_mark = "make owner"
    room_transfer_staged = "new owner"
    room_labelers_label = "Labels the home nest adds to every message"
    room_labeler_on = "labels: on"
    room_labeler_off = "labels: off"
    @staticmethod
    def error_send(*, message: str) -> str:
        return f"Could not send this message: {message}"
    error_attachment_no_composer = "Open a message composer before attaching a file."
    group_conversation_hint = "This will start a group conversation."
    recipient_picker_placeholder = "Type a handle, email, npub, DID, or @user@instance"
    @staticmethod
    def recipient_picker_bridges(*, bridges: str) -> str:
        return f"Also reaches people on: {bridges}"
    recipient_resolve_error = "Lookup failed — try again"
    recipient_resolve_not_found = "Not found — check the address"
    recipient_resolve_resolved = "Resolved"
    recipient_resolve_resolving = "Resolving…"
    reply_all = "Reply all"
    reply_recipient_add_placeholder = "Add recipient…"
    show_full_headers = "Show full headers"
    thread_add_participant = "Add someone…"
    thread_rename = "Rename"
    thread_rename_placeholder = "New name"
    to_line_label = "To:"
    topic_input_placeholder = "Topic (optional)"
    topic_toggle_add = "+ topic"


class _Conversations:
    errors = _ConversationsErrors
    list = _ConversationsList
    compose = _ConversationsCompose
    detail = _ConversationsDetail
    message = _ConversationsMessage
    unified = _ConversationsUnified


class _ContactsFind_user:
    title = "Find User"
    description = "Enter a handle (e.g. alice@fauna.social) or actor ID hex to find a user."
    placeholder = "alice@fauna.social or actor ID hex"
    find = "Find"


class _ContactsMessage_requests:
    title = "Message Requests"
    none = "No pending message requests."
    @staticmethod
    def count(*, count: str) -> str:
        return f"Message Requests ({count})"


class _ContactsAddress_book:
    title = "Address Book"
    no_addressbooks = "No address books yet."
    no_cards = "No contacts yet."
    card_not_found = "That contact is no longer in your address books."
    select_card = "Select a contact to view details."
    email = "Email"
    phone = "Phone"
    address = "Address"
    organization = "Organization"
    note = "Note"


class _Contacts:
    title = "Contacts"
    detail_title = "Contact"
    sign_in_prompt = "Sign in to view your contacts."
    message = "Message"
    handle_not_found = "Handle not found"
    guardian_approval_required = "This account can only message approved contacts."
    ask_guardian = "Ask your guardian"
    contact_request_pending = "Asked — waiting for your guardian"
    no_contact_selected = "Select a contact to view details."
    knock = "Knock"
    sent = "Sent"
    looking_up = "Looking up..."
    wants_to_connect = "wants to connect"
    search_results = "Search Results"
    knocks = "Knocks"
    no_pending_knocks = "No pending knocks."
    no_contacts = "No contacts yet."
    no_matching_contacts = "No matching contacts."
    handle_or_actor_id = "Handle, handle@domain, or actor ID..."
    pending_requests = "Pending Requests"
    no_pending = "No pending requests."
    unattested_mark = "Not reviewed since you recovered your account"
    find_placeholder = "Find by handle..."
    filter_placeholder = "Filter contacts..."
    add_contact = "Add Contact"
    request_sent = "Contact request sent!"
    @staticmethod
    def looking_up_handle(*, handle: str, domain: str) -> str:
        return f"Looking up @{handle}@{domain}…"
    find_user = _ContactsFind_user
    message_requests = _ContactsMessage_requests
    address_book = _ContactsAddress_book


class _EventsRsvp:
    title = "RSVP"
    going = "Going"
    interested = "Interested"
    decline = "Decline"
    tentative = "Tentative"
    declined = "Declined"
    waitlisted = "Waitlisted"
    invited = "Invited"


class _EventsInvite:
    button = "Invite"
    title = "Invite Attendee"
    email_label = "Email"
    email_placeholder = "Attendee email"
    inviting = "Inviting..."


class _EventsReminder:
    title = "Reminder"
    set = "Set"
    current = "Current:"
    select_placeholder = "Select…"
    min_15 = "15 min before"
    hour_1 = "1 hour before"
    day_1 = "1 day before"


class _EventsRefused_changesReason:
    not_the_organizer = "They are not the organizer of this event."
    organizer_changed = "The message tried to change who organizes the event."
    organizer_unresolvable = "No one could be confirmed as this event's organizer."
    no_attested_author = "Your nest could not confirm who sent the message."
    spoofed_organizer = "The sender was not the organizer they claimed to be."
    not_the_attendee = "They are not the guest they answered for."
    attendee_unresolvable = "The guest they answered for could not be confirmed."
    sender_unauthenticated = "The message did not come from a confirmed sender."
    other = "The change was refused."


class _EventsRefused_changes:
    title = "Refused changes"
    @staticmethod
    def cancel_attempt(*, title: str) -> str:
        return f"Someone tried to cancel \"{title}\""
    @staticmethod
    def update_attempt(*, title: str) -> str:
        return f"Someone tried to change \"{title}\""
    @staticmethod
    def reply_attempt(*, title: str) -> str:
        return f"Someone tried to answer for \"{title}\""
    other_attempt = "Someone tried to change an event on your calendar"
    @staticmethod
    def sender(*, who: str) -> str:
        return f"Sent by {who}"
    @staticmethod
    def sender_via_nest(*, who: str, nest: str) -> str:
        return f"Sent by {who}, according to {nest}"
    unknown_sender = "Sender could not be identified"
    @staticmethod
    def attempts(*, count: str) -> str:
        return f"Tried {count} times"
    dismiss = "Dismiss"
    reason = _EventsRefused_changesReason


class _EventsError:
    load_calendars = "Failed to load calendars"
    load_events = "Failed to load events"
    create_calendar = "Failed to create calendar"
    create_event = "Failed to create event"
    delete_event = "Failed to delete event"
    invite = "Failed to invite"
    rsvp = "RSVP failed"
    set_reminder = "Failed to set reminder"
    remove_reminder = "Failed to remove reminder"
    import_ = "Import failed"
    export = "Export failed"


class _Events:
    title = "Events"
    @staticmethod
    def calendar_exported(*, path: str) -> str:
        return f"Calendar exported to {path}"
    calendars = "Calendars"
    new_calendar = "New Calendar"
    calendar_name = "Calendar name"
    no_calendars = "No calendars yet."
    invited_events = "Invited Events"
    new_event = "New Event"
    more_options = "More options..."
    import_ics = "Import .ics"
    importing = "Importing..."
    export_ics = "Export .ics"
    exporting = "Exporting..."
    export_ics_tooltip = "Export a calendar as .ics file"
    import_ics_tooltip = "Import events from .ics file"
    export_calendar_title = "Export Calendar as ICS"
    import_calendar_title = "Import ICS Calendar"
    summary = "Summary"
    summary_placeholder = "New event"
    start = "Start"
    end = "End"
    end_optional = "End (optional)"
    description = "Description"
    location = "Location"
    location_placeholder = "Add a location..."
    attendance_mode = "Attendance Mode"
    capacity = "Capacity (0 = unlimited)"
    view_week = "Week"
    all_day = "All day"
    attendees = "Attendees"
    @staticmethod
    def attendees_count(*, count: str) -> str:
        return f"Attendees ({count})"
    no_attendees = "No attendees yet."
    event_count_one = "1 event"
    @staticmethod
    def event_count(*, count: str) -> str:
        return f"{count} events"
    no_events_yet = "No events yet"
    select_calendar = "Select a calendar"
    event_not_found = "Event not found"
    detail_title = "Event"
    no_event_selected = "No event selected"
    select_event_hint = "Select an event to see its details."
    remove_reminder = "Remove Reminder"
    delete_event = "Delete Event"
    delete_event_confirm = "Are you sure you want to delete this event?"
    create_event = "Create Event"
    start_date = "Start Date"
    end_date = "End Date"
    invalid_datetime = "Enter a date and time as YYYY-MM-DDTHH:MM."
    time = "Time"
    deleting = "Deleting..."
    no_upcoming_events = "No upcoming events"
    no_upcoming_events_desc = "Events from selected calendars will appear here."
    no_events = "No events"
    attendance = "Attendance"
    invite_only = "Invite Only"
    group_only = "Group Only"
    link_code = "Link Code"
    calendar = "Calendar"
    remind_me = "Remind me"
    your_status = "Your status:"
    send_invite = "Send Invite"
    view_agenda = "Agenda"
    view_month = "Month"
    view_day = "Day"
    mail_required = "Calendars and events require mail to be enabled. Enable mail in Settings to use the calendar."
    loading_events = "Loading events..."
    no_events_in_calendar = "No events in this calendar."
    select_calendar_event_hint = "Select a calendar and event to view details."
    @staticmethod
    def import_result(*, imported: str, skipped: str, total: str) -> str:
        return f"Imported: {imported}, Skipped: {skipped}, Total: {total}"
    ics_path_required = "Type the path to an .ics file first, then press Import."
    ics_file_required = "Choose an .ics file first, then press Import."
    set_reminder = "Set Reminder"
    setting = "Setting..."
    rsvp = _EventsRsvp
    invite = _EventsInvite
    reminder = _EventsReminder
    refused_changes = _EventsRefused_changes
    error = _EventsError


class _GroupsMessage:
    encrypted = "Encrypted"
    encrypted_title = "End-to-end encrypted via MLS"
    signed = "Signed"
    signed_title = "Sender signature verified — content is plaintext in the nest"


class _Groups:
    title = "Groups"
    create_group = "Create Group"
    group_name = "Group name"
    my_groups = "My Groups"
    no_groups = "No groups yet."
    select_prompt = "Select or create a group to get started."
    loading_group = "Loading group..."
    members = "Members"
    invite_member = "Invite Member"
    invite_placeholder = "alice@fauna.social or actor ID"
    invite_nest_url_placeholder = "Nest URL (optional, for cross-nest)"
    node_url_placeholder = "Node URL (for cross-nest invites)"
    replying_to = "Replying to"
    no_channel = "No encrypted channel for this group"
    view_threaded = "Threaded"
    group_chat = "Group Chat"
    make_admin = "Make Admin"
    demote = "Demote"
    role = "Role"
    member_role = "Member"
    invite = "Invite"
    group = "Group"
    select_group = "Select a group to view."
    mark_spam = "Mark Spam"
    not_spam = "Not Spam"
    cancel_reply = "Cancel reply"
    invite_to_group = "Invite to Group"
    group_members = "Group Members"
    no_members = "No members yet."
    owner = "Owner"
    @staticmethod
    def members_title(*, name: str) -> str:
        return f"Members — {name}"
    @staticmethod
    def invite_member_title(*, name: str) -> str:
        return f"Invite Member — {name}"
    @staticmethod
    def replying_to_user(*, user: str) -> str:
        return f"Replying to {user}"
    create_group_hint = "Create a group to start messaging."
    new_group = "New Group"
    invitee = "Invitee"
    mute_group = "Mute Group"
    react = "React"
    message_placeholder = "Type a message..."
    message = _GroupsMessage


class _Atproto_settings:
    title = "AT Protocol"
    app_credentials_heading = "App credentials"
    app_credentials_empty = "No app credentials yet."
    mint_button = "New app credential"
    @staticmethod
    def default_credential_label(*, count: str) -> str:
        return f"App credential {count}"
    reveal_button = "Reveal"
    revoke_button = "Revoke"
    @staticmethod
    def credential_created_prefix(*, date: str) -> str:
        return f"Created {date}"
    @staticmethod
    def credential_last_used_prefix(*, date: str) -> str:
        return f"Last used {date}"
    credential_never_used = "Never used"
    connected_apps_heading = "Connected apps"
    connected_apps_empty = "No connected apps yet."
    @staticmethod
    def session_created_prefix(*, date: str) -> str:
        return f"Connected {date}"
    @staticmethod
    def session_expires_prefix(*, date: str) -> str:
        return f"Expires {date}"
    @staticmethod
    def session_scopes_prefix(*, scopes: str) -> str:
        return f"Approved for {scopes}"
    @staticmethod
    def session_sets_prefix(*, sets: str) -> str:
        return f"Granted via {sets}"
    @staticmethod
    def session_set_named(*, title: str, nsid: str) -> str:
        return f"“{title}” ({nsid})"
    @staticmethod
    def session_last_used_prefix(*, date: str) -> str:
        return f"Last seen {date}"
    session_never_used = "Not seen since connecting"
    session_status_live = "Working"
    session_status_suspended = "Paused — turn Bluesky back on to let this app work again"
    external_apps_toggle = "Allow external apps"
    depth_heading = "Integration depth"
    depth_off_title = "Off"
    depth_off_desc = "No Bluesky presence."
    depth_linked_title = "Linked account"
    depth_linked_desc = "Read, interact, and cross-post through an existing Bluesky account."
    depth_hosted_visible_title = "Hosted here — visible"
    depth_hosted_visible_desc = "This nest holds your identity and publishes your public posts to the Bluesky network."
    depth_hosted_full_title = "Hosted here — full access"
    depth_hosted_full_desc = "Additionally, other Bluesky apps can log in as you through this nest."
    depth_card_heading = "Confirm this change"
    depth_confirm_button = "Confirm"
    depth_cancel_button = "Cancel"
    contest_card_heading = "Your AT Protocol identity may have been taken over"
    contest_button = "Undo this change…"
    contest_confirm_button = "Undo it now"
    contest_cancel_button = "Not now"
    did_method_heading = "Identity method"
    did_method_plc_title = "did:plc — recommended"
    did_method_plc_desc = "A portable identity you can move to another server later."
    did_method_web_title = "did:web"
    did_method_web_desc = "Ties your identity to this nest's domain."
    @staticmethod
    def handle_either_way(*, handle: str) -> str:
        return f"Your Bluesky handle will be @{handle} either way."
    history_backfill_label = "Also publish my existing public posts."
    @staticmethod
    def hosted_handle_prefix(*, handle: str) -> str:
        return f"Your Bluesky handle: @{handle}"
    @staticmethod
    def hosted_method_prefix(*, method: str) -> str:
        return f"Method: {method}"
    identity_status_active = "Active"
    identity_status_pending = "Setting up…"
    identity_status_deactivated = "Deactivated"
    identity_status_deleted = "Deleted"
    identity_status_tombstoned = "Permanently retired"
    delete_presence_button = "Delete my Bluesky presence"
    delete_confirm_button = "Delete my presence"
    delete_cancel_button = "Keep my presence"
    delete_retire_identity_label = "Also permanently retire my AT Protocol identity — this cannot be undone"
    delegation_heading = "Posting from other apps"
    delegation_empty = "Other Bluesky apps can sign in, but cannot post as you yet."
    @staticmethod
    def delegation_scope_prefix(*, capabilities: str) -> str:
        return f"Allowed: {capabilities}"
    delegation_capability_post = "post"
    delegation_capability_update_profile = "update your profile"
    @staticmethod
    def delegation_lasts_until(*, authorized: str, expires: str) -> str:
        return f"Authorized {authorized} · until {expires}"
    @staticmethod
    def delegation_lasts_until_no_expiry(*, authorized: str) -> str:
        return f"Authorized {authorized} · no expiry"
    delegation_status_active = "Active"
    delegation_status_expiring_soon = "Expiring soon — re-authorize to keep other apps posting"
    delegation_status_expired = "Expired — other apps can no longer post as you"
    delegation_status_never_expires = "No expiry"
    @staticmethod
    def delegation_last_used(*, when: str) -> str:
        return f"Last reported use: {when}"
    delegation_last_used_never = "No use reported yet"
    delegation_last_used_hint = "Reported by your nest, so treat it as a hint — check your feed for posts marked \"Via connected app\" to see what was actually written."
    delegation_authorize_button = "Let other apps post as me"
    delegation_reauthorize_button = "Re-authorize"
    delegation_revoke_button = "Stop other apps posting as me"
    consent_heading = "An app wants to sign in as you"
    @staticmethod
    def consent_client(*, name: str, client_id: str) -> str:
        return f"{name} — {client_id}"
    @staticmethod
    def consent_client_unnamed(*, client_id: str) -> str:
        return f"{client_id}"
    @staticmethod
    def consent_code(*, code: str) -> str:
        return f"Confirmation code: {code}"
    consent_code_hint = "Approve only if your browser is showing this same code."
    consent_scopes_heading = "It is asking to:"
    @staticmethod
    def consent_set_heading(*, title: str, nsid: str) -> str:
        return f"Some of that comes from “{title}” ({nsid}):"
    @staticmethod
    def consent_set_heading_unnamed(*, nsid: str) -> str:
        return f"Some of that comes from {nsid}:"
    consent_approve_button = "Approve"
    consent_deny_button = "Deny"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load app credentials: {message}"
    @staticmethod
    def error_mint(*, message: str) -> str:
        return f"Failed to create the app credential: {message}"
    @staticmethod
    def error_revoke(*, message: str) -> str:
        return f"Failed to revoke the app credential: {message}"
    @staticmethod
    def error_revoke_session(*, message: str) -> str:
        return f"Failed to disconnect the app: {message}"
    @staticmethod
    def error_toggle(*, message: str) -> str:
        return f"Failed to change external app access: {message}"
    @staticmethod
    def error_authorize(*, message: str) -> str:
        return f"Failed to let external apps post as you: {message}"
    @staticmethod
    def error_deauthorize(*, message: str) -> str:
        return f"Failed to stop external apps posting as you: {message}"
    error_no_identity = "This app cannot authorize posting from external apps. Use another of your devices to turn it on."
    @staticmethod
    def error_delegation_untrusted(*, message: str) -> str:
        return f"The stored authorization for external apps was not created by this account, so it is not being shown. ({message})"
    @staticmethod
    def error_save_local(*, message: str) -> str:
        return f"Created, but this device could not save a copy — copy the password now, it cannot be shown again later. ({message})"
    @staticmethod
    def error_transition(*, message: str) -> str:
        return f"Failed to change the Bluesky integration level: {message}"
    @staticmethod
    def gate_reason(*, domain: str) -> str:
        return f"Hosting an AT Protocol identity needs a public domain — this nest is reachable at \"{domain}\", which the Bluesky network cannot resolve. Claim a real domain to enable these options."
    gate_reason_pending = "Checking whether this nest has a public domain — hosting an AT Protocol identity needs one."
    @staticmethod
    def card_unlink(*, account: str) -> str:
        return f"The link to {account} is removed. The external account itself is untouched — it keeps existing on its own server and is not migrated."
    @staticmethod
    def card_mint(*, handle: str) -> str:
        return f"A new public identity {handle} is created on the Bluesky network."
    @staticmethod
    def card_reactivate(*, handle: str) -> str:
        return f"Your identity {handle} is restored — the same identity you had before, nothing new is created."
    card_publish_consent = "Your public posts become visible to everyone on the Bluesky network."
    card_deactivate = "Publishing stops and the network no longer serves your profile or posts. Your identity is kept — re-enabling restores it exactly."
    card_no_recall = "Copies of already-published posts held by other servers cannot be recalled."
    card_delete_pointer = "\"Delete my Bluesky presence\" below is the separate, stronger action."
    card_open_plane = "Third-party Bluesky apps will be able to log in as this identity once you create an app credential."
    card_dm_honesty = "Bluesky direct messages are not end-to-end encrypted and pass through this nest in transit."
    card_suspend_plane = "Connected apps stop working immediately. Nothing is deleted — your app credentials stay listed, and stepping back up restores them."
    delete_confirm_sweep = "Every post published to Bluesky is deleted, and the network is told to remove them."
    @staticmethod
    def delete_confirm_identity_kept(*, handle: str) -> str:
        return f"Your AT Protocol identity @{handle} is kept. This removes what you published, not who you are — turning AT Protocol hosting back on later restores the same identity."
    delete_confirm_apps_disconnected = "Bluesky apps you are signed in to are disconnected, and other apps can no longer post as you."
    delete_confirm_level_off = "Your Bluesky setting returns to Off."
    @staticmethod
    def delete_confirm_identity_retired(*, handle: str) -> str:
        return f"Your AT Protocol identity @{handle} is also permanently retired once the deletion finishes. This cannot be undone: the identity stops existing on the Bluesky network, and no one — not you, not this nest — can ever restore it. Turning AT Protocol hosting back on later creates a new, different identity."
    delete_retire_unavailable_web = "This identity is tied to your domain, so there is no separate record to retire — it ends when your domain stops serving it."
    delete_retire_unavailable_unpublished = "This identity has not been published yet, so there is nothing to retire."
    @staticmethod
    def error_delete_presence(*, message: str) -> str:
        return f"Could not delete your Bluesky presence: {message}"
    error_nothing_to_delete = "There is no Bluesky presence left to delete."
    @staticmethod
    def error_consent(*, message: str) -> str:
        return f"Could not send your answer: {message}"
    error_consent_gone = "That request is no longer waiting for an answer — it may have expired, or you may have already answered it on another device. Start the sign-in again from the app that asked."
    @staticmethod
    def contest_detail_contestable(*, handle: str) -> str:
        return f"A change to your AT Protocol identity {handle} was made with a key this device does not hold. You can undo it: the change and everything built on it are reversed, and afterwards only your own keys can change who controls this identity. Your posts and profile keep working through this nest — which also means it keeps the key it uses to publish them, so it could still post as you. Undoing the change does not take that key away; replacing it is a separate step."
    @staticmethod
    def contest_detail_window_closed(*, handle: str) -> str:
        return f"A change to your AT Protocol identity {handle} was made with a key this device does not hold, and the time limit for undoing it has passed. The change now stands permanently. Contact whoever runs your nest."
    @staticmethod
    def contest_detail_genesis(*, handle: str) -> str:
        return f"Your AT Protocol identity {handle} was created with a key this device does not hold, so there is no earlier state to return it to. This identity cannot be recovered — create a new one, and contact whoever runs your nest."
    @staticmethod
    def contest_detail_unauthenticated(*, handle: str) -> str:
        return f"The public record of your AT Protocol identity {handle} does not check out: the changes it lists are not signed by keys this identity's own history allows. That points at the record being tampered with, or your connection to it being intercepted — so nothing has been signed or changed from here, and undoing is not offered, because acting on a false record would destroy your real history. Try again from another network, and contact whoever runs your nest."
    @staticmethod
    def contest_deadline(*, hours: str) -> str:
        return f"About {hours} hours left to undo this."
    contest_deadline_soon = "Less than an hour left to undo this."
    @staticmethod
    def error_request_contest(*, message: str) -> str:
        return f"Could not start undoing the change: {message}"
    error_contest_not_contestable = "This change cannot be undone from here."
    @staticmethod
    def contest_confirm_undo(*, handle: str) -> str:
        return f"You are about to undo the change to {handle}, and everything published on top of it, by signing an earlier state of your identity back into place."
    contest_confirm_signs = "This device signs with the recovery key it holds, which puts your own keys back in charge of who can change this identity. Your nest keeps the separate key it publishes your posts with, so this does not stop it posting as you; replacing that key is a separate step."
    contest_confirm_directory_rules = "The public directory decides whether to accept the undo. If it refuses, nothing about your identity changes and trying again is safe."


class _Critical_alerts:
    @staticmethod
    def atproto_custody_mismatch(*, handle: str) -> str:
        return f"Security alert: the published record of your AT Protocol identity {handle} names a recovery key this device does not hold. Your identity may not be under your control — do not trust it for anything sensitive. You may be able to undo this yourself from Settings → AT Protocol, and there is a time limit; you can also contact whoever runs your nest."
    @staticmethod
    def atproto_handle_unbound(*, domain: str, published: str) -> str:
        return f"Security alert: the published record of your AT Protocol identity does not name a handle at {domain}. It is published as {published} instead — people looking for you may be finding someone else. Do not trust this identity for anything sensitive, and contact whoever runs your nest."
    @staticmethod
    def atproto_handle_unbound_none(*, domain: str) -> str:
        return f"Security alert: the published record of your AT Protocol identity names no handle at all, so nobody can find you at {domain}. Do not trust this identity for anything sensitive, and contact whoever runs your nest."
    recovery_replacement_pending = "Security alert: someone used your identity secret to request a replacement of your account recovery key. If that was not you, someone else has your identity secret. Cancel it in Settings, under Recovery kit."
    @staticmethod
    def recovery_replacement_pending_detail(*, days: str, fingerprint: str) -> str:
        return f"The replacement takes effect in {days} days unless cancelled. The pending recovery key begins {fingerprint} — if you hold a recovery kit and it does not begin with those characters, the request was not made with your kit."
    @staticmethod
    def domain_expiring_admin(*, domain: str, days: str) -> str:
        return f"Urgent: {domain} — the name this deployment runs on — expires in {days} days. If it lapses, whoever registers it next receives your mail (including password resets for accounts tied to those addresses), and anyone recovering an account from their recovery phrase alone will no longer be able to find this nest. Renew it at your registrar now."
    @staticmethod
    def domain_expiring_resident(*, domain: str, days: str) -> str:
        return f"Urgent: {domain} — the name this deployment runs on — expires in {days} days. If it lapses, mail sent to your address there will go to whoever registers the name next, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address."
    @staticmethod
    def domain_expired_admin(*, domain: str) -> str:
        return f"Urgent: {domain} — the name this deployment runs on — has expired. Whoever registers it next receives your mail, including password resets for accounts tied to those addresses. Renew or redeem it at your registrar immediately; if it is gone, move the deployment to a new domain."
    @staticmethod
    def domain_expired_resident(*, domain: str) -> str:
        return f"Urgent: {domain} — the name this deployment runs on — has expired. Mail sent to your address there may now reach someone else, and recovering your account from your recovery phrase alone will not work. Contact whoever runs this nest, and make sure you know its direct address."
    @staticmethod
    def domain_lapsing_admin(*, domain: str, status: str) -> str:
        return f"Urgent: the registration for {domain} — the name this deployment runs on — is in a hold or deletion state ({status}) and is being withdrawn from DNS. Whoever registers it next receives your mail. Contact your registrar now; renewal is usually still possible at this stage."
    @staticmethod
    def domain_lapsing_resident(*, domain: str, status: str) -> str:
        return f"Urgent: the registration for {domain} — the name this deployment runs on — is being withdrawn ({status}). Mail to your address there will stop arriving, and recovering your account from your recovery phrase alone will stop working. Contact whoever runs this nest, and make sure you know its direct address."


class _Bridges:
    title = "Bridges"
    detail_title = "Bridge"
    follows = "Follows"
    remove_follow = "Remove Follow"
    refresh_list = "Refresh bridge list"
    link_action = "Link"
    unlink_action = "Unlink"
    unlink_confirm_msg = "This will disconnect the bridge. You can re-link it later."
    no_follows = "No follows yet."
    @staticmethod
    def not_available(*, name: str) -> str:
        return f"{name} not available on this nest."
    unsafe_redirect = "This nest returned an unsafe link (links must be https). Not following it."
    @staticmethod
    def unlink(*, name: str) -> str:
        return f"Unlink {name}"
    @staticmethod
    def link(*, name: str) -> str:
        return f"Link {name}"
    no_link_method = "This bridge has no link method available right now."
    id_to_follow = "ID to follow"
    id_to_follow_placeholder = "e.g. did:plc:... or user.bsky.social"
    petname_optional = "Petname (optional)"
    not_available_node = "Not available on this node"
    loading_bridges = "Loading bridges..."
    link_method = "Link method"
    mark_all_read = "Mark All Read"
    no_notifications = "No notifications"
    link_status = "Link Status"
    status_unavailable = "Unavailable"
    link_bridge = "Link Bridge"
    unlink_bridge = "Unlink Bridge"
    bridge_settings_desc = "Configure bridge-specific settings."
    no_settings = "No settings available."
    no_follows_configured = "No follows configured."
    add_follow = "Add Follow"
    no_bridges = "No bridges available."
    no_bridges_desc = "Bridges connect your nest to other networks."
    select_bridge = "Select a bridge"
    select_bridge_desc = "Select a bridge to view details."
    id = "ID"
    petname = "Petname"
    port = "Port"
    password = "Password"
    subscribe = "Subscribe"
    handle = "Handle"
    or_ = "or"
    friendly_name_placeholder = "Friendly name"
    source_blocked = "This account can only add sources your guardian approves."
    source_request_button = "Ask your guardian"
    source_request_pending = "Asked — waiting for your guardian"
    source_request_approved = "Approved — try again"


class _MediaFile_detail:
    delete_file = "Delete File"
    @staticmethod
    def delete_confirm(*, name: str) -> str:
        return f"Are you sure you want to delete \"{name}\"? This cannot be undone."
    delete_confirm_title = "Delete this file?"
    delete_confirm_button = "Delete"


class _MediaStatus_label:
    synced = "Synced"
    uploading = "Uploading"
    downloading = "Downloading"
    conflict = "Conflict"
    local_only = "Local Only"
    remote_only = "Remote Only"


class _MediaWatched:
    scan_now = "Scan Now"
    scanning = "Scanning..."
    no_watched = "No watched directories"
    add_directory = "Add Watched Directory"
    remove_directory = "Remove directory"
    directory_label = "Directory"
    @staticmethod
    def error_read(*, directory: str, message: str) -> str:
        return f"Failed to read {directory}: {message}"
    @staticmethod
    def error_upload(*, directory: str, message: str) -> str:
        return f"Failed to upload {directory}: {message}"
    target_folder = "Back up into"
    no_folders = "You don't have a folder yet. Create one under Settings → Folders first."
    @staticmethod
    def error_folders(*, message: str) -> str:
        return f"Failed to load your folders: {message}"


class _Media:
    title = "Media"
    view_list = "List"
    view_grid = "Grid"
    sort_name = "Name"
    sort_size = "Size"
    sort_date = "Date"
    sort_ascending = "Ascending"
    sort_descending = "Descending"
    filter_all = "All media"
    no_media_yet = "No media yet"
    choose_file = "Choose file…"
    type_file_path = "Type a file path…"
    file_path_required = "Type the path to a file first, then press Upload."
    file_required = "Choose a file first, then press Upload."
    file_not_found = "That file is no longer in your folders."
    upload_failed = "Upload failed"
    upload = "Upload"
    @staticmethod
    def uploading(*, progress: str) -> str:
        return f"Uploading... {progress}%"
    no_folders = "No folders available. Configure a sync source first."
    source_online = "Files reachable"
    source_offline = "Files unreachable"
    source_offline_notice = "Files unreachable — no device holding them is connected"
    watched_directories = "Watched Directories"
    add_file = "Add File"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load media: {message}"
    @staticmethod
    def error_upload(*, message: str) -> str:
        return f"Failed to upload: {message}"
    @staticmethod
    def error_delete(*, message: str) -> str:
        return f"Failed to delete: {message}"
    error_no_set = "You don't have a folder to upload into yet. Create a folder under Settings → Folders first."
    @staticmethod
    def error_restore(*, message: str) -> str:
        return f"Failed to restore version: {message}"
    error_followed_unavailable = "This folder is no longer shared publicly. Its owner may have stopped sharing it, or removed it."
    @staticmethod
    def error_followed_fetch(*, message: str) -> str:
        return f"Couldn't read that followed folder: {message}"
    error_followed_read_only = "You follow this folder — it's read-only here. Switch to one of your own folders to upload."
    error_metadata_only_folder = "This folder's content stays on your devices, so it can't be uploaded here. Put the file in the folder on a device that syncs it."
    versions_title = "Version history"
    versions_loading = "Loading versions…"
    @staticmethod
    def versions_error(*, message: str) -> str:
        return f"Failed to load versions: {message}"
    @staticmethod
    def version_author(*, author: str) -> str:
        return f"Edited by {author}"
    version_restore = "Restore"
    versions_show_pruned = "Show recently pruned"
    version_pruned_badge = "Pruned"
    version_undelete = "Recover"
    @staticmethod
    def error_undelete(*, message: str) -> str:
        return f"Failed to recover version: {message}"
    restore_confirm_title = "Restore this version?"
    restore_confirm_body = "The file will return to this version on all your devices. The current version stays in the history."
    restore_confirm = "Restore"
    restore_cancel = "Cancel"
    detail_close = "Close"
    download = "Download"
    @staticmethod
    def error_download(*, message: str) -> str:
        return f"Failed to download: {message}"
    external_open = "Open externally"
    @staticmethod
    def external_open_confirm_title(*, name: str) -> str:
        return f"Open \"{name}\" externally?"
    external_open_confirm_body = "The clip is decrypted to a private temporary file and handed to your system's media player."
    external_open_confirm = "Open"
    external_open_cancel = "Cancel"
    @staticmethod
    def error_external_open(*, message: str) -> str:
        return f"Failed to open externally: {message}"
    file_detail = _MediaFile_detail
    status_label = _MediaStatus_label
    watched = _MediaWatched


class _BackupsIntegrity_check:
    title = "Backup Integrity Check"
    @staticmethod
    def description(*, folder: str) -> str:
        return f"Verify that all snapshots, manifests, and chunks are intact for \"{folder}\"."
    options = "Options"
    verify_content = "Verify content (slower, more thorough)"
    results = "Results"
    snapshots_checked = "Snapshots Checked"
    files_checked = "Files Checked"
    manifests_checked = "Manifests Checked"
    chunks_checked = "Chunks Checked"
    issues = "Issues"
    missing_manifests = "Missing Manifests"
    missing_chunks = "Missing Chunks"
    corrupt_manifests = "Corrupt Manifests"
    start_check = "Start Check"
    all_ok = "All OK"
    @staticmethod
    def errors_found(*, count: str) -> str:
        return f"{count} errors found"


class _BackupsRetention:
    keep_last = "Keep Last"
    keep_daily = "Keep Daily"
    keep_weekly = "Keep Weekly"
    keep_monthly = "Keep Monthly"
    keep_yearly = "Keep Yearly"
    prune_preview = "Prune Preview"
    would_prune = "Would Prune"
    would_keep = "Would Keep"
    preview = "Preview"
    @staticmethod
    def keep_last_count(*, count: str) -> str:
        return f"Keep Last: {count}"
    @staticmethod
    def keep_daily_count(*, count: str) -> str:
        return f"Keep Daily: {count}"
    @staticmethod
    def keep_weekly_count(*, count: str) -> str:
        return f"Keep Weekly: {count}"
    @staticmethod
    def keep_monthly_count(*, count: str) -> str:
        return f"Keep Monthly: {count}"
    @staticmethod
    def keep_yearly_count(*, count: str) -> str:
        return f"Keep Yearly: {count}"
    prune_now = "Prune Now"


class _BackupsRepo_stats:
    title = "Repository Statistics"
    total_files = "Total Files"
    raw_size = "Raw Size"
    stored_size = "Stored Size"
    dedup_ratio = "Dedup Ratio"
    storage_backend = "Storage Backend"
    encryption = "Encryption & Compression"
    encryption_algo = "ChaCha20-Poly1305"
    compression = "zstd level 3"
    chunk_sizes = "512 KB - 16 MB avg 4 MB"
    loading_stats = "Loading statistics..."


class _BackupsDetail:
    snapshot_id = "Snapshot ID"
    created = "Created"
    device_id = "Device ID"
    tags = "Tags"
    parent_id = "Parent ID"
    none = "None"
    file_count = "File Count"
    total_size = "Total Size"


class _BackupsDiff:
    title = "Compare"
    compare_with = "Compare with:"
    select_snapshot = "Select snapshot..."
    compare = "Compare"
    no_comparison = "No Comparison"
    no_comparison_desc = "Select a snapshot and tap Compare to view differences."
    @staticmethod
    def added_count(*, count: str) -> str:
        return f"+{count} added"
    @staticmethod
    def removed_count(*, count: str) -> str:
        return f"-{count} removed"
    @staticmethod
    def modified_count(*, count: str) -> str:
        return f"~{count} modified"
    @staticmethod
    def net(*, size: str) -> str:
        return f"Net: {size}"
    added = "Added"
    removed = "Removed"
    modified = "Modified"


class _BackupsNotification:
    view_backups = "View Backups"
    complete_title = "Backup Complete"
    @staticmethod
    def complete_body(*, folder: str, count: str, size: str) -> str:
        return f"{folder}: {count} files ({size})"
    failed_title = "Backup Failed"
    @staticmethod
    def failed_body(*, folder: str, error: str) -> str:
        return f"{folder}: {error}"


class _Backups:
    title = "Backups"
    backup_now = "Backup Now"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load backups: {message}"
    @staticmethod
    def error_create_snapshot(*, message: str) -> str:
        return f"Failed to create snapshot: {message}"
    @staticmethod
    def error_delete_snapshot(*, message: str) -> str:
        return f"Failed to delete snapshot: {message}"
    @staticmethod
    def error_delete_snapshot_immediate(*, message: str) -> str:
        return f"Failed to delete snapshot immediately: {message}"
    @staticmethod
    def error_undelete_snapshot(*, message: str) -> str:
        return f"Failed to recover snapshot: {message}"
    @staticmethod
    def error_prune(*, message: str) -> str:
        return f"Failed to apply the retention policy: {message}"
    @staticmethod
    def error_check(*, message: str) -> str:
        return f"Failed to run the integrity check: {message}"
    @staticmethod
    def error_detail(*, message: str) -> str:
        return f"Failed to open the snapshot: {message}"
    error_download_manifest = "This file's content address could not be read, so it cannot be downloaded."
    last_backed_up = "Last backed up:"
    loading_snapshots = "Loading snapshots..."
    no_snapshots = "No snapshots yet."
    @staticmethod
    def snapshot(*, id: str) -> str:
        return f"Snapshot #{id}"
    folder = "Folder"
    @staticmethod
    def file_count(*, count: str) -> str:
        return f"{count} files"
    file = "File"
    date = "Date"
    download = "Download"
    no_snapshots_desc = "Snapshots are created when you back up files"
    snapshot_title = "Snapshot"
    select_snapshot = "Select a snapshot to view files."
    no_files_in_snapshot = "No files in this snapshot."
    create_snapshot = "Create Snapshot"
    snapshots_title = "Snapshots"
    last_backed_up_never = "Last backed up: never"
    @staticmethod
    def last_backed_up_at(*, when: str) -> str:
        return f"Last backed up: {when}"
    no_folders = "No folders yet — create one under Settings → Folders."
    folder_label = "Folder"
    @staticmethod
    def snapshot_row(*, id: str, when: str, files: str, size: str) -> str:
        return f"#{id} · {when} · {files} · {size}"
    @staticmethod
    def snapshot_state_deletion_pending(*, when: str) -> str:
        return f"Deletion scheduled — cancel before {when}"
    @staticmethod
    def snapshot_state_soft_deleted(*, when: str) -> str:
        return f"Deleted — recoverable until {when}"
    snapshot_state_deletion_pending_undated = "Deletion scheduled"
    snapshot_state_soft_deleted_undated = "Deleted — still recoverable"
    snapshot_integrity_ok = "integrity verified"
    snapshot_integrity_implicated = "integrity problem found in this snapshot"
    snapshot_delete_button = "Delete"
    snapshot_undelete_button = "Recover"
    prune_button = "Apply retention policy"
    prune_preview_title = "Retention policy preview"
    @staticmethod
    def prune_preview_counts(*, would_prune: str, remaining: str) -> str:
        return f"{would_prune} would be deleted, {remaining} kept."
    prune_preview_nothing = "Nothing to prune — every snapshot is within this set's retention policy."
    @staticmethod
    def prune_preview_candidate(*, id: str, when: str) -> str:
        return f"#{id} · {when}"
    prune_policy_not_set = "No retention policy configured for this set. Set one on the Folders page."
    prune_policy_unparseable = "This set's retention policy could not be read, so nothing was pruned. Re-set it on the Folders page."
    prune_execute_button = "Delete them"
    prune_cancel_button = "Cancel"
    check_button = "Check integrity"
    @staticmethod
    def check_result_ok(*, snapshots: str, files: str, chunks: str) -> str:
        return f"Integrity check passed — {snapshots} snapshots, {files} files, {chunks} chunks verified."
    @staticmethod
    def check_result_errors(*, missing_manifests: str, missing_chunks: str, corrupt_manifests: str) -> str:
        return f"Integrity check found problems: {missing_manifests} missing manifests, {missing_chunks} missing chunks, {corrupt_manifests} corrupt manifests."
    busy_create = "Creating a snapshot…"
    busy_delete = "Deleting a snapshot…"
    busy_immediate_delete = "Deleting a snapshot immediately…"
    busy_undelete = "Recovering a snapshot…"
    busy_prune = "Applying the retention policy…"
    busy_check = "Checking integrity…"
    busy_refresh = "Loading…"
    no_backups = "No backups yet."
    restore = "Restore"
    restore_complete = "Restore Complete"
    restore_failed = "Restore Failed"
    start_restore = "Start Restore"
    restore_section_title = "Restore history"
    restore_local_title = "Restore from a local snapshot"
    restore_source_local = "local snapshot"
    @staticmethod
    def restore_history_row(*, kinds: str, source: str, when: str) -> str:
        return f"{kinds} from {source} — {when}"
    restore_kinds_mail = "mail"
    restore_kinds_calendar = "calendar"
    restore_confirm_placeholder = "Re-type the snapshot id to confirm"
    restore_confirm_button = "Restore"
    restore_progress_idle = "Select a snapshot and re-type its id to restore."
    restore_progress_running = "Restoring…"
    restore_progress_done = "Done — restart the bridge."
    restore_warning_config_absent = "Restored, but the bridge's sign-in keys were not part of the restore — the bridge can't sign in after it restarts until they are restored too."
    restore_no_snapshots = "No local snapshots available to restore."
    restore_source_label = "Backup destination"
    restore_snapshot_label = "Snapshot"
    restore_no_destinations = "No backup destinations yet — add one under \"Backup destinations\" above."
    @staticmethod
    def restore_divergence_banner(*, count: str) -> str:
        return f"{count} MUAs reconnected with newer state"
    restore_divergence_modal_title = "Restore divergence (forensic)"
    @staticmethod
    def restore_divergence_detail_row(*, collection: str, mua: str, client: str, server: str, lost: str) -> str:
        return f"{collection} · {mua} · client modseq {client} / server modseq {server} · ~{lost} writes lost"
    restore_divergence_unknown_mua = "(unknown)"
    restore_divergence_footer = "Lost writes cannot be recovered — they died with the source nest. This list is forensic."
    restore_divergence_close = "Close"
    immediate_delete_button = "Delete now"
    @staticmethod
    def immediate_delete_modal_title(*, id: str) -> str:
        return f"Delete snapshot #{id} immediately?"
    immediate_delete_warning = "This permanently deletes the snapshot now, skipping the soft-delete window, and cannot be undone. At least three snapshots are always kept."
    immediate_delete_confirm_id_placeholder = "Re-type the snapshot id to confirm"
    immediate_delete_acknowledge_prompt = "Type this exact phrase to confirm:"
    immediate_delete_acknowledge_placeholder = "Acknowledgement phrase"
    immediate_delete_confirm_button = "Delete immediately"
    immediate_delete_cancel_button = "Cancel"
    backup_destinations_title = "Backup destinations"
    backup_destinations_desc = "Replicate your data to another nest you control. Chunks are sealed under your backup key — the destination never reads them."
    backup_destinations_empty = "No backup destinations configured."
    backup_destination_add_button = "Add destination"
    backup_destination_form_add_title = "Add a backup destination"
    backup_destination_form_edit_title = "Edit backup destination"
    backup_destination_url_placeholder = "Destination nest URL (https://…)"
    backup_destination_name_placeholder = "Friendly name (optional)"
    backup_destination_add_confirm = "Save"
    backup_destination_add_cancel = "Cancel"
    backup_destination_edit_button = "Edit"
    backup_destination_remove_button = "Remove"
    backup_destination_unattested_mark = "Added before you recovered this account — still backing up. Keep it, or remove it if you don't recognise it."
    backup_destination_keep_button = "Keep"
    backup_destination_remove_confirm_title = "Remove this backup destination?"
    backup_destination_remove_confirm_button = "Remove"
    backup_destination_remove_cancel_button = "Cancel"
    backup_destination_edit_different_nest = "That URL points to a different nest. Remove this destination and add the new one."
    backup_destinations_full = "This box's backup list is full. Remove a destination or a covered folder, then try again."
    backup_destination_resolving = "Resolving destination…"
    backup_destination_last_upload_never = "Last synced: never"
    @staticmethod
    def backup_destination_last_upload(*, when: str) -> str:
        return f"Last synced: {when}"
    @staticmethod
    def backup_destination_backlog(*, count: str) -> str:
        return f"{count} queued"
    backup_destination_last_audit_never = "Last checked: never"
    @staticmethod
    def backup_destination_last_audit(*, when: str) -> str:
        return f"Last checked: {when}"
    @staticmethod
    def backup_audit_alert_freshness(*, destination: str, days: str) -> str:
        return f"{destination} is {days} days behind your data. Your backup there is not keeping up."
    @staticmethod
    def backup_audit_alert_inclusion(*, destination: str, missing: str, sampled: str) -> str:
        return f"{destination} is missing {missing} of {sampled} records we checked for. Your backup there is incomplete."
    @staticmethod
    def backup_audit_alert_overdue(*, destination: str, days: str) -> str:
        return f"{destination} has not been checked for {days} days. We cannot confirm your backup there is intact."
    backup_destination_last_self_audit_never = "Self-checked: not yet"
    @staticmethod
    def backup_destination_last_self_audit(*, when: str) -> str:
        return f"Self-checked: {when}"
    @staticmethod
    def backup_audit_alert_self_reported(*, destination: str) -> str:
        return f"{destination} reports that its own copy of your data failed its check. That copy cannot be relied on."
    @staticmethod
    def backup_audit_alert_source_regressed(*, destination: str, days: str) -> str:
        return f"{destination} still holds data your nest lost when it went back to an older copy. It will be kept there for about {days} more days — ask whoever restored your nest whether a newer copy exists."
    @staticmethod
    def backup_audit_alert_source_regressed_until_recovered(*, destination: str) -> str:
        return f"{destination} still holds data your nest lost when it went back to an older copy. It will be kept there until it is recovered — ask whoever restored your nest whether a newer copy exists."
    backup_destination_kind_nest = "Another nest"
    backup_destination_kind_client_device = "This device"
    @staticmethod
    def backup_destination_kind_unknown(*, kind: str) -> str:
        return f"Unsupported destination ({kind})"
    backup_destination_kind_select_label = "Where should the copy live?"
    backup_destination_capacity_placeholder = "Storage limit, e.g. 50 GB"
    backup_destination_capacity_invalid = "Enter a storage limit like \"50 GB\" or \"500 MB\"."
    @staticmethod
    def backup_destination_usage(*, held: str, cap: str) -> str:
        return f"{held} of {cap} used"
    @staticmethod
    def backup_destination_usage_cap_reached(*, held: str, cap: str) -> str:
        return f"{held} of {cap} used — full, older copies are being dropped"
    @staticmethod
    def backup_destination_usage_uncapped(*, held: str) -> str:
        return f"{held} held, no limit set"
    backup_destination_usage_unknown = "Nothing held yet"
    backup_sole_client_destination_warning = "Every backup destination you have is one of your own devices. Devices get lost, wiped and replaced — add a nest destination so a copy lives somewhere else."
    backup_destination_custodian_exposure = "This device will keep a complete offline copy of your data. Anyone who can unlock it can read all of it, not just what is on screen."
    @staticmethod
    def backup_orphaned_store_row(*, held: str) -> str:
        return f"This device is still holding {held} of a backup copy. No destination uses it any more."
    backup_destination_reclaim_button = "Free up this space"
    backup_reclaim_confirm_title = "Delete this device's backup copy?"
    backup_reclaim_confirm_body = "This device can currently restore your data on its own, with no nest reachable. Deleting the copy ends that. You can rebuild it by making this device a backup destination again."
    backup_reclaim_confirm_button = "Delete the copy"
    backup_reclaim_cancel_button = "Keep it"
    backup_reclaim_still_hosting = "This device is still backing up right now, so nothing was deleted. Try again in a moment."
    backup_destination_reseed_button = "Restore my data to this nest"
    backup_reseed_confirm_title = "Restore this device's copy to this nest?"
    backup_reseed_confirm_body = "This copies the backup this device holds onto this nest and makes it live again. Nothing is deleted, here or on the nest. If this nest already holds your data, it stops rather than mixing the two."
    backup_reseed_confirm_button = "Restore"
    backup_reseed_cancel_button = "Not now"
    backup_reseed_running = "Restoring your data to this nest…"
    backup_reseed_no_agent = "This device runs no backup service, so it holds no copy to restore from."
    @staticmethod
    def backup_reseed_failed(*, reason: str) -> str:
        return f"The restore stopped before anything was made live: {reason}"
    @staticmethod
    def backup_reseed_reenroll_failed(*, reason: str) -> str:
        return f"Your data is back on this nest, but this device could not sign up again as its backup: {reason}. Add this device as a backup destination to keep a copy here."
    reseed_result_whole = "Your data is back on this nest."
    reseed_result_incomplete = "The restore is not complete yet. Run it again to finish what is missing."
    reseed_set_mail = "Mail"
    @staticmethod
    def reseed_set_restored(*, set: str, count: str) -> str:
        return f"{set}: {count} restored"
    @staticmethod
    def reseed_set_already_restored(*, set: str) -> str:
        return f"{set}: already restored"
    @staticmethod
    def reseed_set_refused(*, set: str, remedy: str) -> str:
        return f"{set}: not restored. {remedy}"
    reseed_refused_target_not_fresh = "A folder with this name is already shared or published. Rename it or clear that setting, then run the restore again."
    reseed_refused_custody_incomplete = "The copy on this nest is not complete yet. Run the restore again."
    reseed_refused_custody_unsealed = "Some files arrived without their names. Run the restore again after this device's next backup."
    reseed_refused_quota_exceeded = "This nest does not have enough storage for the copy. An admin can raise the limit in the admin app."
    reseed_refused_not_enrolled = "This nest does not hold your backup key. Run the restore again."
    reseed_refused_folder_unnamed = "This device does not know the folder's name, so it was left on the nest without being restored."
    reseed_refused_target_missing = "The folder to restore into could not be created on this nest. Run the restore again."
    reseed_refused_rehome_unsigned = "This device could not sign the folder's files for the restore, so it was left on the nest without being restored. Sign in on this device again, then run the restore again."
    reseed_refused_other = "The nest refused it."
    @staticmethod
    def reseed_gap_sidecarless_segments(*, count: str) -> str:
        return f"{count} parts of your mail arrived without their index, so some mail is still missing. Run the restore again after this device's next backup."
    @staticmethod
    def reseed_gap_unnamed_files(*, count: str) -> str:
        return f"{count} files arrived without their names, so some files are still missing."
    backup_destination_remove_reclaim_checkbox = "Also delete this device's copy now"
    @staticmethod
    def backup_reclaim_after_remove_failed(*, reason: str) -> str:
        return f"The destination was removed, but this device's copy could not be deleted: {reason}"
    backup_reclaim_no_agent = "This device is not running a sync agent, so it holds no backup copy to delete."
    choose_destination = "Choose Destination..."
    no_destination = "No destination selected"
    reveal_in_finder = "Reveal in Finder"
    about_restore = "About Restore"
    verify_integrity = "Verify Integrity"
    retention_and_prune = "Retention & Prune"
    statistics = "Statistics"
    sync_conflicts = "Sync Conflicts"
    all_devices = "All Devices"
    delete_snapshot = "Delete Snapshot"
    delete_snapshot_confirm = "Delete this snapshot? This action cannot be undone."
    prune_snapshots = "Prune Old Snapshots"
    prune_confirm = "Prune old snapshots? Only the latest 3 will be kept."
    @staticmethod
    def prune_result(*, count: str) -> str:
        return f"Pruned {count} snapshot(s)."
    check_integrity = "Check Integrity"
    integrity_passed = "Integrity check passed."
    integrity_failed = "Integrity check found errors:"
    @staticmethod
    def retention_title(*, name: str) -> str:
        return f"Retention Policy — {name}"
    restore_into_directory_desc = "Restores every file in this snapshot into the chosen directory."
    @staticmethod
    def restore_files_written(*, count: str, path: str) -> str:
        return f"Restored {count} files to {path}"
    @staticmethod
    def restore_about_detail(*, id: str) -> str:
        return f"Fetches every file in snapshot #{id}, decrypts it on this device, and writes it into the chosen directory."
    restore_directory_panel_message = "Choose a directory to restore this snapshot into"
    @staticmethod
    def snapshot_count(*, count: str) -> str:
        return f"{count} snapshots"
    status_title = "Backup Status"
    @staticmethod
    def last_seq(*, seq: str) -> str:
        return f"Last Seq: {seq}"
    integrity_check = _BackupsIntegrity_check
    retention = _BackupsRetention
    repo_stats = _BackupsRepo_stats
    detail = _BackupsDetail
    diff = _BackupsDiff
    notification = _BackupsNotification


class _Folders:
    title = "Folders"
    photo_library_section = "Photo Library"
    synced_locations = "Synced Locations"
    no_locations_bound = "No locations bound to this set."
    location_path_placeholder = "Location path"
    bind_location = "Bind Location"
    choose = "Choose…"
    @staticmethod
    def deletes_held(*, count: str) -> str:
        return f"This folder looks empty. Deletions held: {count}. Reconnect the folder, or apply them to your nest."
    @staticmethod
    def apply_deletes(*, count: str) -> str:
        return f"Apply held deletions ({count})"
    @staticmethod
    def unreadable(*, count: str) -> str:
        return f"Fauna couldn't read {count} items in this folder, so it has stopped syncing them. Check that the drive is connected and that Fauna can open the folder."
    keep_syncing_when_logged_out = "Keep syncing when logged out"
    keep_syncing_help_off = "The sync agent only runs while you are logged in. Turn this on to keep it syncing on this machine after you disconnect."
    keep_syncing_help_on = "The sync agent keeps running on this machine after you log out."
    offline_share_section = "Share with someone next to you"
    offline_share_start = "Share a folder"
    offline_share_receive = "Receive a folder"
    offline_share_own_code_label = "Your code"
    offline_share_own_code_help = "Give this to the person next to you — read it out, or let them read it off your screen — and check that what they type back matches. It ends with where your device can be reached, so copy all of it. Never send it in a message: exchanging it in person is what makes it safe."
    offline_share_peer_code_label = "Their code"
    offline_share_begin = "Begin sharing"
    offline_share_expect = "Ready to receive"
    offline_share_status_idle = "Not started"
    offline_share_status_expecting = "Waiting for their invitation…"
    offline_share_status_offer_sent = "Invitation sent"
    offline_share_status_awaiting_consent = "Waiting for them to accept…"
    offline_share_status_delivering = "Setting up the shared folder…"
    offline_share_status_delivered = "Shared"
    offline_share_status_admitted = "Joined"
    offline_share_status_failed = "Did not finish"
    @staticmethod
    def offline_share_from(*, who: str, code: str) -> str:
        return f"{who} wants to share a folder with you ({code})"
    @staticmethod
    def offline_share_set(*, code: str) -> str:
        return f"Shared folder {code}"
    offline_share_code_malformed = "That does not look like a code. It should be 64 letters and numbers."
    offline_share_code_own = "That is this device's own code — type the other person's."
    @staticmethod
    def error_offline_share(*, message: str) -> str:
        return f"Sharing did not finish: {message}"
    share_transfer_section = "Peer transfers"
    share_serve_status_off = "Peer transfers are off — this nest does not enable them"
    share_serve_status_participation_off = "Peer transfers are off on this device"
    share_serve_status_no_sets = "No shared folders to serve"
    @staticmethod
    def share_serve_status_serving(*, count: str) -> str:
        return f"Serving {count} shared folder(s) to members"
    @staticmethod
    def share_transfer_peer_row(*, folder: str, who: str) -> str:
        return f"{folder} — {who}"
    @staticmethod
    def share_transfer_progress(*, files: str, rows: str) -> str:
        return f"{files} file(s), {rows} change(s) this pass"
    share_transfer_state_admission_pending = "Waiting to be admitted"
    share_transfer_state_pulling = "Receiving"
    share_transfer_state_up_to_date = "Up to date"
    @staticmethod
    def share_transfer_state_limited(*, source: str) -> str:
        return f"Limited by {source}"
    share_transfer_source_free_space = "free space on this device"


class _DevicesConflicts:
    title = "sync conflicts need attention"
    folder = "Folder"
    device = "Device"
    section_title = "Sync Conflicts"
    keep_version = "Keep this version"
    resolve = "Resolve"
    @staticmethod
    def candidate_detail(*, device: str, size: str) -> str:
        return f"{device} · {size}"
    col_file = "File"
    col_type = "Type"
    col_time = "Time"
    type_binary = "Binary"
    type_merge = "Merge"
    type_concurrent = "Concurrent edits"
    type_other = "Conflict"
    resolved_merged = "Merged"
    resolved_latest_wins = "Latest kept"
    delete_declined = "Delete declined"
    type_catchup_failed = "Not applied"
    unreadable_path = "(unreadable file name)"
    awaiting_device = "Awaiting device"
    use_other_version = "Use the other version"


class _DevicesWizard:
    name_placeholder = "my-photos"
    name_label = "Name"
    name_required = "Enter a name to continue."
    select_devices = "Select Devices"
    schedule = "Schedule"
    retention_snapshots = "Keep snapshots"
    retention_days = "Keep days"
    review = "Review"
    creating_folder = "Creating folder..."
    failed_create = "Failed to create folder."
    select_devices_roles = "Select devices and choose what each one does (optional — skip to just store files here):"
    review_name = "Name"
    review_retention = "Retention"
    review_devices = "Devices"
    @staticmethod
    def create_error(*, message: str) -> str:
        return f"Failed to create folder: {message}"
    @staticmethod
    def create_member_error(*, message: str) -> str:
        return f"Folder created, but some devices could not be enrolled: {message}"
    no_devices_available = "No devices available. Register a device first."
    next = "Next"
    back = "Back"
    create = "Create"
    place_originates = "Uploads what I change here"
    place_originates_desc = "Files you add or edit on this device are sent to the rest of the folder."
    place_accepts = "Receives changes from elsewhere"
    place_accepts_desc = "Changes made on your other devices land on this one."
    place_applies_deletes = "Applies deletions"
    place_applies_deletes_desc = "When a file is deleted somewhere else, delete it here too. Leave this off and the device keeps every file forever — an archive."
    new_folder = "New Folder"
    review_no_devices = "None selected"
    @staticmethod
    def retention_summary(*, snapshots: str, days: str) -> str:
        return f"{snapshots} snapshots, {days} days"


class _DevicesDetail:
    device_info = "Device Info"
    device_id = "Device ID"
    capabilities = "Capabilities"
    registered = "Registered"
    no_folders = "No folders assigned to this device."
    remove_device = "Remove Device"
    @staticmethod
    def remove_confirm_text(*, label: str) -> str:
        return f"Are you sure you want to remove \"{label}\"? This device will lose access to all folders."


class _DevicesFolder_detail:
    info = "Folder Info"
    total_size = "Total Size"
    update_schedule = "Update Schedule"
    rescan_interval = "Rescan Interval"
    @staticmethod
    def delete_confirm_text(*, name: str) -> str:
        return f"Are you sure you want to delete \"{name}\"? All snapshots and membership data will be removed."


class _DevicesPeers:
    no_peers = "No peers yet"
    no_peers_desc = "Exchange QR codes to add P2P contacts."
    select_peer = "Select a peer"
    select_peer_desc = "Select a peer to view details."
    display_name = "Display Name"
    not_set = "Not set"
    connection = "Connection"
    path_type = "Path Type"
    path_lan = "LAN"
    path_wan_direct = "WAN Direct"
    path_relay = "Relay"
    latency = "Latency"
    last_endpoint = "Last Endpoint"
    success_rate = "Success Rate"
    last_connected = "Last Connected"
    lan_endpoints = "LAN Endpoints"
    remove_peer = "Remove Peer"


class _DevicesSync_locations:
    title = "Sync Locations"
    location_placeholder = "Location path (or use Browse...)"
    browse = "Browse..."
    add_location = "Add Location"
    no_locations = "No sync locations configured."
    folder_placeholder = "Folder name"
    unbound = "(unbound)"
    on_demand_label = "On-demand"
    on_demand_needs_fuse3 = "On-demand needs the fuse3 package. Install it, then restart Fauna."
    on_demand_no_fuse_device = "On-demand isn't available on this system."
    on_demand_unavailable = "On-demand isn't available here."
    on_demand_mount_refused = "On-demand can't be used in this location, so only files already on this device are kept in sync. Choose a folder inside your home folder, or turn on-demand off."
    on_demand_mount_failed = "On-demand couldn't start for this folder, so only files already on this device are kept in sync. Turn on-demand off to keep every file here."
    remove = "Remove"
    helper_unavailable = "Sync service is not running. Start it and try again."


class _Devices:
    title = "Devices"
    my_devices = "My Devices"
    no_devices = "No devices registered. Sign in to Fauna on a device to register it."
    no_folders = "No folders created."
    copy_actor_id = "Copy ID"
    online = "Online"
    offline = "Offline"
    @staticmethod
    def place_two(*, first: str, second: str) -> str:
        return f"{first} · {second}"
    @staticmethod
    def place_three(*, first: str, second: str, third: str) -> str:
        return f"{first} · {second} · {third}"
    place_none = "Doesn't send or receive changes"
    last_seen = "Last seen"
    guardian_marked_badge = "Guardian device"
    this_device_badge = "This device"
    p2p_participation_own = "Peer transfers on this device"
    p2p_participation = "Peer transfers"
    p2p_participation_unreported = "Peer transfers (not reported yet)"
    p2p_participation_off_requested = "Peer transfers (turning off)"
    @staticmethod
    def own_fingerprint(*, fingerprint: str) -> str:
        return f"Key {fingerprint}"
    members_title = "Signed-in devices without a matching entry"
    @staticmethod
    def member_fingerprint(*, fingerprint: str) -> str:
        return f"Device {fingerprint}"
    @staticmethod
    def member_enrolled_at(*, when: str) -> str:
        return f"Says it signed in {when}"
    member_note = "Removing a device here is permanent: there is no undo, and it must sign in again from scratch. A listed device is not necessarily a problem — one that has signed in but not yet registered with the server appears here until it does. Before removing one, compare its fingerprint with the devices you still have: each shows its own on its Devices page, and the one to remove matches none of them. If two appear where you expected one, one of them is a device you hold."
    member_remove_confirm = "Yes, remove it permanently"
    folders = "Folders"
    add_folder = "Add Folder"
    retention = "Retention"
    enrolled_devices = "Enrolled Devices"
    @staticmethod
    def enrolled_devices_count(*, count: str) -> str:
        return f"Enrolled Devices ({count})"
    @staticmethod
    def folders_count(*, count: str) -> str:
        return f"Folders ({count})"
    loading_members = "Loading members..."
    no_devices_enrolled = "No devices enrolled."
    device_activity = "Device activity"
    col_changes = "Changes"
    no_device_activity = "No recorded activity yet."
    selective_sync = "Selective Sync"
    include_paths = "Include Paths (comma-separated)"
    exclude_paths = "Exclude Paths (comma-separated)"
    save_paths = "Save Paths"
    nest_place_section = "What your nest keeps"
    nest_snapshots = "Keep snapshots"
    nest_snapshots_default_label = "Use the default"
    nest_snapshots_on_label = "Keep snapshots"
    nest_snapshots_off_label = "Don't keep snapshots"
    nest_quiet = "Wait for quiet (seconds)"
    nest_retention_snapshots = "Keep at most (snapshots)"
    nest_retention_days = "Keep for at most (days)"
    version_retention_count = "Keep at most (versions per file)"
    version_retention_days = "Keep versions for at most (days)"
    nest_place_blank_hint = "Leave a box empty to use the default. Empty is a choice — saving applies every box together."
    nest_save = "Save Nest Settings"
    keyless_posture_badge = "Relay only — holds no keys"
    custody_holder_section = "Custodians — who holds sealed copies of your data"
    custody_holder_scope = "Trusted to hold sealed copies — cannot read them"
    @staticmethod
    def custody_receipt_fresh(*, when: str) -> str:
        return f"Last confirmed {when}"
    @staticmethod
    def custody_receipt_stale(*, when: str) -> str:
        return f"Stale — last confirmed {when}. Treat this copy as degraded."
    custody_receipt_none = "No confirmation yet"
    @staticmethod
    def custody_held_bytes(*, held: str, cap: str) -> str:
        return f"Holding {held} of {cap}"
    custody_revoke = "Stop trusting this custodian"
    custody_revoke_bound_note = "Stops future copies and serving on honest devices. Copies already held stay held — and stay sealed forever."
    custody_held_section = "Held for others — sealed copies this device keeps"
    @staticmethod
    def custody_held_owner(*, owner: str) -> str:
        return f"Holding for {owner}"
    custody_held_scope_account = "Their account's sealed planes — unreadable on this device"
    custody_budget_label = "Keep at most (bytes)"
    custody_stop = "Stop holding"
    custody_stopped_bytes_remain = "Stopped — stored bytes remain until removed"
    custody_remove = "Remove and free the space"
    custody_remove_done = "Removed — the space is free."
    @staticmethod
    def custody_offer_title(*, owner: str) -> str:
        return f"{owner} asks this device to hold sealed copies"
    custody_offer_floor = "This device would store sealed data it cannot read. It would see only the shape: which scopes exist, how much is stored, and when it changes — never the content."
    custody_offer_accept = "Hold for them"
    custody_offer_decline = "Decline"
    custody_offer_target_label = "Where to hold"
    custody_offer_target_device = "This device"
    custody_offer_target_nest = "My nest"
    custody_degraded_badge = "Degraded — some copies were dropped under the budget"
    custody_mint_button = "Ask a friend to hold sealed copies"
    custody_mint_host_label = "Who to ask"
    custody_mint_host_placeholder = "Choose a contact"
    custody_mint_floor = "Their device will store sealed copies it cannot read. They will see the shape of your data — which scopes exist, how much is stored, and when it changes — never the content. Choosing custodians is choosing who sees that shape."
    custody_mint_confirm = "Send the request"
    custody_mint_no_contacts = "Start a conversation with them first — the request travels over it."
    paths_placeholder = "e.g. Documents, Photos"
    exclude_placeholder = "e.g. node_modules, .git"
    follow_public_folder = "Follow a public folder"
    follow_public_folder_hint = "Read someone else's public folder from your own app. You need their handle and the folder's name."
    follow_folder_name = "Folder name"
    follow_folder_name_hint = "Public folders are named in the clear — type the name exactly as its owner published it."
    follow_confirm = "Follow"
    followed_folders_section = "Folders you follow"
    followed_status_following = "Following"
    followed_status_unavailable = "No longer available"
    followed_unavailable_hint = "Its owner stopped sharing it publicly, or removed it. If they publish it again, it will start working here."
    unfollow_folder = "Remove"
    followed_public_badge = "Public"
    @staticmethod
    def followed_owner(*, owner: str) -> str:
        return f"By {owner}"
    error_follow_not_found = "No public folder by that name for that person. Check the handle and the folder name."
    @staticmethod
    def error_follow_failed(*, message: str) -> str:
        return f"Couldn't follow that folder: {message}"
    @staticmethod
    def error_unfollow_failed(*, message: str) -> str:
        return f"Couldn't remove that folder: {message}"
    folder_audience = "Who can see this folder"
    folder_audience_private = "Private"
    folder_audience_public = "Public"
    folder_audience_shared = "Shared"
    folder_audience_hint = "Private folders are encrypted so only you can read them. Public folders are readable by anyone on the web."
    folder_audience_shared_hint = "This folder is shared with other people, so it can't be made private while it's shared. To make it private, remove the sharing in the section below first — or change who it is shared with there."
    folder_audience_public_bound_hint = "This folder is shared with other people and currently public. Pick Shared to re-seal it so only those people can read it. Anything published while it was public should still be treated as public."
    declassify_title = "Make this folder public?"
    declassify_body = "Anyone on the web will be able to read this folder's files, and its file and folder names too — they become part of the address of each file."
    declassify_irreversible = "Making it private again protects only files you add afterwards. Anything published while the folder is public should be treated as public for good."
    declassify_confirm = "Make public"
    folder_audience_unattested = "This folder is public, but this app can't confirm that you made it public. Until you confirm again, up-to-date apps may keep its files encrypted, and your website may not show them."
    folder_audience_reconfirm = "Confirm public"
    folder_residency = "Content kept on the nest"
    folder_residency_full = "Full — the nest keeps this folder's content"
    folder_residency_metadata_only = "Metadata only — content stays on my devices"
    folder_residency_hint = "The nest keeps a copy of this folder's content, so a device can catch up while your other devices are offline."
    folder_residency_metadata_only_hint = "This folder's content stays on your devices only. It moves between them while one of them holding it is online, and the nest cannot restore it. File names, changes and snapshots still sync through the nest."
    residency_blocked_by_serving = "Turn off website serving, WebDAV serving and any paywall first — those serve this folder's content from the nest, which needs a copy of it."
    residency_confirm_title = "Stop keeping this folder's content on the nest?"
    residency_confirm_body = "The nest's copy of this folder's content is deleted now, and your devices become the only holders. Content moves between your devices only while one of them holding it is online. If your devices lose it, the nest cannot restore it."
    residency_confirm = "Delete the nest's copy"
    @staticmethod
    def error_set_residency(*, message: str) -> str:
        return f"Failed to change where this folder's content is kept: {message}"
    folder_exclusive_editing = "One device at a time may edit this folder"
    folder_lease_free = "No device is editing this folder right now."
    folder_lease_held_here = "This device is editing this folder right now. Your other devices keep their own changes and upload them when it finishes."
    @staticmethod
    def folder_lease_held_by(*, device: str) -> str:
        return f"{device} is editing this folder right now. Changes you make here are kept on this device and upload when it finishes."
    folder_lease_held_elsewhere = "Another device is editing this folder right now. Changes you make here are kept on this device and upload when it finishes."
    @staticmethod
    def error_set_exclusive_editing(*, message: str) -> str:
        return f"Failed to change exclusive editing for this folder: {message}"
    serve_website = "Serve this folder as your website"
    serve_website_hint = "Your site is published from this folder — an index.html here becomes your home page. Switch on your web address in Settings → Web to make it reachable."
    serve_website_live = "Your site is served from this folder at your web address — an index.html here becomes your home page."
    folder_places_title = "Device places"
    @staticmethod
    def error_set_place(*, message: str) -> str:
        return f"Failed to change the device's place: {message}"
    folder_destinations_title = "Destination places"
    folder_destination_attach = "Attach"
    folder_destination_attach_label = "Add a destination place"
    folder_destination_detach = "Detach"
    @staticmethod
    def error_folder_destination(*, message: str) -> str:
        return f"Failed to change the folder's destination places: {message}"
    serve_website_address_off = "Your site is published from this folder, but your web address is switched off, so nobody can reach it yet. Switch it on in Settings → Web."
    serve_website_needs_audience = "Make this folder public, or paywall it to a tier, for the site to be visible to visitors."
    audience_public_blocked_by_webdav = "Turn off WebDAV serving first — WebDAV needs the folder encrypted, and a public folder is not."
    audience_public_blocked_by_paywall = "Remove the paywall first — a paywalled folder cannot also be public to everyone."
    serve_webdav_blocked_by_public = "This folder is public, so there is nothing for WebDAV to encrypt. Make it private to serve it over WebDAV."
    paywall_blocked_by_public = "This folder is public, so a paywall would not restrict anyone. Make it private to paywall it."
    @staticmethod
    def error_set_audience(*, message: str) -> str:
        return f"Failed to change who can see this folder: {message}"
    @staticmethod
    def error_serve_website(*, message: str) -> str:
        return f"Failed to change website serving: {message}"
    serve_webdav = "Serve over WebDAV"
    serve_webdav_hint = "Browse and edit this set from any WebDAV client (Finder, GNOME Files, rclone)."
    serve_webdav_needs_mail = "Set up mail first — serving over WebDAV uses your mail encryption key."
    show_on_demand_finder = "Show in Finder"
    show_on_demand_files = "Show in Files"
    show_on_demand_hint = "Browse this set on demand — files download when opened and can be freed again."
    on_demand_bound_hint = "This set syncs to its bound location; remove the binding to show it on demand instead."
    paywall_tier = "Paywall to tier"
    paywall_tier_hint = "Only subscribers to the chosen tier can view this set's files; other visitors see a teaser."
    paywall_tier_none = "Not paywalled (public)"
    paywall_tier_needs_tier = "Create a subscription tier first to paywall this set."
    conflict_policy = "Conflict policy"
    conflict_policy_auto = "Auto (merge text, else latest wins)"
    conflict_policy_latest_wins = "Latest edit wins"
    sync_defaults = "Sync defaults"
    default_conflict_policy = "Default conflict policy for new sets"
    col_snapshots = "Snapshots"
    col_size = "Size"
    col_role = "Role"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load devices: {message}"
    @staticmethod
    def error_remove_device(*, message: str) -> str:
        return f"Failed to remove device: {message}"
    @staticmethod
    def error_set_p2p_participation(*, message: str) -> str:
        return f"Couldn't change peer transfers: {message}"
    error_p2p_remote_enable = "Peer transfers can only be turned on from that device itself. From here you can only turn them off."
    @staticmethod
    def error_remove_fleet_device(*, message: str) -> str:
        return f"Removed the device, but couldn't finish cleanup: {message}"
    error_remove_own_device = "This is the device you're using, so it wasn't removed. To remove it, sign out on this device."
    error_remove_unverified_device = "Couldn't confirm which of your devices this is, so nothing was removed. Try again in a moment."
    error_remove_row_mismatch = "This entry doesn't match what the device itself reports, so nothing was removed. Trying again won't change that. If the device is not one you hold, remove it by its key from the signed-in devices without a matching entry, below the list."
    @staticmethod
    def error_delete_folder(*, message: str) -> str:
        return f"Failed to delete folder: {message}"
    @staticmethod
    def error_resolve_conflict(*, message: str) -> str:
        return f"Failed to resolve conflict: {message}"
    @staticmethod
    def error_save_paths(*, message: str) -> str:
        return f"Failed to save paths: {message}"
    @staticmethod
    def error_set_conflict_policy(*, message: str) -> str:
        return f"Failed to set conflict policy: {message}"
    @staticmethod
    def error_set_nest_place(*, message: str) -> str:
        return f"Failed to save the snapshot settings: {message}"
    @staticmethod
    def error_use_other_version(*, message: str) -> str:
        return f"Failed to use the other version: {message}"
    error_other_version_unverified = "That version is no longer in this file's verified history, so nothing was changed."
    error_other_version_needs_history = "That version was saved under a previous identity of this account. Restore it from the file's version history instead."
    @staticmethod
    def error_serve_webdav(*, message: str) -> str:
        return f"Failed to change WebDAV serving: {message}"
    @staticmethod
    def error_revoke_custody(*, message: str) -> str:
        return f"Couldn't stop trusting this custodian: {message}"
    @staticmethod
    def error_paywall_set(*, message: str) -> str:
        return f"Failed to paywall the folder: {message}"
    @staticmethod
    def error_set_default_conflict_policy(*, message: str) -> str:
        return f"Failed to set the default conflict policy: {message}"
    delete_folder = "Delete Folder"
    delete_confirm_title = "Delete folder?"
    @staticmethod
    def delete_confirm_body(*, name: str) -> str:
        return f"Delete folder \"{name}\"? All data including snapshots will be permanently removed."
    remove_member = "Remove"
    shared_with = "Shared with"
    share_button = "Share…"
    @staticmethod
    def shared_badge(*, count: str) -> str:
        return f"Shared · {count}"
    not_shared_yet = "Not shared with anyone yet."
    member_access = "Access"
    member_access_reader = "Reader"
    member_access_writer = "Writer"
    member_byte_cap = "Storage cap"
    member_byte_cap_placeholder = "No cap"
    writer_uncapped_warning = "Without a cap, this member can use your entire storage quota."
    writer_public_warning = "This folder is public, so this member can change what anyone can see."
    writer_paywalled_warning = "This folder is sold to subscribers, so this member can change what they see."
    access_revoked_warning = "The owner removed your permission to make changes, so this location is no longer syncing. Your local files are untouched."
    @staticmethod
    def error_share_set(*, message: str) -> str:
        return f"Failed to share folder: {message}"
    @staticmethod
    def error_remove_member(*, message: str) -> str:
        return f"Failed to remove member: {message}"
    @staticmethod
    def error_set_member_access(*, message: str) -> str:
        return f"Failed to change member access: {message}"
    shared_with_you = "Shared with you"
    @staticmethod
    def shared_by(*, who: str) -> str:
        return f"Shared by {who}"
    @staticmethod
    def error_accept_share(*, message: str) -> str:
        return f"Failed to accept share: {message}"
    @staticmethod
    def error_decline_share(*, message: str) -> str:
        return f"Failed to decline share: {message}"
    @staticmethod
    def error_leave_share(*, message: str) -> str:
        return f"Failed to leave share: {message}"
    @staticmethod
    def error_bind_location(*, message: str) -> str:
        return f"Failed to sync this location: {message}"
    conflicts = _DevicesConflicts
    wizard = _DevicesWizard
    detail = _DevicesDetail
    folder_detail = _DevicesFolder_detail
    peers = _DevicesPeers
    sync_locations = _DevicesSync_locations


class _Sessions:
    title = "Sessions"
    kind_app = "App sign-in"
    @staticmethod
    def kind_device(*, name: str) -> str:
        return f"Device: {name}"
    kind_unknown_device = "A device key not in your device list"
    mark_this_app = "This app"
    mark_this_device = "This device"
    @staticmethod
    def detail(*, created: str, last_used: str, expires: str, address: str) -> str:
        return f"Signed in {created} · last active {last_used} · expires {expires} · {address}"
    address_not_recorded = "address not recorded"
    empty = "No sessions to show."
    revoke = "Revoke"
    revoke_others = "Sign Out Everywhere Else"
    revoke_others_confirm = "Yes, Sign Out Everywhere Else"
    revoke_others_cancel = "Cancel"
    revoke_note = "Revoking ends a sign-in and makes whoever held it prove themselves again. It does not sign a device out — a device that holds your secret key or a device grant signs itself straight back in. To end a device for good, remove it under Settings → Devices. If someone else holds your secret key, only your recovery kit ends them (Settings → Account → Recovery Kit → My Identity Was Stolen)."
    lockout_warning = "Lock this account for 24 hours. Every device is signed out, this one too. Nobody can sign in for 24 hours, you included, and there is no unlock. It does not remove somebody who holds your secret key — they come back when the lock ends. They can do the same to you: anyone with your secret key can sign you out and lock the account, again every 24 hours. Your recovery kit still works while the account is locked and moves it to a new key the thief cannot use (Settings → Account → Recovery Kit → My Identity Was Stolen) — a lock you did not set is itself the sign to use it."
    lockout_confirm_placeholder = "Type LOCK to confirm"
    lockout_button = "Lock for 24 Hours"
    @staticmethod
    def lockout_wrong_word(*, word: str) -> str:
        return f"Type {word} exactly to lock the account."
    @staticmethod
    def load_failed(*, error: str) -> str:
        return f"Could not load your sessions: {error}"
    @staticmethod
    def act_failed(*, error: str) -> str:
        return f"That did not go through: {error}"
    no_own_session = "This app does not know its own sign-in yet, so it cannot tell which one to keep. Try again in a moment."


class _SettingsIdentity_export:
    title = "Export Identity"
    desc = "Show a QR code that another device can scan to import your identity."
    warning = "Anyone who scans this QR code gets full access to your identity. Only show it in a trusted environment."
    show_qr = "Show QR Code"
    hide_qr = "Hide QR Code"


class _SettingsRecovery_kit:
    title = "Recovery Kit"
    desc = "An offline key that can recover your account if you lose your identity secret — or take it back if someone steals it. It is shown once and never stored on this device."
    status_never_created = "No recovery kit. If you lose your identity secret, your account cannot be recovered, and if someone steals it, you cannot take it back."
    status_registered = "Your recovery kit is active, and a sealed copy of your identity secret is stored for it."
    status_registered_no_escrow = "Your recovery kit is active, but no sealed copy of your identity secret is stored — your recovery phrase cannot recover this account right now. Enter your kit below and press Restore Phrase Recovery to fix this."
    @staticmethod
    def status_replacement_pending(*, days: str) -> str:
        return f"A replacement of your recovery kit was requested with your identity secret alone, and takes effect in {days} days. Cancel it below if it was not you."
    status_loading = "Checking your recovery kit…"
    create = "Create Recovery Kit"
    replace = "Replace Using My Kit"
    lost = "I Lost My Kit"
    escrow_reseal = "Restore Phrase Recovery"
    sweep_retry = "Finish Moving Your Groups"
    sweep_retry_no_old_state = "This device does not have the conversation history from your previous identity, so it cannot finish the move. If another of your devices still has those conversations, press Finish Moving Your Groups there; otherwise ask another member of each group to remove the old identity and add your new one."
    sweep_retry_not_landed = "No move of this account has been recorded, so there is nothing to finish. If you have just taken your account back, wait a moment and try once more."
    sweep_retry_landed_for_another = "This account was moved to a different identity than the one signed in here. Sign in as your current identity to finish moving your groups."
    @staticmethod
    def sweep_retry_failed(*, reason: str) -> str:
        return f"Your groups could not be updated: {reason}. Nothing was lost — press Finish Moving Your Groups to try once more."
    stolen = "My Identity Was Stolen"
    veto = "Cancel The Pending Replacement"
    stolen_confirm_placeholder = "Type SUCCEED to confirm"
    stolen_warning = "This mints a new identity and re-points your account to it. It cannot be undone, your old identity stops working, and you will need to re-add your devices. Your handle stays yours. Afterwards, create a new recovery kit — this one retires with the old identity."
    @staticmethod
    def unreadable_status(*, rows: str, since: str) -> str:
        return f"{rows} items of your account data, saved since {since}, cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in since {since}, it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take."
    @staticmethod
    def unreadable_status_undated(*, rows: str) -> str:
        return f"{rows} items of your account data cannot be read by any device signed in to your account, and no copy of their key is stored. If one of your devices has not been signed in for a while, it may still be able to read them — sign in there first. Otherwise you can let them go to free the space they take."
    let_go = "Let Go Of Unreadable Data"
    let_go_confirm_placeholder = "Type LET GO to confirm"
    @staticmethod
    def let_go_done(*, retired: str) -> str:
        return f"{retired} unreadable items were let go."
    let_go_kept = "Some of this data became readable again and was kept."
    @staticmethod
    def let_go_failed(*, message: str) -> str:
        return f"Couldn't let the data go: {message}"
    @staticmethod
    def stolen_persist_failed(*, secret: str) -> str:
        return f"Your account was re-pointed to a new identity, but saving it on this device failed. Write this secret key down NOW and import it — it is the only way back into your account: {secret}"
    @staticmethod
    def stolen_ceremony_failed(*, message: str) -> str:
        return f"Couldn't recover your account: {message}"
    @staticmethod
    def stolen_outcome_unknown_saved(*, cause: str, reported: str) -> str:
        return f"Couldn't confirm whether your account was recovered ({cause}). Your new identity is saved on this device — reopen the app to sign in with it. Details: {reported}"
    @staticmethod
    def stolen_outcome_unknown_unsaved(*, cause: str, secret: str, reported: str) -> str:
        return f"Couldn't confirm whether your account was recovered ({cause}), and this device couldn't save your new identity. Write down this secret key now and import it — it is the only way back into your account: {secret}. Details: {reported}"
    @staticmethod
    def stolen_landed_for_another(*, actor: str) -> str:
        return f"Your account has already been moved to a different identity ({actor}) — another device with the same recovery kit got there first. Import that identity to get back into your account."
    @staticmethod
    def status_failed(*, message: str) -> str:
        return f"Could not check your recovery kit: {message}"
    @staticmethod
    def action_failed(*, message: str) -> str:
        return f"Could not update your recovery kit: {message}"
    @staticmethod
    def veto_failed(*, message: str) -> str:
        return f"Could not cancel the pending replacement: {message}"
    kit_phrase_placeholder = "Paste your recovery phrase (fauna://recovery link or 64-character code)"
    kit_phrase_required = "Paste the recovery phrase for this account first."
    sweep_none = "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Press Finish Moving Your Groups below to complete it now, or ask another member of each group to remove the old identity and add your new one."
    @staticmethod
    def sweep_failed(*, reason: str) -> str:
        return f"Your account moved to your new identity, but your groups could not be updated: {reason}. Your old identity may still be able to read them."
    @staticmethod
    def sweep_all_removed(*, groups: str) -> str:
        return f"Your old identity was removed from all {groups} of your group conversations."
    @staticmethod
    def sweep_partial(*, removed: str, groups: str) -> str:
        return f"Your old identity was removed from {removed} of your {groups} group conversations. It can still read the rest — press Finish Moving Your Groups below to finish, or ask another member of those to remove it."
    sweep_none_no_retry = "Your conversations were not running, so your groups still include your old identity, and your new one is not in them. Ask another member of each group to remove the old identity and add your new one."
    @staticmethod
    def sweep_partial_no_retry(*, removed: str, groups: str) -> str:
        return f"Your old identity was removed from {removed} of your {groups} group conversations. It can still read the rest — ask another member of those to remove it."
    @staticmethod
    def sweep_unattested(*, count: str) -> str:
        return f"There are {count} other members across your groups that this cannot confirm you added yourself. If your identity was stolen, one of them could be the thief under another name."
    review_intro = "Keep the people you recognise. Anyone you don't, you can remove from your groups. You do not have to finish now."
    @staticmethod
    def review_row(*, who: str, reason: str) -> str:
        return f"{who} — {reason}"
    review_reason_compromise = "was in your groups before you recovered your account"
    review_reason_other = "this could not confirm their identity"
    review_unknown_person = "Someone no longer in any of your groups"
    review_keep = "Keep"
    review_remove = "Remove From My Groups"
    review_defer = "Review The Rest Later"
    @staticmethod
    def review_remove_partial(*, who: str, removed: str, groups: str) -> str:
        return f"Removed {who} from {removed} of {groups} of your group conversations. The rest still include them — try again, or ask another member of those groups to remove them."
    @staticmethod
    def review_remove_done_here(*, who: str, removed: str) -> str:
        return f"Removed {who} from {removed} of your group conversations."
    @staticmethod
    def review_remove_none_here(*, who: str) -> str:
        return f"{who} is not in any of your group conversations on this device."
    @staticmethod
    def review_remove_folder_seats(*, seats: str) -> str:
        return f"They are also in {seats} shared folder(s). Remove them in each folder's sharing settings — or leave the set, if it is not yours — then choose Remove again."
    @staticmethod
    def review_remove_unsynced_seats(*, seats: str) -> str:
        return f"They are also in {seats} group chat(s) not yet synced to this device. Choose Remove again after syncing completes, or from a device that has those chats."
    @staticmethod
    def review_verdict_failed(*, who: str, reason: str) -> str:
        return f"Removed {who} from your group conversations, but your review list could not be updated yet: {reason}. They may still be listed here until it succeeds."
    backup_regrant_running = "Restarting your backups under your new identity…"
    backup_regrant_done = "Your backups are running again under your new identity."
    @staticmethod
    def backup_regrant_failed(*, reason: str) -> str:
        return f"Your backups could not be restarted under your new identity yet: {reason}. This will be retried the next time you sign in."
    mls_reseal_running = "Unlocking your conversations under your new identity…"
    mls_reseal_done = "Your conversations are now held under your new identity."
    mls_reseal_partly_owed_elsewhere = "Some of your conversations are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there."
    mls_reseal_owed_elsewhere = "Your conversations are still held under your previous identity, and this device does not have the key to unlock them. Sign in on the device you used to take back your account, and it will finish there."
    @staticmethod
    def mls_reseal_failed(*, reason: str) -> str:
        return f"Your conversations could not be moved to your new identity yet: {reason}. This will be retried the next time you sign in."
    grant_remint_running = "Restoring the access you had granted to services, under your new identity…"
    grant_remint_done = "The access you had granted to services (mail filtering, search and similar) is restored under your new identity. Review each one on the Nests page — if your identity was stolen, the thief could have granted access you never did."
    grant_remint_partial = "Some of the access you had granted to services could not be restored yet. This will be retried the next time you sign in — anything already restored is listed on the Nests page for review."
    @staticmethod
    def grant_remint_failed(*, reason: str) -> str:
        return f"The access you had granted to services could not be restored under your new identity yet: {reason}. This will be retried the next time you sign in."
    corpus_reseal_running = "Moving your files across to your new identity…"
    corpus_reseal_done = "Your files are now held under your new identity."
    @staticmethod
    def corpus_reseal_partly_owed(*, done: str, remaining: str) -> str:
        return f"Moving your files across to your new identity: {done} done, {remaining} still to go. This continues on its own, and picks up where it left off each time you sign in."
    corpus_reseal_owed_elsewhere = "Your files are still held under your previous identity, and this device does not have the key to move them. Sign in on the device you used to take back your account, and it will finish there."
    @staticmethod
    def corpus_reseal_failed(*, reason: str) -> str:
        return f"Your files could not be moved to your new identity yet: {reason}. This will be retried the next time you sign in."
    mail_burn_running = "Replacing your mail keys, because your previous identity's passwords could open your mailbox…"
    @staticmethod
    def mail_burn_done(*, count: str) -> str:
        return f"Your mail encryption key was replaced and your {count} mail app password(s) were revoked — whoever held your previous identity could have used them to read your mail. Your mailbox keeps receiving as normal, but each mail app needs setting up again with a new password from this page."
    @staticmethod
    def inherited_filters(*, count: str) -> str:
        return f"{count} of your email filter rule(s) were set up before your account recovery and are still unchecked. A filter can silently bin or redirect incoming mail, so it is worth confirming you recognise each one — Settings ▸ Privacy ▸ Email Filters."
    @staticmethod
    def mail_burn_failed(*, reason: str) -> str:
        return f"Your mail keys could not be replaced yet: {reason}. Until this finishes, anyone who had your previous identity can still read new mail. This will be retried the next time you sign in."
    tier_period_rotation_running = "Replacing the keys your subscriber-only posts are locked with, because your previous identity could open them…"
    @staticmethod
    def tier_period_rotation_done(*, count: str) -> str:
        return f"New posts to your {count} subscriber tier(s) are now locked with keys your previous identity never had. Your subscribers keep their access, and posts you published before taking your account back stay readable to them — and to anyone who had the old keys, which is why only new posts are covered."
    tier_period_rotation_nest_too_old = "The keys for your subscriber-only posts could not be replaced: this nest is too old to accept them. Until it is updated, anyone who had your previous identity can read the subscriber-only posts you publish from now on."
    @staticmethod
    def tier_period_rotation_partial(*, count: str, failed: str) -> str:
        return f"Keys were replaced for {count} of your subscriber tier(s), but {failed} could not be finished. Until they are, anyone who had your previous identity can read new posts to those tiers. This will be retried the next time you sign in."
    @staticmethod
    def tier_period_rotation_failed(*, reason: str) -> str:
        return f"The keys for your subscriber-only posts could not be replaced yet: {reason}. Until this finishes, anyone who had your previous identity can read the subscriber-only posts you publish from now on. This will be retried the next time you sign in."
    drafts_reseal_running = "Recovering your unsent drafts under your new identity…"
    drafts_reseal_done = "Your unsent drafts are available again under your new identity."
    drafts_reseal_partly_owed_elsewhere = "Some of your unsent drafts are still held under a previous identity that this device cannot unlock. Sign in on the device you used to take back your account, and it will finish there."
    drafts_reseal_owed_elsewhere = "Your unsent drafts are still held under your previous identity, and this device does not have the key to unlock them. They are safe. Sign in on the device you used to take back your account, and it will finish there."
    @staticmethod
    def drafts_reseal_failed(*, reason: str) -> str:
        return f"Your unsent drafts could not be recovered yet: {reason}. They are safe, and this will be retried the next time you sign in."


class _SettingsIcloud_backup:
    title = "iCloud Backup"
    toggle = "Back up identity to iCloud Keychain"
    footer = "When off (the default), your identity stays on this device and never syncs to iCloud. Turn it on to let iCloud Keychain restore your identity on a new device. Fauna's own device-add and recovery flows are the primary way to use multiple devices."


class _SettingsPush_notifications:
    title = "Push Notifications"
    description = "Receive notifications in this browser even when the app is not open."
    update_failed = "Failed to update push notification settings."
    device_description = "Get a notification for new messages, knocks and invites on this device, even when Fauna is closed."
    opt_in_label = "Notify me on this device"
    agent_unreachable = "The Fauna sync agent is not running on this computer, so it cannot show notifications while Fauna is closed."
    no_sink = "This computer has no desktop notification service, so notifications cannot be shown here."


class _SettingsMail:
    section_title = "Mail & Calendar"
    section_description = "Enable to set up third-party mail and calendar apps like Apple Mail, Thunderbird, and Apple Calendar."
    enable_title = "Enable mail"
    enable_subtitle = "Allow a third-party mail app to connect over IMAP and SMTP"
    disable_title = "Disable mail?"
    disable_warning = "This revokes all your mail credentials and clears your mail encryption key. Third-party mail apps (IMAP/CalDAV) will stop working until you re-enable mail."
    disable_confirm = "Disable mail"
    status_disabled = "Mail is disabled"
    status_enabled = "All up to date"
    status_syncing = "Syncing mail credentials…"
    @staticmethod
    def status_rotation(*, count: str) -> str:
        return f"Rotation in progress ({count} remaining)"
    credentials_title = "Credentials"
    credentials_description = "Mail credentials you've created for your mail apps"
    credentials_empty = "No mail credentials yet"
    credentials_empty_subtitle = "Add a credential to connect a mail app"
    add_credential = "Add credential"
    rotate_keys = "Rotate mail keys"
    keys_title = "Mail encryption keys"
    banner_title = "A previous mail-credential rotation didn't finish."
    banner_subtitle = "Resume to complete it."
    resume = "Resume"
    add_title = "Add mail credential"
    name_placeholder = "Credential name (e.g. iPhone Mail)"
    type_selector = "Use a password (PLAIN) instead of a bearer token"
    password_placeholder = "Password"
    show = "Show"
    hide = "Hide"
    submit_enable = "Enable mail"
    submit_add = "Add"
    cancel = "Cancel"
    done = "Done"
    password_required = "Enter a password for this credential."
    strength_weak = "Weak"
    strength_fair = "Fair"
    strength_strong = "Strong"
    autogenerate = "Auto-generate a strong password"
    weak_password_warning = "A password you choose yourself limits how strongly your stored mail is protected at rest on an encrypted nest. Letting Fauna generate one is recommended."
    token_warning = "Copy this token into your mail app now — it is shown only once."
    copy_token = "Copy token"
    copied = "Copied"
    rotate_title = "Rotate mail keys"
    rotate_warning = "Rotating replaces your mail encryption key and re-wraps it under every surviving credential. Already-received mail stays readable. Every connected mail app must re-authenticate, and any credential you mark below as compromised loses access. This is safe to interrupt — it resumes automatically."
    rotate_exclude_caption = "Exclude compromised credentials (they lose access):"
    rotate_confirm = "Rotate keys"
    kind_password = "Password"
    kind_bearer = "Bearer token"
    credential_revoked = "Access revoked — set this mail app up again with a new password"
    revoke = "Revoke"
    revoke_confirm = "Confirm?"
    created_prefix = "created"
    last_used_never = "Never"
    copy_username = "Copy address"
    reveal_secret = "Reveal secret"
    hide_secret = "Hide secret"
    copy_secret = "Copy secret"
    @staticmethod
    def reveal_secret_failed(*, error: str) -> str:
        return f"Could not reveal secret: {error}"
    @staticmethod
    def copy_secret_failed(*, error: str) -> str:
        return f"Could not copy secret: {error}"
    secret_label = "Secret"
    mua_title = "Mail, calendar & files app setup"
    mua_description = "Connection details to enter in your mail, calendar and file apps"
    mua_imap_host = "IMAP host"
    mua_imap_port = "IMAP port"
    mua_smtp_host = "SMTP host"
    mua_smtp_port = "SMTP port"
    mua_caldav_host = "CalDAV host"
    mua_caldav_port = "CalDAV port"
    mua_webdav_url = "WebDAV URL"
    mua_username = "Username"
    mua_auth = "Authentication"


class _SettingsAccount_page:
    title = "Account"
    identity = "Identity"
    actor_id = "Actor ID"
    node = "Node"
    bluesky = "Bluesky"
    unlink = "Unlink"
    link = "Link"
    bluesky_handle_placeholder = "yourname.bsky.social"
    copied_clipboard = "Copied to clipboard"
    change_handle = "Change Handle"
    new_handle = "New handle"
    new_handle_placeholder = "Enter new handle"
    handle_changed = "Handle changed"
    delete_requested = "Account deletion scheduled"
    data_export = "Data Export"
    export_my_data = "Export My Data"
    delete_account = "Delete Account"
    delete_confirm_text = "This permanently removes your handle and data from this node. Your key is not affected."
    connected_services = "Connected Services"
    usage = "Usage"
    bridges_description = "Connect to other social networks"
    bridge_management = "Bridge Management"
    bridge_management_subtitle = "Open the Bridges section in the sidebar to link or manage accounts on other networks (Bluesky, ActivityPub, Nostr, Email)"
    data = "Data"
    export_subtitle = "Download a copy of all your data, content included — it may be large"
    export_dialog_title = "Export Account Data"
    session = "Session"
    sign_out_subtitle = "Remove local credentials and return to onboarding. Your secret key is still required to sign back in."
    delete_subtitle = "Permanently delete your account and all data"
    accounts = "Accounts"
    accounts_subtitle = "Switch between the identities on this device, or add another."
    add_account = "Add account"
    open_new_instance = "Open in new window"
    @staticmethod
    def open_new_instance_copied(*, command: str, account: str) -> str:
        return f"Copied: {command} — paste it into a new terminal window to open {account}"
    require_confirm_toggle = "Require confirmation to switch"
    reauth_reason = "switch to this account"
    reauth_prompt_title = "Confirm account switch"
    @staticmethod
    def reauth_prompt_body(*, account: str) -> str:
        return f"This account asks for confirmation before you switch to it. Switch to {account} now?"
    reauth_confirm = "Switch"


class _SettingsPending_actions:
    title = "Pending actions"
    @staticmethod
    def title_count(*, count: str) -> str:
        return f"Pending actions ({count})"
    none_scheduled = "Nothing is scheduled."
    @staticmethod
    def applies(*, time: str) -> str:
        return f"Applies {time}"
    cancel = "Cancel"
    @staticmethod
    def change_handle_to(*, handle: str) -> str:
        return f"Change handle to {handle}"
    delete_account = "Delete this account"
    @staticmethod
    def delete_snapshot(*, snapshot: str) -> str:
        return f"Delete snapshot {snapshot}"
    @staticmethod
    def admin_delete_user(*, target: str) -> str:
        return f"Delete the account {target}"
    @staticmethod
    def admin_add(*, target: str) -> str:
        return f"Grant {target} the admin role"
    @staticmethod
    def admin_remove(*, target: str) -> str:
        return f"Revoke the admin role of {target}"
    @staticmethod
    def admin_change_role(*, target: str) -> str:
        return f"Change the admin role of {target}"


class _SettingsMember_review_page:
    title = "Members To Review"
    intro = "These are the people you have not decided about since you recovered your account. Keep the ones you recognise, and remove anyone you do not from your groups."
    empty = "There is nobody waiting for you to review."


class _SettingsEncryption_page:
    title = "Encryption"
    mls_key_packages = "MLS Key Packages"
    mls_description = "Key material used for end-to-end encrypted group messaging"
    refresh_keys = "Refresh Keys"
    refresh_keys_description = "Generate and upload new key packages to your nest"
    available_key_packages = "Available key packages"
    low_key_warning_title = "Low Key Packages"
    low_key_warning_subtitle = "Generate new key packages to ensure uninterrupted encrypted messaging"
    @staticmethod
    def low_key_warning_body(*, count: str) -> str:
        return f"Only {count} key package(s) remaining. Refresh to generate more and maintain end-to-end encryption availability."
    @staticmethod
    def error_key_count(*, message: str) -> str:
        return f"Could not load key count: {message}"
    @staticmethod
    def error_refresh_keys(*, message: str) -> str:
        return f"Failed to refresh keys: {message}"


class _SettingsGeneral_page:
    appearance_note = "Fauna follows your terminal's own color scheme — there is no separate theme picker here."
    theme = "Theme"
    theme_subtitle = "Choose the application colour scheme"
    theme_follow_system = "Follow System"
    theme_light = "Light"
    theme_dark = "Dark"
    launch_at_login = "Launch at login"
    launch_at_login_subtitle = "Start Fauna automatically when you log in"
    behaviour = "Behaviour"
    no_tray_subtitle = "No system tray detected — closing the window will quit Fauna"
    notification_sound = "Notification sound"
    notification_sound_subtitle = "Play a sound when a new message arrives"
    keyboard_shortcuts = "Keyboard Shortcuts"
    keyboard_shortcuts_description = "Shortcuts available while the application window is focused"
    raise_window = "Raise window from tray"
    raise_window_subtitle = "Click the system tray icon to bring the window back"
    version = "Version"
    @staticmethod
    def update_available(*, version: str) -> str:
        return f"{version} available"
    shortcut_new_message = "New message"
    shortcut_new_group = "New group"
    shortcut_compose_email = "Compose email (SMTP)"
    shortcut_close_window = "Close window / hide to tray"
    shortcut_hide_to_tray = "Hide to system tray"
    shortcut_minimize_to_tray = "Minimize to tray (when enabled)"
    shortcut_quit = "Quit application"
    shortcut_quick_switcher = "Quick switcher"
    shortcut_preferences = "Preferences"
    shortcut_switch_section = "Switch sidebar section"


class _SettingsModeration_page:
    title = "Content Moderation"


class _SettingsPrivacy_page:
    title = "Privacy"
    email_filters = "Email Filters"
    no_filters = "No filters"
    add_filter = "Add Filter"
    edit_filter = "Edit filter"
    delete_filter = "Delete filter"
    new_filter = "New Filter"
    action = "Action"
    forward_address = "Forward to"
    keep_local_copy = "Keep a local copy"
    spam_preferences = "Spam Preferences"
    inbox_mode_description = "Control who can send you messages"
    inbox_mode_subtitle = "Who can message you directly"
    inbox_mode_unknown = "Your current inbox mode has not loaded, so none of the four below is marked. Your setting is unchanged — reopen this page once your nest is reachable to see and change it."
    email_filters_description = "Rules applied to incoming messages"
    no_filters_configured = "No filters configured"
    filter_inherited = "From before your account recovery — check you recognise this rule"
    filter_keep = "I recognise this"
    no_filters_subtitle = "Add a filter to automatically sort or reject messages"
    match_value = "Match value"
    spam_protection = "Spam Protection"
    spam_protection_description = "Threshold scores for automatic filtering"
    spam_threshold_subtitle = "Messages above this score are marked as spam"
    phishing_threshold_subtitle = "Messages above this score are flagged as phishing"
    apply_changes = "Apply changes"


class _SettingsAdmin_page:
    overview = "Overview"
    total = "Total"


class _SettingsSync_page:
    title = "Sync Settings"
    synced_locations = "Synced Locations"
    no_locations_synced = "No locations synced"
    location_path = "Location path"
    add_location = "Add location"
    add_location_subtitle = "Add the typed location path, bound to the named folder"
    select_directory_dialog = "Select Directory to Sync"
    open_directory = "Open directory"


class _SettingsP2p_page:
    title = "P2P"
    tunnel_group_title = "P2P Tunnel"
    tunnel_group_description = "Direct peer-to-peer connections between your devices"
    start = "Start"
    stop = "Stop"
    tunnel_row_title = "Tunnel"
    tunnel_row_subtitle = "Start or stop the P2P tunnel"
    node_id = "Node ID"
    contacts_group_title = "Contacts"
    contacts_group_description = "P2P peers you can reach directly (local to this device)"
    connection_group_title = "Connection"
    connection_group_description = "Network information for P2P connectivity"
    lan_addresses = "LAN Addresses"
    lan_none = "(no active interfaces detected)"


class _SettingsErrors:
    publish_keys = "Failed to publish keys"
    update_inbox = "Failed to update inbox mode"
    change_handle = "Failed to change handle"
    delete_account = "Deletion failed"
    export = "Export failed"
    @staticmethod
    def export_status(*, status: str) -> str:
        return f"Export failed: {status}"
    create_filter = "Failed to create filter"
    update_filter = "Failed to save filter"
    delete_filter = "Failed to delete filter"
    keep_filter = "Failed to record that you recognise this rule"
    save_prefs = "Failed to save preferences"
    enable_notifications = "Failed to enable notifications."
    disable_notifications = "Failed to disable notifications."
    train = "Training failed"
    task_assignment_stale = "That option is no longer available — reopen the page and try again"


class _Settings:
    title = "Settings"
    check_for_updates = "Check for Updates"
    up_to_date = "Up to date"
    check_failed = "Check failed"
    @staticmethod
    def update_available_notice(*, version: str, url: str) -> str:
        return f"Version {version} is available. Get it at {url}"
    mls_available = "MLS encryption: Available"
    mls_not_available = "MLS encryption: Not available"
    configuration = "Configuration"
    privacy = "Privacy"
    moderation = "Moderation"
    data = "Data"
    nest_admin = "Nest Admin"
    sign_out = "Sign Out"
    exit_settings = "Exit settings"
    sign_out_confirm = "Are you sure you want to sign out? You will need your secret key to sign back in."
    @staticmethod
    def sign_out_residue(*, count: str) -> str:
        return f"Signed out, but {count} item(s) of your data could not be removed from this device — another program may still be using them. Press Remove Again to try once more."
    sign_out_residue_credentials = "Signed out, but your sign-in credentials could not be removed from this device — its secure storage may be locked or unavailable. Press Remove Again to try once more."
    @staticmethod
    def sign_out_residue_with_credentials(*, count: str) -> str:
        return f"Signed out, but your sign-in credentials and {count} item(s) of your data could not be removed from this device. Press Remove Again to try once more."
    sign_out_residue_retry = "Remove Again"
    sign_out_residue_retry_blocked_other_window = "Not removed — another Fauna window is using some of this data. Close it, then press Remove Again."
    sign_out_blocked_other_window = "Still signed in — another Fauna window is using this account on this device. Close it, then sign out again."
    remove_account_blocked_other_window = "Not removed — another Fauna window is using that account on this device. Close it, then remove the account again."
    remove_account_blocked_this_window = "Not removed — this window is using that account. Close this window, then remove the account from another one."
    @staticmethod
    def switch_refused_no_secret(*, account: str) -> str:
        return f"Not switched — this device can no longer sign in as {account}: its secret key is missing here. You are still on the identity you were using. To use {account} here again, add it back with its secret key or recovery kit."
    @staticmethod
    def switch_refused(*, account: str) -> str:
        return f"Not switched to {account} — you are still on the identity you were using."
    account_list_full = "Not added — this device's account list is full. Remove an account this device no longer uses, then try again."
    publishing_key_packages = "Publishing key packages..."
    filter_name = "Filter name"
    rule_type = "Rule type"
    rule_value = "Rule value"
    spam_threshold = "Spam threshold"
    nest_url = "Nest URL"
    startup = "Startup"
    autostart_header = "Start Fauna when you sign in"
    close_to_tray_header = "Close to tray"
    close_to_tray_subtitle = "Keep Fauna running in the system tray when the window is closed"
    privacy_desc = "Control who can contact you."
    inbox_mode = "Inbox Mode"
    configure_nest = "Configure Nest"
    configure_nest_desc = "Change the nest URL this app connects to."
    new_nest_url = "New Nest URL"
    new_nest_url_placeholder = "https://nest.fauna.social"
    update_button = "Update"
    updates = "Updates"
    storage_desc = "Account storage usage."
    about = "About"
    about_name = "Fauna for Windows"
    about_desc = "Encrypted messaging, contacts, and file sync."
    danger_zone_desc = "Permanently delete your account and all associated data. This action cannot be undone."
    delete_confirm_placeholder = "Type DELETE to confirm"
    general = "General"
    appearance = "Appearance"
    show_in_dock = "Show in Dock"
    auto_check_updates = "Automatically check for updates"
    auto_download_updates = "Automatically download updates"
    last_checked = "Last checked:"
    p2p_redirect = "Peer connections and bridges are managed on the Devices and Bridges pages."
    open_devices = "Open Devices"
    open_bridges = "Open Bridges"
    identity_export = _SettingsIdentity_export
    recovery_kit = _SettingsRecovery_kit
    icloud_backup = _SettingsIcloud_backup
    push_notifications = _SettingsPush_notifications
    mail = _SettingsMail
    account_page = _SettingsAccount_page
    pending_actions = _SettingsPending_actions
    member_review_page = _SettingsMember_review_page
    encryption_page = _SettingsEncryption_page
    general_page = _SettingsGeneral_page
    moderation_page = _SettingsModeration_page
    privacy_page = _SettingsPrivacy_page
    admin_page = _SettingsAdmin_page
    sync_page = _SettingsSync_page
    p2p_page = _SettingsP2p_page
    errors = _SettingsErrors


class _Nests:
    title = "Nests"
    view_now = "Now"
    view_history = "History"
    not_trusted = "This nest is not trusted to read any of your content."
    escrow_holder_badge = "Holds your recovery escrow"
    @staticmethod
    def custody_nest_label(*, host: str) -> str:
        return f"{host}'s nest — trusted to hold sealed copies"
    trusted_to_read = "Trusted to read:"
    scope_mail = "Mail — and the spam-filter model and training history derived from it"
    scope_calendar = "Calendar"
    scope_posts = "Posts"
    scope_spam_labels = "Write spam labels"
    @staticmethod
    def scope_mail_labeler(*, labeler: str) -> str:
        return f"Mail — only to run community labeler {labeler} over it"
    @staticmethod
    def scope_labeler_labels(*, labeler: str) -> str:
        return f"Write labels for community labeler {labeler}"
    scope_spam_model = "Your spam-filter training, for the shared spam baseline"
    @staticmethod
    def scope_posts_tier(*, tier: str) -> str:
        return f"Posts — {tier}"
    @staticmethod
    def scope_folder(*, folder: str) -> str:
        return f"Your folder \"{folder}\""
    scope_folder_deleted = "A folder you have since deleted"
    lasts_until = "Trusted until:"
    status_active = "Active"
    status_expiring = "Expiring soon"
    status_expired = "Paused — renew to resume"
    status_auto_renewing = "Auto-renewing"
    renew = "Renew"
    revoke = "Revoke"
    grant_unattested_mark = "Given before you recovered this account — still active. Keep it, or revoke it if you don't recognise it."
    grant_keep_button = "Keep"
    bound_note_standing = "On an honest nest, revoking stops future access and re-acquisition. It cannot un-see what was already read, and does not yet block content arriving after revocation."
    bound_note_bounded_mail = "This trust is cryptographically time-boxed: once its window ends (accurate to within about a week), this nest can no longer read new mail at all — not even by re-acquiring a key. Within the window it can only open mail sealed under that period's rotating keys, so content sealed before the schedule caught up may still be unreadable to it."
    @staticmethod
    def history_minted(*, scope: str, when: str) -> str:
        return f"Trusted to read {scope} · {when}"
    @staticmethod
    def history_renewed(*, scope: str, when: str) -> str:
        return f"Trust renewed: {scope} · {when}"
    @staticmethod
    def history_revoked(*, scope: str, when: str) -> str:
        return f"Trust revoked: {scope} · {when}"
    backup_scope_seal = "Backs up your messages for you"
    @staticmethod
    def backup_scope_writer(*, destination: str) -> str:
        return f"Writes your backups to {destination}"
    backup_since = "Trusted since:"
    backup_status_active = "Active"
    backup_status_unreachable = "Could not reach this destination"
    backup_status_missing = "Not set up to accept your backups"
    backup_revoke = "Stop backing up"
    backup_bound_note_seal = "Stopping this means your nest can no longer make new backups of your messages. Backups it already made stay where they are until the box holding them clears them out."
    backup_bound_note_writer = "Stopping this means your nest can no longer send new backups to this destination. Backups already stored there stay until that box clears them out. You can stop it here even if your own nest is misbehaving."
    @staticmethod
    def generation_path(*, path: str) -> str:
        return f"Backup of {path}"
    @staticmethod
    def generation_path_unknown(*, hash: str) -> str:
        return f"Backup {hash}"
    generation_superseded = "Replaced:"
    @staticmethod
    def generation_expires(*, when: str) -> str:
        return f"Can be restored until {when}, and counts against your storage until then"
    generation_status_listed = "Can be restored"
    generation_status_unreachable = "Could not reach this destination"
    generation_restore = "Restore this version"
    generation_restored = "That version has been restored."
    generation_past_window = "That version is past the recovery window, so it can no longer be restored."
    mint_button = "Add trust…"
    mint_scope_placeholder = "Choose what to trust it with…"
    mint_option_mail = "Read and filter my mail — and the spam-filter model and training history derived from it"
    mint_option_calendar = "Read my calendar"
    @staticmethod
    def mint_option_paywalled(*, tier: str) -> str:
        return f"Serve paywalled posts — {tier}"
    mint_holder_placeholder = "Which service on this nest?"
    mint_duration_one_off = "For a few hours"
    mint_duration_standard = "For 90 days"
    mint_confirm = "Trust"
    blessed_toggle = "Keep this box's trust renewed"
    description = "Nests you have linked to sync your account's content."
    empty = "No linked nests yet."
    add_button = "Link a nest"
    authorize_subtitle = "Authorize one of your nests to sync this account"
    add_input_placeholder = "Nest address (https://…) — links both ends — or a 64-hex identity"
    nest_to_link = "Nest to link"
    add_submit = "Link"
    add_cancel = "Cancel"
    list_title = "Your linked nests"
    unlink = "Unlink"
    capabilities_label = "Syncs:"
    capability_account_replica = "sealed copy of your account settings"
    expiry_label = "Expires:"
    expiry_never = "Never expires"
    link_recovery_keys_differ = "These two nests hold different recovery keys for this account, so they can't be linked."
    @staticmethod
    def forward_queue_summary(*, count: str) -> str:
        return f"{count} of your posts are waiting to reach your relay."
    @staticmethod
    def forward_queue_stuck(*, count: str) -> str:
        return f"{count} of them have been refused for more than eight hours. Check that this nest is allowed to forward your posts on the relay — linking the relay again from here grants that — or stop forwarding them below."
    @staticmethod
    def forward_queue_last_error(*, error: str) -> str:
        return f"Last attempt failed: {error}"
    forward_retry = "Retry now"
    forward_discard = "Stop forwarding these"


class _Linked_nests:
    title = "Linked nests"
    unlink = "Unlink"


class _Logs:
    title = "Logs"
    description = "Recent activity recorded on this device, newest first. No message contents or secrets are ever logged — only what happened, when, and where."
    filter_label = "Severity"
    filter_all = "All"
    level_error = "Error"
    level_warn = "Warn"
    level_info = "Info"
    level_debug = "Debug"
    level_trace = "Trace"
    copy_button = "Copy"
    clear_button = "Clear"
    empty = "No log entries yet."


class _Mail_settings:
    title = "Mail & Calendar"
    keys_info = "Your mail is protected by an encryption key held on your Fauna devices. Each app password or token you add unlocks that same key for one email app (Thunderbird, Apple Mail, …). Rotate your keys if a password or token may have leaked, or if a device that had your mail set up was lost or stolen — rotating replaces the key so the exposed credential can no longer read your mail (messages you've already received stay readable). If you're just retiring an app you no longer use, revoke that one credential instead — you don't need to rotate."
    credentials_on_connected_apps = "Your app passwords are listed under Settings → Connected apps — copy, reveal or disconnect each one there."
    serve_here_label = "Serve my mail & calendar over IMAP/CalDAV on this nest"
    serve_here_subtitle = "When on, this nest answers IMAP and CalDAV for your mailbox so email and calendar apps can connect here. Turn it off if you read your mail on a different nest — your own Fauna apps are unaffected either way."
    forwarding_title = "Forwarding"
    forward_all_to_label = "Forward all incoming mail to"
    forward_all_to_subtitle = "Every message you receive is also sent on to this address, and you keep your own copy. Clear the field to stop forwarding."
    forward_per_hour_label = "Hourly forwarding limit"
    @staticmethod
    def forward_per_hour_subtitle(*, ceiling: str) -> str:
        return f"The most messages forwarded for you in one hour, up to {ceiling}. Past the limit, forwards wait for the next hour."
    forward_all_to_invalid = "That doesn't look like an email address."
    forward_per_hour_not_a_number = "Enter the hourly forwarding limit as a whole number."
    forward_per_hour_zero = "The hourly forwarding limit must be at least 1."
    @staticmethod
    def forward_per_hour_above_ceiling(*, ceiling: str) -> str:
        return f"The hourly forwarding limit can't be more than {ceiling}."


class _Web_settings:
    title = "Web"
    subdomain_toggle_label = "Publish my website"
    subdomain_toggle_subtitle = "When on, this nest serves your Web files and web-published posts at your personal address. Off by default."
    subdomain_url_label = "Your site"
    subdomain_no_handle = "Set a handle first to get a personal web address."
    subdomain_reserved = "Your handle is a reserved name and can't host a website."
    subdomain_no_serving_domain = "This nest has no web address yet, so it can't serve websites. An admin needs to give it a domain first."
    render_status_down = "Your published pages are temporarily unavailable. This nest is restoring them by itself — there is nothing you need to do. Synced files are not affected."
    content_info = "Add content by turning on a folder's website toggle under Settings → Folders, or by publishing individual posts to the web."
    published_posts_title = "Published posts"
    published_posts_empty = "No published posts yet"
    @staticmethod
    def published_post_gated_badge(*, tier: str) -> str:
        return f"Paid: {tier}"
    link_disabled_subdomain_off = "Turn on \"Publish my website\" above to get links you can share."
    link_disabled_no_handle = "Set a handle first — your published posts need a web address before they can be linked to."
    link_disabled_reserved = "Your handle is a reserved name, so your published posts have no public address to link to."


class _Web_publish:
    publish_to_web = "Publish to web"
    unpublish = "Unpublish"
    copy_web_link = "Copy web link"
    copy_paywall_link = "Copy paywall link"
    paywall_link_note = "A paywall link opens the full post for anyone, but only for about 10 minutes — it's for a quick preview, not for giving lasting free access. For that, send a claim code instead."
    @staticmethod
    def copied_link(*, url: str) -> str:
        return f"Copied: {url}"
    @staticmethod
    def copied_paywall_link(*, url: str) -> str:
        return f"Copied, works for about 10 minutes: {url}"
    menu_no_link_reason = "These links need a web address. Turn on your website in Settings → Web."
    @staticmethod
    def error_publish(*, message: str) -> str:
        return f"Failed to publish post: {message}"
    @staticmethod
    def error_unpublish(*, message: str) -> str:
        return f"Failed to unpublish post: {message}"
    @staticmethod
    def error_paywall_link(*, message: str) -> str:
        return f"Failed to create paywall link: {message}"


class _Mail_aliases:
    title = "Aliases"
    description = "Extra mail addresses that all deliver to you — share a different one with each service so you can see who leaked your address and turn any of them off."
    add_button = "Add alias"
    generate_button = "Generate disposable"
    empty = "No aliases yet"
    loading = "Loading your aliases…"
    form_title = "Add alias"
    kind_wildcard_label = "Wildcard prefix (matches anything starting with it)"
    pattern_placeholder = "Address (e.g. shop, news-)"
    label_placeholder = "Label (optional)"
    spam_threshold_placeholder = "Spam threshold override (0–15, optional)"
    rate_per_hour_placeholder = "Rate limit per hour (optional)"
    ttl_placeholder = "Disposable lifetime in days (optional)"
    uses_placeholder = "Disposable max uses (optional)"
    submit = "Add"
    cancel = "Cancel"
    revoke = "Revoke"
    delete = "Delete"
    edit = "Edit"
    show_audit = "Show audit"
    disabled_badge = "disabled"
    active_toggle_label = "Active"
    active_toggle_tooltip = "When on, this address receives mail. Turn off to bounce mail to it without deleting the address — you can turn it back on anytime."
    primary_address_badge = "Primary address"
    primary_address_tooltip = "Your main address and sign-in identity. It can't be disabled, renamed, or deleted so your mail and login always work."
    copied = "Copied address"
    kind_exact = "Exact"
    kind_subaddress = "+suffix"
    kind_wildcard = "Wildcard"
    kind_disposable = "Disposable"
    kind_catchall = "Catch-all"
    kind_forwarder = "Forwarder"
    kind_other = "Alias"
    no_default_domain = "Enable mail before adding aliases."
    @staticmethod
    def hits(*, count: str) -> str:
        return f"{count} hits"
    @staticmethod
    def hits_with_last(*, count: str, date: str) -> str:
        return f"{count} hits · last {date}"
    import_button = "Import addresses"
    import_title = "Import addresses"
    import_subtitle = "Paste one address per line. Each becomes an exact alias that delivers to you; addresses you already have are skipped."
    import_placeholder = "One address per line"
    import_submit = "Import"
    import_cancel = "Cancel"
    @staticmethod
    def import_result(*, created: str, existed: str, invalid: str) -> str:
        return f"{created} created · {existed} already existed · {invalid} invalid"
    @staticmethod
    def import_invalid_line(*, address: str, reason: str) -> str:
        return f"{address} — {reason}"


class _Mail_spam:
    title = "Spam"
    description = "Your spam filter learns from what you mark as spam or not-spam. Reset its training, choose whether to help the deployment's shared filter, and undo any past training here."
    reset_button = "Reset spam classifier"
    reset_confirm = "Reset for good? This cannot be undone."
    reset_subtitle = "Deletes your spam-training model and history. Future mail starts from scratch. This cannot be undone."
    contribute_baseline_label = "Contribute to deployment spam baseline"
    contribute_baseline_subtitle = "Off by default. When on, your training helps seed the shared filter new accounts start from — your individual messages are never shared."
    share_reports_label = "Share spam reports (anonymized)"
    share_reports_subtitle = "Off by default. When on, the fact that you flagged a message as spam joins an anonymized count your nest shares — but only once at least 3 people here have flagged the same content, and never your identity or the message itself."
    threshold_override_label = "Spam-folder threshold override (0–15, optional)"
    threshold_override_subtitle = "Messages scoring at or above this override are filed to Junk, instead of the deployment default. 0 turns automatic filing off for this account; leave blank to follow the default."
    published_title = "What this nest publishes"
    published_description = "The anonymized report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 reporters."
    published_empty = "This nest publishes no report aggregates yet"
    published_reporters = "reporters"
    history_title = "Training history"
    empty = "No training history yet"
    undo = "Undo"
    label_spam = "Spam"
    label_ham = "Not spam"
    label_unknown = "Other"
    source_explicit_button = "Fauna app"
    source_imap_junk_flag = "Junk flag"
    source_imap_junk_move = "Junk move"
    source_unknown = "Other"
    backend_unbuilt = "Spam-classifier training is not available on this nest yet."


class _Mail_export:
    title = "Export mailbox"
    description = "Download your whole mailbox in a standard format you can import into another mail app. The export is encrypted until you download it."
    format_title = "Step 1 — Format"
    format_mbox = "mbox (one file per mailbox — broadest support)"
    format_maildir = "Maildir++ (one file per message; preserves flags)"
    format_eml = "EML zip (one .eml per message + manifest)"
    scope_title = "Step 2 — What to include"
    scope_mailboxes_label = "Mailboxes"
    scope_mailboxes_empty = "No mailboxes to export."
    scope_date_from_placeholder = "From date (optional, YYYY-MM-DD)"
    scope_date_to_placeholder = "To date (optional, YYYY-MM-DD)"
    scope_strip_headers_label = "Strip transit headers"
    scope_strip_headers_subtitle = "Removes the headers mail servers add in transit — relay hops, server names and IP addresses. Off keeps full forensic fidelity."
    confirm_title = "Step 3 — Confirm"
    confirm_pending = "Estimate unavailable until the export backend is ready."
    start_button = "Start export"
    progress_title = "Step 4 — Exporting"
    pause_button = "Pause"
    resume_button = "Resume"
    cancel_button = "Cancel"
    error_log_title = "Skipped / errored messages"
    done_title = "Step 5 — Done"
    download_button = "Download export"
    download_url_label = "Download link (for another device)"
    discard_button = "Discard now"
    next = "Next"
    back = "Back"
    backend_unbuilt = "Mailbox export is not available on this nest yet."
    @staticmethod
    def confirm_summary_fmt(*, format: str, mailboxes: str) -> str:
        return f"{format} · {mailboxes} mailbox(es)"
    @staticmethod
    def progress_summary_fmt(*, exported: str, total: str, skipped: str, errored: str) -> str:
        return f"{exported} of {total} · {skipped} skipped · {errored} errored"
    @staticmethod
    def done_summary_fmt(*, format: str, bytes: str) -> str:
        return f"{format} · {bytes} bytes"
    @staticmethod
    def saved_summary_fmt(*, format: str, bytes: str, path: str) -> str:
        return f"{format} · {bytes} bytes · saved to {path}"


class _Mail_lists:
    title = "Lists"
    description = "Run a newsletter or mailing list from your own address, with one-click unsubscribe built in."
    add_button = "Add list"
    empty = "No lists yet"
    loading = "Loading your lists…"
    form_title = "Add list"
    name_placeholder = "List name (e.g. Bob's Weekly)"
    local_part_placeholder = "Address (e.g. newsletter)"
    domain_label = "Domain"
    description_placeholder = "Description (optional)"
    list_help_placeholder = "List-Help URL (optional)"
    list_archive_placeholder = "List-Archive URL (optional)"
    per_send_placeholder = "Recipients per send (optional)"
    submit = "Add"
    cancel = "Cancel"
    edit = "Edit"
    delete = "Delete"
    delete_confirm = "Delete the list and all its members?"
    members = "Members"
    no_domain = "Add a mail domain before creating lists."
    members_title = "Members"
    members_no_list = "Open a list from the Lists page to manage its members."
    members_loading = "Loading members…"
    @staticmethod
    def summary_fmt(*, subscribed: str, unsubscribed: str) -> str:
        return f"{subscribed} subscribed · {unsubscribed} unsubscribed"
    add_member_button = "Add member"
    add_member_placeholder = "Email address"
    add_member_submit = "Add"
    add_member_cancel = "Cancel"
    import_button = "Import"
    import_placeholder = "One email address per line"
    import_submit = "Import"
    import_cancel = "Cancel"
    @staticmethod
    def import_result(*, added: str, existed: str, invalid: str) -> str:
        return f"{added} added · {existed} already subscribed · {invalid} invalid"
    unsubscribe = "Unsubscribe"
    resubscribe = "Resubscribe"
    status_subscribed = "Subscribed"
    status_unsubscribed = "Unsubscribed"
    backend_unbuilt = "Mailing lists are not available on this nest yet."


class _Mail_import:
    title = "Import mailbox"
    description = "Pull your existing mail from Gmail, Outlook, iCloud, or any IMAP server into your Fauna mailbox. Your credentials never leave this device."
    source_title = "Step 1 — Source"
    source_unavailable = "Importing from another mail server isn't available in the browser yet. You can start an import from the desktop or terminal app, and watch or pause it here."
    source_gmail = "Gmail"
    source_outlook = "Outlook / Hotmail / Office365"
    source_icloud = "iCloud"
    source_generic = "Generic IMAP"
    source_app_password_label = "App password"
    source_app_password_help_gmail = "Requires 2FA on your Google account. Generate one at myaccount.google.com/apppasswords and paste it here."
    source_app_password_help_icloud = "Requires 2FA on your Apple ID. Generate one at appleid.apple.com and paste it here."
    source_oauth_button = "Connect with Microsoft"
    source_host_placeholder = "Server hostname"
    source_port_placeholder = "Port (default 993)"
    tls_implicit = "Implicit TLS (993)"
    tls_starttls = "STARTTLS (143)"
    source_username_placeholder = "Username"
    source_password_placeholder = "Password"
    connect_button = "Connect"
    scope_title = "Step 2 — What to import"
    scope_mailboxes_label = "Mailboxes"
    scope_mailboxes_empty = "No mailboxes found on the source server."
    scope_date_from_placeholder = "From date (optional, YYYY-MM-DD)"
    scope_max_size_label = "Max message size (MB)"
    scope_mailbox_mapping_label = "Mailboxes map 1:1 by name — INBOX to INBOX, Sent to Sent, and so on. Mailboxes with no matching Fauna mailbox are created, named after the source."
    confirm_title = "Step 3 — Confirm"
    @staticmethod
    def confirm_summary_fmt(*, source: str, mailboxes: str, messages: str) -> str:
        return f"{source} · {mailboxes} mailbox(es) · {messages} messages"
    start_button = "Start import"
    progress_title = "Step 4 — Importing"
    @staticmethod
    def progress_summary_fmt(*, imported: str, total: str, skipped: str, errored: str) -> str:
        return f"{imported} of {total} · {skipped} skipped · {errored} errored"
    @staticmethod
    def progress_row_fmt(*, count: str) -> str:
        return f"{count} messages"
    pause_button = "Pause"
    resume_button = "Resume"
    cancel_button = "Cancel"
    error_log_title = "Skipped / errored messages"
    done_title = "Step 5 — Done"
    @staticmethod
    def done_summary_fmt(*, imported: str, skipped: str, errored: str) -> str:
        return f"{imported} imported · {skipped} skipped · {errored} errored"
    view_imported_button = "View imported messages"
    review_skipped_button = "Review skipped"
    next = "Next"
    back = "Back"


class _Archive_import:
    title = "Import from other services"
    description = "Bring your posts, photos and events from a Facebook or Instagram export archive into Fauna, at their original dates and audiences. The archive itself stays in a sealed folder on your nest."
    unavailable = "Importing needs a session that holds your identity key; this one does not."
    source_title = "Step 1 — Where the archive comes from"
    source_help_facebook = "On Facebook, open Settings & privacy → Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON, any media quality. The download link expires after a few days, so save the zip as soon as it is ready."
    source_help_instagram = "On Instagram, open Settings → Accounts Center → Your information and permissions → Download your information. Choose \"Download to device\", format JSON. The download link expires after a few days, so save the zip as soon as it is ready."
    archive_title = "Step 2 — The archive"
    archive_path_placeholder = "Path to the export .zip"
    archive_open_button = "Open archive"
    @staticmethod
    def archive_summary_fmt(*, platform: str, owner: str, first: str, last: str, archive_size: str, media_size: str) -> str:
        return f"{platform} · {owner} · {first} – {last} · {archive_size} archive, {media_size} of photos and videos"
    @staticmethod
    def archive_summary_undated(*, platform: str, owner: str, archive_size: str, media_size: str) -> str:
        return f"{platform} · {owner} · no dated records · {archive_size} archive, {media_size} of photos and videos"
    scope_title = "Step 3 — What to import"
    scope_categories_label = "Categories"
    @staticmethod
    def scope_category_row_fmt(*, category: str, count: str) -> str:
        return f"{category} ({count})"
    @staticmethod
    def scope_category_kept_fmt(*, category: str, count: str) -> str:
        return f"{category} ({count}) — kept in the archive for later"
    scope_audience_mode_label = "Audience"
    audience_original = "Keep original audiences"
    audience_only_me = "Only me"
    @staticmethod
    def scope_audience_summary_fmt(*, known: str, unknown: str) -> str:
        return f"{known} posts and albums have a recorded audience and import to it; {unknown} have none recorded and will be visible only to you."
    scope_audience_summary_only_me = "Everything will be visible only to you."
    scope_hidden_tiers_unavailable = "This nest predates hidden tiers, so only public posts can be imported now. Update the nest, then import again to pick up the rest."
    scope_date_from_placeholder = "From date (optional, YYYY-MM-DD)"
    scope_date_to_placeholder = "To date (optional, YYYY-MM-DD)"
    confirm_title = "Step 4 — Confirm"
    @staticmethod
    def confirm_summary_fmt(*, records: str, bytes: str) -> str:
        return f"{records} records · about {bytes} to upload"
    start_button = "Start import"
    progress_title = "Step 5 — Importing"
    @staticmethod
    def progress_summary_fmt(*, state: str, imported: str, total: str, skipped: str) -> str:
        return f"{state} · {imported} of {total} · {skipped} skipped"
    @staticmethod
    def progress_row_fmt(*, imported: str, count: str, skipped: str) -> str:
        return f"{imported} of {count} · {skipped} skipped"
    state_running = "Importing"
    state_paused = "Paused"
    state_cancelled = "Cancelled"
    state_completed = "Done"
    state_errored = "Stopped after an error"
    pause_button = "Pause"
    resume_button = "Resume"
    cancel_button = "Cancel"
    error_log_title = "Skipped records"
    done_title = "Step 6 — Done"
    @staticmethod
    def done_summary_fmt(*, imported: str, skipped: str) -> str:
        return f"{imported} imported · {skipped} skipped"
    view_imported_button = "View imported posts"
    review_skipped_button = "Review skipped"
    profile_prefill_button = "Use the archive's profile name and bio"
    @staticmethod
    def folder_link_fmt(*, folder: str) -> str:
        return f"Archive folder: {folder}"
    category_posts = "Posts"
    category_albums = "Albums"
    category_comments = "Comments"
    category_reactions = "Reactions"
    category_events = "Events"
    category_groups = "Groups"
    category_friends = "Friends"
    category_threads = "Message threads"
    category_messages = "Messages"
    category_profile = "Profile"
    next = "Next"
    back = "Back"


class _Search_page:
    title = "Search Results"
    sign_in_prompt = "Sign in to search."
    placeholder = "Search posts, profiles, email..."
    hide_search_bar = "Hide search bar"
    show_search_bar = "Show search bar"
    no_results = "No results for"
    no_results_short = "No results"
    all = "All"
    clear = "Clear"
    search_failed = "Search failed"
    @staticmethod
    def search_failed_reason(*, reason: str) -> str:
        return f"Search failed: {reason}"
    load_more_failed = "Load more failed"
    search_messages = "Search messages..."
    badge_post = "Post"
    badge_profile = "Profile"
    badge_email = "Email"
    badge_event = "Event"
    badge_message = "Message"
    badge_contact = "Contact"
    badge_file = "File"
    badge_draft = "Draft"
    badge_media = "Media"


class _NostrLink_account:
    title = "Link Nostr Account"
    description = "Connect a Nostr identity to your Fauna account."
    mode_label = "Link mode"
    generate = "Generate new keypair"
    import_nsec = "Import nsec"
    nip07 = "NIP-07 browser extension"
    nsec_label = "nsec key"
    nsec_placeholder = "nsec1..."
    nip07_prompt = "Your browser extension will be prompted for the public key."
    link_button = "Link Account"
    enter_nsec = "Please enter an nsec key"
    no_nip07 = "No NIP-07 browser extension detected"


class _NostrAccount:
    title = "Nostr Account"
    description = "Link a Nostr identity to your Fauna account"
    public_key = "Public Key"
    signing_mode = "Signing Mode"
    mode_generated = "Generated keypair"
    mode_imported = "Imported nsec"
    mode_remote = "NIP-46 bunker"
    mode_nip07 = "NIP-07 extension"
    mode_proxied = "Proxied (paired nest signs)"
    generate_key_button = "Generate Key"
    link_subtitle = "Generate a new Nostr keypair on the nest"
    unlink = "Unlink Account"
    unlink_button = "Unlink"
    unlink_subtitle = "Remove Nostr identity from your nest account"
    status_no_client = "No client"
    status_unavailable = "Nostr unavailable on this nest"


class _NostrNpub_confirm:
    @staticmethod
    def banner(*, npub: str) -> str:
        return f"A recent account recovery changed your Nostr key. Please confirm the public key shown above ({npub}) is yours."
    yes_button = "Yes, that's my npub"
    no_button = "No / nothing is linked"


class _NostrSettings:
    title = "Content Settings"
    description = "Configure Nostr publishing behavior"
    auto_publish = "Auto-publish posts"
    auto_publish_subtitle = "Automatically publish Fauna posts to Nostr relays"
    publish_replies = "Publish replies"
    publish_reactions = "Publish reactions"
    inbound_title = "Inbound to feed"
    inbound_subtitle = "Show events from followed Nostr users in your feed"
    expose_title = "Expose content"
    expose_subtitle = "Allow Nostr users to see your Fauna content"


class _NostrRelays:
    title = "Relays"
    none = "No relays configured. Default relays will be used."
    add = "Add Relay"
    placeholder = "wss://relay.example.com"
    invalid_url = "Relay URL must start with wss:// or ws://"
    private_address = "Relays on a private network or on this device can't be used. Use a public relay address."


class _NostrFollows:
    title = "Follows"
    add = "Add"
    none = "No Nostr follows yet."
    pubkey_placeholder = "npub1... or hex pubkey"
    petname_placeholder = "Petname"


class _NostrConnected_apps:
    title = "Connected apps"
    description = "Sign in to Nostr apps with your nest using Nostr Connect. Your key stays on the nest — apps only ask it to sign."
    connect_button = "Connect an app"
    reveal_title = "Scan or paste this in your Nostr app"
    reveal_hint = "Shown once — copy it now."
    qr_alt = "Nostr Connect QR code"
    none = "No connected apps yet."
    unnamed = "Unnamed app"
    pending = "Waiting to connect…"
    @staticmethod
    def last_used(*, time: str) -> str:
        return f"Last used {time}"
    never_used = "Never used"
    @staticmethod
    def expires(*, time: str) -> str:
        return f"Expires {time}"
    disconnect = "Disconnect"


class _NostrZap_signers:
    title = "Zap signers"
    description = "Zaps are Lightning tips. A zap receipt is signed by the wallet provider that received the payment — not by the sender — so your nest only believes receipts from signers you name here."
    pubkey_placeholder = "64-character hex signer pubkey"
    label_placeholder = "Label (optional)"
    add = "Designate signer"
    remove = "Stop trusting"
    none = "You have not designated any signer, so no zap is counted as paid. Add your wallet provider's signer key to start believing its receipts."
    unnamed = "Unnamed signer"
    invalid_pubkey = "A signer pubkey must be 64 hexadecimal characters."


class _Nostr:
    title = "Nostr"
    unavailable = "Nostr isn't available on this nest — it was built without Nostr support."
    link_account = _NostrLink_account
    account = _NostrAccount
    npub_confirm = _NostrNpub_confirm
    settings = _NostrSettings
    relays = _NostrRelays
    follows = _NostrFollows
    connected_apps = _NostrConnected_apps
    zap_signers = _NostrZap_signers


class _StatusIdentity:
    not_configured = "Not configured"


class _StatusConnection:
    service = "Service"


class _StatusSync_agent:
    running = "Running"
    restart_pending = "Restart pending"
    not_running = "Not running"
    keys_pending = "Keys pending"
    not_enrolled = "Not enrolled"


class _StatusSync:
    syncing = "Syncing"
    stopped = "Stopped"
    @staticmethod
    def menu_status(*, status: str) -> str:
        return f"Sync: {status}"
    files_synced = "Files Synced"
    files_pending = "Files Pending"
    bytes_pending = "Bytes Pending"
    last_sync = "Last Sync"
    @staticmethod
    def pending_summary(*, files: str, bytes: str) -> str:
        return f"{files} files, {bytes}"


class _StatusQuota:
    title = "Quota"
    inbox_usage = "Inbox Usage"
    storage_usage = "Storage Usage"


class _StatusP2p:
    title = "P2P"
    tunnel = "Tunnel"
    @staticmethod
    def tunnel_active_with_address(*, address: str) -> str:
        return f"Active ({address})"


class _StatusNode:
    title = "Node"


class _StatusActions:
    clear_cache = "Clear Cache"


class _StatusInbox_privacy:
    title = "Inbox Privacy"
    description = "Control who can send you messages."
    open = "Open"
    open_desc = "Anyone can message you directly."
    allow_knock = "Allow Knocks"
    allow_knock_desc = "New contacts must send a knock request first."
    contacts_only = "Contacts Only"
    contacts_only_desc = "Only confirmed contacts can message you."
    closed = "Closed"
    closed_desc = "No new messages accepted."


class _StatusEncryption:
    key_packages = "Key Packages"
    @staticmethod
    def available(*, count: str) -> str:
        return f"{count} available"
    low_keys = "Low key packages. Publishing more..."
    checking = "Checking encryption status..."
    publishing = "Publishing..."
    mls_engine = "MLS Engine"
    dm_channels = "DM Channels"


class _StatusNotifications:
    enabled = "Push notifications are enabled."
    description = "Get notified when messages arrive, even when Fauna is closed."
    enable = "Enable Notifications"
    disable = "Disable Notifications"
    enabling = "Enabling..."
    disabling = "Disabling..."
    permission_denied = "Notification permission was denied."
    title = "Push Notifications"
    content_encrypted = "Notification content is encrypted end-to-end."
    registered = "Registered with server"
    denied_title = "Notifications Disabled"
    denied_hint = "Enable notifications in system Settings to receive alerts."
    open_settings = "Open Settings"
    unavailable = "Push notifications are not available in this app build."


class _StatusData_export:
    description = "Download all your data, content included, as a zip archive. It may be large."
    @staticmethod
    def exported_to(*, path: str) -> str:
        return f"Data exported to {path}"


class _StatusEmail_filters:
    none = "No email filters configured."
    sender_is = "Sender is"
    sender_domain = "Sender domain"
    subject_contains = "Subject contains"
    body_contains = "Body contains"
    header_exists = "Header exists"
    action_allow = "Allow"
    action_discard = "Discard"
    action_reject = "Reject"
    action_file_into = "File"
    action_forward = "Forward"
    action_auto_reply = "Auto-reply"
    action_add_label = "Label"


class _StatusSpam:
    title = "Spam Filtering"
    description = "Adjust how aggressively spam is filtered from your inbox and feeds."
    spam_threshold = "Spam threshold"
    phishing_threshold = "Phishing threshold"
    aggressive = "Aggressive"
    moderate = "Moderate"
    permissive = "Permissive"
    save = "Save Spam Preferences"


class _StatusChange_handle:
    placeholder = "new-handle"
    changing = "Changing..."


class _StatusDanger_zone:
    delete_hint = "Permanently removes your handle and data from this node. Your key is not affected."


class _StatusAdmin_section:
    title = "Nest Administration"
    description = "You are an admin of this nest."
    dashboard = "Admin Dashboard"


class _StatusSelf_host:
    title = "Run your own nest"


class _StatusBuild:
    title = "Build"
    commit = "Commit"
    verify_hint = "Verify this build: clone the repo at this commit, build locally, and compare file hashes."


class _Status:
    identity = _StatusIdentity
    connection = _StatusConnection
    sync_agent = _StatusSync_agent
    sync = _StatusSync
    quota = _StatusQuota
    p2p = _StatusP2p
    node = _StatusNode
    actions = _StatusActions
    inbox_privacy = _StatusInbox_privacy
    encryption = _StatusEncryption
    notifications = _StatusNotifications
    data_export = _StatusData_export
    email_filters = _StatusEmail_filters
    spam = _StatusSpam
    change_handle = _StatusChange_handle
    danger_zone = _StatusDanger_zone
    admin_section = _StatusAdmin_section
    self_host = _StatusSelf_host
    build = _StatusBuild


class _AdminDashboard:
    title = "Dashboard"
    loading = "Loading stats..."
    @staticmethod
    def load_error(*, message: str) -> str:
        return f"Failed to load admin stats: {message}"
    nest_domain = "Nest Domain"
    version = "Version"
    mail = "Mail"
    total_storage = "Total Storage"
    email = "Email"
    tls = "TLS"
    registration = "Registration"
    paired_nests = "Paired Nests"
    loading_dashboard = "Loading dashboard..."
    not_admin = "Not an Admin"
    not_admin_desc = "Enter a valid admin token to access the dashboard."
    admin_token = "Admin token"
    token_prompt = "Enter your admin Bearer token to view nest statistics."
    nest_dashboard = "Nest Dashboard"
    suspended = "Suspended"
    connections = "Connections"
    no_users = "No users"


class _AdminUsers_page:
    title = "Users"
    loading = "Loading users..."
    @staticmethod
    def total(*, count: str) -> str:
        return f"{count} users total"
    no_handle = "no handle"
    evict = "Evict"
    suspend = "Suspend"
    cancel_eviction = "Cancel Eviction"
    @staticmethod
    def evict_confirm(*, id: str) -> str:
        return f"Start eviction for {id}? The user will be warned and given time to export data."
    evict_default_reason = "Evicted by admin"
    suspend_default_reason = "Suspended by admin"
    make_admin = "Make Admin"
    remove_admin = "Remove Admin"
    prev_page = "Previous"
    next_page = "Next"
    @staticmethod
    def page_indicator(*, current: str, pages: str) -> str:
        return f"Page {current} of {pages}"
    section_requests = "Pending requests"
    section_registration = "Registration"
    section_admit = "Admit someone directly"
    section_invite = "Invite"
    section_users = "Users"
    section_pending = "Pending admin actions"
    @staticmethod
    def pending_count(*, count: str) -> str:
        return f"Pending admin actions ({count})"
    pending_none = "Nothing is pending."
    @staticmethod
    def pending_by(*, who: str) -> str:
        return f"by {who}"
    @staticmethod
    def pending_approvals(*, given: str, needed: str) -> str:
        return f"{given} of {needed} approvals"
    pending_approve = "Approve"
    admit_actor_label = "Their account key (64 characters)"
    admit_handle_label = "Their handle (blank admits without one — they cannot send email until they have a handle)"
    admit_button = "Admit"
    admit_actor_hint = "The account key must be exactly 64 hex characters."
    registration_mode_label = "Who may create an account"
    registration_mode_open = "Anyone"
    registration_mode_invite_required = "Only people with an invite code"
    registration_mode_closed = "Nobody — only I can admit people"
    max_free_users_label = "Limit free accounts"
    max_free_users_hint = "Blank for no limit. Counts every free account, including yours."
    registration_save = "Save"
    @staticmethod
    def registration_mode_unknown(*, mode: str) -> str:
        return f"This nest uses a registration setting this app version does not recognize ({mode}). Update the app to change it."
    copy_code = "Copy code"
    @staticmethod
    def minted_code(*, code: str) -> str:
        return f"New invite code: {code}"
    serving_here = "Serving here"
    serving_disabled = "Not serving"
    no_pending_requests = "No pending requests."
    no_users = "No users."
    code_minted = "Code minted —"
    @staticmethod
    def user_count(*, count: str) -> str:
        return f"{count} users"
    guardian_label = "Guardian"
    guardian_none = "None"
    age_verification_required_label = "Accept only signups carrying app age verification"
    cancel = "Cancel"
    delete = "Delete"


class _AdminAliases_page:
    title = "External Forwarders"
    loading = "Loading aliases..."
    local_part = "Local part"
    target_address = "Target address"
    domain_optional = "Domain (optional)"
    create_button = "Create Alias"
    @staticmethod
    def count(*, count: str) -> str:
        return f"{count} aliases"
    no_aliases = "No aliases configured yet."
    target_col = "Target"
    created_col = "Created"
    forward = "Forward"
    forwarders_title = "External Forwarders"
    forwarders_desc = "Map an address on one of your domains to an external destination. Forwarded addresses have no local mailbox."
    forwarder_domain = "Domain"
    forwarder_local_part = "Local part"
    forwarder_target = "Forwards to"
    forwarder_target_placeholder = "name@example.com"
    forwarder_local_part_placeholder = "info"
    create_forwarder = "Add Forwarder"
    delete_forwarder = "Delete"
    no_forwarders = "No external forwarders configured yet."
    @staticmethod
    def forwarder_row(*, address: str, target: str) -> str:
        return f"{address} → {target}"


class _AdminSettings_page:
    title = "Tiers"
    @staticmethod
    def load_tiers_error(*, message: str) -> str:
        return f"Failed to load tiers: {message}"
    @staticmethod
    def save_tier_error(*, message: str) -> str:
        return f"Failed to save tier: {message}"
    save_tier_error_invalid_cap = "Can't save this tier — every limit must be a whole number."
    save_membership_tier_error_no_tier = "Can't save this row — pick an \"Admits at\" tier first."
    invite_codes = "Invite Codes"
    loading_codes = "Loading invite codes..."
    no_codes = "No invite codes."
    @staticmethod
    def uses_left(*, remaining: str, total: str) -> str:
        return f"{remaining}/{total} uses left"
    @staticmethod
    def uses_left_n(*, count: str) -> str:
        return f"{count} uses left"
    @staticmethod
    def created(*, date: str) -> str:
        return f"created {date}"
    create_code = "Create Invite Code"
    max_uses = "Max Uses"
    tiers = "Tiers"
    loading_tiers = "Loading tiers..."
    no_tiers = "No tier definitions found."
    inbox_limit = "Inbox Limit"
    storage_limit = "Storage Limit"
    @staticmethod
    def tier_caps(*, inbox: str, storage: str, devices: str) -> str:
        return f"Inbox {inbox} · Storage {storage} · {devices} devices"
    cap_inbox_bytes = "Inbox (bytes)"
    cap_storage_bytes = "Storage (bytes)"
    cap_devices = "Devices"
    cap_blob_size = "Blob size (bytes)"
    cap_feeds = "Feeds"
    save_tier = "Save"
    add_tier_section = "Define a new tier"
    add_tier_name = "Tier name"
    add_tier = "Add tier"
    @staticmethod
    def add_tier_error(*, message: str) -> str:
        return f"Failed to add tier: {message}"
    add_tier_error_empty_name = "Can't add this tier — give it a name."
    add_tier_error_invalid_cap = "Can't add this tier — every limit must be a whole number."
    membership_section = "Membership"
    loading_membership = "Loading membership designations..."
    no_membership_tiers = "You have no subscription tiers yet — create one in your Tiers tab, then designate it here for paid nest access."
    membership_admits_at = "Admits at"
    membership_lapses_to = "Lapses to"
    membership_save = "Save"
    membership_clear = "Clear"
    @staticmethod
    def load_membership_tiers_error(*, message: str) -> str:
        return f"Failed to load membership designations: {message}"
    @staticmethod
    def save_membership_tier_error(*, message: str) -> str:
        return f"Failed to save membership designation: {message}"
    @staticmethod
    def clear_membership_tier_error(*, message: str) -> str:
        return f"Failed to clear membership designation: {message}"
    email_domains = "Email Domains"
    loading_domains = "Loading email domains..."
    no_domains = "No email domains configured."
    unverified = "Unverified"
    factory_reset_section = "Danger Zone"
    factory_reset_title = "Factory Reset This Nest"
    factory_reset_desc = "Wipe all deployment state (users, mail, stored content) and return this nest to a fresh, unclaimed state. The nest identity and TLS certificate are preserved. You will re-claim the nest immediately afterward."
    factory_reset_button = "Factory Reset…"
    factory_reset_confirm_title = "Factory reset this nest?"
    factory_reset_confirm_body = "This permanently deletes all users, mail, and deployment configuration on this nest and returns it to an unclaimed state. The nest identity and TLS certificate are kept, and you will be guided through re-claiming it. This cannot be undone."
    factory_reset_confirm_button = "Factory Reset"
    factory_reset_cancel = "Cancel"
    factory_reset_failed = "Factory reset failed. The nest is unchanged."
    factory_reset_persist_failed = "Could not save the new setup code on this device, so the reset was not started and your nest is unchanged. Free up storage and try again."


class _AdminNest_page:
    title = "Nest"
    description = "Nest-wide settings for this deployment."
    retire_title = "Retire this server"
    retire_desc = "Delete this server at your cloud provider and remove the DNS records that point at it. Unlike a factory reset, which wipes a server you keep, this destroys the server itself. You'll need your cloud provider's token."
    retire_button = "Retire this server…"
    @staticmethod
    def load_settings_error(*, message: str) -> str:
        return f"Failed to load nest settings: {message}"
    @staticmethod
    def update_setting_error(*, message: str) -> str:
        return f"Failed to update nest setting: {message}"
    @staticmethod
    def load_serving_port_error(*, message: str) -> str:
        return f"Failed to load serving port: {message}"
    @staticmethod
    def set_serving_port_error(*, message: str) -> str:
        return f"Failed to set serving port: {message}"
    serving_port_label = "Serving port"
    serving_port_desc = "The port this nest's client-facing API and web app listen on for desktop or IP-only nests with no router in front — reach it at https://this-host:port/. Default 443. Behind the cloud router this is ignored: the external port is set by the deployment. Takes effect after the nest restarts."
    serving_port_save = "Save port"
    serving_port_invalid = "Enter a port number between 1 and 65535."
    serving_port_fronted_hint = "Served on 443 by this deployment."
    nat_mode_label = "Network mode"
    nat_mode_loading = "Loading network mode…"
    nat_mode_choosing = "Applies to mail serving immediately; certificates and connectivity re-check at the next restart."
    nat_mode_submitting = "Saving network mode…"
    nat_mode_saved = "Network mode saved. Mail serving updated now; certificates and connectivity re-check at the next restart."
    nat_mode_save = "Save mode"
    @staticmethod
    def nat_mode_error_load(*, cause: str) -> str:
        return f"Couldn't load the current network mode: {cause}. You can still pick and save a mode."
    @staticmethod
    def nat_mode_error_transient(*, cause: str) -> str:
        return f"Couldn't save the network mode: {cause}. Try again."
    @staticmethod
    def nat_mode_error_terminal(*, cause: str) -> str:
        return f"Couldn't save the network mode: {cause}."
    web_app_origin_label = "Web app"
    web_app_origin_desc = "What this server's own address answers when someone opens the app there."
    web_app_origin_bundled = "Serve the app this server ships"
    @staticmethod
    def web_app_origin_central(*, origin: str) -> str:
        return f"Send people to {origin}, with this server filled in"
    web_app_origin_save = "Save web app choice"
    web_app_origin_loading = "Loading the web app choice…"
    web_app_origin_status_bundled = "This server's address serves the app it ships."
    @staticmethod
    def web_app_origin_status_central(*, target: str) -> str:
        return f"People who open this server's address are sent to {target}"
    @staticmethod
    def web_app_origin_status_domainless(*, origin: str) -> str:
        return f"Sending people to {origin} is chosen, but this server has no domain to fill in yet, so its address keeps serving the app it ships."
    @staticmethod
    def web_app_origin_status_unknown_mode(*, mode: str) -> str:
        return f"This server uses a web app choice this app doesn't recognize ({mode}). Update the app to change it."
    web_app_origin_status_predates = "This server is too old to offer this choice; its address always serves the app it ships. Update the server to change it."
    @staticmethod
    def web_app_origin_scope(*, origin: str) -> str:
        return f"This changes only what this server's own address answers. Anyone who opens {origin} directly loads the app from there either way, and an address someone types or bookmarks always wins."
    os_up_to_date = "OS up to date"
    os_updates_pending = "Security updates pending"
    os_restart_pending = "Restart pending — will restart automatically when idle"
    os_restart_now = "Restart now"
    @staticmethod
    def os_restart_now_error(*, message: str) -> str:
        return f"Failed to request a host restart: {message}"
    region_label = "Region"
    region_desc = "The region whose laws this deployment operates under. You declare it; it is never detected from an address or a network. If that region has an authority publishing feature rules, they apply to accounts hosted here."
    region_none = "No region declared"
    @staticmethod
    def region_declared(*, region: str) -> str:
        return f"Declared region: {region}"
    region_unreadable = "The stored region declaration can't be read. Declare the region again (or withdraw it) to fix this; any region rules already received stay in force."
    region_placeholder = "Country or region code, e.g. NO"
    region_save = "Declare region"
    region_withdraw = "Withdraw declaration"
    region_not_enrolled = "No authority is enrolled for this region, so no region rules apply here."
    region_enrolled_no_document = "An authority is enrolled for this region; no rules have been published yet."
    @staticmethod
    def region_document(*, authority: str, sequence: str) -> str:
        return f"Region rules in force, published by {authority} (version {sequence})."
    region_stale = "Haven't been able to check for updated region rules recently. The rules already received stay in force."
    region_invalid = "Enter a 2–8 character region code in capitals, like NO or EU."
    @staticmethod
    def region_load_error(*, message: str) -> str:
        return f"Failed to load the declared region: {message}"
    @staticmethod
    def region_save_error(*, message: str) -> str:
        return f"Failed to save the declared region: {message}"
    rotate_seed_label = "Deployment identity"
    rotate_seed_desc = "Give this nest a brand-new identity. Apps that already trust it re-trust it automatically, and anyone still holding the old identity — a removed admin, a lost device — stops being able to use it. It does not undo anything they already saw. Remove the admin first: everyone on the roster inherits the new identity."
    rotate_seed_button = "Rotate deployment identity"
    rotate_seed_confirm_body = "These admins inherit the new identity and can still recover this nest. Anyone not listed loses that ability. This cannot be undone."
    rotate_seed_confirm_button = "Rotate now"
    rotate_seed_cancel_button = "Cancel"
    rotate_seed_roster_loading = "Checking who currently administers this nest…"
    @staticmethod
    def rotate_seed_roster_error(*, cause: str) -> str:
        return f"Couldn't check who currently administers this nest: {cause}. Nothing was rotated — try again."
    rotate_seed_roster_empty = "This nest reported no administrators, which can't be right. Nothing was rotated — reload this page and try again."
    rotate_seed_working = "Rotating the deployment identity…"
    rotate_seed_done = "Deployment identity rotated. Apps re-trust this nest automatically."
    rotate_seed_done_unmarked = "Deployment identity rotated. Your recovery list still shows the old identity — reconnect from this device to clear it."
    rotate_seed_mismatch = "This nest reported a different identity than the one that was sent. Nothing further was changed; check the nest before trying again."
    @staticmethod
    def rotate_seed_failed(*, cause: str) -> str:
        return f"Couldn't rotate the deployment identity: {cause}"
    takedown_label = "Legal takedown"
    takedown_desc = "The one nest-wide content removal, for legal compulsion only (a court order, a statutory demand). Every takedown serves a visible tombstone in place of the content, can be appealed by the author, and writes a permanent audit record. It is never a policy or opinion lever."
    takedown_content_id_label = "Content id"
    takedown_type_post = "Post"
    takedown_type_conversation = "Conversation message"
    takedown_reference_label = "Legal reference"
    takedown_restore_label = "Overturn an existing takedown (restore)"
    takedown_arm_takedown = "Take down…"
    takedown_arm_restore = "Restore…"
    takedown_blocked_no_content = "Enter the content id of the item named by the legal obligation."
    takedown_blocked_no_reference = "A legal reference is required — a takedown without one is refused."
    @staticmethod
    def takedown_confirm_takedown(*, content_type: str, content_id: str, reference: str) -> str:
        return f"Take down {content_type} {content_id}, citing \"{reference}\"? A visible tombstone will be served in its place, the author can appeal, and a permanent audit row records this action."
    @staticmethod
    def takedown_confirm_restore(*, content_type: str, content_id: str) -> str:
        return f"Overturn the takedown of {content_type} {content_id}? The content serves again; the takedown record remains as history."
    takedown_confirm_button_takedown = "Confirm takedown"
    takedown_confirm_button_restore = "Confirm restore"
    takedown_cancel_button = "Cancel"
    takedown_working = "Submitting…"
    takedown_done = "Taken down. A tombstone is served in its place and the author can appeal."
    takedown_restored = "Restored. The content is served again; the takedown stays on record."
    @staticmethod
    def takedown_failed(*, error: str) -> str:
        return f"The nest refused the request: {error}"
    reports_label = "Reports"
    reports_desc = "Reports from users of this nest, and reports forwarded from other nests about accounts hosted here. A report is evidence for you to weigh; it removes nothing by itself. Acting means the legal-takedown console or a suspension — resolving a row only records what you decided."
    reports_loading = "Loading reports…"
    reports_empty = "No open reports."
    @staticmethod
    def reports_origin_local(*, handle: str) -> str:
        return f"Reported by {handle}"
    @staticmethod
    def reports_origin_forwarded(*, nest: str) -> str:
        return f"Reported by a user of {nest}"
    reports_open_takedown = "Open in takedown console"
    reports_acted = "Mark as acted on"
    reports_dismiss = "Dismiss"
    reports_resolved_acted = "Recorded as acted on. The reporter is told the outcome, nothing more."
    reports_resolved_dismissed = "Dismissed. The reporter is told the outcome, nothing more."
    @staticmethod
    def reports_failed(*, error: str) -> str:
        return f"Could not update the report: {error}"
    oauth_label = "Outside-app sign-in keys"
    oauth_desc = "The keys this nest signs outside apps' sign-in passes with, and the secret behind their saved sign-ins. Replacing them is a response to an exposure, never routine upkeep."
    oauth_keys_loading = "Checking which sign-in keys this nest uses…"
    @staticmethod
    def oauth_keys_error(*, cause: str) -> str:
        return f"Couldn't read this nest's sign-in keys: {cause}. Reload this page to try again."
    @staticmethod
    def oauth_key_signing(*, kid: str) -> str:
        return f"{kid} — signing now"
    @staticmethod
    def oauth_key_retiring(*, kid: str, minutes: str) -> str:
        return f"{kid} — replaced; still accepted for {minutes} min"
    @staticmethod
    def oauth_key_retired(*, kid: str) -> str:
        return f"{kid} — replaced; no longer accepted"
    oauth_rotate_button = "Replace sign-in key"
    @staticmethod
    def oauth_rotate_desc(*, minutes: str) -> str:
        return f"A precaution: the current key stays accepted for {minutes} min after it is replaced, so nobody is signed out. Use this for a suspected exposure."
    oauth_force_rotate_button = "Replace sign-in key at once…"
    oauth_secret_force_rotate_button = "End all saved sign-ins…"
    oauth_force_rotate_confirm_one = "Replace the sign-in key at once? The key in use stops being accepted immediately, so every outside app signed in with it must sign in again. Use this when the key is known to have leaked."
    @staticmethod
    def oauth_force_rotate_confirm_many(*, count: str) -> str:
        return f"Replace the sign-in key at once? All {count} keys accepted now stop being accepted immediately, so every outside app signed in with them must sign in again. Use this when a key is known to have leaked."
    oauth_force_rotate_confirm_button = "Replace at once"
    oauth_secret_force_rotate_confirm = "End every saved sign-in? Every connected outside app must be approved again. After a known leak, do this as well as replacing the sign-in key — replacing the key alone leaves saved sign-ins able to get new passes."
    oauth_secret_force_rotate_confirm_button = "End saved sign-ins"
    oauth_cancel_button = "Cancel"
    oauth_working = "Working…"
    @staticmethod
    def oauth_rotate_done(*, kid: str) -> str:
        return f"Replaced. {kid} signs from now on; the previous key stays accepted until it times out."
    @staticmethod
    def oauth_force_rotate_done(*, kid: str, dropped: str) -> str:
        return f"Replaced at once. {kid} is now the only key accepted. Stopped working: {dropped}."
    @staticmethod
    def oauth_force_rotate_done_none(*, kid: str) -> str:
        return f"Replaced at once. {kid} is now the only key accepted."
    @staticmethod
    def oauth_secret_force_rotate_done(*, minted: str, apps: str) -> str:
        return f"Ended. Every saved sign-in issued since {minted} stopped working, and {apps} outside apps were signed out; each must be approved again the next time it is used."
    @staticmethod
    def oauth_secret_force_rotate_done_one(*, minted: str) -> str:
        return f"Ended. Every saved sign-in issued since {minted} stopped working, and one outside app was signed out; it must be approved again the next time it is used."
    @staticmethod
    def oauth_secret_force_rotate_done_none(*, minted: str) -> str:
        return f"Ended. Every saved sign-in issued since {minted} stopped working. No outside apps were connected here."
    oauth_secret_force_rotate_first = "Done. There were no saved sign-ins to end."
    @staticmethod
    def oauth_rotate_failed(*, cause: str) -> str:
        return f"The nest didn't confirm the change: {cause}. Check the keys listed here before trying again."


class _AdminWeb_page:
    title = "Web"
    description = "Web-content hosting for this deployment."
    apex_select_label = "Home page"
    apex_select_subtitle = "Choose whose website serves at this deployment's main address. None serves the built-in info page."
    apex_none = "None (info page)"
    @staticmethod
    def apex_info(*, url: str) -> str:
        return f"The main address serves at {url}."


class _AdminCalendar_page:
    title = "Calendar"
    description = "Calendar (CalDAV) sync for this deployment."
    enabled_label = "Enable calendar (CalDAV) on this nest"
    enabled_subtitle = "Serve calendar sync (CalDAV) for all users. Needs only a real domain with a public certificate — no email infrastructure — so calendar can run with or without email. The shared mail-and-calendar bridge runs whenever this or Enable mail is on."
    caldav_port_label = "CalDAV port"
    caldav_port_desc = "The port the calendar (CalDAV) server listens on for desktop or IP-only nests that have no domain — reach it at https://this-host:port/. Default 8443. On a domain nest this is ignored: CalDAV is served at mail.your-domain on port 443."
    caldav_port_save = "Save port"
    caldav_port_invalid = "Enter a port number between 1 and 65535."


class _AdminContacts_page:
    title = "Contacts"
    description = "Contact (CardDAV) sync for this deployment."
    enabled_label = "Enable contacts (CardDAV) on this nest"
    enabled_subtitle = "Serve contact sync (CardDAV) for all users. Rides the same server and certificate as calendar — no email infrastructure — so contacts can run with or without email or calendar. The shared bridge runs whenever this, Enable calendar, or Enable mail is on."


class _AdminFiles_page:
    title = "Files"
    description = "File (WebDAV) sync for this deployment."
    enabled_label = "Enable files (WebDAV) on this nest"
    enabled_subtitle = "Serve file access (WebDAV) for all users. Rides the same server and certificate as calendar and contacts — no email infrastructure — so files can run with or without email, calendar, or contacts. Nothing is served until a user flags a folder for WebDAV. The shared bridge runs whenever this, Enable contacts, Enable calendar, or Enable mail is on."


class _AdminInvite_requests_page:
    title = "Invite Requests"
    description = "Review and approve or deny invite requests submitted by users."
    empty = "No pending invite requests."
    column_handle = "Handle"
    column_actor = "Actor"
    column_message = "Message"
    approve = "Approve"
    deny = "Deny"
    deny_reason_placeholder = "Optional reason"
    approving = "Approving..."
    denying = "Denying..."
    approve_failed = "Could not approve. Please try again."
    deny_failed = "Could not deny. Please try again."


class _AdminServices_page:
    title = "Services"
    description = "Enable or disable nest sidecar services."
    bridge = "Email Bridge"
    bridge_desc = "IMAP, SMTP, and CalDAV (calendar) access for all users."
    dns = "DNS"
    dns_desc = "Automatic DNS record management."
    pairing = "Nest Pairing"
    pairing_desc = "Let users link their own nests to sync their account (per-user multi-homing)."
    enabled = "Enabled"
    disabled = "Disabled"
    manage_dns = "Manage DNS"


class _AdminLogs_page:
    title = "Nest Logs"
    description = "Recent activity recorded on the nest, newest first. No message contents or secrets are logged — only what happened, when, and where."
    empty = "No nest log entries yet."


class _AdminCustody_hosting:
    title = "Held Custody"
    description = "Data this nest holds on behalf of other people's accounts, at the request of an account holder here. Each row makes this nest dial an outside address on a schedule and keep what it serves."
    empty = "No account here has asked this nest to hold data for anyone."
    @staticmethod
    def count(*, count: str) -> str:
        return f"{count} held for others"
    host = "Requested by"
    owner = "Held for"
    url = "Pulled from"
    budget = "Budget"
    budget_default = "Default"
    held = "Now holding"
    stopped = "Paused"
    active = "Active"
    receipt_fresh = "Confirmed recently"
    receipt_stale = "Not confirmed lately"
    receipt_none = "Never confirmed"
    remove = "Remove"
    remove_confirm_title = "Remove this held custody?"
    remove_confirm_body = "This frees the space now. Pausing only stops the schedule and keeps what is already stored. Removing cannot be undone from here — the account holder would have to ask again."
    remove_confirm = "Remove it"
    remove_cancel = "Keep it"
    removed = "Removed."
    removed_with_store = "Removed, and the stored copy was freed."
    remove_missing = "That row was already gone."


class _AdminBridges_pending:
    title = "Bridges"
    description = "Mail and calendar bridges awaiting your approval."
    empty = "No bridges awaiting approval."
    empty_desc = "Mail and calendar bridges that connect to this nest appear here for approval."
    pubkey = "Public key"
    role = "Role"
    source_ip = "Source IP"
    first_seen = "First seen"
    source_ip_unknown = "—"
    approve = "Approve"
    reject = "Reject"
    name_mail_calendar = "Mail & calendar bridge"
    name_mail = "Mail bridge"
    name_bluesky = "Bluesky bridge"
    name_bridge = "Bridge"
    pending_section = "Pending approval"
    approved_section = "Approved bridges"
    approved_empty = "No approved bridges yet."
    approved_at = "Approved"
    rotate = "Rotate service-user key"


class _AdminBridges_rotate:
    title = "Rotate service-user key?"
    warning = "The bridge will be marked revoked and will exit; the supervisor restarts it with a fresh key. On a mail-enabled box the new key is approved automatically."
    confirm = "Rotate key"
    cancel = "Cancel"


class _AdminMail_page:
    title = "Mail"
    description = "Box-wide mail policy — enable mail and tune the inbound perimeter and authentication enforcement. DKIM, TLS, and DNS records are managed automatically."
    enabled_label = "Enable mail"
    enabled_subtitle = "Run the mail subsystem (SMTP / IMAP / CalDAV) for this nest."
    health_title = "Mail health"
    health_state_off = "Mail: off"
    health_state_bridge_down = "Mail: mail service not connected"
    health_state_blocklisted = "Mail: server address is blocklisted"
    health_state_queue_stalled = "Mail: outgoing mail is delayed"
    health_state_records_failing = "Mail: DNS records need attention"
    health_state_warming_up = "Mail: warming up"
    health_state_delivering = "Mail: delivering"
    health_state_unknown = "Mail: needs attention"
    health_check_bridge = "Mail service connection"
    health_check_blocklist = "Blocklist check"
    health_check_queue = "Outgoing queue"
    health_check_records = "DNS and authentication records"
    health_check_warmup = "Sending warm-up"
    health_check_last_delivered = "Last delivered"
    health_check_last_received = "Last received"
    health_check_pass = "OK"
    health_check_warn = "Warning"
    health_check_fail = "Problem"
    health_check_info = "Info"
    health_never = "Never"
    @staticmethod
    def health_status_line(*, state: str, delivered: str, received: str) -> str:
        return f"{state} — last delivered: {delivered} · last received: {received}"
    health_delist = "Request removal from the blocklist"
    health_recheck = "Check again"
    health_warmup_reset = "Restart warm-up"
    health_warmup_reset_confirm = "Restart the sending warm-up at day 1? Do this only after the server's outgoing IP address changed."
    auto_enable_new_users_label = "Auto-enable mail for new users"
    auto_enable_new_users_subtitle = "New users automatically get a mailbox at their handle on first sign-in. Each user can still turn their own mail off."
    spam_group_title = "Spam and inbound perimeter"
    spam_group_desc = "How inbound mail is scored, rate-limited, and gated before delivery."
    threshold_junk_label = "Junk threshold"
    threshold_junk_subtitle = "Combined score (0–15) above which mail is delivered to Junk. 0 disables."
    threshold_reject_label = "Reject threshold"
    threshold_reject_subtitle = "Score above which mail is rejected outright. 0 disables."
    dnsbl_label = "DNS blocklists"
    dnsbl_subtitle = "One blocklist host per line, queried during delivery."
    reject_no_rdns_label = "Reject senders with no rDNS"
    greylist_enabled_label = "Greylisting"
    greylist_delay_label = "Greylist delay (seconds)"
    max_conn_per_min_label = "Max connections / minute (per IP)"
    fcrdns_mode_label = "Forward-confirmed rDNS"
    fcrdns_off = "Off"
    fcrdns_score_signal = "Score signal"
    fcrdns_enforce = "Enforce"
    helo_identity_label = "Require HELO identity"
    reject_fcrdns_fail_label = "Reject on FCrDNS failure"
    max_message_bytes_label = "Max message size (bytes)"
    bayesian_weight_label = "Bayesian weight (milli)"
    bayesian_weight_subtitle = "Weight of each user's own model in the combined spam score, in milli — 700 = 0.7. Range 0–1000."
    bayesian_min_samples_label = "Bayesian min samples"
    bayesian_min_samples_subtitle = "Training samples below which a user's own model is ignored (default 50)."
    bayesian_full_confidence_samples_label = "Bayesian full-confidence samples"
    bayesian_full_confidence_samples_subtitle = "Samples at which a user's model reaches full weight (default 200). Must be above min samples."
    training_history_retention_label = "Training history retention (days)"
    training_history_retention_subtitle = "How long each user's per-message training-undo history is kept (default 30)."
    unlisted_recipient_penalty_label = "Unlisted-recipient penalty (points)"
    unlisted_recipient_penalty_subtitle = "Extra spam points added when mail arrives at an address that isn't one of a user's aliases (delivered via catch-all). 0 = off; a large value (e.g. 1000) forces such mail to Junk."
    spam_save = "Save spam policy"
    publish_spam_baseline_button = "Publish deployment baseline"
    publish_spam_baseline_subtitle = "Aggregate every opted-in user's spam training into a baseline that new users start from. Never reveals who contributed, and needs at least 3 contributors."
    @staticmethod
    def spam_baseline_published(*, contributors: str, samples: str) -> str:
        return f"Published from {contributors} contributors ({samples} samples)."
    @staticmethod
    def spam_baseline_withheld(*, contributors: str) -> str:
        return f"Not published — too few contributors ({contributors}); at least 3 must opt in."
    @staticmethod
    def spam_baseline_skipped_contributors(*, count: str) -> str:
        return f"{count} opted-in contributor(s) could not be merged this run."
    spam_baseline_standing_label = "Keep a shared spam baseline published"
    spam_baseline_standing_subtitle = "Republishes the baseline every 24 hours while enough users contribute. Turning this off withdraws the published baseline."
    @staticmethod
    def spam_baseline_state_published(*, contributors: str, date: str) -> str:
        return f"Published over {contributors} contributors on {date}."
    spam_baseline_state_none = "No baseline published."
    spam_baseline_waiting = "Waiting for more contributor activity."
    auth_group_title = "Authentication enforcement"
    auth_group_desc = "Which SPF / DKIM / DMARC failures reject inbound mail at delivery."
    enforce_dmarc_label = "Enforce DMARC reject"
    enforce_dmarc_quarantine_label = "Enforce DMARC quarantine"
    enforce_spf_hardfail_label = "Enforce SPF hardfail"
    enforce_dkim_label = "Enforce DKIM"
    log_only_label = "Log only (never reject)"
    max_failures_label = "AUTH failure limit / minute"
    max_conn_per_ip_label = "Max concurrent connections (per IP)"
    auth_save = "Save authentication policy"
    submission_group_title = "Submission quotas"
    submission_group_desc = "Per-actor ceilings on outbound message submission."
    submission_max_per_day_label = "Messages per day (per actor)"
    submission_max_per_day_subtitle = "How many messages each account may submit per day."
    submission_max_recipients_label = "Recipients per message"
    submission_max_recipients_subtitle = "Maximum recipients allowed on a single submitted message."
    submission_save = "Save submission policy"
    imap_group_title = "IMAP server policy"
    imap_group_desc = "How the IMAP/MDA serves mailboxes to mail clients."
    imap_idle_timeout_label = "IDLE timeout (seconds)"
    imap_idle_timeout_subtitle = "How long an idle IMAP session is held before the server ends it."
    imap_tombstone_retention_label = "Tombstone retention (days)"
    imap_tombstone_retention_subtitle = "How long expunged-message markers are kept for resync (minimum 7)."
    imap_delete_nonempty_label = "Delete non-empty mailbox"
    imap_delete_forbidden = "Forbidden"
    imap_delete_allowed = "Allowed"
    imap_bodystructure_cache_label = "BodyStructure cache size (entries)"
    imap_bodystructure_cache_subtitle = "In-memory derivation cache the MDA keeps per session."
    imap_storage_bytes_label = "Storage quota (bytes)"
    imap_storage_bytes_subtitle = "Per-actor storage ceiling across all mailboxes."
    imap_message_count_label = "Message-count quota"
    imap_message_count_subtitle = "Per-actor message-count ceiling across all mailboxes."
    imap_save = "Save IMAP policy"
    outbound_group_title = "Outbound delivery"
    outbound_group_desc = "Retry, bounce, and TLS-reporting behavior for outbound mail."
    outbound_retry_schedule_label = "Retry schedule (seconds)"
    outbound_retry_schedule_subtitle = "Delay before each successive attempt — one value per line."
    outbound_permfail_timeout_label = "Permanent-failure timeout (hours)"
    outbound_permfail_timeout_subtitle = "Total retry budget before a message permanently fails."
    outbound_delay_warning_label = "Delay-warning time (hours)"
    outbound_delay_warning_subtitle = "When a delay-warning notice is sent to the sender."
    outbound_ndr_rate_limit_label = "Bounce rate-limit window (days)"
    outbound_ndr_rate_limit_subtitle = "Per-recipient window for suppressing repeated bounce notices."
    outbound_suppress_ndr_spf_label = "Suppress bounce on SPF hardfail"
    outbound_suppress_ndr_dmarc_label = "Suppress bounce on DMARC reject"
    outbound_postmaster_cc_label = "CC postmaster on bounces"
    outbound_postmaster_cc_subtitle = "Disabled — project policy never copies the postmaster."
    outbound_tlsrpt_send_label = "Send TLSRPT reports"
    outbound_ipv6_label = "IPv6 outbound"
    outbound_treat_5xx_label = "Treat as transient (5xx codes)"
    outbound_treat_5xx_subtitle = "Enhanced-status codes to retry even when the reply is 5xx — one per line."
    outbound_save = "Save outbound policy"
    alias_group_title = "Aliases"
    alias_group_desc = "Per-account alias limits and the inbound address-resolution rules."
    alias_exact_max_label = "Exact aliases per account (max)"
    alias_exact_max_subtitle = "Cap on user-added exact aliases beyond the signup address."
    alias_reserved_label = "Reserved local-parts"
    alias_reserved_subtitle = "Role addresses users cannot claim — one local-part per line; empty clears the reservation."
    alias_subaddressing_label = "Sub-addressing (plus-suffix)"
    alias_subaddressing_subtitle = "Allow plus-tagged aliases that route to the base address."
    alias_wildcard_prefix_label = "Wildcard-prefix aliases"
    alias_wildcard_prefix_subtitle = "Allow name-prefixed aliases that route to the same user."
    alias_save = "Save alias policy"


class _AdminDnsCert:
    label = "Certificate:"
    status_valid = "Valid"
    status_on_floor = "Renew needed"
    status_expiring = "Expiring soon"
    self_signed = "self-signed"
    @staticmethod
    def expires(*, date: str) -> str:
        return f"expires {date}"
    auto_renew = "Auto-renew"
    issue = "Get certificate"
    issue_complete = "I've added the record"
    issue_cancel = "Cancel"
    paste_instructions = "Add this DNS record at your registrar, then confirm:"
    delegate = "Automate renewals"
    delegate_zone_label = "Delegate to zone:"
    delegate_submit = "Delegate"
    delegate_cancel = "Cancel"
    remove_delegation = "Remove delegation"
    renewals_automated = "Renewals automated"
    delegate_no_zones = "Add a DNS-provider credential first to delegate renewals."


class _AdminDnsRename:
    button = "Rename primary domain"
    promote = "Promote to primary"
    renaming_to = "Renaming to"
    sheet_title = "Rename primary domain"
    new_primary_label = "New primary domain"
    grace_days_label = "Grace window (days, default 7)"
    submit = "Start rename"
    cancel = "Cancel"
    banner_title = "Primary-domain rename in progress"
    state_label = "State:"
    grace_ends = "Grace ends:"
    grace_elapsed = "Grace window elapsed"
    complete = "Complete now"
    complete_confirm = "Confirm complete"
    complete_force_warning = "Completing before the grace window ends may briefly break mail delivery for peers whose caches have not yet refreshed."
    extend = "Extend grace"
    extend_days_label = "Extend by (days):"
    abort = "Abort rename"
    abort_confirm = "Confirm abort"
    abort_postflip_warning = "Aborting after the anchor flip re-flips the primary and rewrites every domain's DNS — expensive but safe."


class _AdminDns:
    title = "DNS"
    description = "Every DNS record each of your domains needs, with the exact value to set and a live check against public DNS."
    empty = "No domains yet."
    empty_desc = "Add a mail domain and its required DNS records appear here."
    field_name = "Name"
    field_type = "Type"
    field_value = "Value"
    copy = "Copy"
    ptr_provider_note = "Reverse DNS (PTR) is set at your server's IP provider, not published here. Most VPS providers let you set it in their control panel."
    status_ok = "OK"
    status_missing = "Missing"
    status_mismatch = "Mismatch"
    @staticmethod
    def status_mismatch_found(*, found: str) -> str:
        return f"Mismatch — found {found}"
    status_checking = "Checking…"
    add_domain = "Add domain"
    add_domain_placeholder = "example.com"
    add_domain_submit = "Add"
    add_domain_primary_warning = "This becomes your nest's primary domain and can never be removed — undoing it later requires renaming onto a different domain."
    primary_badge = "Primary"
    remove = "Remove"
    removed_title = "Recently removed"
    removed_desc = "Restorable for 30 days."
    restore = "Restore"
    refresh = "Refresh"
    credentials_title = "DNS-provider credentials"
    credentials_empty = "No DNS-provider credentials yet."
    credential_zones = "Zones"
    add_credential = "Add credential"
    add_credential_submit = "Add"
    manage_all = "Fauna controls all domains"
    mode_managed = "Fauna-managed"
    mode_manual = "Manual"
    catch_all_label = "Catch-all:"
    catch_all_none = "None"
    catch_all_cleared_by_succession = "A succession cleared this domain's catch-all — unmatched mail now bounces. Re-designate below if you want a new one."
    role_address_label = "Role addresses:"
    role_address_admin_default = "Admin (default)"
    cert = _AdminDnsCert
    rename = _AdminDnsRename


class _AdminView:
    nest_statistics = "Nest Statistics"
    registered_users = "Registered Users"
    total_storage_used = "Total Storage Used"
    total_inbox_messages = "Total Inbox Messages"
    active_sessions = "Active Sessions"
    recent_users = "Recent Users"
    recent_users_desc = "Last 10 registered users"
    no_users_loaded = "No users loaded yet."
    server_status = "Server Status"
    uptime = "Uptime"
    workers = "Workers"
    no_registered_users = "No registered users."
    no_user_data = "No user data available."


class _Admin:
    aliases = "Aliases"
    exit = "Exit admin"
    @staticmethod
    def actor_id_fallback_label(*, short: str) -> str:
        return f"actor {short}…"
    dashboard = _AdminDashboard
    users_page = _AdminUsers_page
    aliases_page = _AdminAliases_page
    settings_page = _AdminSettings_page
    nest_page = _AdminNest_page
    web_page = _AdminWeb_page
    calendar_page = _AdminCalendar_page
    contacts_page = _AdminContacts_page
    files_page = _AdminFiles_page
    invite_requests_page = _AdminInvite_requests_page
    services_page = _AdminServices_page
    logs_page = _AdminLogs_page
    custody_hosting = _AdminCustody_hosting
    bridges_pending = _AdminBridges_pending
    bridges_rotate = _AdminBridges_rotate
    mail_page = _AdminMail_page
    dns = _AdminDns
    view = _AdminView


class _SetupDns:
    title = "Configure DNS"
    @staticmethod
    def description(*, domain: str) -> str:
        return f"We need API access to your DNS provider to create records for {domain}."
    verified = "Token verified."
    provision = "Provision"


class _SetupServer:
    title = "Choose a server provider"


class _SetupByo:
    title = "Run Fauna on your server"


class _SetupByo_status:
    title = "Connecting to your nest"


class _Setup:
    back = "Back"
    dns = _SetupDns
    server = _SetupServer
    byo = _SetupByo
    byo_status = _SetupByo_status


class _C2pa:
    provenance = "Content Provenance"
    signer = "Signer"
    tool = "Tool"
    valid = "Valid"
    validation_issue = "Validation issue"
    verified_title = "C2PA verified provenance"
    invalid_title = "C2PA provenance (validation issue)"
    view_label = "View content provenance"
    image_viewer = "Image viewer"
    badge_label = "C2PA"


class _P2p:
    title = "P2P Contacts"
    no_contacts = "No P2P contacts yet"
    delete_contact = "Delete Contact"
    copy_to_clipboard = "Copy to Clipboard"


class _ModerationReportReason:
    spam = "Spam"
    harassment = "Harassment"
    hate = "Hateful content"
    violence = "Violence or threats"
    sexual = "Sexual content"
    illegal = "Illegal content"
    impersonation = "Impersonation"
    other = "Something else"


class _ModerationReport:
    title = "Report"
    reason_label = "Why are you reporting this?"
    note_label = "Anything the admins should know? (optional)"
    include_text_label = "Include the text of this message — the admins will be able to read it"
    block_author_label = "Also block this person"
    submit = "Send report"
    cancel = "Cancel"
    blocked_no_reason = "Choose a reason before sending the report."
    blocked_note_too_long = "The note is too long. Shorten it before sending the report."
    @staticmethod
    def sent_local(*, nest: str) -> str:
        return f"Report sent to the admins of {nest}."
    @staticmethod
    def sent_forwarded(*, nest: str, home_nest: str) -> str:
        return f"Report sent to the admins of {nest} and forwarded, without your name, to the admins of {home_nest}."
    @staticmethod
    def failed(*, error: str) -> str:
        return f"The report could not be sent: {error}"
    hidden_placeholder = "You reported this"
    ledger_title = "Your reports"
    ledger_empty = "You have not reported anything."
    @staticmethod
    def ledger_routed_to(*, destinations: str) -> str:
        return f"Sent to {destinations}"
    status_open = "Open"
    status_resolved = "Resolved"
    status_withdrawn = "Withdrawn"
    outcome_acted = "Acted on"
    outcome_dismissed = "Dismissed"
    withdraw = "Withdraw"
    withdrawn = "Report withdrawn. Your note and any attached text were deleted everywhere they went."
    reason = _ModerationReportReason


class _ModerationCategory:
    spam = "Spam"
    trusted = "Trusted"
    nsfw = "NSFW"
    phishing = "Phishing"
    commercial = "Commercial"


class _ModerationAction:
    rejected = "Rejected"
    quarantined = "Quarantined"
    suppressed = "Hidden from feeds"
    rate_limited = "Rate limited"
    logged = "Logged"
    labeled = "Labeled"
    flagged = "Flagged"
    taken_down = "Removed under legal obligation"


class _ModerationLegal_takedown:
    @staticmethod
    def tombstone(*, reference: str) -> str:
        return f"Removed under legal obligation ({reference})"


class _Moderation:
    spam_protection = "Spam Protection"
    spam_hint = "Content scoring above this threshold is filtered as spam."
    phishing_hint = "Content scoring above this threshold is flagged as phishing."
    stats_title = "Moderation Stats"
    total_labels = "Total labels:"
    spam_detected = "Spam detected:"
    avg_confidence = "Avg spam confidence:"
    enforcement_title = "Enforcement Actions"
    no_actions = "No enforcement actions on your content."
    confidence = "confidence"
    correct = "Correct"
    @staticmethod
    def flagged_count(*, count: str) -> str:
        return f"{count} flagged"
    appeal = "Appeal"
    appeal_reason_label = "Why should this decision be reviewed?"
    @staticmethod
    def appeal_summary(*, content_id: str) -> str:
        return f"Appealing the enforcement action on {content_id}."
    appeal_submit = "Submit appeal"
    appeal_cancel = "Cancel"
    appeal_blocked_no_content = "No content selected to appeal."
    appeal_blocked_no_reason = "Enter a reason before submitting the appeal."
    appeal_blocked_reason_too_long = "The reason is too long. Shorten it before submitting the appeal."
    appeal_recorded = "Appeal recorded. An administrator will review it."
    @staticmethod
    def appeal_failed(*, error: str) -> str:
        return f"Appeal failed: {error}"
    report = _ModerationReport
    category = _ModerationCategory
    action = _ModerationAction
    legal_takedown = _ModerationLegal_takedown


class _Muted_words:
    title = "Muted words"
    description = "Conversation messages containing one of these words are collapsed behind a “Show anyway” button. This list stays on your devices — the server never sees it."
    input_placeholder = "Add a word to mute"
    add = "Mute word"
    empty = "You haven’t muted any words yet."
    remove = "Un-mute"


class _Personalization:
    title = "Personalization"
    feeds_link = "Feeds"
    muted_words_link = "Muted words"
    labelers_empty = "You haven’t subscribed to any community labelers yet."
    browse_catalog = "Browse labeler catalog"
    trained_topics_title = "Trained topics"
    trained_topics_empty = "You haven’t created any trained topics yet."
    trained_factor_placeholder = "Topic name"
    trained_factor_create = "New trained topic"
    trained_factor_rename = "Rename"
    trained_factor_save = "Save name"
    @staticmethod
    def trained_factor_examples(*, count: str) -> str:
        return f"{count} examples"
    @staticmethod
    def trained_factor_cap(*, max: str) -> str:
        return f"You already have {max} trained topics — delete one to create another."
    trained_factor_blank_name = "A trained topic needs a name."
    trained_factor_engagement_toggle = "Learn from my activity"
    clear_engagement_data = "Clear activity data"
    trained_factor_publish = "Publish…"
    publish_sheet_title = "Publish this topic"
    publish_name_label = "Public name for this list"
    publish_name_placeholder = "e.g. Small orange cats"
    publish_limitation_note = "You’re sharing the posts below — not the topic itself or anything it learned about you. Only posts this device has already loaded and seen can be included, so the list won’t cover posts you never saw. It’s published anonymously: nothing links the list back to you or your account."
    publish_exemplars_title = "Posts to include"
    publish_exemplars_empty = "This topic hasn’t scored any of the posts loaded so far. Open a feed it ranks, then try again."
    publish_exemplar_include = "Include"
    @staticmethod
    def publish_score(*, score: str) -> str:
        return f"Score {score}"
    publish_submit = "Publish"
    publish_name_blank = "A published name can’t be blank."
    @staticmethod
    def publish_name_too_long(*, max: str) -> str:
        return f"A published name is at most {max} characters."
    publish_kind_label = "Share as"
    publish_kind_list = "List of posts"
    publish_kind_model = "Word-pattern model"
    @staticmethod
    def publish_kind_unknown(*, kind: str) -> str:
        return f"{kind}"
    publish_name_label_model = "Public name for this model"
    publish_limitation_note_model = "You’re sharing the word patterns this topic learned — not the topic itself, and none of the posts. Unlike a list, a model also matches posts nobody here has seen yet. Only patterns that appear in at least 3 of your marked public posts are included, and that covers both what you marked as more like this and what you marked as less like this — the direction column below shows which is which. It’s published anonymously, and it’s built only from posts you marked by hand: nothing you merely read or watched goes into it."
    @staticmethod
    def publish_corpus_size(*, included: str, marked: str) -> str:
        return f"Built from {included} public examples of your {marked} marked posts."
    publish_ngrams_title = "Word patterns to include"
    publish_ngrams_empty = "No word pattern appears in at least 3 of this topic’s public examples yet, so there’s nothing that can be shared without quoting a single post. Mark a few more public posts for this topic, then try again."
    publish_vocabulary_empty = "This topic needs more public examples before it can be shared as a model."
    publish_ngram_direction_more = "More like this"
    publish_ngram_direction_less = "Less like this"
    publish_ngram_direction_both = "Both"
    @staticmethod
    def publish_ngram_count(*, count: str) -> str:
        return f"In {count} posts"
    share_signals_title = "Anonymous signal sharing"
    share_signals_label = "Share anonymous engagement signals"
    share_signals_subtitle = "Off by default. When on, whether you watched or skipped a public post joins an anonymized count your nest shares — but only once at least 3 people here have the same verdict on the same post, and never your identity or your activity."
    signal_published_title = "What this nest publishes"
    signal_published_description = "The anonymized signal and report counts your nest shares with peers, shown exactly as a peer nest sees them. Nothing appears here below 3 contributors."
    signal_published_empty = "This nest publishes no signal aggregates yet"
    signal_published_contributors = "contributors"


class _Labeler_catalog:
    title = "Community labelers"
    empty = "No community labelers published yet."
    inspect = "Inspect"
    subscribe = "Subscribe"
    unsubscribe = "Unsubscribe"
    close_inspect = "Close"
    @staticmethod
    def list_name(*, name: str) -> str:
        return f"List name: {name}"
    @staticmethod
    def list_entry_count(*, count: str) -> str:
        return f"{count} entries"
    unnamed_list = "Unnamed list"
    @staticmethod
    def model_name(*, name: str) -> str:
        return f"Model name: {name}"
    @staticmethod
    def model_ngram_count(*, count: str) -> str:
        return f"{count} word patterns"
    unnamed_model = "Unnamed model"
    kind_needs_newer_app = "needs a newer app"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load community labelers: {message}"
    @staticmethod
    def error_inspect(*, message: str) -> str:
        return f"Failed to inspect this labeler: {message}"
    @staticmethod
    def error_subscribe(*, message: str) -> str:
        return f"Failed to subscribe: {message}"
    @staticmethod
    def error_unsubscribe(*, message: str) -> str:
        return f"Failed to unsubscribe: {message}"
    subscribed_without_mail_holder = "Subscribed, but this labeler cannot run over your mail yet: this nest has no mail service to trust with it."
    subscribed_without_mail = "Subscribed, but this labeler cannot run over your mail until mail is set up for your account."


class _Task_delegation:
    title = "Task delegation"
    description = "Heavy background tasks run on one capable, always-on device — a nest or a plugged-in computer — and stay off battery phones. Each task picks its device automatically; pin one if you prefer."
    kind_backup_upload = "Backup uploads"
    kind_content_rescore = "Content re-scoring"
    kind_index = "Search indexing"
    assignment_automatic = "Automatic"
    assignment_this_device = "This device"
    @staticmethod
    def assignment_other_name(*, name: str) -> str:
        return f"{name}"
    runner_this_device = "Running on this device"
    @staticmethod
    def runner_other_device(*, device: str) -> str:
        return f"Running on {device}"
    runner_waiting = "Waiting for an eligible device"
    error_device_id = "This device could not load its own identity, so task assignments cannot be shown or changed here. Restart the app; if that does not help, its local data directory may not be writable."


class _Features:
    name_payments = "Payments"
    name_zaps = "Zaps"
    name_p2p_share = "File sharing"
    name_other = "A feature this app does not know yet"
    tier_structural = "Fauna's built-in limits"
    tier_region = "Your region's rules"
    tier_admin = "Your nest admin"
    tier_guardian = "Your guardian"
    tier_self = "Your own setting"
    tier_other = "Another rule-setter"
    denied_by_structural = "Turned off by Fauna's built-in limits."
    denied_by_region = "Turned off by your region's rules."
    denied_by_admin = "Turned off by your nest admin."
    denied_by_guardian = "Turned off by your guardian."
    denied_by_self = "You turned this off."
    denied_by_other = "Turned off by another rule-setter."
    @staticmethod
    def exhausted_structural(*, window: str) -> str:
        return f"You've used up Fauna's built-in limit for this {window}."
    @staticmethod
    def exhausted_region(*, window: str) -> str:
        return f"You've used up the limit your region's rules set for this {window}."
    @staticmethod
    def exhausted_admin(*, window: str) -> str:
        return f"You've used up the limit your nest admin set for this {window}."
    @staticmethod
    def exhausted_guardian(*, window: str) -> str:
        return f"You've used up the limit your guardian set for this {window}."
    @staticmethod
    def exhausted_self(*, window: str) -> str:
        return f"You've used up the limit you set for this {window}."
    @staticmethod
    def exhausted_other(*, window: str) -> str:
        return f"You've used up the limit another rule-setter set for this {window}."
    window_day = "day"
    window_week = "week"
    window_month = "month"
    section_title = "Feature limits"
    empty = "No feature limits apply on this nest."
    status_available = "Available"
    status_restricted = "Restricted"
    dimension_operations = "Uses"
    dimension_counterparties = "People"
    dimension_volume = "Amount"
    @staticmethod
    def quota_label(*, dimension: str, window: str) -> str:
        return f"{dimension} per {window}"
    @staticmethod
    def quota_value(*, remaining: str, limit: str) -> str:
        return f"{remaining} left of {limit}"
    @staticmethod
    def quota_value_exhausted(*, limit: str) -> str:
        return f"none left of {limit}"
    @staticmethod
    def magnitude_sats(*, value: str) -> str:
        return f"{value} sats"
    admin_section_title = "Feature limits for everyone"
    admin_section_desc = "Limits set here apply to every account on this nest. They can only tighten what Fauna and your region already allow."
    authored_none = "No limit set"
    authored_off = "Turned off"
    authored_on = "On, no limits"
    authored_limited_one = "On, 1 limit"
    @staticmethod
    def authored_limited(*, count: str) -> str:
        return f"On, {count} limits"
    authored_unreadable = "This limit can't be read, so the feature is off until it's set again or removed."
    own_label = "Your own limit"
    own_edit = "Set your own limit"
    admin_edit = "Edit"
    @staticmethod
    def editor_title_admin(*, feature: str) -> str:
        return f"{feature}: limits for everyone on this nest"
    @staticmethod
    def editor_title_self(*, feature: str) -> str:
        return f"{feature}: limits only for you"
    @staticmethod
    def editor_title_guardian(*, feature: str, ward: str) -> str:
        return f"{feature}: limits only for {ward}"
    editor_on = "On"
    editor_off = "Off"
    editor_hint = "Leave a box empty for no limit. Zero is a limit."
    @staticmethod
    def editor_volume_label_bytes(*, window: str) -> str:
        return f"Amount per {window} (for example 50 GB)"
    @staticmethod
    def editor_volume_label_sats(*, window: str) -> str:
        return f"Amount per {window}, in sats"
    editor_per_operation_bytes = "Largest single item (for example 2 GB)"
    editor_per_operation_sats = "Largest single payment, in sats"
    editor_save = "Save"
    editor_remove = "Remove limit"
    editor_cancel = "Cancel"
    editor_saved = "Saved."
    editor_removed = "Limit removed."
    @staticmethod
    def editor_invalid_count(*, value: str) -> str:
        return f"\"{value}\" isn't a whole number. Type one, or leave the box empty."
    @staticmethod
    def editor_invalid_size(*, value: str) -> str:
        return f"\"{value}\" isn't a size. Try something like 50 GB, or leave the box empty."
    @staticmethod
    def editor_invalid_sats(*, value: str) -> str:
        return f"\"{value}\" isn't a whole number of sats. Type one, or leave the box empty."
    @staticmethod
    def editor_save_failed(*, error: str) -> str:
        return f"Could not save the limit: {error}"
    @staticmethod
    def editor_load_failed(*, error: str) -> str:
        return f"Could not load the limits: {error}"
    editor_ward_missing = "This account is no longer in your family, so the limit was not saved."
    guardian_section_title = "Feature limits"
    guardian_section_desc = "Limits set here apply only to this child. They can only tighten what already applies."
    @staticmethod
    def no_effect_structural(*, limit: str) -> str:
        return f"No effect: Fauna's built-in limit is already {limit}."
    @staticmethod
    def no_effect_region(*, limit: str) -> str:
        return f"No effect: your region's rules already limit this to {limit}."
    @staticmethod
    def no_effect_admin(*, limit: str) -> str:
        return f"No effect: your nest admin already limits this to {limit}."
    @staticmethod
    def no_effect_guardian(*, limit: str) -> str:
        return f"No effect: your guardian already limits this to {limit}."
    @staticmethod
    def no_effect_self(*, limit: str) -> str:
        return f"No effect: your own setting already limits this to {limit}."
    @staticmethod
    def no_effect_other(*, limit: str) -> str:
        return f"No effect: another rule-setter already limits this to {limit}."


class _FamilyAge_band:
    label = "Age band"
    not_set = "Not set"
    u13 = "Under 13"
    teen_13_15 = "13–15"
    teen_16_17 = "16–17"
    adult = "18+"
    provenance_guardian_asserted = "set by guardian"
    provenance_attested_android = "verified on Android"
    provenance_attested_ios = "verified on iOS"
    provenance_none = "declared, not verified"
    @staticmethod
    def ward_line(*, band: str, provenance: str) -> str:
        return f"Age band: {band} · {provenance}"
    @staticmethod
    def ward_line_band_only(*, band: str) -> str:
        return f"Age band: {band}"
    @staticmethod
    def own_summary(*, band: str, provenance: str) -> str:
        return f"Your age band: {band} · {provenance}, set at admission"
    @staticmethod
    def own_summary_band_only(*, band: str) -> str:
        return f"Your age band: {band}, set at admission"
    @staticmethod
    def claim_line(*, band: str, provenance: str) -> str:
        return f"Age {band} · {provenance}"
    claim_none = "No app age verification"
    @staticmethod
    def notice_attested(*, band: str, store: str, verifier: str) -> str:
        return f"Your age range ({band}) from {store} will be shared with this nest's admin, verified by {verifier}"
    @staticmethod
    def notice_declared(*, band: str) -> str:
        return f"Your age range ({band}) will be shared with this nest's admin as declared, not verified"
    store_android = "Google Play"
    store_ios = "the App Store"
    verifier_android = "Google"
    verifier_ios = "Apple"


class _Family:
    title = "Family"
    @staticmethod
    def supervised_indicator(*, guardian: str) -> str:
        return f"This account is supervised by {guardian}"
    @staticmethod
    def supervised_notice_onboarding(*, guardian: str) -> str:
        return f"This account will be supervised by {guardian}"
    wards_heading = "Accounts you supervise"
    no_wards = "You are not supervising any accounts."
    policy_contact_approval_label = "Require my approval for new contacts"
    policy_unknown_sender_label = "Unknown email senders"
    policy_federation_label = "Allow contact from other nests"
    policy_feed_sources_label = "New feed sources"
    policy_feed_sources_caveat = "Blocks new external accounts and follows. Messages arriving through an already-connected account are governed by \"Unknown message senders\"."
    policy_unknown_peer_dm_label = "Unknown message senders"
    policy_content_nsfw_label = "Adult content"
    policy_content_spam_label = "Spam"
    policy_content_phishing_label = "Phishing and scams"
    policy_content_commercial_label = "Ads and promotions"
    policy_content_notify_label = "Notify me about flagged content"
    policy_screen_heading = "Screen time"
    policy_screen_window_start_label = "Usable from (HH:MM)"
    policy_screen_window_end_label = "Usable until (HH:MM)"
    policy_screen_daily_minutes_label = "Daily limit (minutes, all devices)"
    policy_screen_caveat = "Enforced by the apps on your child's devices. Leave a field empty to remove that limit."
    policy_save_button = "Save policy"
    value_allow = "Allow"
    value_hold = "Hold for review"
    value_reject = "Reject"
    value_block = "Block"
    value_inherit = "Use my settings"
    value_collapse = "Collapse"
    content_blocked_notice = "Hidden by your family policy"
    content_collapsed_notice = "Flagged content"
    content_reveal_button = "Show anyway"
    @staticmethod
    def ward_content_notice_count(*, count: str) -> str:
        return f"{count} flagged today"
    ward_usage_today_label = "Screen time today"
    @staticmethod
    def ward_usage_today_of_budget(*, used: str, budget: str) -> str:
        return f"{used} of {budget} minutes"
    @staticmethod
    def ward_usage_today(*, minutes: str) -> str:
        return f"{minutes} minutes"
    ward_devices_heading = "Devices"
    @staticmethod
    def ward_devices_hint(*, handle: str) -> str:
        return f"Mark the device you enrolled into this account. {handle} cannot remove a marked device, and graduating un-enrolls it automatically."
    no_ward_devices = "No devices registered yet."
    device_mark_label = "Guardian device"
    blocked_peers_heading = "Denied message senders"
    @staticmethod
    def blocked_peers_hint(*, handle: str) -> str:
        return f"People you denied for {handle}. Their new messages are refused; anything already delivered stays readable. Allowing lets them message again."
    no_blocked_peers = "Nobody denied."
    blocked_peer_allow = "Allow again"
    screen_lock_title = "Screen time is off"
    @staticmethod
    def screen_lock_window(*, resumes: str, guardian: str) -> str:
        return f"Your screen time starts again at {resumes}. Set by {guardian}."
    @staticmethod
    def screen_lock_budget(*, minutes: str, guardian: str) -> str:
        return f"You have used today's {minutes} minutes. Set by {guardian}."
    screen_lock_family_hint = "You can still open Family to see your settings."
    approvals_heading = "Approvals"
    no_approvals = "No pending approvals."
    approval_no_sender = "No sender (delivery notice)"
    approve = "Approve"
    deny = "Deny"
    contact_add_placeholder = "Actor ID (hex)"
    contact_add_button = "Pre-approve contact"
    contact_add_invalid_actor_id = "Not a valid actor ID (expected hex)"
    graduate_button = "Graduate to full account"
    @staticmethod
    def graduate_confirm_button(*, handle: str) -> str:
        return f"Yes, graduate {handle}"
    transfer_placeholder = "New guardian actor ID (hex)"
    transfer_button = "Propose new guardian"
    @staticmethod
    def transfer_pending(*, handle: str) -> str:
        return f"Waiting for {handle} to accept guardianship"
    transfer_cancel_button = "Cancel proposal"
    incoming_transfers_heading = "Guardianship requests"
    @staticmethod
    def incoming_transfer_text(*, guardian: str, ward: str) -> str:
        return f"{guardian} asks you to take over supervision of {ward}"
    incoming_transfer_accept_button = "Accept guardianship"
    incoming_transfer_decline_button = "Decline"
    @staticmethod
    def guardian_label(*, guardian: str) -> str:
        return f"Supervised by {guardian}"
    policy_summary_heading = "Current policy"
    age_band = _FamilyAge_band


class _Navigation:
    quick_switcher = "Quick Switcher"
    quick_switcher_placeholder = "Search conversations, groups…"
    no_matches = "No matches"
    navigate = "Navigate"
    @staticmethod
    def members(*, count: str) -> str:
        return f"{count} members"
    section = "Section"
    category_conversation = "Conversation"
    category_group = "Group"
    category_file = "File"
    category_action = "Go to"


class _Notifications:
    @staticmethod
    def message_from(*, sender: str) -> str:
        return f"Message from {sender}"
    @staticmethod
    def group_message(*, sender: str, group: str) -> str:
        return f"{sender} in {group}"
    knock_title = "New Contact Request"
    @staticmethod
    def knock_body(*, name: str) -> str:
        return f"{name} wants to connect"
    sync_complete_title = "Sync Complete"
    @staticmethod
    def sync_complete_body(*, filename: str) -> str:
        return f"{filename} uploaded"
    event_reminder_title = "Upcoming Event"
    @staticmethod
    def event_reminder_body(*, name: str, minutes: str) -> str:
        return f"{name} in {minutes} minutes"
    group_invite_title = "Group Invite"
    @staticmethod
    def group_invite_body(*, name: str, group: str) -> str:
        return f"{name} invited you to {group}"
    default_body = "New notification"
    type_like = "Like"
    type_repost = "Repost"
    type_mention = "Mention"
    type_quote = "Quote"
    type_message = "Message"
    type_follow = "Follow"
    type_reply = "Reply"
    type_event_invite = "Event invite"
    type_group_invite = "Group invite"
    type_knock = "Contact request"
    type_report = "Report"
    type_default = "Notification"
    update_available_summary = "Fauna update available"
    @staticmethod
    def update_available_body(*, version: str) -> str:
        return f"Version {version} is available. Visit fauna.social to download."
    @staticmethod
    def row_like(*, sender: str) -> str:
        return f"{sender} liked your post"
    @staticmethod
    def row_reply(*, sender: str) -> str:
        return f"{sender} replied to your post"
    @staticmethod
    def row_repost(*, sender: str) -> str:
        return f"{sender} reposted your post"
    @staticmethod
    def row_quote(*, sender: str) -> str:
        return f"{sender} quoted your post"
    @staticmethod
    def row_mention(*, sender: str) -> str:
        return f"{sender} mentioned you"
    @staticmethod
    def row_follow(*, sender: str) -> str:
        return f"{sender} followed you"
    @staticmethod
    def row_interaction(*, sender: str) -> str:
        return f"{sender} interacted with your content"
    @staticmethod
    def row_knock(*, sender: str, message: str) -> str:
        return f"{sender} wants to connect: {message}"
    @staticmethod
    def row_forward_queue_evicted(*, dest: str, cap: str) -> str:
        return f"Forward to {dest} dropped because your forward queue is full. Configured rate: {cap}/hour. Reduce inbound or increase the cap."
    row_family_content_notice = "Filtered content was seen on an account you supervise. Open Family to review."
    row_family_contact_request = "An account you supervise asked to add a contact. Open Family to decide."
    row_family_feed_source_request = "An account you supervise asked to add a source. Open Family to decide."
    row_family_feed_source_approved = "Your guardian approved the source you asked for. Try adding it again."
    row_abuse_report_received = "A report is waiting in the reports queue. Open Admin → Nest to review it."
    @staticmethod
    def row_abuse_report_resolved(*, outcome: str) -> str:
        return f"Your report was reviewed: {outcome}."
    @staticmethod
    def row_security_pending_action_queued(*, action_type: str, action_id: str, execute_after: str) -> str:
        return f"Security: a pending action ({action_type}, #{action_id}) was queued by your account and runs at {execute_after}. If this was not you, cancel it under Settings → Pending actions."
    @staticmethod
    def row_security_action_executed(*, action_type: str) -> str:
        return f"Security: a pending action ({action_type}) was executed on your account. If you did not authorize it, contact your nest administrator."
    @staticmethod
    def row_security_action_cancelled(*, action_type: str, cancelled_by: str) -> str:
        return f"Security: a pending action ({action_type}) on your account was cancelled by {cancelled_by}."
    @staticmethod
    def row_security_action_expired(*, action_type: str, action_id: str) -> str:
        return f"Security: a pending action ({action_type}, #{action_id}) on your account expired without the approvals it needed. Nothing changed."
    @staticmethod
    def row_security_pending_action_against_you(*, by: str, action_type: str, action_id: str, execute_after: str) -> str:
        return f"Security: administrator {by} scheduled {action_type} (#{action_id}) against your account; it runs at {execute_after}. You can cancel it under Settings → Pending actions."
    @staticmethod
    def row_security_admin_action_pending(*, by: str, action_type: str, action_id: str, target: str, execute_after: str, approvals_needed: str) -> str:
        return f"Security: admin {by} scheduled {action_type} (#{action_id}) on {target}; it runs at {execute_after} and still needs {approvals_needed} approval(s). Approve or cancel it under Admin → Users → Pending admin actions."
    @staticmethod
    def row_security_admin_action_cancelled(*, action_type: str, action_id: str, target: str, cancelled_by: str) -> str:
        return f"Security: the pending admin action {action_type} (#{action_id}) on {target} was cancelled by {cancelled_by}."
    @staticmethod
    def row_security_admin_action_expired(*, action_type: str, action_id: str, target: str) -> str:
        return f"Security: the pending admin action {action_type} (#{action_id}) on {target} expired without the approvals it needed. Nothing changed."
    @staticmethod
    def row_security_new_token(*, ip: str) -> str:
        return f"Security: new sign-in from a different IP address ({ip}). If this was not you, change your keys and contact your nest administrator."
    @staticmethod
    def row_security_admin_change(*, change_type: str, target: str) -> str:
        return f"Security: an administrative change ({change_type}) was made to {target}. If you did not request it, contact your nest administrator."
    @staticmethod
    def row_security_recovery_replacement_pending(*, new_key: str, lands_at: str) -> str:
        return f"Security: a recovery key replacement was requested (new key {new_key}) and lands at {lands_at}. If this was not you, veto it within 30 days."
    @staticmethod
    def row_security_recovery_replacement_landed(*, new_key: str) -> str:
        return f"Security: your recovery key was replaced (new key {new_key})."
    @staticmethod
    def row_security_recovery_replacement_cancelled(*, cancelled_by: str) -> str:
        return f"Security: the pending recovery key replacement was cancelled by {cancelled_by}."
    @staticmethod
    def row_security_identity_succeeded(*, new_actor_id: str) -> str:
        return f"Security: this identity was succeeded. Your account moved to a new key ({new_actor_id})."
    @staticmethod
    def row_security_archive_exported(*, ip: str) -> str:
        return f"Security: your full account archive was downloaded from {ip}. If this was not you, change your keys and contact your nest administrator."
    @staticmethod
    def row_security_mailbox_export_downloaded(*, format: str, ip: str) -> str:
        return f"Security: a mailbox export ({format}) was downloaded from {ip}. If this was not you, change your keys and contact your nest administrator."


class _Widget:
    description = "Shows unread message count and quick compose"
    unread_label = "unread"


class _Markdown:
    bold = "Bold"
    italic = "Italic"
    code = "Code"
    link = "Link"
    heading = "Heading"
    list = "List"
    list_item = "List item"
    toggle_markers = "Show/hide markdown markers"


class _Composer:
    quote = "Quote"
    quote_post = "Quote Post"
    new_post = "New Post"


class _Profile:
    title = "Profile"
    following = "Following"
    followers = "Followers"
    no_posts = "No posts"
    posts = "Posts"
    tiers = "Tiers"
    edit = "Edit Profile"
    follow = "Follow"
    copy_id = "Copy ID"
    start_dm = "Message"
    block = "Block"
    blocked = "Blocked"
    unblock = "Unblock"
    request_contact = "Request contact"
    request_contact_sent = "Request sent"
    report = "Report account"
    edit_display_name = "Display name"
    edit_bio = "Bio"
    edit_link_label = "Label"
    edit_link_url = "URL"
    edit_add_link = "Add link"
    edit_remove_link = "Remove"
    edit_avatar = "Avatar image path"
    edit_remove_avatar = "Remove avatar"
    edit_banner = "Banner image path"
    edit_remove_banner = "Remove banner"
    edit_save = "Save"
    edit_cancel = "Cancel"
    private_title = "Only you can see this"
    private_nickname = "Nickname"
    private_notes = "Notes"
    private_labels = "Labels"
    private_label_add = "Add label"
    private_label_remove = "Remove"
    private_save = "Save"
    @staticmethod
    def private_nickname_too_long(*, max: str) -> str:
        return f"Nickname is too long — keep it to {max} characters"
    @staticmethod
    def private_notes_too_long(*, max: str) -> str:
        return f"Notes are too long — keep them under {max} KB"
    private_label_empty = "Type a label before adding it"
    @staticmethod
    def private_label_too_long(*, max: str) -> str:
        return f"Label is too long — keep it to {max} characters"
    @staticmethod
    def private_too_many_labels(*, max: str) -> str:
        return f"This person already has {max} labels — remove one first"
    private_label_history_full = "This person has had too many labels — this one cannot be added"
    @staticmethod
    def private_save_failed(*, reason: str) -> str:
        return f"Could not save your private notes: {reason}"


class _Subscriptions:
    title = "Subscriptions"
    my_tiers = "My Tiers"
    my_subscriptions = "My Subscriptions"
    pending_requests = "Pending Requests"
    subscribers = "Subscribers"
    create_tier = "Create Tier"
    no_tiers = "No tiers yet"
    no_requests = "No pending requests"
    no_subscribers = "No subscribers"
    no_subscriptions = "You have no subscriptions yet"
    tier_name = "Name"
    rank = "Rank"
    description = "Description"
    price_hint = "Price"
    asking_price = "Asking price (sats, optional)"
    payment_url = "Payment Link"
    unsafe_payment_url = "This payment link is unsafe (links must be https). Not opening it."
    auto_approve = "Auto-approve"
    save = "Save"
    cancel = "Cancel"
    edit = "Edit"
    delete = "Delete"
    approve = "Approve"
    reject = "Reject"
    remove = "Remove"
    approving = "Approving — minting keys…"
    subscribe = "Subscribe"
    unsubscribe = "Unsubscribe"
    tier_select_label = "Tier:"
    offers = "Subscription Tiers"
    no_offers = "This creator offers no subscription tiers yet"
    payment_providers = "Payment Providers"
    add_provider = "Add Provider"
    no_providers = "No payment providers yet"
    provider_kind_label = "Provider:"
    provider_tier_label = "Tier:"
    webhook_secret = "Webhook secret"
    webhook_url_label = "Webhook URL:"
    redeem_claim_title = "Redeem a claim code"
    claim_code = "Claim code"
    redeem = "Redeem"
    offer_status_none = "Not subscribed"
    offer_status_pending = "Pending approval"
    offer_status_active = "Subscribed"
    paid = "Paid"
    manual_claims = "Manual Claim Codes"
    mint_claim = "Mint Code"
    no_claims = "No claim codes yet"
    claim_status_unredeemed = "Unredeemed"
    claim_status_redeemed = "Redeemed"
    claim_status_voided = "Voided"
    provider_status_configured = "Configured"
    provider_status_verified = "Verified"
    provider_status_error = "Error"


class _Photo_backup:
    title = "Photo Backup"
    enable = "Enable Photo Backup"
    photo_access_required = "Photo library access required. Grant access in Settings."
    wifi_only = "WiFi Only"
    @staticmethod
    def syncing(*, uploaded: str, total: str) -> str:
        return f"Syncing {uploaded} of {total}..."
    last_sync = "Last Sync"
    @staticmethod
    def pending_count(*, count: str) -> str:
        return f"{count} pending"
    sync_now = "Sync Now"
    back_up_photos = "Back up Photos library"
    photos_access_granted = "Photos access granted"
    auto_upload_desc = "New photos and videos will be automatically uploaded to your nest."
    backup_in_progress = "Backup in progress..."
    backed_up = "Backed up"
    @staticmethod
    def photos_count(*, count: str) -> str:
        return f"{count} photos"
    @staticmethod
    def remaining_count(*, count: str) -> str:
        return f"{count} remaining"
    last_backup = "Last backup"
    notification_starting = "Starting backup..."
    @staticmethod
    def notification_progress(*, uploaded: str, total: str) -> str:
        return f"Backing up photos... {uploaded}/{total}"
    notification_complete = "Backup complete"
    @staticmethod
    def error_prepare_set(*, message: str) -> str:
        return f"Could not prepare the photo library folder: {message}"
    error_wifi_lost = "WiFi lost, sync paused"
    @staticmethod
    def error_upload_item(*, uri: str, message: str) -> str:
        return f"Failed to upload {uri}: {message}"


class _File_sync:
    new_folder = "New Folder"
    info = "Info"
    saf_root_summary = "Synced files"


class _Conflicts:
    no_conflicts = "No sync conflicts."
    resolve = "Resolve"
    local = "Local"


class _File_context_menu:
    share = "Share"
    version_history = "Version history"
    info_unavailable = "File info unavailable"
    devices_none = "Not synced to any device"
    devices_one = "On this device only"
    @staticmethod
    def devices_count(*, count: str) -> str:
        return f"Synced to {count} devices"
    versions_none = "No saved versions"
    versions_one = "1 saved version"
    @staticmethod
    def versions_one_dated(*, date: str) -> str:
        return f"1 saved version ({date})"
    @staticmethod
    def versions_count(*, count: str) -> str:
        return f"{count} saved versions"
    @staticmethod
    def versions_count_dated(*, count: str, date: str) -> str:
        return f"{count} saved versions, latest {date}"
    version_restored = "Version restored."
    version_restore_failed = "Could not restore version."
    @staticmethod
    def version_restore_failed_detail(*, message: str) -> str:
        return f"Could not restore version: {message}"
    share_not_available = "This item can't be shared from here"
    share_open_failed = "Couldn't open Fauna to share this item"
    keep_on_device = "Always keep on this device"
    make_on_demand = "Make available on-demand"


class _ErrorConversations:
    forbidden = "This person isn't accepting new conversations right now. You can send them a contact request instead."
    rate_limited = "This is happening too quickly. Please wait a moment and try again."


class _ErrorProfile:
    handle_taken = "That handle already belongs to someone else on your nest. Choose a different one."
    handle_cooldown = "That handle was released recently and can't be taken yet. Choose a different one, or try again later."


class _ErrorBridges:
    recipient_on_local_domain = "That address is on your own server, so it can't be a list member. Add it as an alias instead."
    forward_target_on_local_domain = "That address is on your own server, so mail can't be forwarded to it. Add it as an alias instead."
    over_quota = "Your storage is full, so this wasn't saved. Delete something to make room, or ask your nest admin for more space."
    address_refused = "None of your connected bridges can reach that address. Check how it is written."
    conversation_store_full = "This bridge has too many messages waiting, so this wasn't sent. Try again once it catches up."
    guardian_approval_required = "This account can only message approved contacts."


class _ErrorNest:
    outdated = "This nest is running an outdated version and must be updated before you can connect."
    schema_mismatch = "This nest's database does not match its software version and must be updated."


class _ErrorFederation:
    peer_nest_outdated = "The other side's nest is running an outdated version and does not support this yet."


class _ErrorProtocol:
    unknown_kind = "This nest does not support that request. It may need to be updated."
    malformed = "The request could not be processed."
    timeout = "The nest took too long to respond. Please try again."
    cancelled = "The request was cancelled."
    internal = "The nest ran into an unexpected error. Please try again."
    encode = "The nest sent a response that could not be read."
    replay_too_large = "There is too much to catch up on at once. Please try again."
    disconnected = "The connection to the nest was lost."


class _ErrorEmail:
    too_large = "This message is too large to send. Remove attachments or shorten it and try again."
    no_handle = "You need to set a handle for your account before you can send email."
    permission_denied = "You can only send email from your own address. Check the account you are sending from."
    rate_limited = "You have reached your sending limit for now. Try again later."


class _ErrorSync:
    device_limit_exceeded = "This account already has as many devices as its plan allows, so this device could not be added. Remove a device you no longer use under Settings → Devices, or ask your admin for a bigger tier."


class _ErrorSend:
    generic = "Something went wrong. Please try again."
    auth_required = "Sign in again to send this message."
    not_supported = "This action is not available for this conversation."
    no_recipients = "There are no recipients to send this message to."
    room_invite_not_permitted = "Only the room's owner and admins can invite people to this room."
    room_remove_not_permitted = "Only the room's owner and admins can remove people from this room."
    room_owner_not_removable = "The room's owner cannot be removed. Ownership has to be handed over first."
    room_policy_not_permitted = "Only the room's owner and admins can change this room's settings."
    room_admins_owner_only = "Only the room's owner can appoint or demote admins."
    room_transfer_owner_only = "Only the room's owner can hand the room over."
    room_transfer_not_a_member = "The room can only be handed over to one of its other members."
    room_transfer_superseded = "The room's settings changed while the hand-over was waiting, so it did not go through. Hand the room over again."
    room_policy_unavailable = "This room has no room settings, so there is nothing to change here."
    room_owner_cannot_leave = "Hand the room over to someone else before you leave — a room always has an owner."
    room_leave_failed = "Leaving this room did not go through. Try again."
    attachment_upload_foreign_failed = "The attachment couldn't be uploaded to this conversation's home server. Try sending it again."
    @staticmethod
    def attachment_missing(*, filename: str) -> str:
        return f"Attach {filename} again — the file is not on this device."


class _Error:
    unexpected = "Something went wrong talking to the nest. Please try again."
    authorization = "You do not have permission to do this."
    rate_limited = "This is happening too quickly. Please wait a moment and try again."
    conversations = _ErrorConversations
    profile = _ErrorProfile
    bridges = _ErrorBridges
    nest = _ErrorNest
    federation = _ErrorFederation
    protocol = _ErrorProtocol
    email = _ErrorEmail
    sync = _ErrorSync
    send = _ErrorSend


class _Errors:
    auth_failed = "Sign-in failed"
    secret_key_invalid = "Secret key must be exactly 64 hexadecimal characters"
    nest_unreachable = "Cannot connect to nest at the specified URL"
    nest_timeout = "Nest did not come online within the expected time"
    snapshot_device_unknown = "This snapshot's owning device is unknown, so its files cannot be downloaded."
    no_identity = "No identity configured"
    not_connected_to_nest = "Not connected to your nest — no active session"
    nostr_nsec_required = "Enter your nsec"
    nostr_bunker_required = "Enter bunker URL"
    feed_not_ready = "Feed isn't ready yet. Try again in a moment."
    photos_access_denied = "Photos access denied. Grant access in System Settings > Privacy > Photos."
    bluesky_bridge_not_available = "Bluesky bridge not available on this nest."
    photo_backup_not_configured = "Photo backup engine not configured. Connect to a nest first."
    calendar_requires_mail = "Calendar requires mail to be enabled"
    no_calendar_selected = "No calendar selected"
    event_load_for_invite_failed = "Could not load the event to invite"
    @staticmethod
    def http_error(*, detail: str) -> str:
        return f"HTTP error: {detail}"
    @staticmethod
    def auth_error(*, detail: str) -> str:
        return f"Authentication error: {detail}"
    @staticmethod
    def api_error(*, status: str, message: str) -> str:
        return f"API error ({status}): {message}"
    @staticmethod
    def decode_error(*, detail: str) -> str:
        return f"Decode error: {detail}"
    @staticmethod
    def websocket_error(*, detail: str) -> str:
        return f"WebSocket error: {detail}"
    @staticmethod
    def could_not_resolve_domain(*, domain: str) -> str:
        return f"Could not resolve domain: {domain}"
    subprotocol_mismatch = "Your app is out of date and can't connect to this nest. Please update to continue."
    @staticmethod
    def nest_identity_changed(*, host: str) -> str:
        return f"The identity of {host} has changed and could no longer be verified. For your safety, the connection was stopped."
    @staticmethod
    def recovery_identity_unreadable(*, detail: str) -> str:
        return f"This session's identity secret is unreadable: {detail}"
    recovery_superseded = "This identity has already been succeeded by another one. Import your new identity to continue."
    recovery_no_escrow = "No sealed copy of your identity secret is stored for this recovery kit, so it cannot recover this account right now."
    recovery_not_registered = "No recovery key is registered for this identity, so there is nothing to recover from."
    recovery_already_succeeded = "This identity has already been succeeded and cannot be recovered again."
    recovery_invalid_nonce = "That recovery request has expired or was already used. Please try again."
    recovery_signature_failed = "That recovery kit was refused — it may have already been replaced."
    recovery_kit_not_current = "This is not your most recently created recovery kit. Enter your newest kit instead."
    recovery_successor_exists = "That identity already has an account on this nest."
    @staticmethod
    def recovery_transport(*, detail: str) -> str:
        return f"Connection error: {detail}"
    @staticmethod
    def recovery_crypto(*, detail: str) -> str:
        return f"A cryptographic step failed: {detail}"
    @staticmethod
    def recovery_malformed(*, detail: str) -> str:
        return f"The nest sent back something unexpected: {detail}"
    recovery_prior_kit_required = "A recovery key is already registered. Enter your current kit to replace it, or use the seed-alone replacement option instead."
    recovery_prior_kit_mismatch = "That kit does not match the one currently registered for this identity."
    @staticmethod
    def recovery_prior_escrow_unreadable(*, reason: str) -> str:
        return f"Your existing recovery data could not be read ({reason}), so replacing it now would destroy it. Try again from a device and connection that can read it."
    @staticmethod
    def recovery_group_ceremony(*, detail: str) -> str:
        return f"Updating your groups failed: {detail}"


class _RegistrarStep:
    title = "Register a new domain"


class _RegistrarContact:
    first_name = "First name"
    last_name = "Last name"
    email = "Email"
    phone = "Phone (+CC.number)"
    address1 = "Street address"
    city = "City"
    state = "State/province"
    postal_code = "Postal code"
    country = "Country"


class _Registrar:
    domain = "Domain name"
    available = "Available"
    unavailable = "Not available — try a different name"
    price_confirm = "I accept this price and authorize the charge on my registrar account. Registration starts immediately when I continue, and I understand this means I give up any right to withdraw from this purchase."
    step = _RegistrarStep
    contact = _RegistrarContact


class _ProvisioningHosted_auth:
    connect = "Sign in at the provider…"
    @staticmethod
    def pending(*, code: str) -> str:
        return f"Finish signing in in your browser — code {code}"
    connected = "Connected"
    @staticmethod
    def failed(*, message: str) -> str:
        return f"Sign-in failed: {message}"


class _ProvisioningBundled:
    name = "Bundled provider (open API)"
    help = "One company that registers your domain, hosts its DNS and rents you the server — you sign up and pay once, there. Paste the address the company gave you, then sign in; it must implement the open Fauna Bundled Provider API (the link above)."
    base_url = "Provider address (https://…)"
    account = "Account"
    location = "Datacenter"


class _ProvisioningCloudflare:
    name = "Cloudflare"
    help = "DNS and domain registration. Create an API token at dash.cloudflare.com → My Profile → API Tokens with Zone DNS and Registrar permissions; find your account ID on the Account home page's API section."
    api_token = "API token"
    zone = "DNS zone"
    account_id = "Account ID"
    registrar_account_contact_note = "Cloudflare uses the contact details on your Cloudflare account for domain registration. Make sure they're set at dash.cloudflare.com → Domain Registration → Contacts before you continue."


class _ProvisioningPorkbun:
    name = "Porkbun"
    help = "Registrar + DNS. Enable API access at porkbun.com → Account → API Access."
    key = "API key"
    secret = "Secret API key"
    domain = "Domain"
    registrar_account_contact_note = "Porkbun uses the contact details on your Porkbun account for domain registration. Make sure they're set at porkbun.com/account/settings before you continue."


class _ProvisioningHetzner:
    name = "Hetzner Cloud"
    help = "VPS and DNS. Create one Read & Write project token at console.hetzner.cloud — the same token manages both your server and your DNS."
    api_token = "Cloud API token"
    location = "Datacenter"


class _ProvisioningNamecheap:
    name = "Namecheap"
    help = "Enable API access at namecheap.com → Profile → Tools → API Access. Namecheap also requires the calling IP address to be allowlisted there — if it isn't, the error message will name the exact address to add."
    api_user = "API user"
    api_key = "API key"
    domain = "Domain"


class _ProvisioningGandi:
    name = "Gandi"
    help = "Create a Personal Access Token at account.gandi.net → Security → Personal Access Tokens."
    personal_access_token = "Personal access token"
    domain = "Domain"


class _ProvisioningDigitalocean:
    name = "DigitalOcean"
    help = "Create an API token at cloud.digitalocean.com → API → Generate New Token (read and write)."
    api_token = "API token"
    location = "Region"


class _ProvisioningVultr:
    name = "Vultr"
    help = "Create an API key at my.vultr.com → Account → API."
    api_key = "API key"
    location = "Region"


class _ProvisioningOvh:
    name = "OVH Cloud"
    help = "Create app credentials at api.ovh.com/createApp, then generate a consumer key via the OVH token endpoint for your app key and secret."
    app_key = "Application key"
    app_secret = "Application secret"
    consumer_key = "Consumer key"
    project = "Project"


class _ProvisioningLinode:
    name = "Linode (Akamai)"
    help = "Create a Personal Access Token at cloud.linode.com → Profile → API Tokens."
    api_token = "API token"
    location = "Region"


class _ProvisioningManaged:
    disabled = "Managed subdomains on fauna.social are coming soon. For now, pick another option."


class _Provisioning:
    verify_credentials = "Verify credentials"
    hosted_auth = _ProvisioningHosted_auth
    bundled = _ProvisioningBundled
    cloudflare = _ProvisioningCloudflare
    porkbun = _ProvisioningPorkbun
    hetzner = _ProvisioningHetzner
    namecheap = _ProvisioningNamecheap
    gandi = _ProvisioningGandi
    digitalocean = _ProvisioningDigitalocean
    vultr = _ProvisioningVultr
    ovh = _ProvisioningOvh
    linode = _ProvisioningLinode
    managed = _ProvisioningManaged


class _Region:
    @staticmethod
    def blocked_notice(*, region: str, authority: str) -> str:
        return f"Not shown in {region} — blocked under the policy of {authority}"
    @staticmethod
    def collapsed_notice(*, region: str, authority: str) -> str:
        return f"Hidden in {region} under the policy of {authority} — select to show"
    reveal_button = "Show"
    section_title = "Region"
    @staticmethod
    def declared(*, region: str) -> str:
        return f"Your region: {region}"
    none_declared = "No region is declared on this device"
    source_storefront = "From your app store region — change it in your store account"
    source_system_region = "From your system region setting — change it in your system settings"
    source_system_locale = "From your system locale — change it in your system settings"
    source_browser_locale = "From your browser language — change it in your browser settings"
    no_policy = "No regional content policy is in force"
    @staticmethod
    def policy_authority(*, region: str, authority: str) -> str:
        return f"{region}: {authority}"
    @staticmethod
    def policy_version(*, sequence: str, issued: str) -> str:
        return f"Version {sequence}, issued {issued}"
    @staticmethod
    def inert_notice(*, version: str) -> str:
        return f"Uses a policy format this app does not understand (version {version}) — nothing is blocked under it"
    malformed_notice = "This policy could not be read — nothing is blocked under it"
    @staticmethod
    def last_checked(*, time: str) -> str:
        return f"Last checked {time}"
    stale_warning = "Could not check for policy updates recently — the policies above stay in force"


class _Connected_apps:
    title = "Connected apps"
    description = "Apps and services that act for you from outside Fauna. Each one can only reach what is listed under it, and you can disconnect any of them at any time."
    @staticmethod
    def verbatim(*, text: str) -> str:
        return f"{text}"
    unnamed = "Unnamed app"
    signer_pending = "Waiting to connect…"
    scope_nostr_sign = "Sign Nostr events with your key"
    scope_mail = "Read and send your mail, and sync your calendar, contacts and files"
    requests_heading = "Requests"
    consent_ends_holder = "This app now uses a new key. Approving ends the access its earlier key was given."
    consent_ends_writer = "This app now signs with a new key. Approving ends its earlier key's permission to write."
    connect_heading = "Connect an app"
    connect_hint = "If an app on another device shows you a code, type it here."
    connect_placeholder = "Code from the app"
    connect_submit = "Connect"
    roster_heading = "Your connected apps"
    empty = "No connected apps yet."
    block = "Never show requests from this app"
    revoke = "Disconnect"
    revoke_confirm = "Disconnect now"
    revoke_cancel = "Keep"
    @staticmethod
    def revoke_prompt(*, name: str) -> str:
        return f"Disconnect {name}? It will no longer be able to act for you."
    @staticmethod
    def publisher(*, domain: str) -> str:
        return f"From {domain}"
    class_remote = "Website or service"
    class_device = "App on a device"
    class_wasm = "Plugin on your nest"
    class_container = "Plugin on your nest"
    class_app_password = "Signed in with an app password"
    class_signer = "Nostr signer app"
    class_oauth = "Signed-in app"
    not_connected = "Not connected right now"
    @staticmethod
    def created(*, time: str) -> str:
        return f"Added {time}"
    @staticmethod
    def last_used(*, time: str) -> str:
        return f"Last used {time}"
    never_used = "Never used"
    @staticmethod
    def lasts_until(*, time: str) -> str:
        return f"Until {time}"
    open_ended = "Until you disconnect it"
    @staticmethod
    def error_refresh(*, message: str) -> str:
        return f"Failed to load connected apps: {message}"
    @staticmethod
    def error_revoke(*, message: str) -> str:
        return f"Failed to disconnect the app: {message}"
    @staticmethod
    def error_resolve(*, message: str) -> str:
        return f"Failed to answer the request: {message}"
    error_request_gone = "That request is no longer waiting — it was answered or it expired."
    error_code_expired = "That code has expired — ask the app for a new one."
    error_handoff_expired = "That link has expired or was already used — start again from the app."
    @staticmethod
    def error_block(*, message: str) -> str:
        return f"Failed to block the app: {message}"
    @staticmethod
    def error_secret(*, message: str) -> str:
        return f"Could not read the secret: {message}"
    blocked_heading = "Blocked apps"
    blocked_hint = "Requests from these apps are never shown to you."
    @staticmethod
    def blocked_since(*, time: str) -> str:
        return f"Blocked {time}"
    unblock = "Allow requests again"


class _Share_link:
    button = "Share a link"
    @staticmethod
    def create_title(*, name: str) -> str:
        return f"Share a link to {name}"
    create_body = "Anyone with the link can open this file until it expires or you revoke it."
    key_notice = "This file is private, so the link carries the key that unlocks it. Anyone holding the link can open the file — and wherever you paste it, anyone who can read that place can open it too."
    expiry_label = "Link expires after"
    expiry_1d = "1 day"
    expiry_7d = "7 days"
    expiry_30d = "30 days"
    expiry_1y = "1 year"
    create = "Create link"
    creating = "Creating link…"
    cancel = "Cancel"
    close = "Close"
    copy = "Copy link"
    list_button = "Shared links"
    list_title = "Your shared links"
    list_loading = "Loading your shared links…"
    empty = "You haven't shared any links yet."
    @staticmethod
    def expires(*, date: str) -> str:
        return f"Expires {date}"
    state_active = "Active"
    state_expired = "Expired"
    state_revoked = "Revoked"
    revoke = "Revoke"
    revoke_confirm_title = "Revoke this link?"
    @staticmethod
    def revoke_confirm_body(*, name: str) -> str:
        return f"The link to {name} stops working for everyone. This cannot be undone — you can make a new link at any time."
    revoke_confirm = "Revoke link"
    @staticmethod
    def error_create(*, message: str) -> str:
        return f"Couldn't create the link: {message}"
    @staticmethod
    def error_list(*, message: str) -> str:
        return f"Couldn't load your shared links: {message}"
    @staticmethod
    def error_revoke(*, message: str) -> str:
        return f"Couldn't revoke the link: {message}"


class _Share_viewer:
    title = "A file shared with Fauna"
    generic_body = "Someone shared a file with you through Fauna. Open the complete link you were sent to see it."
    loading = "Opening the file…"
    download = "Download"
    keep_note = "A copy you download stays with you, even after the link stops working."
    gone = "This link has expired or was revoked."
    withheld = "This file is not available for legal reasons."
    not_found = "This link was not found. It may be mistyped, or the file is no longer stored."
    damaged = "This link could not be opened. Check that you copied the whole link."
    unavailable = "The file could not be loaded right now. Try again later."


class S:
    common = _Common
    time = _Time
    size = _Size
    tips = _Tips
    onboarding = _Onboarding
    launch = _Launch
    credential_store = _Credential_store
    tui_unlock = _Tui_unlock
    tui_nav_hints = _Tui_nav_hints
    tui_settings = _Tui_settings
    feed = _Feed
    conversations = _Conversations
    contacts = _Contacts
    events = _Events
    groups = _Groups
    atproto_settings = _Atproto_settings
    critical_alerts = _Critical_alerts
    bridges = _Bridges
    media = _Media
    backups = _Backups
    folders = _Folders
    devices = _Devices
    sessions = _Sessions
    settings = _Settings
    nests = _Nests
    linked_nests = _Linked_nests
    logs = _Logs
    mail_settings = _Mail_settings
    web_settings = _Web_settings
    web_publish = _Web_publish
    mail_aliases = _Mail_aliases
    mail_spam = _Mail_spam
    mail_export = _Mail_export
    mail_lists = _Mail_lists
    mail_import = _Mail_import
    archive_import = _Archive_import
    search_page = _Search_page
    nostr = _Nostr
    status = _Status
    admin = _Admin
    setup = _Setup
    c2pa = _C2pa
    p2p = _P2p
    moderation = _Moderation
    muted_words = _Muted_words
    personalization = _Personalization
    labeler_catalog = _Labeler_catalog
    task_delegation = _Task_delegation
    features = _Features
    family = _Family
    navigation = _Navigation
    notifications = _Notifications
    widget = _Widget
    markdown = _Markdown
    composer = _Composer
    profile = _Profile
    subscriptions = _Subscriptions
    photo_backup = _Photo_backup
    file_sync = _File_sync
    conflicts = _Conflicts
    file_context_menu = _File_context_menu
    error = _Error
    errors = _Errors
    registrar = _Registrar
    provisioning = _Provisioning
    region = _Region
    connected_apps = _Connected_apps
    share_link = _Share_link
    share_viewer = _Share_viewer
