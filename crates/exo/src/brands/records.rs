//! What a brand's logo rests on: the mail provider's DMARC verdict on a message (its own
//! `Authentication-Results`), and the sending domain's DMARC and BIMI records in DNS.

use exo_auth::Provider;
use exo_store::Header;
use mail_parser::{Address, MessageParser};

/// The domain whose policy the provider applied to a message that passed DMARC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Passed {
    /// The `From` address's domain, lower-cased, aligned with `header.from`.
    pub(super) domain: String,
}

/// The provider's verdict in `headers`: a single RFC 5322 `From` header naming one mailbox, and
/// the provider's own `Authentication-Results` saying `dmarc=pass` for an aligned `header.from`.
///
/// For Google (`mx.google.com`), the inline policy comment must quarantine or reject failing mail
/// (the subdomain policy for a subdomain). For Microsoft / Exchange Online (an `*.outlook.com` /
/// `outlook.com` authserv-id, or Exchange's authserv-id-less header starting with an RFC 8601
/// method because Exchange renames incoming external headers to `Authentication-Results-Original`),
/// an inline policy comment is enforced when present and DNS `_dmarc` enforces policy otherwise.
pub(super) fn passed(headers: &[Header], provider: Provider) -> Option<Passed> {
    let from_domain = single_from_domain(headers)?;
    let results = auth_results(headers, provider)?;
    let dmarc = outside_comments(results)
        .into_iter()
        .find(|(method, _)| method.trim().starts_with("dmarc="))?;
    let (method, comment) = dmarc;
    let mut words = method.split_whitespace();
    if words.next()? != "dmarc=pass" {
        return None;
    }
    let auth_domain = words
        .find_map(|word| word.strip_prefix("header.from="))?
        .trim_end_matches('.')
        .to_lowercase();
    if auth_domain.is_empty() || !domains_align(&from_domain, &auth_domain) {
        return None;
    }
    let tag = |name: &str| {
        comment
            .split_whitespace()
            .find_map(|tag| tag.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_uppercase)
    };
    let organization = psl::domain_str(&from_domain).unwrap_or(&from_domain);
    let inline_policy = match tag("sp") {
        Some(policy) if organization != from_domain => Some(policy),
        _ => tag("p"),
    };
    let policy_ok = match (provider, inline_policy.as_deref()) {
        (_, Some("QUARANTINE" | "REJECT")) => true,
        (_, Some(_)) => false,
        // Gmail always includes `(p=... sp=...)`; Exchange Online typically emits `dmarc=pass
        // action=none header.from=...` without a policy comment, so DNS `_dmarc` enforces it.
        (Provider::Microsoft, None) => true,
        (Provider::Google, None) => false,
    };
    policy_ok.then_some(Passed {
        domain: from_domain,
    })
}

/// The results portion of the `Authentication-Results` header added by `provider`.
///
/// Exchange Online renames any incoming `Authentication-Results` header to
/// `Authentication-Results-Original` before prepending its own, so for Microsoft accounts only the
/// topmost `Authentication-Results` header is inspected (preventing fallthrough to lower forged
/// headers when the topmost header is untrusted).
fn auth_results(headers: &[Header], provider: Provider) -> Option<&str> {
    let mut auth_headers = headers.iter().filter_map(|header| {
        header
            .name
            .eq_ignore_ascii_case("Authentication-Results")
            .then_some(header.value.as_str())
    });
    match provider {
        Provider::Google => auth_headers.find_map(|value| {
            let (server, results) = value.split_once(';')?;
            (server.split_whitespace().next()? == "mx.google.com").then_some(results)
        }),
        Provider::Microsoft => exchange_auth_results(auth_headers.next()?),
    }
}

/// Extracts the results section from an Exchange Online `Authentication-Results` header value,
/// accepting either an `outlook.com` / `*.outlook.com` authserv-id or Exchange's authserv-id-less
/// format (`spf=...; dkim=...; dmarc=...; compauth=...`). Rejects any untrusted explicit
/// authserv-id.
fn exchange_auth_results(value: &str) -> Option<&str> {
    let first = value.split(';').next()?.trim();
    if first.is_empty() {
        return None;
    }
    let token = first.split_whitespace().next()?;
    if let Some((method, _)) = token.split_once('=') {
        return is_auth_method(method).then_some(value);
    }
    let (_, results) = value.split_once(';')?;
    is_exchange_authserv_id(token).then_some(results)
}

/// Whether `server` is a Microsoft Exchange Online / EOP authserv-id (`outlook.com` or a valid
/// subdomain such as `*.protection.outlook.com` or `*.prod.outlook.com`).
fn is_exchange_authserv_id(server: &str) -> bool {
    let lower = server.trim_end_matches('.').to_ascii_lowercase();
    lower == "outlook.com"
        || lower
            .strip_suffix(".outlook.com")
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.ends_with('.'))
}

/// Whether `method` is a recognized RFC 8601 / Exchange Online authentication method name.
fn is_auth_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "spf"
            | "dkim"
            | "dmarc"
            | "compauth"
            | "arc"
            | "auth"
            | "iprev"
            | "sender-id"
            | "dkim-adsp"
            | "bimi"
    )
}

/// The lower-cased domain of the one mailbox in `headers`' sole `From` header, or `None` when
/// `headers` has zero or multiple `From` headers, multiple mailboxes, group syntax or no valid
/// address.
fn single_from_domain(headers: &[Header]) -> Option<String> {
    let mut from_headers = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("From"));
    let value = from_headers.next()?.value.as_str();
    if from_headers.next().is_some() || has_unquoted_list_separator(value) {
        return None;
    }
    let raw = format!("From: {value}\r\n\r\n");
    let parsed = MessageParser::default().parse(raw.as_bytes())?;
    let Address::List(mailboxes) = parsed.from()? else {
        return None;
    };
    let [mailbox] = mailboxes.as_slice() else {
        return None;
    };
    let address = mailbox.address()?.trim();
    if address
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    let (local, domain) = address.rsplit_once('@')?;
    let domain = domain.trim_end_matches('.').to_lowercase();
    (!local.is_empty()
        && !local.contains('@')
        && !domain.is_empty()
        && !domain.starts_with('.')
        && domain.contains('.'))
    .then_some(domain)
}

/// Whether `header` has `,` or `;` outside RFC 5322 quoted-strings and comments, marking a
/// multi-mailbox list or group syntax.
fn has_unquoted_list_separator(header: &str) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    let mut comments = 0usize;
    for ch in header.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && (quoted || comments > 0) {
            escaped = true;
            continue;
        }
        if comments == 0 && ch == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            match ch {
                '(' => comments += 1,
                ')' => comments = comments.saturating_sub(1),
                ',' | ';' if comments == 0 => return true,
                _ => {}
            }
        }
    }
    quoted
}

/// Whether `from_domain` and `auth_domain` align under DMARC: exact match or shared registrable
/// organizational domain.
fn domains_align(from_domain: &str, auth_domain: &str) -> bool {
    from_domain == auth_domain
        || psl::domain_str(from_domain)
            .zip(psl::domain_str(auth_domain))
            .is_some_and(|(from_org, auth_org)| from_org == auth_org)
}

/// `results`' `;`-separated methods, each with what its comments say (`(p=REJECT sp=REJECT)`).
fn outside_comments(results: &str) -> Vec<(String, String)> {
    let (mut methods, mut text, mut comment, mut depth) =
        (Vec::new(), String::new(), String::new(), 0usize);
    for character in results.chars() {
        match character {
            '(' => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    comment.push(' ');
                    continue;
                }
            }
            ';' if depth == 0 => {
                methods.push((std::mem::take(&mut text), std::mem::take(&mut comment)));
                continue;
            }
            _ => {}
        }
        if depth > 0 {
            comment.push(character);
        } else {
            text.push(character);
        }
    }
    methods.push((text, comment));
    methods
}

/// A DNS TXT record's `tag=value` pairs (DMARC's and BIMI's syntax), tags lower-cased.
fn tags(record: &str) -> Vec<(String, String)> {
    record
        .split(';')
        .filter_map(|pair| {
            let (tag, value) = pair.split_once('=')?;
            Some((tag.trim().to_lowercase(), value.trim().to_owned()))
        })
        .collect()
}

/// Whether `record` starts with `v=DMARC1`.
fn is_dmarc_record(record: &str) -> bool {
    tags(record)
        .first()
        .map(|(tag, value)| (tag.as_str(), value.as_str()))
        == Some(("v", "DMARC1"))
}

/// Whether `records` (all TXT strings at `_dmarc`) contain exactly one `v=DMARC1` record
/// (RFC 7489 §6.6.3 treats multiple DMARC records as invalid / no policy) and that sole record
/// enforces `quarantine` or `reject` on all failing mail.
pub(super) fn dmarc_records_enforce(records: &[String], org_fallback: bool) -> bool {
    let mut dmarc = records.iter().filter(|record| is_dmarc_record(record));
    let Some(sole) = dmarc.next() else {
        return false;
    };
    dmarc.next().is_none() && dmarc_enforces(sole, org_fallback)
}

/// Whether `record` is a DMARC record (`v=DMARC1` first) that enforces `quarantine` or `reject`
/// on all failing mail (`pct=100`, the default). When `org_fallback` is true (a subdomain fell
/// back to its organizational domain's `_dmarc` record), `sp=` overrides `p=` per RFC 7489.
pub(super) fn dmarc_enforces(record: &str, org_fallback: bool) -> bool {
    let tags = tags(record);
    let version = tags
        .first()
        .map(|(tag, value)| (tag.as_str(), value.as_str()));
    if version != Some(("v", "DMARC1")) {
        return false;
    }
    let pct_all = tags
        .iter()
        .find(|(tag, _)| tag == "pct")
        .is_none_or(|(_, pct)| pct == "100");
    if !pct_all {
        return false;
    }
    let tag = |name: &str| {
        tags.iter()
            .find(|(tag, _)| tag == name)
            .map(|(_, value)| value.to_ascii_lowercase())
    };
    let policy = if org_fallback {
        tag("sp").or_else(|| tag("p"))
    } else {
        tag("p")
    };
    matches!(policy.as_deref(), Some("quarantine" | "reject"))
}

/// A BIMI assertion record's logo and certificate (`l=` and `a=`), each an HTTPS URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Bimi {
    pub(super) logo: Option<String>,
    pub(super) certificate: Option<String>,
}

/// `record` as a BIMI assertion record (`v=BIMI1` first), or `None` when it isn't one.
pub(super) fn bimi(record: &str) -> Option<Bimi> {
    let tags = tags(record);
    let (tag, version) = tags.first()?;
    if (tag.as_str(), version.as_str()) != ("v", "BIMI1") {
        return None;
    }
    let https = |name: &str| {
        tags.iter()
            .find(|(tag, _)| tag == name)
            .map(|(_, value)| value.clone())
            .filter(|value| value.starts_with("https://"))
    };
    Some(Bimi {
        logo: https("l"),
        certificate: https("a"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_headers(from: &str, value: &str) -> Vec<Header> {
        vec![
            Header {
                name: "From".into(),
                value: from.into(),
            },
            Header {
                name: "Authentication-Results".into(),
                value: value.into(),
            },
        ]
    }

    fn results(value: &str) -> Vec<Header> {
        message_headers("\"Brand, Inc.\" <news@brand.example>", value)
    }

    #[test]
    fn gmails_dmarc_pass_under_an_enforcing_policy_counts() {
        let gmail = "mx.google.com;\r\n       dkim=pass header.i=@brand.example header.s=s1;\r\n       spf=pass (google.com: domain of bounce@brand.example designates 192.0.2.1 as permitted sender) smtp.mailfrom=bounce@brand.example;\r\n       dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=Brand.example";
        assert_eq!(
            passed(&results(gmail), Provider::Google),
            Some(Passed {
                domain: "brand.example".into()
            })
        );
        assert_eq!(
            passed(&results(gmail), Provider::Microsoft),
            None,
            "Exchange accounts reject Gmail's authserv-id"
        );
        let monitoring = gmail.replace("p=REJECT sp=REJECT", "p=NONE sp=NONE");
        assert_eq!(
            passed(&results(&monitoring), Provider::Google),
            None,
            "p=none enforces nothing"
        );
        let failed = gmail.replace("dmarc=pass", "dmarc=fail");
        assert_eq!(passed(&results(&failed), Provider::Google), None);
        let forged = gmail.replace("mx.google.com", "mx.attacker.example");
        assert_eq!(
            passed(&results(&forged), Provider::Google),
            None,
            "only Gmail's own counts"
        );
    }

    #[test]
    fn exchange_dmarc_pass_accepts_eop_headers_and_rejects_forged_ones() {
        let with_eop = "spfeop.protection.outlook.com; spf=pass (sender IP is 192.0.2.1) smtp.mailfrom=brand.example; dkim=pass (signature was verified) header.d=brand.example; dmarc=pass action=none header.from=Brand.example; compauth=pass reason=100";
        assert_eq!(
            passed(&results(with_eop), Provider::Microsoft),
            Some(Passed {
                domain: "brand.example".into()
            })
        );
        assert_eq!(
            passed(&results(with_eop), Provider::Google),
            None,
            "Google accounts reject Exchange's authserv-id"
        );

        let with_prod = "BL0PR02MB4567.namprd02.prod.outlook.com; dmarc=pass action=none header.from=brand.example";
        assert_eq!(
            passed(&results(with_prod), Provider::Microsoft),
            Some(Passed {
                domain: "brand.example".into()
            })
        );

        let authserv_less = "spf=pass (sender IP is 192.0.2.1) smtp.mailfrom=brand.example; dkim=pass header.d=brand.example; dmarc=pass action=none header.from=brand.example; compauth=pass reason=100";
        assert_eq!(
            passed(&results(authserv_less), Provider::Microsoft),
            Some(Passed {
                domain: "brand.example".into()
            })
        );

        // `dmarc=bestguesspass` means no published DMARC record and must be rejected.
        let best_guess = with_eop.replace("dmarc=pass", "dmarc=bestguesspass");
        assert_eq!(passed(&results(&best_guess), Provider::Microsoft), None);

        // Explicit inline `p=NONE` is rejected even on Exchange.
        let inline_none =
            "eur01.protection.outlook.com; dmarc=pass (p=NONE) header.from=brand.example";
        assert_eq!(passed(&results(inline_none), Provider::Microsoft), None);

        // Forged authserv-ids, lookalike suffixes, and bogus method tokens are rejected.
        for forged in [
            "mx.attacker.example; dmarc=pass action=none header.from=brand.example",
            "fakeoutlook.com; dmarc=pass action=none header.from=brand.example",
            "protection.outlook.com.evil.example; dmarc=pass action=none header.from=brand.example",
            "evil=host.example; dmarc=pass action=none header.from=brand.example",
        ] {
            assert_eq!(
                passed(&results(forged), Provider::Microsoft),
                None,
                "forged header must be rejected: {forged}"
            );
        }

        // A top Exchange `dmarc=fail` does not fall through to an older `dmarc=pass` below.
        let stacked = vec![
            Header {
                name: "From".into(),
                value: "\"Brand\" <news@brand.example>".into(),
            },
            Header {
                name: "Authentication-Results".into(),
                value: "spfeop.protection.outlook.com; dmarc=fail action=oreject header.from=brand.example".into(),
            },
            Header {
                name: "Authentication-Results".into(),
                value: with_eop.into(),
            },
        ];
        assert_eq!(passed(&stacked, Provider::Microsoft), None);
    }

    #[test]
    fn a_subdomain_takes_the_subdomain_policy() {
        let mail =
            "mx.google.com; dmarc=pass (p=REJECT sp=NONE dis=NONE) header.from=news.brand.co.uk";
        assert_eq!(
            passed(
                &message_headers("Brand <news@news.brand.co.uk>", mail),
                Provider::Google
            ),
            None
        );
        let enforced = mail.replace("sp=NONE", "sp=QUARANTINE");
        assert_eq!(
            passed(
                &message_headers("Brand <news@news.brand.co.uk>", &enforced),
                Provider::Google
            ),
            Some(Passed {
                domain: "news.brand.co.uk".into()
            })
        );
    }

    #[test]
    fn requires_one_single_mailbox_from_aligned_with_header_from() {
        let gmail =
            "mx.google.com; dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=brand.example";
        // Missing From header is rejected.
        assert_eq!(
            passed(
                &[Header {
                    name: "Authentication-Results".into(),
                    value: gmail.into(),
                }],
                Provider::Google
            ),
            None
        );
        // Multiple From headers are rejected.
        assert_eq!(
            passed(
                &[
                    Header {
                        name: "From".into(),
                        value: "news@brand.example".into(),
                    },
                    Header {
                        name: "From".into(),
                        value: "spoof@paypal.com".into(),
                    },
                    Header {
                        name: "Authentication-Results".into(),
                        value: gmail.into(),
                    },
                ],
                Provider::Google
            ),
            None
        );
        // Multiple mailboxes or RFC 5322 group syntax in From are rejected.
        assert_eq!(
            passed(
                &message_headers("news@brand.example, spoof@paypal.com", gmail),
                Provider::Google
            ),
            None
        );
        assert_eq!(
            passed(
                &message_headers("Brand: news@brand.example;", gmail),
                Provider::Google
            ),
            None
        );
        // Unaligned From vs Authentication-Results header.from is rejected.
        assert_eq!(
            passed(
                &message_headers("Security <security@paypal.com>", gmail),
                Provider::Google
            ),
            None
        );
        assert_eq!(
            passed(
                &message_headers(
                    "a@one.co.uk",
                    "mx.google.com; dmarc=pass (p=REJECT sp=REJECT) header.from=two.co.uk"
                ),
                Provider::Google
            ),
            None
        );
        // Organizational-domain alignment passes and returns From's domain.
        assert_eq!(
            passed(
                &message_headers("Alerts <alerts@mail.brand.example>", gmail),
                Provider::Google
            ),
            Some(Passed {
                domain: "mail.brand.example".into()
            })
        );
    }

    #[test]
    fn records_are_read_by_their_tags() {
        assert!(dmarc_enforces(
            "v=DMARC1; p=reject; rua=mailto:d@brand.example",
            false
        ));
        assert!(dmarc_enforces("v=DMARC1; p=quarantine; pct=100", false));
        assert!(!dmarc_enforces("v=DMARC1; p=none; pct=100", false));
        assert!(!dmarc_enforces("v=DMARC1; p=quarantine; pct=50", false));
        assert!(!dmarc_enforces("v=spf1 -all", false));
        // On organizational-domain fallback, `sp=` overrides `p=`.
        assert!(!dmarc_enforces("v=DMARC1; p=reject; sp=none", true));
        assert!(dmarc_enforces("v=DMARC1; p=none; sp=quarantine", true));
        assert!(dmarc_enforces("v=DMARC1; p=reject", true));
        // RFC 7489 §6.6.3: multiple DMARC records must be discarded (no policy enforced).
        assert!(dmarc_records_enforce(
            &[
                "v=spf1 -all".into(),
                "v=DMARC1; p=reject; rua=mailto:d@brand.example".into(),
            ],
            false
        ));
        assert!(!dmarc_records_enforce(
            &["v=DMARC1; p=reject".into(), "v=DMARC1; p=none".into(),],
            false
        ));
        // Malformed empty-label Exchange authserv-ids are rejected.
        assert!(!is_exchange_authserv_id(".outlook.com"));
        assert!(!is_exchange_authserv_id("a..outlook.com"));
        assert!(is_exchange_authserv_id("outlook.com"));
        assert!(is_exchange_authserv_id(
            "NAM11-CO1-obe.outbound.protection.outlook.com"
        ));
        // On Microsoft, an untrusted topmost Authentication-Results must not fall through to a
        // lower forged header.
        assert_eq!(
            passed(
                &[
                    Header {
                        name: "From".into(),
                        value: "news@brand.example".into(),
                    },
                    Header {
                        name: "Authentication-Results".into(),
                        value: "mx.attacker.example; dmarc=pass header.from=brand.example".into(),
                    },
                    Header {
                        name: "Authentication-Results".into(),
                        value: "spf=pass; dmarc=pass action=none header.from=brand.example; compauth=pass"
                            .into(),
                    },
                ],
                Provider::Microsoft
            ),
            None
        );
        assert_eq!(
            bimi("v=BIMI1; l=https://brand.example/logo.svg; a=https://brand.example/vmc.pem"),
            Some(Bimi {
                logo: Some("https://brand.example/logo.svg".into()),
                certificate: Some("https://brand.example/vmc.pem".into()),
            })
        );
        assert_eq!(
            bimi("v=BIMI1; l=http://brand.example/logo.svg; a="),
            Some(Bimi {
                logo: None,
                certificate: None
            }),
            "only HTTPS"
        );
        assert_eq!(bimi("l=https://brand.example/logo.svg"), None);
    }
}
