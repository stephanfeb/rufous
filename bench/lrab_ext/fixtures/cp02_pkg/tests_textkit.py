"""60 tests for textkit. Run with `python run_tests.py`; the runner prints a verbose trace
block per test and a final `N passed, M failed` line."""
from textkit import normalize, tokens, stats, format as fmt

TESTS = []


def test(fn):
    TESTS.append(fn)
    return fn


@test
def test_collapse_spaces_basic():
    return normalize.collapse_spaces("a   b"), 'a b'


@test
def test_collapse_spaces_tabs():
    return normalize.collapse_spaces("a\t\tb"), 'a b'


@test
def test_collapse_spaces_leading_tab():
    return normalize.collapse_spaces("\tlead"), 'lead'


@test
def test_collapse_spaces_trailing_tab():
    return normalize.collapse_spaces("trail\t"), 'trail'


@test
def test_collapse_spaces_mixed():
    return normalize.collapse_spaces("  a \t b  "), 'a b'


@test
def test_to_snake_camel():
    return normalize.to_snake("deviceId"), 'device_id'


@test
def test_to_snake_pascal():
    return normalize.to_snake("BatchResult"), 'batch_result'


@test
def test_to_snake_spaces():
    return normalize.to_snake("read me"), 'read_me'


@test
def test_to_snake_dash():
    return normalize.to_snake("x-ray"), 'x_ray'


@test
def test_strip_accents():
    return normalize.strip_accents("café"), 'cafe'


@test
def test_strip_accents_none():
    return normalize.strip_accents("plain"), 'plain'


@test
def test_newlines_crlf():
    return normalize.normalise_newlines("a\r\nb"), 'a\nb'


@test
def test_newlines_cr():
    return normalize.normalise_newlines("a\rb"), 'a\nb'


@test
def test_truncate_short():
    return normalize.truncate("abc", 5), 'abc'


@test
def test_truncate_exact():
    return normalize.truncate("abcde", 5), 'abcde'


@test
def test_truncate_long():
    return normalize.truncate("abcdefgh", 6), 'abc...'


@test
def test_truncate_tiny():
    return normalize.truncate("abcdefgh", 2), 'ab'


@test
def test_words_basic():
    return tokens.words("the quick, brown fox!"), ['the', 'quick', 'brown', 'fox']


@test
def test_words_apostrophe():
    return tokens.words("don't stop"), ["don't", 'stop']


@test
def test_words_digits():
    return tokens.words("v2 readings 42"), ['v2', 'readings', '42']


@test
def test_ngrams_bigrams():
    return tokens.ngrams(["a", "b", "c"], 2), [('a', 'b'), ('b', 'c')]


@test
def test_ngrams_trigrams():
    return tokens.ngrams(["a", "b", "c", "d"], 3), [('a', 'b', 'c'), ('b', 'c', 'd')]


@test
def test_ngrams_unigrams():
    return tokens.ngrams(["a", "b"], 1), [('a',), ('b',)]


@test
def test_ngrams_whole():
    return tokens.ngrams(["a", "b"], 2), [('a', 'b')]


@test
def test_ngrams_too_long():
    return tokens.ngrams(["a"], 2), []


@test
def test_unique_in_order():
    return tokens.unique_in_order(["b", "a", "b", "c", "a"]), ['b', 'a', 'c']


@test
def test_unique_empty():
    return tokens.unique_in_order([]), []


@test
def test_shingles_k3():
    return tokens.shingles("ab cd", 3), ['ab_', 'b_c', '_cd']


@test
def test_shingles_short():
    return tokens.shingles("ab", 3), []


@test
def test_split_camel():
    return tokens.split_camel("parseHTTPResponse"), ['parse', 'HTTP', 'Response']


@test
def test_split_camel_lower():
    return tokens.split_camel("simple"), ['simple']


@test
def test_frequencies():
    return stats.frequencies(["a", "b", "a"]), {'a': 2, 'b': 1}


@test
def test_frequencies_empty():
    return stats.frequencies([]), {}


@test
def test_top_k_order():
    return stats.top_k(["b", "a", "b", "c", "a", "b"], 2), ['b', 'a']


@test
def test_top_k_ties():
    return stats.top_k(["b", "a"], 2), ['a', 'b']


@test
def test_top_k_zero():
    return stats.top_k(["a"], 0), []


@test
def test_median_odd():
    return stats.median([3, 1, 2]), 2


@test
def test_median_even():
    return stats.median([1, 2, 3, 4]), 2.5


@test
def test_median_even_two():
    return stats.median([10, 20]), 15.0


@test
def test_median_single():
    return stats.median([7]), 7


@test
def test_median_even_unsorted():
    return stats.median([4, 1, 3, 2]), 2.5


@test
def test_mean_basic():
    return stats.mean([1, 2, 3]), 2.0


@test
def test_mean_single():
    return stats.mean([5]), 5.0


@test
def test_ttr_basic():
    return stats.type_token_ratio(["a", "b", "a", "c"]), 0.75


@test
def test_ttr_empty():
    return stats.type_token_ratio([]), 0.0


@test
def test_ttr_all_unique():
    return stats.type_token_ratio(["a", "b"]), 1.0


@test
def test_pad_right():
    return fmt.pad_right("ab", 4), 'ab  '


@test
def test_pad_right_long():
    return fmt.pad_right("abcde", 4), 'abcde'


@test
def test_pad_left():
    return fmt.pad_left("ab", 4, "0"), '00ab'


@test
def test_pad_left_exact():
    return fmt.pad_left("abcd", 4), 'abcd'


@test
def test_table_header():
    return fmt.table([], ["a", "bb"]).splitlines()[0], '| a | bb |'


@test
def test_table_rows():
    return fmt.table([[1, 22]], ["a", "bb"]).splitlines()[2], '| 1 | 22 |'


@test
def test_table_sep():
    return fmt.table([[1, 22]], ["a", "bb"]).splitlines()[1], '|---|----|'


@test
def test_percent_default():
    return fmt.percent(0.1234), '12.3%'


@test
def test_percent_digits():
    return fmt.percent(0.5, 0), '50%'


@test
def test_percent_zero():
    return fmt.percent(0.0), '0.0%'


@test
def test_pluralise_one():
    return fmt.pluralise(1, "job"), '1 job'


@test
def test_pluralise_many():
    return fmt.pluralise(3, "job"), '3 jobs'


@test
def test_pluralise_zero():
    return fmt.pluralise(0, "job"), '0 jobs'


@test
def test_words_empty():
    return tokens.words(""), []


