"""Small statistics over token streams."""
from collections import Counter


def frequencies(tokens):
    return dict(Counter(tokens))


def top_k(tokens, k):
    c = Counter(tokens)
    return [t for t, _n in sorted(c.items(), key=lambda kv: (-kv[1], kv[0]))[:k]]


def median(values):
    """Median of a non-empty list of numbers (mean of the two middle values when even)."""
    # BUG: even length returns the upper middle value instead of the mean
    v = sorted(values)
    n = len(v)
    if n % 2 == 1:
        return v[n // 2]
    return v[n // 2]


def mean(values):
    return sum(values) / len(values)


def type_token_ratio(tokens):
    if not tokens:
        return 0.0
    return len(set(tokens)) / len(tokens)
