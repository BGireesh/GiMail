//! What a brand's logo rests on: Gmail's DMARC verdict on a message (its own
//! `Authentication-Results`), and the sending domain's DMARC and BIMI records in DNS.

use exo_store::Header;

/// The domain whose policy Gmail applied to a message that passed DMARC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Passed {
    /// `header.from`'s domain, lower-cased.
    pub(super) domain: String,
}

/// Gmail's verdict in `headers`: its own `Authentication-Results` (the first naming
/// `mx.google.com`, which Gmail adds above any the message came with) saying `dmarc=pass` under a
/// policy that quarantines or rejects mail that fails (the subdomain policy for a subdomain).
pub(super) fn passed(headers: &[Header]) -> Option<Passed> {
    let results = headers.iter().find_map(|header| {
        let value = header
            .name
            .eq_ignore_ascii_case("Authentication-Results")
            .then_some(header.value.as_str())?;
        let (server, results) = value.split_once(';')?;
        (server.split_whitespace().next()? == "mx.google.com").then_some(results)
    })?;
    let dmarc = outside_comments(results)
        .into_iter()
        .find(|(method, _)| method.trim().starts_with("dmarc="))?;
    let (method, comment) = dmarc;
    let mut words = method.split_whitespace();
    if words.next()? != "dmarc=pass" {
        return None;
    }
    let domain = words
        .find_map(|word| word.strip_prefix("header.from="))?
        .to_lowercase();
    let tag = |name: &str| {
        comment
            .split_whitespace()
            .find_map(|tag| tag.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_uppercase)
    };
    let organization = psl::domain_str(&domain).unwrap_or(&domain);
    let policy = match tag("sp") {
        Some(policy) if organization != domain => policy,
        _ => tag("p")?,
    };
    matches!(policy.as_str(), "QUARANTINE" | "REJECT").then_some(Passed { domain })
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

/// Whether `record` is a DMARC record (`v=DMARC1` first) that applies its policy to all of the
/// mail that fails (`pct=100`, the default).
pub(super) fn dmarc_applies_to_all(record: &str) -> bool {
    let tags = tags(record);
    let version = tags
        .first()
        .map(|(tag, value)| (tag.as_str(), value.as_str()));
    version == Some(("v", "DMARC1"))
        && tags
            .iter()
            .find(|(tag, _)| tag == "pct")
            .is_none_or(|(_, pct)| pct == "100")
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

    fn results(value: &str) -> Vec<Header> {
        vec![Header {
            name: "Authentication-Results".into(),
            value: value.into(),
        }]
    }

    #[test]
    fn gmails_dmarc_pass_under_an_enforcing_policy_counts() {
        let gmail = "mx.google.com;\r\n       dkim=pass header.i=@brand.example header.s=s1;\r\n       spf=pass (google.com: domain of bounce@brand.example designates 192.0.2.1 as permitted sender) smtp.mailfrom=bounce@brand.example;\r\n       dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=Brand.example";
        assert_eq!(
            passed(&results(gmail)),
            Some(Passed {
                domain: "brand.example".into()
            })
        );
        let monitoring = gmail.replace("p=REJECT sp=REJECT", "p=NONE sp=NONE");
        assert_eq!(
            passed(&results(&monitoring)),
            None,
            "p=none enforces nothing"
        );
        let failed = gmail.replace("dmarc=pass", "dmarc=fail");
        assert_eq!(passed(&results(&failed)), None);
        let forged = gmail.replace("mx.google.com", "mx.attacker.example");
        assert_eq!(passed(&results(&forged)), None, "only Gmail's own counts");
    }

    #[test]
    fn a_subdomain_takes_the_subdomain_policy() {
        let mail =
            "mx.google.com; dmarc=pass (p=REJECT sp=NONE dis=NONE) header.from=news.brand.co.uk";
        assert_eq!(passed(&results(mail)), None);
        let enforced = mail.replace("sp=NONE", "sp=QUARANTINE");
        assert!(passed(&results(&enforced)).is_some());
    }

    #[test]
    fn records_are_read_by_their_tags() {
        assert!(dmarc_applies_to_all(
            "v=DMARC1; p=reject; rua=mailto:d@brand.example"
        ));
        assert!(!dmarc_applies_to_all("v=DMARC1; p=quarantine; pct=50"));
        assert!(!dmarc_applies_to_all("v=spf1 -all"));
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
