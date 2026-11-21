//! How much of its URL a link's text shows: all of it, its origin, or its host with some of the
//! parts after it. A URL pasted over a link writes the new URL the same way.

use url::Url;

/// A URL's parts, as a browser's `URL` gives them. An empty query or fragment is `""`, not `?` or
/// `#`.
struct Parts {
    href: String,
    origin: String,
    protocol: String,
    host: String,
    hostname: String,
    pathname: String,
    search: String,
    hash: String,
}

impl Parts {
    fn of(url: &Url) -> Self {
        let hostname = url.host_str().unwrap_or_default().to_owned();
        let host = match url.port() {
            Some(port) => format!("{hostname}:{port}"),
            None => hostname.clone(),
        };
        let prefixed = |sign: char, part: Option<&str>| {
            part.filter(|part| !part.is_empty())
                .map_or_else(String::new, |part| format!("{sign}{part}"))
        };
        Self {
            href: url.as_str().to_owned(),
            origin: url.origin().ascii_serialization(),
            protocol: format!("{}:", url.scheme()),
            host,
            hostname,
            pathname: url.path().to_owned(),
            search: prefixed('?', url.query()),
            hash: prefixed('#', url.fragment()),
        }
    }
}

/// The text for a link to `new` that shows it as `text` showed `old`. Text that's the old URL's
/// origin or whole URL becomes the new one's. Text that starts with the old host (with its port,
/// or without it, or with the scheme before it) starts with the new one's, followed by the new
/// URL's path, query and fragment where the old text showed the old one's, or the old URL had
/// none. `None` when the text doesn't start with the old URL's host at all.
pub(crate) fn matched(new: &Url, old: &Url, text: &str) -> Option<String> {
    let (new, old) = (Parts::of(new), Parts::of(old));
    if text == old.origin {
        return Some(new.origin);
    }
    if text == old.href {
        return Some(new.href);
    }
    let shown = |part: &str| text.contains(part);
    let path = if new.pathname != "/" && (old.pathname == "/" || shown(&old.pathname)) {
        new.pathname.as_str()
    } else {
        ""
    };
    let search = if shown(&old.search) {
        new.search.as_str()
    } else {
        ""
    };
    let hash = if shown(&old.hash) {
        new.hash.as_str()
    } else {
        ""
    };
    let (new_start, old_start) = if text.starts_with(&old.host) {
        (new.host, old.host)
    } else if text.starts_with(&old.hostname) {
        (new.hostname, old.hostname)
    } else if text.starts_with(&format!("{}//{}", old.protocol, old.hostname)) {
        (new.origin, old.origin)
    } else {
        return None;
    };
    Some(if text == old_start {
        new_start
    } else {
        format!("{new_start}{path}{search}{hash}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(new: &str, old: &str, text: &str) -> Option<String> {
        matched(
            &Url::parse(new).expect("a URL"),
            &Url::parse(old).expect("a URL"),
            text,
        )
    }

    #[test]
    fn the_whole_url_or_its_origin_stays_whole() {
        assert_eq!(
            format(
                "https://new.org/b?c=d",
                "https://old.com/a",
                "https://old.com/a"
            )
            .as_deref(),
            Some("https://new.org/b?c=d")
        );
        assert_eq!(
            format("https://new.org/b", "https://old.com/a", "https://old.com").as_deref(),
            Some("https://new.org")
        );
    }

    #[test]
    fn a_host_stays_a_host() {
        assert_eq!(
            format("https://new.org/b", "https://old.com/", "old.com").as_deref(),
            Some("new.org")
        );
        assert_eq!(
            format(
                "https://new.org:8080/b",
                "https://old.com:8443/",
                "old.com:8443"
            )
            .as_deref(),
            Some("new.org:8080"),
            "with its port"
        );
        assert_eq!(
            format("https://new.org:8080/", "https://old.com:8443/", "old.com").as_deref(),
            Some("new.org"),
            "or without it"
        );
    }

    #[test]
    fn the_parts_the_old_text_showed_follow_the_host() {
        // The path was shown, the query wasn't, and the old URL had no fragment.
        assert_eq!(
            format(
                "https://new.org/b?y=2#z",
                "https://old.com/a?x=1",
                "old.com/a"
            )
            .as_deref(),
            Some("new.org/b#z")
        );
        assert_eq!(
            format("https://new.org/", "https://old.com/a", "old.com/a").as_deref(),
            Some("new.org"),
            "a bare new host has no path to show"
        );
        assert_eq!(
            format(
                "https://new.org/b",
                "https://old.com/a",
                "https://old.com/a and more"
            )
            .as_deref(),
            Some("https://new.org/b"),
            "the scheme and host"
        );
    }

    #[test]
    fn text_not_showing_the_url_is_left_alone() {
        assert_eq!(
            format("https://new.org/b", "https://old.com/a", "our docs"),
            None
        );
    }
}
