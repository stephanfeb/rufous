"""Tokenisation helpers."""
import re

_WORD = re.compile(r"[A-Za-z0-9']+")


def words(s):
    return _WORD.findall(s)


def ngrams(tokens, n):
    """All contiguous n-grams of a token list, as tuples, in order."""
    # BUG: drops the last n-gram
    return [tuple(tokens[i:i + n]) for i in range(len(tokens) - n)]


def unique_in_order(tokens):
    seen = set()
    out = []
    for t in tokens:
        if t not in seen:
            seen.add(t)
            out.append(t)
    return out


def shingles(s, k):
    s = s.replace(" ", "_")
    return [s[i:i + k] for i in range(max(0, len(s) - k + 1))]


def split_camel(s):
    return re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])", s)
