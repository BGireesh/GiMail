//! Where Microsoft Graph and the Microsoft identity platform live. Paths are fixed by the APIs.
//! Only origins are configurable, which is what lets Exo switch between Microsoft and the mock by
//! swapping this one value.

use url::Url;

/// Graph's version segment. Every call is a v1.0 call.
pub(crate) const VERSION: &str = "v1.0";

/// Origins of Graph and of sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Microsoft Graph (`/v1.0/...`).
    pub graph: Url,
    /// The identity platform's endpoints for the `common` authority, which signs in personal and
    /// work or school accounts alike.
    pub identity: IdentityEndpoints,
}

/// Full URLs of the identity platform's OAuth 2.0 endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityEndpoints {
    /// Where the user is sent to sign in and consent (`/common/oauth2/v2.0/authorize`).
    pub authorization: Url,
    /// Code exchange and refresh (`/common/oauth2/v2.0/token`).
    pub token: Url,
}

impl Endpoints {
    /// Microsoft's production endpoints.
    #[must_use]
    pub fn microsoft() -> Self {
        Self::at(
            &parse("https://graph.microsoft.com/"),
            &parse("https://login.microsoftonline.com/"),
        )
    }

    /// Graph and sign-in served from one origin, the way `mock-servers` serves them.
    #[must_use]
    pub fn mock(origin: &Url) -> Self {
        Self::at(origin, origin)
    }

    fn at(graph: &Url, login: &Url) -> Self {
        let login = |path: &str| {
            let mut url = login.clone();
            url.set_path(path);
            url.set_query(None);
            url.set_fragment(None);
            url
        };
        let mut graph = graph.clone();
        graph.set_path("/");
        graph.set_query(None);
        graph.set_fragment(None);
        Self {
            graph,
            identity: IdentityEndpoints {
                authorization: login("/common/oauth2/v2.0/authorize"),
                token: login("/common/oauth2/v2.0/token"),
            },
        }
    }

    /// Whether `link` is a Graph URL, so it may carry the access token. Graph's paging links are
    /// absolute, and a link elsewhere would hand the token to another host.
    pub(crate) fn is_graph(&self, link: &Url) -> bool {
        link.origin() == self.graph.origin()
            && link
                .path_segments()
                .and_then(|mut segments| segments.next())
                == Some(VERSION)
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::microsoft()
    }
}

fn parse(url: &str) -> Url {
    Url::parse(url).expect("hard-coded Microsoft URLs are valid")
}

/// Graph's `segments` under `/v1.0/`, percent-encoded.
pub(crate) fn join<'a>(graph: &Url, segments: impl IntoIterator<Item = &'a str>) -> Url {
    let mut url = graph.clone();
    url.path_segments_mut()
        .expect("http(s) URLs can have paths")
        .pop_if_empty()
        .push(VERSION)
        .extend(segments);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_serves_everything_from_one_origin() {
        let origin = Url::parse("http://127.0.0.1:8787/ignored?foo=bar#frag").unwrap();
        let endpoints = Endpoints::mock(&origin);
        assert_eq!(endpoints.graph.as_str(), "http://127.0.0.1:8787/");
        assert_eq!(
            endpoints.identity.token.as_str(),
            "http://127.0.0.1:8787/common/oauth2/v2.0/token"
        );
        assert_eq!(
            endpoints.identity.authorization.as_str(),
            "http://127.0.0.1:8787/common/oauth2/v2.0/authorize"
        );
        assert_eq!(
            Endpoints::microsoft().identity.authorization.as_str(),
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
        );
    }

    #[test]
    fn join_encodes_segments() {
        let url = join(&Endpoints::microsoft().graph, ["me", "messages", "a/b c"]);
        assert_eq!(
            url.as_str(),
            "https://graph.microsoft.com/v1.0/me/messages/a%2Fb%20c"
        );
    }

    #[test]
    fn only_graph_links_carry_the_token() {
        let endpoints = Endpoints::microsoft();
        let link = |url: &str| endpoints.is_graph(&Url::parse(url).unwrap());
        assert!(link(
            "https://graph.microsoft.com/v1.0/me/messages?$skiptoken=x"
        ));
        assert!(!link("https://graph.microsoft.com/beta/me/messages"));
        assert!(!link("https://graph.example.com/v1.0/me/messages"));
        assert!(!link("http://graph.microsoft.com/v1.0/me/messages"));
    }
}
