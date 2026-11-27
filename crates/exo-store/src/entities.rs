//! HTML entities, which stored snippets and HTML bodies keep, and readers decode.

/// Decodes HTML entities left in snippets and HTML bodies (`&#39;`, `&amp;`, `&mdash;`, ...).
pub(crate) fn unescape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest.find(';').filter(|&end| end <= 10).and_then(|end| {
            let entity = &rest[1..end];
            let ch = named_html_entity(entity).or_else(|| {
                let digits = entity.strip_prefix('#')?;
                let code = if let Some(hex) = digits
                    .strip_prefix('x')
                    .or_else(|| digits.strip_prefix('X'))
                {
                    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return None;
                    }
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    digits.parse::<u32>().ok()?
                };
                char::from_u32(code)
            })?;
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_html_entity(entity: &str) -> Option<char> {
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "hellip" => '\u{2026}',
        "bull" => '\u{2022}',
        "middot" => '\u{00B7}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "euro" => '\u{20AC}',
        "pound" => '\u{00A3}',
        "yen" => '\u{00A5}',
        "cent" => '\u{00A2}',
        "sect" => '\u{00A7}',
        "para" => '\u{00B6}',
        "deg" => '\u{00B0}',
        "plusmn" => '\u{00B1}',
        "times" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "laquo" => '\u{00AB}',
        "raquo" => '\u{00BB}',
        "lsaquo" => '\u{2039}',
        "rsaquo" => '\u{203A}',
        "sbquo" => '\u{201A}',
        "bdquo" => '\u{201E}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "permil" => '\u{2030}',
        "prime" => '\u{2032}',
        "Prime" => '\u{2033}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "harr" => '\u{2194}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200C}',
        "zwj" => '\u{200D}',
        "shy" => '\u{00AD}',
        "frac14" => '\u{00BC}',
        "frac12" => '\u{00BD}',
        "frac34" => '\u{00BE}',
        "sup1" => '\u{00B9}',
        "sup2" => '\u{00B2}',
        "sup3" => '\u{00B3}',
        "iexcl" => '\u{00A1}',
        "iquest" => '\u{00BF}',
        "brvbar" => '\u{00A6}',
        "macr" => '\u{00AF}',
        "acute" => '\u{00B4}',
        "cedil" => '\u{00B8}',
        "uml" => '\u{00A8}',
        "ordf" => '\u{00AA}',
        "ordm" => '\u{00BA}',
        "not" => '\u{00AC}',
        "micro" => '\u{00B5}',
        "Agrave" => '\u{00C0}',
        "Aacute" => '\u{00C1}',
        "Acirc" => '\u{00C2}',
        "Atilde" => '\u{00C3}',
        "Auml" => '\u{00C4}',
        "Aring" => '\u{00C5}',
        "AElig" => '\u{00C6}',
        "Ccedil" => '\u{00C7}',
        "Egrave" => '\u{00C8}',
        "Eacute" => '\u{00C9}',
        "Ecirc" => '\u{00CA}',
        "Euml" => '\u{00CB}',
        "Igrave" => '\u{00CC}',
        "Iacute" => '\u{00CD}',
        "Icirc" => '\u{00CE}',
        "Iuml" => '\u{00CF}',
        "ETH" => '\u{00D0}',
        "Ntilde" => '\u{00D1}',
        "Ograve" => '\u{00D2}',
        "Oacute" => '\u{00D3}',
        "Ocirc" => '\u{00D4}',
        "Otilde" => '\u{00D5}',
        "Ouml" => '\u{00D6}',
        "Oslash" => '\u{00D8}',
        "Ugrave" => '\u{00D9}',
        "Uacute" => '\u{00DA}',
        "Ucirc" => '\u{00DB}',
        "Uuml" => '\u{00DC}',
        "Yacute" => '\u{00DD}',
        "THORN" => '\u{00DE}',
        "szlig" => '\u{00DF}',
        "agrave" => '\u{00E0}',
        "aacute" => '\u{00E1}',
        "acirc" => '\u{00E2}',
        "atilde" => '\u{00E3}',
        "auml" => '\u{00E4}',
        "aring" => '\u{00E5}',
        "aelig" => '\u{00E6}',
        "ccedil" => '\u{00E7}',
        "egrave" => '\u{00E8}',
        "eacute" => '\u{00E9}',
        "ecirc" => '\u{00EA}',
        "euml" => '\u{00EB}',
        "igrave" => '\u{00EC}',
        "iacute" => '\u{00ED}',
        "icirc" => '\u{00EE}',
        "iuml" => '\u{00EF}',
        "eth" => '\u{00F0}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "oacute" => '\u{00F3}',
        "ocirc" => '\u{00F4}',
        "otilde" => '\u{00F5}',
        "ouml" => '\u{00F6}',
        "oslash" => '\u{00F8}',
        "ugrave" => '\u{00F9}',
        "uacute" => '\u{00FA}',
        "ucirc" => '\u{00FB}',
        "uuml" => '\u{00FC}',
        "yacute" => '\u{00FD}',
        "thorn" => '\u{00FE}',
        "yuml" => '\u{00FF}',
        "OElig" => '\u{0152}',
        "oelig" => '\u{0153}',
        "Scaron" => '\u{0160}',
        "scaron" => '\u{0161}',
        "Yuml" => '\u{0178}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_snippets() {
        assert_eq!(
            unescape_html("it&#39;s &lt;b&gt; &amp; &#x263A; &bogus; & done"),
            "it's <b> & ☺ &bogus; & done"
        );
        assert_eq!(
            unescape_html(
                "&ldquo;caf&eacute;&rdquo; &mdash; &euro;5 &frac12; &copy; &rarr; &hellip;"
            ),
            "\u{201C}caf\u{00E9}\u{201D} \u{2014} \u{20AC}5 \u{00BD} \u{00A9} \u{2192} \u{2026}"
        );
        assert_eq!(
            unescape_html("&Aacute;&ntilde;&Ccedil;&szlig;&OElig;&Yuml;"),
            "\u{00C1}\u{00F1}\u{00C7}\u{00DF}\u{0152}\u{0178}"
        );
        // Leading signs in numeric entities must not be accepted.
        assert_eq!(
            unescape_html("&#+39; &#-39; &#x+27; &#X-27; &#; &#x;"),
            "&#+39; &#-39; &#x+27; &#X-27; &#; &#x;"
        );
    }
}
