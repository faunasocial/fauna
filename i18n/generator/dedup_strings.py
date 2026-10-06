"""Deduplicate i18n strings by removing keys that shadow common.* entries,
consolidating exact duplicates into common.*, and standardizing near-duplicates.

Outputs:
  1. A cleaned en.yaml with duplicates removed
  2. A key_mapping.json mapping removed_key -> canonical_key (for code reference updates)
"""

from __future__ import annotations

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from generate import load_strings, flatten, REPO_ROOT, STRINGS_FILE


def normalize_for_comparison(value: str) -> str:
    """Normalize for near-duplicate comparison."""
    v = value.strip().rstrip(".:!?…").strip()
    v = re.sub(r"\s+", " ", v)
    return v.lower()


def pick_canonical_key(keys: list[str]) -> str:
    """Pick the best canonical key from a group of duplicates.
    Prefer common.*, then shortest path, then alphabetical.
    """
    common_keys = [k for k in keys if k.startswith("common.")]
    if common_keys:
        return common_keys[0]
    # Prefer shorter keys (more general)
    return sorted(keys, key=lambda k: (len(k.split(".")), k))[0]


def compute_common_key(value: str) -> str:
    """Generate a common.* key name from a value."""
    # Convert to snake_case
    v = value.strip().rstrip(".:!?…").strip()
    v = re.sub(r"[^a-zA-Z0-9\s]", "", v)
    v = re.sub(r"\s+", "_", v).lower()
    return f"common.{v}"


def set_nested(tree: dict, dotted_key: str, value: str) -> None:
    """Set a value in a nested dict using a dot-separated key."""
    parts = dotted_key.split(".")
    d = tree
    for part in parts[:-1]:
        if part not in d:
            d[part] = {}
        d = d[part]
    d[parts[-1]] = value


def delete_nested(tree: dict, dotted_key: str) -> bool:
    """Delete a key from a nested dict. Returns True if deleted.
    Also cleans up empty parent dicts.
    """
    parts = dotted_key.split(".")
    # Build path of (dict, key) pairs
    path = []
    d = tree
    for part in parts[:-1]:
        if part not in d or not isinstance(d[part], dict):
            return False
        path.append((d, part))
        d = d[part]
    if parts[-1] not in d:
        return False
    del d[parts[-1]]
    # Clean up empty parents
    for parent_dict, key in reversed(path):
        if isinstance(parent_dict[key], dict) and len(parent_dict[key]) == 0:
            del parent_dict[key]
    return True


def main():
    tree = load_strings()
    flat = flatten(tree)

    # Track the mapping of removed_key -> canonical_key
    key_mapping: dict[str, str] = {}

    # =========================================================================
    # Phase 1: Standardize near-duplicate values
    # =========================================================================
    # These are value fixes only - no key removal needed
    value_fixes = {
        # Standardize capitalization (Title Case for UI labels)
        "admin.dashboard.load_more": "Load More",
        "search_page.load_more": "Load More",
        "bridges.mark_all_read": "Mark All Read",
        "feed.rule_types.body_contains": "Body contains",
        "status.email_filters.body_contains": "Body contains",
        "media.no_folders_title": "No Folders",
        "file_sync.no_folders": "No Folders",
        "errors.no_folder_selected": "No Folder Selected",
        "onboarding.byo_setup.docker_commands": "Docker commands",
        "setup.byo.docker_commands": "Docker commands",
        "status.self_host.get_started": "Get Started",
        "onboarding.complete.save_recovery_key": "Save Your Recovery Key",
        "setup.complete.save_recovery_key": "Save Your Recovery Key",
        "onboarding.domain.own_domain": "Own Domain",
        "onboarding.provision_mode.own_domain": "Own Domain",
        "setup.server.provision_server": "Provision Server",
        "onboarding.provision.title": "Provisioning Your Nest",
        "onboarding.provision_status.title": "Provisioning Your Nest",
        "setup.provision.title": "Provisioning Your Nest",
        "setup.server.comparison": "Server Comparison",
        "onboarding.server.comparison": "Server Comparison",
        "setup.complete.setup_sync": "Set Up Sync",
        "onboarding.sync_setup.setup_sync": "Set Up Sync",
        "setup.complete.sync_title": "Sync Your Files",
        "onboarding.sync_setup.title": "Sync Your Files",
        "onboarding.welcome.join_nest": "Join a Nest",
        "onboarding.join.title": "Join a Nest",
        "devices.sync_daemon.delete_folder": "Delete Folder",
        "file_sync.delete_folder": "Delete Folder",
        "devices.sync_daemon.exclude_paths": "Exclude Paths (comma-separated)",
        "devices.sync_daemon.include_paths": "Include Paths (comma-separated)",
        "devices.sync_daemon.rescan_interval": "Rescan Interval",
        "events.remove_reminder": "Remove Reminder",
        "settings.allow_knocks": "Allow Knocks",
        "status.inbox_privacy.allow_knock": "Allow Knocks",
        "settings.privacy_page.inbox_mode": "Inbox Mode",
        "settings.inbox_mode": "Inbox Mode",
        # Standardize with period
        "feed.post.no_posts_empty": "No posts yet.",
        "media.no_files_yet": "No files yet.",
        "backups.no_files_in_snapshot": "No files in this snapshot.",
        # Remove trailing colon from "Status:" (it's just "Status")
        "p2p.registration.tunnel_status": "Status",
        # Standardize "available" to lowercase (it's a status indicator, not a heading)
        "settings.update_available": "Available",
        # Remove "Domain:" → "Domain" (colon is formatting, not content)
        "onboarding.complete.domain_label": "Domain",
    }

    for key, new_value in value_fixes.items():
        if key in flat:
            set_nested(tree, key, new_value)
            flat[key] = new_value

    # =========================================================================
    # Phase 2: Remove exact-duplicate pairs (same key, same value, one is redundant)
    # =========================================================================
    # These are keys where two keys have the SAME value and one is clearly
    # a duplicate of the other (within the same section)
    redundant_keys = [
        # conversations: new_tooltip and new_conversation are identical
        "conversations.list.new_tooltip",  # keep conversations.list.new_conversation
        # conversations: new_dialog_title same as new_conversation
        "conversations.compose.new_dialog_title",  # keep conversations.list.new_conversation
        # conversations: message appears 4 times
        "conversations.compose.message_header",  # keep conversations.compose.message_placeholder
        "conversations.compose.message_section",  # keep conversations.compose.message_placeholder
        # groups: type_message and message_placeholder are identical
        "groups.type_message",  # keep groups.message_placeholder
        # events: events_list and title both say "Events"
        "events.events_list",  # keep events.title
        # events: open duplicates mode_open
        "events.open",  # keep events.mode_open
        # contacts: contacts and contacts_list.title both say "Contacts"
        "contacts.contacts",  # keep contacts.title
        "contacts.contacts_list.title",  # keep contacts.title
        # onboarding: two "Starting..." entries
        "onboarding.provision_status.starting",  # keep onboarding.provision.starting
        # media: no_files and no_files_yet both say "No files yet."
        "media.no_files",  # keep media.no_files_yet (after value fix)
        # settings: danger_zone duplicates account_page.danger_zone
        "settings.danger_zone",  # keep settings.account_page.danger_zone
        # settings: encryption and encryption_page.title both say "Encryption"
        "settings.encryption",  # keep settings.encryption_page.title
        # settings: photo_backup and photo_backup_page.title
        "settings.photo_backup",  # keep settings.photo_backup_page.title
        # settings: identity duplicates account_page.identity
        "settings.identity",  # keep settings.account_page.identity
    ]

    for key in redundant_keys:
        if key in flat:
            # Find the sibling key with same value
            value = flat[key]
            siblings = [k for k, v in flat.items() if v == value and k != key]
            if siblings:
                canonical = pick_canonical_key(siblings)
                key_mapping[key] = canonical
                delete_nested(tree, key)
                del flat[key]

    # =========================================================================
    # Phase 3: Remove common.* shadows
    # =========================================================================
    common_entries = {k: v for k, v in flat.items() if k.startswith("common.")}
    common_by_value: dict[str, str] = {}
    for k, v in common_entries.items():
        if v not in common_by_value:  # first common key wins
            common_by_value[v] = k

    shadows_to_remove = []
    for key, value in list(flat.items()):
        if key.startswith("common."):
            continue
        if value in common_by_value:
            shadows_to_remove.append((key, common_by_value[value]))

    for shadow_key, common_key in shadows_to_remove:
        key_mapping[shadow_key] = common_key
        delete_nested(tree, shadow_key)
        del flat[shadow_key]

    # =========================================================================
    # Phase 4: Add new common.* entries for remaining exact duplicates
    # =========================================================================
    # Re-scan for duplicates after shadow removal
    by_value: dict[str, list[str]] = defaultdict(list)
    for key, value in flat.items():
        if "{" not in value:  # skip parameterized
            by_value[value].append(key)

    for value, keys in by_value.items():
        if len(keys) < 3:
            continue
        # Check if there's already a common.* key
        has_common = any(k.startswith("common.") for k in keys)
        if has_common:
            continue  # Already handled by shadow removal

        # Add a new common.* entry
        common_key = compute_common_key(value)
        # Avoid collisions
        if common_key in flat:
            continue

        set_nested(tree, common_key, value)
        flat[common_key] = value

        # Remove all but keep mapping
        for key in keys:
            key_mapping[key] = common_key
            delete_nested(tree, key)
            del flat[key]

    # =========================================================================
    # Phase 5: Handle remaining 2-key exact duplicates
    # =========================================================================
    by_value2: dict[str, list[str]] = defaultdict(list)
    for key, value in flat.items():
        if "{" not in value:
            by_value2[value].append(key)

    for value, keys in by_value2.items():
        if len(keys) != 2:
            continue
        # For 2-key duplicates, keep the one with the shorter/more canonical path
        canonical = pick_canonical_key(keys)
        for key in keys:
            if key != canonical:
                key_mapping[key] = canonical
                delete_nested(tree, key)
                del flat[key]

    # =========================================================================
    # Output
    # =========================================================================
    import yaml

    class _StringDumper(yaml.SafeDumper):
        pass

    # Make yaml.dump preserve string formatting
    def str_representer(dumper, data):
        if "\n" in data:
            return dumper.represent_scalar("tag:yaml.org,2002:str", data, style="|")
        if any(c in data for c in ":#{}[]&*?|>!%@`"):
            return dumper.represent_scalar("tag:yaml.org,2002:str", data, style="'")
        return dumper.represent_scalar("tag:yaml.org,2002:str", data)

    _StringDumper.add_representer(str, str_representer)

    output = yaml.dump(tree, Dumper=_StringDumper, default_flow_style=False,
                       allow_unicode=True, sort_keys=False, width=120)

    STRINGS_FILE.write_text(output, encoding="utf-8")
    print(f"Wrote deduplicated en.yaml ({len(flat)} keys)")

    mapping_file = REPO_ROOT / "i18n" / "generator" / "key_mapping.json"
    mapping_file.write_text(json.dumps(key_mapping, indent=2, sort_keys=True))
    print(f"Wrote key mapping ({len(key_mapping)} removed keys) to {mapping_file}")

    # Print summary
    print(f"\nRemoved {len(key_mapping)} duplicate keys")
    print(f"Top targets: {', '.join(k.split('.')[0] for k in list(key_mapping.keys())[:20])}")


if __name__ == "__main__":
    main()
