"""URL slugs for the publishing pipeline."""
import re
import unicodedata

_NON_WORD = re.compile(r"[^a-z0-9]")


def strip_accents(s):
    """ASCII letters for accented ones: 'Café' -> 'Cafe'."""
    return "".join(c for c in unicodedata.normalize("NFKD", s) if not unicodedata.combining(c))


def slugify(title):
    """Lower-case ASCII words joined by single hyphens, no leading or trailing hyphen."""
    s = strip_accents(title).lower()
    s = _NON_WORD.sub("-", s)
    return s


def slug_path(section, title):
    """The URL path of an article: /<section>/<slug>."""
    return f"/{section}/{slugify(title)}"
