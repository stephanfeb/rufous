"""Rendering helpers."""


def pad_right(s, width, fill=" "):
    return s + fill * max(0, width - len(s))


def pad_left(s, width, fill=" "):
    return fill * max(0, width - len(s)) + s


def table(rows, headers):
    widths = [max(len(str(h)), *(len(str(r[i])) for r in rows)) if rows else len(str(h)) for i, h in enumerate(headers)]
    line = lambda cells: "| " + " | ".join(pad_right(str(c), w) for c, w in zip(cells, widths)) + " |"
    sep = "|" + "|".join("-" * (w + 2) for w in widths) + "|"
    return "\n".join([line(headers), sep] + [line(r) for r in rows])


def percent(x, digits=1):
    return f"{x * 100:.{digits}f}%"


def pluralise(n, word):
    return f"{n} {word}" if n == 1 else f"{n} {word}s"
