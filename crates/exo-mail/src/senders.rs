//! Fixes for particular senders' mail: rules that follow the restyle in mail whose `From` address
//! is at one of these domains, exactly (a subdomain is another sender) and in any case.

/// Each domain's rules.
const FIXES: &[(&str, &str)] = &[
    // This help desk's Zendesk mail writes the ticket id in white, for Zendesk's eyes only, which
    // the restyle's colours would show.
    (
        "superhuman.com",
        r#".zd_encoded_id, .zd_encoded_id ~ [aria-hidden="true"] { display: none; }"#,
    ),
    // Its Zendesk comments' headings at the size of the text around them, and the button links
    // under them inline.
    (
        "superhuman.zendesk.com",
        ".ShadowHTML .ShadowBody .zd-comment section h2 { font-size: 14px !important; }\
         .zd-comment section h2 + div a { min-width: unset !important; margin: 0 10px !important; padding: 0 !important; }\
         .zd-comment section h2 + div a span:not(:has(img)) { margin-left: 6px !important; }",
    ),
    // Linear's code blocks wrap rather than widen the message.
    (
        "linear.app",
        ".email-formatted-document pre, .email-formatted-document code { word-break: break-word; white-space: normal; }",
    ),
    // DocuSign's header image fits its 640px column.
    (
        "docusign.net",
        r#"td[width="640"] table tr:first-child img[width=""] { max-width: 100%; }"#,
    ),
];

/// The fixes for mail from `email`, or nothing. The domain is what follows its first `@`.
pub(crate) fn css(email: &str) -> &'static str {
    let domain = email.split('@').nth(1).unwrap_or_default().to_lowercase();
    FIXES
        .iter()
        .find(|(fixed, _)| *fixed == domain)
        .map_or("", |(_, css)| css)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mail_from_the_domain_itself_is_fixed() {
        assert!(css("support@superhuman.com").contains(".zd_encoded_id"));
        assert!(css("help@superhuman.zendesk.com").contains(".zd-comment"));
        assert!(css("notifications@linear.app").contains(".email-formatted-document"));
        assert!(css("Notifications@Linear.APP").contains(".email-formatted-document"));
        assert!(css("dse@docusign.net").contains(r#"img[width="""#));
        // The same markup from anyone else is theirs.
        for email in [
            "support@acme.zendesk.com",
            "support@help.superhuman.com",
            "notifications@linear.dev",
            "dse@na3.docusign.net",
            "no-at-sign",
        ] {
            assert_eq!(css(email), "", "{email}");
        }
    }
}
