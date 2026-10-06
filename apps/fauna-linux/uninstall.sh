#!/bin/bash
set -e

# Fauna Linux uninstaller — thin alias for `install.sh --uninstall` so there
# is exactly ONE uninstall implementation (this file previously duplicated it
# and drifted). PREFIX / sudo semantics are install.sh's.
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install.sh" --uninstall "$@"
