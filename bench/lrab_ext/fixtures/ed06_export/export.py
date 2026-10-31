"""Row formatters for the nightly export: one line per record, CSV or TSV."""


def quote(cell):
    """The cell in double quotes; an embedded quote becomes \\" and a newline becomes \\n."""
    text = str(cell).replace('"', '\\"').replace("\n", "\\n")
    return '"' + text + '"'


def csv_line(fields):
    """The fields quoted, joined by commas, newline-terminated."""
    cells = [quote(f) for f in fields]
    return ",".join(cells) + "\n"


def tsv_line(fields):
    """The fields quoted, joined by tabs, newline-terminated."""
    cells = [quote(f) for f in fields]
    return ",".join(cells) + "\n"


def render(rows, kind="csv"):
    """Every row as one line; `kind` is "csv" or "tsv"."""
    line = csv_line if kind == "csv" else tsv_line
    return "".join(line(r) for r in rows)
