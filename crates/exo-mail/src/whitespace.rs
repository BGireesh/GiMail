//! White space the way Gmail shows it.
//!
//! - Text pasted from web editors (Confluence and Jira's lists) brings an inline
//!   `white-space-collapse: preserve` that keeps the markup's indenting. Gmail drops it, so it's
//!   dropped here too, and the indenting collapses.
//! - Blitz has no tab stops and draws a tab with the font's missing-glyph box, so preserved text
//!   (`<pre>`, plain-text mail) showed rows of `□`. Tabs become spaces up to the next stop every
//!   [`TAB_SIZE`] columns, as browsers lay them out. Where white space collapses, a tab and a
//!   space collapse alike.

use crate::dom::{self, NodeRef, Style};

/// Drops inline `white-space-collapse` under `root`.
pub(crate) fn drop_pasted_collapse(root: &NodeRef) {
    for element in dom::select(root, |node| dom::has_attr(node, "style")) {
        let mut style = Style::of(&element);
        if !style.get("white-space-collapse").is_empty() {
            style.remove("white-space-collapse");
            style.apply(&element);
        }
    }
}

/// CSS's initial `tab-size`.
const TAB_SIZE: usize = 8;

/// Expands the tabs in every text node under `root`. Columns count from the last line break in
/// the same text node, which is where a tab in preserved text almost always starts.
pub(crate) fn expand_tabs(root: &NodeRef) {
    for node in dom::descendants(root) {
        if let Some(text) = dom::text(&node).filter(|text| text.contains('\t')) {
            dom::set_text(&node, &expanded(&text));
        }
    }
}

fn expanded(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut column = 0;
    for c in text.chars() {
        match c {
            '\t' => {
                let spaces = TAB_SIZE - column % TAB_SIZE;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | '\r' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_stop_every_eight_columns() {
        assert_eq!(expanded("a\tb"), "a       b");
        assert_eq!(expanded("\t\tx"), " ".repeat(16) + "x");
        assert_eq!(expanded("abcdefgh\tx"), "abcdefgh        x");
        assert_eq!(
            expanded("ab\n\tc"),
            "ab\n        c",
            "a line break starts over"
        );
        assert_eq!(expanded("no tabs"), "no tabs");
    }

    #[test]
    fn every_text_node_is_expanded() {
        let document = dom::parse("<ul style=\"white-space: pre\">\n\t\t<li><p>\tx</p></li></ul>");
        expand_tabs(&document);
        assert!(!dom::outer_html(&document).contains('\t'));
    }

    #[test]
    fn pasted_white_space_collapse_is_dropped() {
        let document =
            dom::parse("<ul style=\"display: flow-root; white-space-collapse: preserve\"></ul>");
        drop_pasted_collapse(&document);
        let list = dom::select_first(&document, |node| dom::is(node, "ul")).expect("the list");
        let style = Style::of(&list);
        assert_eq!(style.get("white-space-collapse"), "");
        assert_eq!(style.get("display"), "flow-root");
    }
}
