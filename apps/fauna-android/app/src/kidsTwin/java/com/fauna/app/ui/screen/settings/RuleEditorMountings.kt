package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.Composable
import com.fauna.ffi.FfiFamilyWardInfo

/**
 * The shared rule editor's mountings — **the excised half** (the kids build
 * type's twin of `src/noKids/…/RuleEditorMountings.kt`). `rule-editor` rides
 * `store-safe`'s tail, so this flavor's bindings carry no `FfiRuleEditor`; a
 * kids account is never an admin or a guardian, so neither mounting has anyone
 * to serve here and both paint nothing. The member's side of the nest rules
 * (the placeholder, the rules surface) is in `src/main/` and binds this flavor
 * like every other.
 */
@Composable
fun AdminNestRulesSection() = Unit

/** Excised — see the file comment. */
@Suppress("UNUSED_PARAMETER")
@Composable
fun FamilyRulesSection(ward: FfiFamilyWardInfo, onSaved: () -> Unit) = Unit
