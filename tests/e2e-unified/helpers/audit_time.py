"""Reading an audit timestamp an app painted.

`value-formatting.md` § Absolute local timestamp display: an audit/technical
time (a mail credential's created-at, a bridge's first-seen and approved-at)
renders in the fixed local form ``YYYY-MM-DD HH:MM`` — `format_unix_local` on
the native Rust apps, its JS twin `localTimestamp` on web.
"""

import datetime

#: The fixed form's `strptime` pattern.
AUDIT_TIME_FORMAT = "%Y-%m-%d %H:%M"


def audit_time_near(text: str, made: datetime.datetime, within_s: float = 600.0) -> bool:
    """``text`` is the fixed local audit timestamp and lies within ``within_s``
    of ``made`` — a fresh stamp, never a placeholder or the epoch."""
    try:
        when = datetime.datetime.strptime(text, AUDIT_TIME_FORMAT)
    except ValueError:
        return False
    return abs((when - made).total_seconds()) < within_s
