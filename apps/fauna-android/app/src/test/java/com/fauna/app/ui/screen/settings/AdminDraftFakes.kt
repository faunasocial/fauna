package com.fauna.app.ui.screen.settings

import com.fauna.ffi.FfiAdminUpdateChannelView
import com.fauna.ffi.FfiAdminUserRowControl
import com.fauna.ffi.FfiAdminUserRowControls
import com.fauna.ffi.FfiAdminWebAppOriginView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

/**
 * FFI-free stand-ins for the shared admin draft-validity predicates
 * (`fauna_client_admin::drafts`, `admin_policy::knobs_draft_possibility` /
 * `microsoft_draft_possibility`, `CaldavPolicyMachine::port_draft_possibility`,
 * `ForwarderMachine::create_draft_possibility`; admin.md § Errors & edge cases,
 * the form bullet), so the Robolectric screen tests stay off the native path.
 * Each answers with the shared reason KEY, so a test can assert the painted
 * sentence; the rule itself is Rust-tested, these only pin the wiring.
 */
internal object AdminDraftFakes {
    fun blocked(key: String): Possibility = Possibility.Blocked(LocalizedText(key, emptyMap()))

    private fun whole(s: String) = s.trim().toULongOrNull() != null

    val knobs: (List<String>, List<String>, List<String>) -> Possibility = { counts, u64, lists ->
        when {
            counts.any { it.trim().toUIntOrNull() == null } -> blocked("admin.mail_page.knob_invalid")
            u64.any { !whole(it) } -> blocked("admin.mail_page.knob_invalid")
            lists.any { l -> l.lines().map(String::trim).filter(String::isNotEmpty).any { !whole(it) } } ->
                blocked("admin.mail_page.knob_list_invalid")
            else -> Possibility.Possible
        }
    }

    /** The pair check, reduced to "a client id needs a tenant and vice versa". */
    val microsoft: (String, String) -> Possibility = { id, tenant ->
        if (id.isBlank() != tenant.isBlank()) blocked("admin.mail_page.microsoft_pair_invalid") else Possibility.Possible
    }

    val port: (String) -> Possibility = { raw ->
        val p = raw.trim().toIntOrNull()
        if (p != null && p in 1..65535) Possibility.Possible else blocked("admin.nest_page.serving_port_invalid")
    }

    val caldavPort: (String) -> Possibility = { raw ->
        if (port(raw) is Possibility.Possible) Possibility.Possible else blocked("admin.calendar_page.caldav_port_invalid")
    }

    val tierCaps: (List<String>) -> Possibility = { caps ->
        if (caps.size == 5 && caps.all(::whole)) Possibility.Possible
        else blocked("admin.settings_page.save_tier_error_invalid_cap")
    }

    val tierCreate: (String, List<String>) -> Possibility = { name, caps ->
        when {
            name.isBlank() -> blocked("admin.settings_page.add_tier_error_empty_name")
            !(caps.size == 5 && caps.all(::whole)) -> blocked("admin.settings_page.add_tier_error_invalid_cap")
            else -> Possibility.Possible
        }
    }

    val membershipSave: (String) -> Possibility = { tier ->
        if (tier.isBlank()) blocked("admin.settings_page.save_membership_tier_error_no_tier") else Possibility.Possible
    }

    val registrationCeiling: (String) -> Possibility = { raw ->
        if (raw.isBlank() || whole(raw)) Possibility.Possible else blocked("admin.users_page.max_free_users_invalid")
    }

    val admitActor: (String) -> Possibility = { raw ->
        val t = raw.trim()
        if (t.length == 64 && t.all { it.isDigit() || it.lowercaseChar() in 'a'..'f' }) Possibility.Possible
        else blocked("admin.users_page.admit_actor_hint")
    }

    val forwarderCreate: (String, String, String) -> Possibility = { d, p, t ->
        if (listOf(d, p, t).any(String::isBlank)) blocked("admin.aliases_page.forwarder_draft_incomplete")
        else Possibility.Possible
    }

    /** `AdminUserRowControls::*_possibility`: possible where the row offers
     *  the control, absent otherwise. */
    val rowControl: (FfiAdminUserRowControls, FfiAdminUserRowControl) -> Possibility = { c, k ->
        val offered = when (k) {
            FfiAdminUserRowControl.SUSPEND -> c.`suspend`
            FfiAdminUserRowControl.EVICT -> c.evict
            FfiAdminUserRowControl.RESTORE -> c.restore
            FfiAdminUserRowControl.MAKE_ADMIN -> c.makeAdmin
            FfiAdminUserRowControl.TAKE_OVER -> c.takeOver
        }
        if (offered) Possibility.Possible else Possibility.Absent
    }

    private val stillLoading = blocked("common.still_loading")

    /** `web_app_origin_set_possibility` / `update_channel_set_possibility`:
     *  blocked still-loading before the read, by the status sentence where
     *  the choice cannot be set. */
    val nestSet = AdminNestSetAnswers(
        webAppOrigin = { v, _ ->
            when {
                v == null -> stillLoading
                v.canSet -> Possibility.Possible
                else -> Possibility.Blocked(v.status)
            }
        },
        updateChannel = { v: FfiAdminUpdateChannelView? ->
            when {
                v == null -> stillLoading
                v.canSet -> Possibility.Possible
                else -> Possibility.Blocked(v.status)
            }
        },
    )
}
