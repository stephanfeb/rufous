"""Duration strings for the scheduler config: '2h', '45m', '1h30m'."""
import re

_PATTERN = re.compile(r"(?:(\d+)h)?(?:(\d+)m)?")


def parse_duration(text):
    """Minutes for a duration string, or None when the string is not a duration."""
    m = _PATTERN.fullmatch(text.strip())
    if not m or not any(m.groups()):
        return None
    hours, minutes = (int(g) if g else 0 for g in m.groups())
    return hours * 60 + minutes


def format_duration(minutes):
    """The shortest string parse_duration reads back to `minutes`."""
    h, m = divmod(int(minutes), 60)
    return (f"{h}h" if h else "") + (f"{m}m" if m or not h else "")
