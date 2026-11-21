//! Brand logos from BIMI, which show where a contact's photo would for a sender Google has no
//! photo of: the contact pane, notifications and the calendar's guests.
//!
//! A domain earns its logo through its mail. A message Gmail recorded as passing DMARC under a
//! policy that quarantines or rejects all of the mail that fails ([`records::passed`], the
//! domain's `_dmarc` record) sends Exo to the domain's BIMI record (`default._bimi`, through
//! macOS's resolver). Its logo shows only with a Verified Mark or Common Mark Certificate that
//! vouches for it ([`certificate`]), so another domain can't show a brand it doesn't own. Lookups
//! are kept with the account (`exo_store::BrandRecord`) and made again after the record's TTL, at
//! most once a day. The logo and certificate are fetched once (HTTPS only), and the logo, drawn,
//! is kept with the photos until the certificate expires, or until its CA's revocation list,
//! fetched about once a day, revokes it. Nothing a message names is ever fetched: the record comes
//! from DNS. `Block all remote images` stops these fetches, as the sender's DNS names their URLs,
//! and leaves the logos already validated.

mod certificate;
mod dns;
mod records;

use std::{
    collections::HashMap,
    io::Read as _,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use exo_store::{BrandCheck, BrandRecord, Header, RemoteContentBlocking, Settings, Store};
use exo_ui::AvatarPhoto;
use jiff::Timestamp;

/// The shortest a lookup is kept: a record is looked up again at most once a day. A CA's
/// revocation list is kept as long, and the logos are checked against it as often.
const KEPT: Duration = Duration::from_secs(24 * 60 * 60);
/// How long fetching a brand's logo or certificate, or a revocation list, may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The most a logo, certificate or revocation list may weigh: eight times the 32 KB Gmail
/// recommends for a logo.
const MAX_FILE: usize = 256 << 10;

/// The validated logos so far, by domain. It lives in the context's data, so whatever draws an
/// avatar can find it ([`logo`]).
type Shown = Arc<Mutex<HashMap<String, AvatarPhoto>>>;

fn shown(ctx: &egui::Context) -> Shown {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Shown>(egui::Id::new("exo::brands"))
            .clone()
    })
}

/// The validated logo of `email`'s domain, once mail from it has earned one.
pub(crate) fn logo(ctx: &egui::Context, email: &str) -> Option<AvatarPhoto> {
    let domain = email.rsplit_once('@')?.1.to_lowercase();
    let shown = shown(ctx);
    let shown = shown.lock().unwrap_or_else(PoisonError::into_inner);
    shown.get(&domain).cloned()
}

/// An account's lookups: its store, which keeps them, its photo cache, which keeps the logos, and
/// the context whose avatars show them.
#[derive(Clone)]
pub(crate) struct Brands {
    ctx: egui::Context,
    store: Store,
    dir: PathBuf,
    /// For logos and certificates, over HTTPS only.
    http: crate::remote_content::Client,
    /// For revocation lists, which CAs serve over plain HTTP: they're signed.
    lists: crate::remote_content::Client,
    /// Whether DNS may be asked. Not on gmail-mock, whose domains aren't the world's.
    lookups: bool,
}

impl Brands {
    /// `store`'s brands, whose logos are kept in `dir`, the account's photo cache.
    pub(crate) fn new(ctx: &egui::Context, store: Store, dir: PathBuf, lookups: bool) -> Self {
        Self {
            ctx: ctx.clone(),
            store,
            dir,
            http: crate::remote_content::Client::https_only(FETCH_TIMEOUT),
            lists: crate::remote_content::Client::new(FETCH_TIMEOUT, false),
            lookups,
        }
    }

    /// The context whose avatars show the logos.
    pub(crate) fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// The logo (a PNG) of the brand that sent the message with `headers`, looked up first when
    /// that's due, which shows wherever its people are drawn ([`logo`]) from then on. `None` when
    /// the message or its domain doesn't earn one.
    pub(crate) async fn logo_for(&self, headers: &[Header]) -> Option<Vec<u8>> {
        let domain = records::passed(headers)?.domain;
        let now = Timestamp::now();
        let kept = match self.store.brands().await {
            Ok(mut brands) => brands.remove(&domain),
            Err(error) => {
                log::warn!("reading the brands looked up: {error}");
                None
            }
        };
        let mut record = match kept {
            Some(record) if record.next > now => record,
            _ if !self.lookups => return None,
            kept => {
                let record = carried(kept, look_up(&domain, now).await?, now);
                self.save(&domain, &record).await;
                record
            }
        };
        let png = self.checked_logo(&domain, &mut record, now).await;
        let photo = png.as_deref().and_then(|png| decode(&self.ctx, png));
        let shown = shown(&self.ctx);
        let mut shown = shown.lock().unwrap_or_else(PoisonError::into_inner);
        match photo {
            Some(photo) => shown.insert(domain, photo),
            // A domain whose record or certificate no longer earns it a logo.
            None => shown.remove(&domain),
        };
        drop(shown);
        self.ctx.request_repaint();
        png
    }

    /// Shows the logos earlier runs validated, then checks them against their CAs' revocation
    /// lists, at once and each day after.
    pub(crate) async fn watch(&self) {
        self.restore().await;
        loop {
            self.recheck().await;
            tokio::time::sleep(KEPT).await;
        }
    }

    /// Shows the logos kept from earlier runs whose certificates are still in date.
    async fn restore(&self) {
        let brands = match self.store.brands().await {
            Ok(brands) => brands,
            Err(error) => {
                log::warn!("reading the brands looked up: {error}");
                return;
            }
        };
        let now = Timestamp::now();
        let logos: Vec<_> = brands
            .into_iter()
            .filter(|(_, record)| is_valid(record.checked, now))
            .filter_map(|(domain, record)| {
                let (png, _) = self.kept(&record)?;
                Some((domain, decode(&self.ctx, &png)?))
            })
            .collect();
        shown(&self.ctx)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(logos);
        self.ctx.request_repaint();
    }

    /// Checks the certificates of the logos validated so far against their CAs' revocation lists.
    /// A revoked certificate's logo goes, and its record waits for its next lookup, as a refused
    /// one does.
    async fn recheck(&self) {
        let brands = match self.store.brands().await {
            Ok(brands) => brands,
            Err(error) => {
                log::warn!("reading the brands looked up: {error}");
                return;
            }
        };
        let now = Timestamp::now();
        for (domain, mut record) in brands {
            if !is_valid(record.checked, now) {
                continue;
            }
            let Some((_, pem)) = self.kept(&record) else {
                continue;
            };
            if self.revoked(&pem, now).await != Some(true) {
                continue;
            }
            log::info!("{domain}'s certificate is revoked: its logo goes");
            record.checked = Some(BrandCheck::Invalid);
            self.save(&domain, &record).await;
            shown(&self.ctx)
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&domain);
            self.ctx.request_repaint();
        }
    }

    /// The logo `record` names, drawn, when its certificate vouches for it: the copy kept with the
    /// photos, else fetched and checked, which `record` keeps the outcome of. A fetch that fails,
    /// like a certificate that doesn't vouch for the logo, is tried again when the record is next
    /// looked up. The sender's servers answer these fetches, so one that always failed would
    /// otherwise hear from Exo with each of the domain's messages.
    async fn checked_logo(
        &self,
        domain: &str,
        record: &mut BrandRecord,
        now: Timestamp,
    ) -> Option<Vec<u8>> {
        let (Some(logo), Some(certificate)) = (record.logo.clone(), record.certificate.clone())
        else {
            return None;
        };
        let kept = self.kept(record);
        match step(record.checked, kept.is_some(), self.blocked().await, now) {
            Step::Kept => return kept.map(|(png, _)| png),
            Step::Nothing => return None,
            Step::Fetch => {}
        }
        let (svg, pem) = futures::join!(fetch(&self.http, &logo), fetch(&self.http, &certificate));
        let drawn = match svg.and_then(uncompressed).zip(pem) {
            Some((svg, pem)) => self.vouched(domain, &svg, &pem, now).await,
            None => None,
        };
        record.checked = Some(match &drawn {
            Some((until, png, pem)) => {
                for (url, bytes) in [(&logo, png), (&certificate, pem)] {
                    if let Err(error) = crate::people::save(&self.file(url), bytes) {
                        log::warn!("keeping {domain}'s logo: {error}");
                    }
                }
                BrandCheck::Valid { until: *until }
            }
            None => BrandCheck::Invalid,
        });
        self.save(domain, record).await;
        drawn.map(|(_, png, _)| png)
    }

    /// `svg` drawn, with until when the certificate `pem` vouches for it as `domain`'s logo, and
    /// `pem` itself, unless the certificate doesn't vouch for it or its CA has revoked it. A
    /// revocation list that can't be had doesn't stand in the way.
    async fn vouched(
        &self,
        domain: &str,
        svg: &[u8],
        pem: &[u8],
        now: Timestamp,
    ) -> Option<(Timestamp, Vec<u8>, Vec<u8>)> {
        let refused = |why: &dyn std::fmt::Display| {
            log::info!("not showing {domain}'s logo: {why}");
        };
        let until = certificate::validate(pem, svg, domain, now)
            .inspect_err(|invalid| refused(invalid))
            .ok()?;
        if self.revoked(pem, now).await == Some(true) {
            refused(&"its certificate is revoked");
            return None;
        }
        let png = draw(svg);
        if png.is_none() {
            refused(&"it doesn't draw");
        }
        Some((until, png?, pem.to_vec()))
    }

    /// Whether the CA of the certificate in `pem` has revoked it, by its revocation list
    /// ([`revocation_list`](Self::revocation_list)). `None` when the list can't be had or can't
    /// tell, which leaves the logo as the rest of its check left it, until a list that tells.
    async fn revoked(&self, pem: &[u8], now: Timestamp) -> Option<bool> {
        let list = self
            .revocation_list(&certificate::revocation_lists(pem))
            .await?;
        certificate::revoked(pem, &list, now)
    }

    /// The revocation list at `urls`, a CA's and its mirrors: the copy kept with the photos while
    /// it's under a day old, else the first of them that answers, kept, else the kept copy however
    /// old (one past its next update can't tell). Under `Block all remote images`, the kept copy.
    async fn revocation_list(&self, urls: &[String]) -> Option<Vec<u8>> {
        let file = self.file(urls.first()?);
        let kept = std::fs::read(&file).ok();
        let age = std::fs::metadata(&file)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        let fresh = kept.is_some() && age.is_some_and(|age| age < KEPT);
        if fresh || self.blocked().await {
            return kept;
        }
        for url in urls {
            if let Some(list) = fetch(&self.lists, url).await {
                if let Err(error) = crate::people::save(&file, &list) {
                    log::warn!("keeping a revocation list: {error}");
                }
                return Some(list);
            }
        }
        kept
    }

    /// Whether the account blocks all remote images, which stops these fetches too: the sender's
    /// DNS names their URLs. Read as each fetch is due, so a change applies at once. When the
    /// setting can't be read, nothing is fetched.
    async fn blocked(&self) -> bool {
        match self.store.settings::<Settings>().await {
            Ok(settings) => settings.remote_content_blocking == Some(RemoteContentBlocking::All),
            Err(error) => {
                log::warn!("reading the images setting: {error}");
                true
            }
        }
    }

    /// The drawn logo and the certificate `record` names, as kept with the photos.
    fn kept(&self, record: &BrandRecord) -> Option<(Vec<u8>, Vec<u8>)> {
        let png = std::fs::read(self.file(record.logo.as_deref()?)).ok()?;
        let pem = std::fs::read(self.file(record.certificate.as_deref()?)).ok()?;
        Some((png, pem))
    }

    /// Where what came from `url` is kept: a logo (drawn), a certificate or a revocation list.
    fn file(&self, url: &str) -> PathBuf {
        self.dir.join(crate::people::file_name(url))
    }

    async fn save(&self, domain: &str, record: &BrandRecord) {
        if let Err(error) = self.store.save_brand(domain, record).await {
            log::warn!("keeping a brand looked up: {error}");
        }
    }
}

/// The file at `url`, through `http`, up to [`MAX_FILE`].
async fn fetch(http: &crate::remote_content::Client, url: &str) -> Option<Vec<u8>> {
    match http.bytes(url, MAX_FILE).await {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => {
            log::info!("not fetching {url}: over {MAX_FILE} bytes");
            None
        }
        Err(error) => {
            log::info!("fetching {url}: {error}");
            None
        }
    }
}

/// What showing a brand's logo takes next.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The copy kept with the photos shows.
    Kept,
    /// The logo and certificate are fetched and checked.
    Fetch,
    /// Nothing shows.
    Nothing,
}

/// The step for a brand whose last check came to `checked`, whose logo and certificate are
/// `kept` with the photos or not, while remote images are `blocked` or not.
fn step(checked: Option<BrandCheck>, kept: bool, blocked: bool, now: Timestamp) -> Step {
    match checked {
        // A refused certificate, or files that couldn't be had, wait for the next lookup.
        Some(BrandCheck::Invalid) => Step::Nothing,
        checked if kept && is_valid(checked, now) => Step::Kept,
        _ if blocked => Step::Nothing,
        _ => Step::Fetch,
    }
}

/// Whether `checked` says a certificate vouches for its logo at `now`.
fn is_valid(checked: Option<BrandCheck>, now: Timestamp) -> bool {
    matches!(checked, Some(BrandCheck::Valid { until }) if until > now)
}

/// `fresh`, a new lookup of the domain whose last one is `kept`, keeping what `kept`'s check came
/// to while the record names the same files and the certificate is in date, so they aren't
/// fetched again.
fn carried(kept: Option<BrandRecord>, mut fresh: BrandRecord, now: Timestamp) -> BrandRecord {
    if let Some(kept) = kept
        && (&kept.logo, &kept.certificate) == (&fresh.logo, &fresh.certificate)
        && is_valid(kept.checked, now)
    {
        fresh.checked = kept.checked;
    }
    fresh
}

/// `domain`'s brand as DNS has it now: its BIMI record, when its DMARC policy applies to all of its
/// failing mail, and when to look again. `None` when DNS doesn't answer, to ask again next time.
async fn look_up(domain: &str, now: Timestamp) -> Option<BrandRecord> {
    // A domain without records of its own takes its organizational domain's.
    let organization = psl::domain_str(domain).unwrap_or(domain).to_owned();
    let lookup = async |prefix: &str| -> Result<Option<dns::Txt>, ()> {
        match dns::txt(&format!("{prefix}.{domain}")).await? {
            None if organization != domain => dns::txt(&format!("{prefix}.{organization}")).await,
            found => Ok(found),
        }
    };
    let dmarc = lookup("_dmarc").await.ok()?;
    let applies = dmarc.as_ref().is_some_and(|dmarc| {
        dmarc
            .records
            .iter()
            .any(|record| records::dmarc_applies_to_all(record))
    });
    let bimi = if applies {
        lookup("default._bimi").await.ok()?
    } else {
        None
    };
    let ttl = bimi
        .as_ref()
        .or(dmarc.as_ref())
        .map_or(KEPT, |found| found.ttl.max(KEPT));
    let record = bimi.and_then(|bimi| bimi.records.iter().find_map(|record| records::bimi(record)));
    Some(BrandRecord {
        next: now + jiff::SignedDuration::try_from(ttl).unwrap_or_default(),
        logo: record.as_ref().and_then(|record| record.logo.clone()),
        certificate: record.and_then(|record| record.certificate),
        checked: None,
    })
}

/// `logo` uncompressed when it's SVGZ, up to [`MAX_FILE`].
fn uncompressed(logo: Vec<u8>) -> Option<Vec<u8>> {
    if !logo.starts_with(&[0x1f, 0x8b]) {
        return Some(logo);
    }
    let mut svg = Vec::new();
    flate2::read::GzDecoder::new(logo.as_slice())
        .take(MAX_FILE as u64 + 1)
        .read_to_end(&mut svg)
        .ok()?;
    (svg.len() <= MAX_FILE).then_some(svg)
}

/// `svg` drawn as a PNG the size of the photos, fitted and centred on white: Gmail asks for a
/// square logo on a solid background, and one that leaves its background clear shows on white
/// rather than on the default avatar. It draws nothing from elsewhere, images or files.
fn draw(svg: &[u8]) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};

    let options = usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(svg, &options).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(crate::people::SIZE, crate::people::SIZE)?;
    pixmap.fill(tiny_skia::Color::WHITE);
    let (side, size) = (crate::people::SIZE as f32, tree.size());
    let scale = (side / size.width()).min(side / size.height());
    let transform = tiny_skia::Transform::from_translate(
        (side - size.width() * scale) / 2.0,
        (side - size.height() * scale) / 2.0,
    )
    .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

/// `png` decoded into an avatar's textures.
fn decode(ctx: &egui::Context, png: &[u8]) -> Option<AvatarPhoto> {
    let image = image::load_from_memory(png).ok()?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(AvatarPhoto::new(
        ctx,
        "brand logo",
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(time: &str) -> Timestamp {
        time.parse().expect("a time")
    }

    fn record(logo: &str, checked: Option<BrandCheck>) -> BrandRecord {
        BrandRecord {
            next: at("2026-06-02T00:00:00Z"),
            logo: Some(format!("https://brand.example/{logo}.svg")),
            certificate: Some("https://brand.example/vmc.pem".to_owned()),
            checked,
        }
    }

    #[test]
    fn a_valid_certificate_isnt_fetched_again_while_the_record_names_it() {
        let now = at("2026-06-01T00:00:00Z");
        let valid = Some(BrandCheck::Valid {
            until: at("2027-01-01T00:00:00Z"),
        });
        let kept = |checked| Some(record("logo", checked));
        assert_eq!(
            carried(kept(valid), record("logo", None), now).checked,
            valid
        );
        // A new logo, an expired certificate or one that didn't validate are checked again.
        assert_eq!(carried(kept(valid), record("new", None), now).checked, None);
        let later = at("2027-02-01T00:00:00Z");
        assert_eq!(
            carried(kept(valid), record("logo", None), later).checked,
            None
        );
        let invalid = Some(BrandCheck::Invalid);
        assert_eq!(
            carried(kept(invalid), record("logo", None), now).checked,
            None
        );
        assert_eq!(carried(None, record("logo", None), now).checked, None);
    }

    #[test]
    fn blocking_all_remote_images_stops_the_fetches_but_not_a_kept_logo() {
        let now = at("2026-06-01T00:00:00Z");
        let valid = Some(BrandCheck::Valid {
            until: at("2027-01-01T00:00:00Z"),
        });
        let expired = Some(BrandCheck::Valid {
            until: at("2026-05-01T00:00:00Z"),
        });
        for blocked in [false, true] {
            assert_eq!(step(valid, true, blocked, now), Step::Kept);
            assert_eq!(
                step(Some(BrandCheck::Invalid), true, blocked, now),
                Step::Nothing
            );
        }
        // What isn't kept, or no longer vouches, is fetched again, unless images are blocked.
        for (checked, kept) in [(None, false), (valid, false), (expired, true)] {
            assert_eq!(step(checked, kept, false, now), Step::Fetch);
            assert_eq!(step(checked, kept, true, now), Step::Nothing);
        }
    }

    #[test]
    fn a_logo_is_drawn_fitted_and_centred_on_white() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50" fill="#ff0000"/></svg>"##;
        let png = draw(svg).expect("a logo");
        let image = image::load_from_memory(&png).expect("a PNG").into_rgba8();
        let side = crate::people::SIZE;
        assert_eq!(image.dimensions(), (side, side));
        assert_eq!(image.get_pixel(side / 2, side / 2).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(side / 2, 4).0, [255, 255, 255, 255]);
        assert!(draw(b"<svg").is_none());
    }

    #[test]
    fn an_svgz_logo_is_uncompressed() {
        use std::io::Write as _;

        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
        let mut svgz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        svgz.write_all(&svg).expect("compressed");
        let svgz = svgz.finish().expect("compressed");
        assert_eq!(uncompressed(svgz), Some(svg.clone()));
        assert_eq!(uncompressed(svg.clone()), Some(svg));
    }
}
