"""Text formatting helpers for the report writer."""
import re

_WS = re.compile(r"\s+")


def collapse_spaces(s):
    """Runs of whitespace become one space; the ends are trimmed."""
    return _WS.sub(" ", s).strip()


def truncate(s, n, ellipsis="..."):
    """At most n characters, ending in the ellipsis when cut."""
    if len(s) <= n:
        return s
    return s[: max(0, n - len(ellipsis))] + ellipsis


def pad(s, width, fill=" "):
    """Right-pad to width."""
    return s + fill * max(0, width - len(s))
