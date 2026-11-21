//! Whether a brand's mark certificate (a Verified Mark or Common Mark Certificate) vouches for its
//! logo, checked as Gmail checks it: the chain up to one of the roots Gmail trusts for marks, every
//! certificate on it in date and issued for BIMI (its extended key usage), the sending domain or
//! its organizational domain among the certificate's names, and the logo's hash among those of the
//! logo the certificate carries (its logotype extension, RFC 3709). And whether its CA has revoked
//! it since, by the CA's revocation list.

use std::{sync::LazyLock, time::Duration};

use jiff::Timestamp;
use rustls_pki_types::{CertificateDer, ServerName, TrustAnchor, UnixTime, pem::PemObject};

/// `id-kp-BrandIndicatorforMessageIdentification` (1.3.6.1.5.5.7.3.31), the extended key usage of
/// mark certificates and of the CAs that issue them.
const BRAND_INDICATOR: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x1f];
/// `id-pe-logotype` (1.3.6.1.5.5.7.1.12), the extension that carries the mark.
const LOGOTYPE: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x0c];
/// `id-ce-cRLDistributionPoints` (2.5.29.31), the extension that says where the CA's revocation
/// list is.
const CRL_DISTRIBUTION_POINTS: &[u8] = &[0x55, 0x1d, 0x1f];

/// The DER tags read here.
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const SEQUENCE: u8 = 0x30;
/// Context-specific constructed tags `[0]`, `[2]` and `[3]`.
const CONTEXT_0: u8 = 0xa0;
const CONTEXT_2: u8 = 0xa2;
const CONTEXT_3: u8 = 0xa3;
/// A GeneralName's `uniformResourceIdentifier [6]`.
const URI: u8 = 0x86;

/// The roots Gmail trusts for mark certificates: the issuers the BIMI Group lists.
static ROOTS: LazyLock<Vec<TrustAnchor<'static>>> = LazyLock::new(|| {
    macro_rules! root {
        ($name:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/mark-roots/",
                $name
            ))
            .as_slice()
        };
    }
    [
        root!("DigiCertVerifiedMarkRootCA.pem"),
        root!("GlobalSignVerifiedMarkRootR42.pem"),
        root!("SSLcomVMCECCRootCA2024.pem"),
        root!("SSLcomVMCRSARootCA2024.pem"),
    ]
    .into_iter()
    .filter_map(|pem| {
        let der = CertificateDer::from_pem_slice(pem).ok()?;
        Some(webpki::anchor_from_trusted_cert(&der).ok()?.to_owned())
    })
    .collect()
});

/// Why a certificate doesn't vouch for a logo.
#[derive(Debug, thiserror::Error)]
pub(super) enum Invalid {
    #[error("the file holds no certificate")]
    Empty,
    #[error("the certificate doesn't parse")]
    Malformed,
    #[error("the certificate doesn't validate: {0}")]
    Chain(#[from] webpki::Error),
    #[error("the certificate doesn't name {0}")]
    Domain(String),
    #[error("the logo isn't the certificate's")]
    Logo,
}

/// Until when the mark certificate `pem` (the certificate, then its issuers) vouches for `svg`, an
/// uncompressed SVG, as the logo of the sending domain `domain`, at `now`.
pub(super) fn validate(
    pem: &[u8],
    svg: &[u8],
    domain: &str,
    now: Timestamp,
) -> Result<Timestamp, Invalid> {
    validate_against(&ROOTS, pem, svg, domain, now)
}

fn validate_against(
    roots: &[TrustAnchor<'_>],
    pem: &[u8],
    svg: &[u8],
    domain: &str,
    now: Timestamp,
) -> Result<Timestamp, Invalid> {
    let chain = chain(pem)?;
    let certificate = verify(roots, &chain, now, None)?;
    if !names(&certificate, domain) {
        return Err(Invalid::Domain(domain.to_owned()));
    }
    let leaf = Leaf::read(&chain[0]).ok_or(Invalid::Malformed)?;
    let vouched = leaf
        .hashes
        .iter()
        .any(|&(algorithm, hash)| digest(algorithm, svg).is_some_and(|svg| svg.as_ref() == hash));
    if !vouched {
        return Err(Invalid::Logo);
    }
    Ok(leaf.not_after)
}

/// Whether `crl`, a revocation list, says the CA revoked the certificate in `pem` (as
/// [`validate`] takes it), at `now`. `None` when the list can't tell: it isn't that CA's, it's
/// past its next update, or it doesn't verify.
pub(super) fn revoked(pem: &[u8], crl: &[u8], now: Timestamp) -> Option<bool> {
    revoked_against(&ROOTS, pem, crl, now)
}

fn revoked_against(
    roots: &[TrustAnchor<'_>],
    pem: &[u8],
    crl: &[u8],
    now: Timestamp,
) -> Option<bool> {
    let chain = chain(pem).ok()?;
    let crl =
        webpki::CertRevocationList::from(webpki::BorrowedCertRevocationList::from_der(crl).ok()?);
    let crls = [&crl];
    let revocation = webpki::RevocationOptionsBuilder::new(&crls)
        .ok()?
        .with_depth(webpki::RevocationCheckDepth::EndEntity)
        .with_status_policy(webpki::UnknownStatusPolicy::Deny)
        .with_expiration_policy(webpki::ExpirationPolicy::Enforce)
        .build();
    match verify(roots, &chain, now, Some(revocation)) {
        Ok(_) => Some(false),
        Err(Invalid::Chain(webpki::Error::CertRevoked)) => Some(true),
        Err(invalid) => {
            log::info!("a revocation list can't tell: {invalid}");
            None
        }
    }
}

/// Where the certificate in `pem` says its CA's revocation list is: its CRL distribution points'
/// URLs, in order.
pub(super) fn revocation_lists(pem: &[u8]) -> Vec<String> {
    let chain = chain(pem).unwrap_or_default();
    let points = chain
        .first()
        .and_then(|der| extension(der, CRL_DISTRIBUTION_POINTS))
        .and_then(|points| first(points, SEQUENCE))
        .unwrap_or_default();
    // DistributionPoint ::= SEQUENCE { distributionPoint [0] DistributionPointName, … }, whose
    // `fullName [0]` holds GeneralNames.
    elements(points)
        .filter_map(|point| first(point.contents, CONTEXT_0))
        .filter_map(|name| first(name, CONTEXT_0))
        .flat_map(elements)
        .filter(|name| name.tag == URI)
        .filter_map(|uri| String::from_utf8(uri.contents.to_vec()).ok())
        .collect()
}

/// The certificates in `pem`: the end entity's, then its issuers'.
fn chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, Invalid> {
    CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Invalid::Malformed)
}

/// The end entity of `chain`, once its chain up to `roots` checks at `now` for BIMI, and with
/// `revocation`, its CA's revocation list.
fn verify<'a>(
    roots: &[TrustAnchor<'_>],
    chain: &'a [CertificateDer<'a>],
    now: Timestamp,
    revocation: Option<webpki::RevocationOptions<'_>>,
) -> Result<webpki::EndEntityCert<'a>, Invalid> {
    let (der, issuers) = chain.split_first().ok_or(Invalid::Empty)?;
    let certificate = webpki::EndEntityCert::try_from(der)?;
    let time = UnixTime::since_unix_epoch(Duration::from_secs(
        u64::try_from(now.as_second()).unwrap_or_default(),
    ));
    certificate.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        roots,
        issuers,
        time,
        webpki::KeyUsage::required(BRAND_INDICATOR),
        revocation,
        None,
    )?;
    Ok(certificate)
}

/// Whether `certificate` names `domain` or its organizational domain, as itself or as its default
/// BIMI selector (`default._bimi.<domain>`).
fn names(certificate: &webpki::EndEntityCert<'_>, domain: &str) -> bool {
    let organization = psl::domain_str(domain).unwrap_or(domain);
    [domain, organization]
        .into_iter()
        .flat_map(|domain| [domain.to_owned(), format!("default._bimi.{domain}")])
        .filter_map(|name| ServerName::try_from(name).ok())
        .any(|name| certificate.verify_is_valid_for_subject_name(&name).is_ok())
}

/// `data`'s digest by the hash algorithm the OID `algorithm` names, of those mark certificates use.
fn digest(algorithm: &[u8], data: &[u8]) -> Option<ring::digest::Digest> {
    let algorithm = match algorithm {
        // 1.3.14.3.2.26
        [0x2b, 0x0e, 0x03, 0x02, 0x1a] => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        // 2.16.840.1.101.3.4.2.1, .2 and .3
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01] => &ring::digest::SHA256,
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02] => &ring::digest::SHA384,
        [0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03] => &ring::digest::SHA512,
        _ => return None,
    };
    Some(ring::digest::digest(algorithm, data))
}

/// What a certificate says that its chain doesn't prove: when it expires, and the hashes of the
/// SVG logo it carries, each with its algorithm's OID.
struct Leaf<'a> {
    not_after: Timestamp,
    hashes: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Leaf<'a> {
    fn read(der: &'a [u8]) -> Option<Self> {
        let tbs = first(first(der, SEQUENCE)?, SEQUENCE)?;
        // An optional version, then the serial number, signature algorithm, issuer and validity.
        let validity = elements(tbs)
            .skip_while(|field| field.tag == CONTEXT_0)
            .nth(3)?;
        let not_after = time(elements(validity.contents).nth(1)?)?;
        Some(Self {
            not_after,
            hashes: svg_hashes(extension(der, LOGOTYPE)?),
        })
    }
}

/// The value of the certificate `der`'s extension `oid`.
fn extension<'a>(der: &'a [u8], oid: &[u8]) -> Option<&'a [u8]> {
    let tbs = first(first(der, SEQUENCE)?, SEQUENCE)?;
    let extensions = first(first(tbs, CONTEXT_3)?, SEQUENCE)?;
    elements(extensions).find_map(|extension| {
        let mut parts = elements(extension.contents);
        let id = parts.next()?;
        if (id.tag, id.contents) != (OID, oid) {
            return None;
        }
        // Past an optional `critical`.
        parts
            .find(|part| part.tag == OCTET_STRING)
            .map(|value| value.contents)
    })
}

/// The hashes of the SVG images of a logotype extension's subject logo, each with its algorithm.
fn svg_hashes(logotype: &[u8]) -> Vec<(&[u8], &[u8])> {
    // LogotypeExtn ::= SEQUENCE { …, subjectLogo [2] EXPLICIT LogotypeInfo, … }, whose
    // `direct [0] IMPLICIT LogotypeData` starts with `image SEQUENCE OF LogotypeImage`.
    let images = first(logotype, SEQUENCE)
        .and_then(|logotype| first(logotype, CONTEXT_2))
        .and_then(|subject| first(subject, CONTEXT_0))
        .and_then(|direct| first(direct, SEQUENCE))
        .unwrap_or_default();
    // LogotypeImage ::= SEQUENCE { imageDetails LogotypeDetails, … }, and
    // LogotypeDetails ::= SEQUENCE { mediaType IA5String, logotypeHash SEQUENCE OF HashAlgAndValue,
    // logotypeURI … }.
    elements(images)
        .filter_map(|image| first(image.contents, SEQUENCE))
        .filter_map(|details| {
            let mut fields = elements(details);
            let media = fields.next()?;
            (media.contents == b"image/svg+xml").then(|| fields.next())?
        })
        .flat_map(|hashes| elements(hashes.contents))
        .filter_map(|hash| {
            // HashAlgAndValue ::= SEQUENCE { hashAlg AlgorithmIdentifier, hashValue OCTET STRING }
            let mut parts = elements(hash.contents);
            let algorithm = first(parts.next()?.contents, OID)?;
            Some((algorithm, parts.next()?.contents))
        })
        .collect()
}

/// A certificate's UTCTime or GeneralizedTime.
fn time(element: Element<'_>) -> Option<Timestamp> {
    let text = std::str::from_utf8(element.contents).ok()?;
    let text = match element.tag {
        // Two-digit years from 50 are the 1900s (RFC 5280).
        UTC_TIME if text.get(..2)? >= "50" => format!("19{text}"),
        UTC_TIME => format!("20{text}"),
        GENERALIZED_TIME => text.to_owned(),
        _ => return None,
    };
    let time = jiff::civil::DateTime::strptime("%Y%m%d%H%M%SZ", &text).ok()?;
    Some(time.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp())
}

/// A DER element: its tag (in the short form, as all of those read here are) and contents.
#[derive(Clone, Copy)]
struct Element<'a> {
    tag: u8,
    contents: &'a [u8],
}

/// The DER elements of `input` in order, up to the first that doesn't parse.
fn elements(mut input: &[u8]) -> impl Iterator<Item = Element<'_>> {
    std::iter::from_fn(move || {
        let (&tag, rest) = input.split_first()?;
        if tag & 0x1f == 0x1f {
            return None;
        }
        let (&length, rest) = rest.split_first()?;
        let (length, rest) = match length {
            0..=0x7f => (usize::from(length), rest),
            // The long form: that many bytes of length, big-endian.
            0x81..=0x84 => {
                let (bytes, rest) = rest.split_at_checked(usize::from(length & 0x7f))?;
                let length = bytes
                    .iter()
                    .fold(0, |length, &byte| length << 8 | usize::from(byte));
                (length, rest)
            }
            _ => return None,
        };
        let (contents, rest) = rest.split_at_checked(length)?;
        input = rest;
        Some(Element { tag, contents })
    })
}

/// The contents of the first of `input`'s elements tagged `tag`.
fn first(input: &[u8], tag: u8) -> Option<&[u8]> {
    elements(input)
        .find(|element| element.tag == tag)
        .map(|element| element.contents)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use rcgen::{
        BasicConstraints, CertificateParams, CertificateRevocationListParams, CertifiedIssuer,
        CrlDistributionPoint, CrlIssuingDistributionPoint, CustomExtension, DistinguishedName,
        DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair, RevokedCertParams,
        SerialNumber, date_time_ymd,
    };

    use super::*;

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" version="1.2" baseProfile="tiny-ps" viewBox="0 0 96 96"><title>Brand</title><rect width="96" height="96" fill="#0a7"/></svg>"##;
    const SHA1: &[u8] = &[0x2b, 0x0e, 0x03, 0x02, 0x1a];
    const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    const BRAND_INDICATOR: [u64; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 31];
    /// Where the test CA's certificates say its revocation list is, and its mirror.
    const LISTS: [&str; 2] = ["http://crl.example/ca.crl", "http://crl2.example/ca.crl"];
    /// The serial number of the certificates the CA issues here.
    const SERIAL: &[u8] = &[0x42];

    fn now() -> Timestamp {
        "2026-06-01T00:00:00Z".parse().expect("a time")
    }

    /// A DER element tagged `tag` holding `parts`.
    fn der(tag: u8, parts: &[&[u8]]) -> Vec<u8> {
        let contents = parts.concat();
        let length = contents.len();
        let mut element = vec![tag];
        match u8::try_from(length) {
            Ok(short @ 0..0x80) => element.push(short),
            Ok(byte) => element.extend([0x81, byte]),
            Err(_) => element.extend([0x82, (length >> 8) as u8, length as u8]),
        }
        element.extend(contents);
        element
    }

    /// A logotype extension carrying `svg` with its hash by `algorithm`, as mark certificates do.
    fn logotype(svg: &[u8], algorithm: &[u8]) -> Vec<u8> {
        let hash = digest(algorithm, svg).expect("a hash we know");
        let uri = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        let details = der(
            SEQUENCE,
            &[
                &der(0x16, &[b"image/svg+xml"]),
                &der(
                    SEQUENCE,
                    &[&der(
                        SEQUENCE,
                        &[
                            &der(SEQUENCE, &[&der(OID, &[algorithm])]),
                            &der(OCTET_STRING, &[hash.as_ref()]),
                        ],
                    )],
                ),
                &der(SEQUENCE, &[&der(0x16, &[uri.as_bytes()])]),
            ],
        );
        let images = der(SEQUENCE, &[&der(SEQUENCE, &[&details])]);
        der(SEQUENCE, &[&der(CONTEXT_2, &[&der(CONTEXT_0, &[&images])])])
    }

    /// A mark root, the CA it issued, and what the CA issues.
    struct Marks {
        root: CertifiedIssuer<'static, KeyPair>,
        ca: CertifiedIssuer<'static, KeyPair>,
    }

    impl Marks {
        fn new() -> Self {
            Self::with_ca_usages(vec![ExtendedKeyUsagePurpose::Other(
                BRAND_INDICATOR.to_vec(),
            )])
        }

        fn with_ca_usages(usages: Vec<ExtendedKeyUsagePurpose>) -> Self {
            let authority = |name: &str| {
                let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
                params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                params.distinguished_name = DistinguishedName::new();
                params.distinguished_name.push(DnType::CommonName, name);
                params
            };
            let root = CertifiedIssuer::self_signed(
                authority("Mark Root"),
                KeyPair::generate().expect("a key"),
            )
            .expect("the root");
            let mut ca = authority("Mark CA");
            ca.extended_key_usages = usages;
            let ca = CertifiedIssuer::signed_by(ca, KeyPair::generate().expect("a key"), &root)
                .expect("the CA");
            Self { root, ca }
        }

        fn anchors(&self) -> Vec<TrustAnchor<'static>> {
            vec![
                webpki::anchor_from_trusted_cert(self.root.der())
                    .expect("an anchor")
                    .to_owned(),
            ]
        }

        /// The PEM a brand serves: its certificate for `names`, carrying `logotype`, then the CA.
        fn pem(
            &self,
            names: &[&str],
            logotype: Vec<u8>,
            usages: Vec<ExtendedKeyUsagePurpose>,
        ) -> String {
            let mut params = CertificateParams::new(
                names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>(),
            )
            .expect("params");
            params.not_before = date_time_ymd(2026, 1, 1);
            params.not_after = date_time_ymd(2027, 1, 1);
            params.serial_number = Some(SerialNumber::from_slice(SERIAL));
            params.crl_distribution_points = vec![CrlDistributionPoint {
                uris: LISTS.map(str::to_owned).to_vec(),
            }];
            params.extended_key_usages = usages;
            params.custom_extensions = vec![CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 5, 5, 7, 1, 12],
                logotype,
            )];
            let leaf = params
                .signed_by(&KeyPair::generate().expect("a key"), &self.ca)
                .expect("the certificate");
            format!("{}{}", leaf.pem(), self.ca.pem())
        }

        fn brand(&self, names: &[&str]) -> String {
            self.pem(
                names,
                logotype(SVG, SHA1),
                vec![ExtendedKeyUsagePurpose::Other(BRAND_INDICATOR.to_vec())],
            )
        }

        /// A revocation list from `issuer`, listing `serials`, current until `next_update`.
        fn crl(
            issuer: &CertifiedIssuer<'static, KeyPair>,
            serials: &[&[u8]],
            next_update: (i32, u8, u8),
        ) -> Vec<u8> {
            let params = CertificateRevocationListParams {
                this_update: date_time_ymd(2026, 5, 1),
                next_update: date_time_ymd(next_update.0, next_update.1, next_update.2),
                crl_number: SerialNumber::from(1u64),
                issuing_distribution_point: Some(CrlIssuingDistributionPoint {
                    distribution_point: CrlDistributionPoint {
                        uris: LISTS.map(str::to_owned).to_vec(),
                    },
                    scope: None,
                }),
                revoked_certs: serials
                    .iter()
                    .map(|serial| RevokedCertParams {
                        serial_number: SerialNumber::from_slice(serial),
                        revocation_time: date_time_ymd(2026, 4, 1),
                        reason_code: None,
                        invalidity_date: None,
                    })
                    .collect(),
                key_identifier_method: KeyIdMethod::Sha256,
            };
            params
                .signed_by(issuer)
                .expect("the revocation list")
                .der()
                .to_vec()
        }

        fn revoked(&self, pem: &str, crl: &[u8]) -> Option<bool> {
            revoked_against(&self.anchors(), pem.as_bytes(), crl, now())
        }

        fn validate(
            &self,
            pem: &str,
            svg: &[u8],
            domain: &str,
            now: Timestamp,
        ) -> Result<Timestamp, Invalid> {
            validate_against(&self.anchors(), pem.as_bytes(), svg, domain, now)
        }
    }

    #[test]
    fn a_mark_certificate_vouches_for_its_logo_until_it_expires() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        let until = marks.validate(&pem, SVG, "brand.example", now());
        assert_eq!(
            until.ok(),
            Some("2027-01-01T00:00:00Z".parse().expect("a time"))
        );
        // A subdomain's mail takes its organizational domain's certificate.
        assert!(
            marks
                .validate(&pem, SVG, "news.brand.example", now())
                .is_ok()
        );
        // The hash may be any of those certificates use.
        let pem = marks.pem(
            &["brand.example"],
            logotype(SVG, SHA256),
            vec![ExtendedKeyUsagePurpose::Other(BRAND_INDICATOR.to_vec())],
        );
        assert!(marks.validate(&pem, SVG, "brand.example", now()).is_ok());
        // Past its end, it vouches for nothing.
        let later = "2027-02-01T00:00:00Z".parse().expect("a time");
        assert!(matches!(
            marks.validate(&pem, SVG, "brand.example", later),
            Err(Invalid::Chain(webpki::Error::CertExpired { .. }))
        ));
    }

    #[test]
    fn a_certificate_names_its_domain_or_its_default_selector() {
        let marks = Marks::new();
        let selector = marks.brand(&["default._bimi.brand.example"]);
        assert!(
            marks
                .validate(&selector, SVG, "brand.example", now())
                .is_ok()
        );
        let other = marks.brand(&["other.example", "selector._bimi.brand.example"]);
        assert!(matches!(
            marks.validate(&other, SVG, "brand.example", now()),
            Err(Invalid::Domain(_))
        ));
    }

    #[test]
    fn a_logo_the_certificate_doesnt_carry_isnt_vouched_for() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        let other = String::from_utf8_lossy(SVG).replace("#0a7", "#d00");
        assert!(matches!(
            marks.validate(&pem, other.as_bytes(), "brand.example", now()),
            Err(Invalid::Logo)
        ));
    }

    #[test]
    fn only_bimi_certificates_from_the_mark_roots_count() {
        let marks = Marks::new();
        // Not issued by a root Gmail trusts for marks.
        let pem = marks.brand(&["brand.example"]);
        assert!(matches!(
            validate(pem.as_bytes(), SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::UnknownIssuer))
        ));
        // A certificate for TLS isn't one for BIMI.
        let tls = marks.pem(
            &["brand.example"],
            logotype(SVG, SHA1),
            vec![ExtendedKeyUsagePurpose::ServerAuth],
        );
        assert!(matches!(
            marks.validate(&tls, SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::RequiredEkuNotFoundContext(_)))
        ));
        // Nor is one from a CA that doesn't issue them.
        let tls_ca = Marks::with_ca_usages(vec![ExtendedKeyUsagePurpose::ServerAuth]);
        let pem = tls_ca.brand(&["brand.example"]);
        assert!(matches!(
            tls_ca.validate(&pem, SVG, "brand.example", now()),
            Err(Invalid::Chain(webpki::Error::RequiredEkuNotFoundContext(_)))
        ));
        assert!(matches!(
            marks.validate("", SVG, "brand.example", now()),
            Err(Invalid::Empty)
        ));
    }

    #[test]
    fn a_cas_revocation_list_says_whether_it_revoked_a_certificate() {
        let marks = Marks::new();
        let pem = marks.brand(&["brand.example"]);
        assert_eq!(revocation_lists(pem.as_bytes()), LISTS);
        let listed = Marks::crl(&marks.ca, &[SERIAL], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &listed), Some(true));
        let others = Marks::crl(&marks.ca, &[&[0x07]], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &others), Some(false));
        // A list past its next update, the root's (not the CA's), or no list at all can't tell.
        let stale = Marks::crl(&marks.ca, &[SERIAL], (2026, 5, 15));
        assert_eq!(marks.revoked(&pem, &stale), None);
        let roots = Marks::crl(&marks.root, &[SERIAL], (2026, 7, 1));
        assert_eq!(marks.revoked(&pem, &roots), None);
        assert_eq!(marks.revoked(&pem, b"not a list"), None);
    }

    #[test]
    fn the_mark_roots_all_load() {
        assert_eq!(ROOTS.len(), 4);
    }

    #[test]
    fn certificate_times_read_both_forms() {
        let utc = Element {
            tag: UTC_TIME,
            contents: b"270901235959Z",
        };
        assert_eq!(time(utc), "2027-09-01T23:59:59Z".parse().ok());
        let generalized = Element {
            tag: GENERALIZED_TIME,
            contents: b"20510101000000Z",
        };
        assert_eq!(time(generalized), "2051-01-01T00:00:00Z".parse().ok());
    }
}
