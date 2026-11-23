//! A table's height is a minimum (CSS 2.1 §17.5.3): browsers grow the table to fit its rows.
//! Blitz lays a table out at exactly the height it asks for, so content taller than that spills
//! out and whatever follows the table is drawn over it. Rows and cells are already minimums
//! there. This turns a table's pixel height, from its `style` or its `height` attribute, into a
//! `min-height` before Blitz sees it. Heights from a `<style>` sheet are left as they are.

use crate::dom::{self, NodeRef, Style};

/// Rewrites every table under `root` whose height is in pixels to grow past it.
pub(crate) fn grow_to_fit(root: &NodeRef) {
    for table in dom::select(root, |node| dom::is(node, "table")) {
        let mut style = Style::of(&table);
        let inline = style.get("height").trim().to_owned();
        let height = if inline.is_empty() {
            dom::attr(&table, "height").and_then(|value| attribute_pixels(&value))
        } else {
            css_pixels(&inline)
        };
        let Some(height) = height else {
            continue;
        };
        let important = style.is_important("height");
        let min = css_pixels(style.get("min-height").trim());
        if min.is_none_or(|min| min < height) {
            style.set(
                "min-height",
                &format!("{height}px"),
                important || style.is_important("min-height"),
            );
        }
        style.set("height", "auto", important);
        style.apply(&table);
        dom::remove_attr(&table, "height");
    }
}

/// A `height` attribute in pixels: `450` or `450px`, not `100%`.
fn attribute_pixels(value: &str) -> Option<f32> {
    let value = value.trim();
    let number = value.strip_suffix("px").unwrap_or(value).trim();
    number
        .parse()
        .ok()
        .filter(|px: &f32| px.is_finite() && *px >= 0.0)
}

/// A CSS length in pixels: `158px`, or a bare `0`.
fn css_pixels(value: &str) -> Option<f32> {
    if value == "0" {
        return Some(0.0);
    }
    value
        .strip_suffix("px")
        .and_then(|number| number.trim().parse().ok())
        .filter(|px: &f32| px.is_finite() && *px >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table in `html` after [`grow_to_fit`]: its `height` and `min-height`, and whether it
    /// still has a `height` attribute.
    fn grown(html: &str) -> (String, String, bool) {
        let document = dom::parse(html);
        grow_to_fit(&document);
        let table = dom::select_first(&document, |node| dom::is(node, "table")).expect("a table");
        let style = Style::of(&table);
        (
            style.get("height").to_owned(),
            style.get("min-height").to_owned(),
            dom::has_attr(&table, "height"),
        )
    }

    #[test]
    fn pixel_heights_become_minimums() {
        let expected = |min: &str| ("auto".to_owned(), min.to_owned(), false);
        assert_eq!(
            grown("<table height=\"450\" width=\"500\"></table>"),
            expected("450px")
        );
        assert_eq!(
            grown("<table style=\"height:158px\" height=\"158\"></table>"),
            expected("158px")
        );
        assert_eq!(
            grown("<table style=\"height:50px;min-height:80px\"></table>"),
            expected("80px"),
            "a larger minimum stays"
        );
    }

    #[test]
    fn other_heights_are_left_alone() {
        assert_eq!(
            grown("<table height=\"100%\"></table>"),
            (String::new(), String::new(), true)
        );
        assert_eq!(
            grown("<table style=\"height:50%\" height=\"40\"></table>"),
            ("50%".to_owned(), String::new(), true)
        );
        assert_eq!(
            grown("<table></table>"),
            (String::new(), String::new(), false)
        );
    }
}
