//! `text::one_line` (bead boubou-h5d): the ⚙ log line must stay one line whatever the tool
//! result contains, or its status tag lands on the next line and the bench reads none.

use rufous_kernel::text::{head, one_line};

#[test]
fn one_line_folds_every_line_break_and_keeps_the_rest() {
    assert_eq!(one_line("plain"), "plain");
    assert_eq!(
        one_line("[edit failed: old text not found. Closest match, lines 13–15:\nThe primary ris"),
        "[edit failed: old text not found. Closest match, lines 13–15: ⏎ The primary ris"
    );
    assert_eq!(one_line("a\r\nb\rc\td"), "a ⏎ b ⏎ c d");
    assert_eq!(one_line(""), "");
    // Composed with the head cut the log uses: the cut first, then the fold.
    assert_eq!(one_line(head("ab\ncd", 3)), "ab ⏎ ");
}
