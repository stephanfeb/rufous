"""Whitespace and case normalisation."""
import re

_WS = re.compile(r" +")


def collapse_spaces(s):
    """Collapse runs of spaces and tabs into one space and strip the ends."""
    # BUG: the pattern only matches spaces, so tabs survive
    return _WS.sub(" ", s).strip()


def to_snake(s):
    out = []
    for i, ch in enumerate(s):
        if ch.isupper() and i > 0 and not s[i - 1].isupper():
            out.append("_")
        out.append(ch.lower())
    return "".join(out).replace(" ", "_").replace("-", "_")


def strip_accents(s):
    import unicodedata
    return "".join(c for c in unicodedata.normalize("NFD", s) if unicodedata.category(c) != "Mn")


def normalise_newlines(s):
    return s.replace("\r\n", "\n").replace("\r", "\n")


def truncate(s, n, ellipsis="..."):
    if len(s) <= n:
        return s
    if n <= len(ellipsis):
        return s[:n]
    return s[: n - len(ellipsis)] + ellipsis
