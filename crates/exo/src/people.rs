//! What Google says about people beyond their names: the photos, companies and job titles of the
//! accounts' saved contacts and Workspace colleagues, which the contact pane, the calendar's
//! guests and mail notifications show.
//!
//! Each account hands over its people whenever Google lists them ([`Photos::publish`]). A photo
//! loads the first time something draws it ([`photo`]): from the account's photo cache on disk,
//! else from Google, saving it there. Only Google's photo hosts are ever asked, so a contact
//! can't make Exo load an image from anywhere else.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use exo_store::GoogleContact;
use exo_ui::AvatarPhoto;
use sha2::{Digest, Sha256};

use crate::account::Loader;

pub(crate) mod listing;

/// The size photos are asked for (Google's `=s{n}`), and brands' logos drawn at: the contact
/// pane's 56px at 2×.
pub(crate) const SIZE: u32 = 112;
/// Where the photos handed to notifications are written, in the temp directory.
const NOTIFICATION_PHOTOS: &str = "ExoNotificationPhotos";
/// How long a notification waits for a photo that isn't saved yet, or a brand's logo.
pub(crate) const NOTIFICATION_WAIT: Duration = Duration::from_secs(5);

/// What Google says about someone, besides their name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// Their photo's URL, as Google lists it.
    pub photo: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
}

/// Where an account's people's photos come from and where they're kept.
#[derive(Clone)]
pub(crate) struct Photos {
    ctx: egui::Context,
    account: String,
    loader: Loader,
    /// The photo cache.
    dir: PathBuf,
    /// Gmail is served from this Mac (gmail-mock), so its photos are too.
    local: bool,
}

/// The people the accounts listed, and the photos loaded so far. It lives in the context's data,
/// so whatever draws a person can find it.
#[derive(Default)]
struct Registry {
    /// Each account's people that have something to show, by lower-cased address, with where
    /// their photos come from.
    accounts: HashMap<String, (Photos, HashMap<String, Person>)>,
    /// Photos by URL.
    photos: HashMap<String, Slot>,
}

enum Slot {
    Loading,
    Shown(AvatarPhoto),
    /// Google didn't give it, or it isn't an image.
    Missing,
}

type Shared = Arc<Mutex<Registry>>;

fn registry(ctx: &egui::Context) -> Shared {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Shared>(egui::Id::new("exo::people"))
            .clone()
    })
}

impl Photos {
    /// `account`'s photos, fetched by `loader` and kept in `dir`. With `local`, they may come
    /// from this Mac.
    pub(crate) fn new(
        ctx: &egui::Context,
        account: &str,
        loader: Loader,
        dir: PathBuf,
        local: bool,
    ) -> Self {
        Self {
            ctx: ctx.clone(),
            account: account.to_owned(),
            loader,
            dir,
            local,
        }
    }

    /// The account's address.
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// Shows `contacts`, the account's latest listing, from now on. An address takes the first
    /// photo, and the first company and title, its entries have.
    pub(crate) fn publish<'a>(&self, contacts: impl IntoIterator<Item = &'a GoogleContact>) {
        let mut people: HashMap<String, Person> = HashMap::new();
        for contact in contacts {
            if contact.photo.is_none() && contact.company.is_none() && contact.title.is_none() {
                continue;
            }
            let person = people.entry(contact.email.to_lowercase()).or_default();
            if person.photo.is_none() {
                person.photo.clone_from(&contact.photo);
            }
            if person.company.is_none() && person.title.is_none() {
                person.company.clone_from(&contact.company);
                person.title.clone_from(&contact.title);
            }
        }
        let shared = registry(&self.ctx);
        let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        registry
            .accounts
            .insert(self.account.clone(), (self.clone(), people));
        // Photos that couldn't be had are asked for again.
        registry
            .photos
            .retain(|_, slot| !matches!(slot, Slot::Missing));
        drop(registry);
        self.ctx.request_repaint();
    }
}

/// `account` closed: its people show no more.
pub(crate) fn forget(ctx: &egui::Context, account: &str) {
    let shared = registry(ctx);
    let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    registry.accounts.remove(account);
}

/// What Google says about `email`, from whichever account knows them.
pub fn person(ctx: &egui::Context, email: &str) -> Option<Person> {
    let shared = registry(ctx);
    let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let email = email.to_lowercase();
    registry
        .accounts
        .values()
        .find_map(|(_, people)| people.get(&email).cloned())
}

/// `email`'s photo, once it's loaded. The first ask starts loading it.
pub fn photo(ctx: &egui::Context, email: &str) -> Option<AvatarPhoto> {
    let shared = registry(ctx);
    let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let email = email.to_lowercase();
    let (photos, url) = registry.accounts.values().find_map(|(photos, people)| {
        let url = people.get(&email)?.photo.as_deref()?;
        Some((photos.clone(), sized(url)))
    })?;
    match registry.photos.get(&url) {
        Some(Slot::Shown(photo)) => return Some(photo.clone()),
        Some(Slot::Loading | Slot::Missing) => return None,
        None => {}
    }
    registry.photos.insert(url.clone(), Slot::Loading);
    drop(registry);
    load(&shared, photos, url);
    None
}

/// Paints `email`'s avatar filling `rect`: the default avatar (a `small` one's colours, as the
/// calendar's guests have), with their photo (else their brand's logo) over it once it's loaded,
/// snapped to the screen's pixels as a browser paints a background.
pub(crate) fn paint_avatar(painter: &egui::Painter, rect: egui::Rect, email: &str, small: bool) {
    exo_ui::paint_default_avatar(painter, rect, small);
    // Without a photo of the person, their brand's logo.
    let ctx = painter.ctx();
    if let Some(photo) = photo(ctx, email).or_else(|| crate::brands::logo(ctx, email)) {
        let rect = egui::emath::GuiRounding::round_to_pixels(rect, painter.pixels_per_point());
        exo_ui::paint_photo(painter, rect, &photo);
    }
}

/// A copy of `email`'s photo for the notification `key` to show: the account's own when `email` is
/// one of the accounts, else a contact's, from the cache or, when it isn't there, from Google if it
/// comes within [`NOTIFICATION_WAIT`]. macOS moves the copy into its own store.
pub(crate) async fn notification_photo(
    ctx: &egui::Context,
    email: &str,
    key: &str,
) -> Option<PathBuf> {
    let photo = match crate::profile_pictures::saved(ctx, email) {
        Some(photo) => photo.to_vec(),
        None => contact_photo(ctx, email).await?,
    };
    notification_copy(&photo, key)
}

/// A copy of `photo` for the notification `key` to show, which macOS moves into its own store.
pub(crate) fn notification_copy(photo: &[u8], key: &str) -> Option<PathBuf> {
    // macOS knows an image's type by its extension.
    let format = image::guess_format(photo).ok()?;
    let extension = format.extensions_str().first()?;
    let file = std::env::temp_dir()
        .join(NOTIFICATION_PHOTOS)
        .join(format!("{}.{extension}", file_name(key)));
    save(&file, photo)
        .inspect_err(|error| log::warn!("copying a photo for a notification: {error}"))
        .ok()?;
    Some(file)
}

/// `email`'s photo, if Google has one of them: the saved copy, else Google's within
/// [`NOTIFICATION_WAIT`], which is saved and shows wherever they're drawn.
async fn contact_photo(ctx: &egui::Context, email: &str) -> Option<Vec<u8>> {
    let shared = registry(ctx);
    let email = email.to_lowercase();
    let (photos, url, shown) = {
        let registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
        let (photos, url) = registry.accounts.values().find_map(|(photos, people)| {
            let url = people.get(&email)?.photo.as_deref()?;
            Some((photos.clone(), sized(url)))
        })?;
        let shown = matches!(registry.photos.get(&url), Some(Slot::Shown(_)));
        (photos, url, shown)
    };
    let photo = tokio::time::timeout(NOTIFICATION_WAIT, fetch(&photos, &url))
        .await
        .inspect_err(|_| log::info!("a notification goes without its photo, which is slow to come"))
        .ok()??;
    if !shown {
        show(&shared, ctx, &url, &photo);
    }
    Some(photo)
}

/// Loads the photo at `url` into `shared`, or marks it missing.
fn load(shared: &Shared, photos: Photos, url: String) {
    let (shared, ctx, source, asked) = (
        shared.clone(),
        photos.ctx.clone(),
        photos.clone(),
        url.clone(),
    );
    photos.loader.run(
        move |_| async move { fetch(&source, &asked).await },
        move |photo| show(&shared, &ctx, &url, &photo.unwrap_or_default()),
    );
}

/// The photo at `url`: the saved copy, else Google's, which is saved. `None` when Google doesn't
/// give an image, or `url` isn't one of its photos.
async fn fetch(photos: &Photos, url: &str) -> Option<Vec<u8>> {
    let file = photos.dir.join(file_name(url));
    if let Ok(saved) = std::fs::read(&file) {
        return Some(saved);
    }
    if !is_google_photo(url, photos.local) {
        log::warn!("not loading a contact photo from {url}");
        return None;
    }
    let photo = photos.loader.get(url).await?;
    image::load_from_memory(&photo).ok()?;
    if let Err(error) = save(&file, &photo) {
        log::warn!("saving a contact photo: {error}");
    }
    Some(photo)
}

/// Shows `bytes` as the photo at `url`, or none when they aren't an image.
fn show(shared: &Shared, ctx: &egui::Context, url: &str, bytes: &[u8]) {
    let slot = match image::load_from_memory(bytes) {
        Ok(image) => {
            let image = image.into_rgb8();
            let size = [image.width() as usize, image.height() as usize];
            Slot::Shown(AvatarPhoto::new(
                ctx,
                "contact photo",
                egui::ColorImage::from_rgb(size, image.as_raw()),
            ))
        }
        Err(_) => Slot::Missing,
    };
    let mut registry = shared.lock().unwrap_or_else(PoisonError::into_inner);
    registry.photos.insert(url.to_owned(), slot);
    drop(registry);
    ctx.request_repaint();
}

/// Writes `bytes` to `file` through a temporary file, so a crash never leaves half a photo.
pub(crate) fn save(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file)
}

/// `url` asking for [`SIZE`] pixels in place of the size Google lists (`=s100`).
fn sized(url: &str) -> String {
    match url.rsplit_once("=s") {
        Some((base, size)) if !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{base}=s{SIZE}")
        }
        _ => url.to_owned(),
    }
}

/// A file name for `text` (a photo's or logo's URL, a notification's key).
pub(crate) fn file_name(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `url` is one of Google's photos (`https://lh3.googleusercontent.com/…`), or, with
/// `local`, from this Mac (gmail-mock serves its own).
fn is_google_photo(url: &str, local: bool) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(host)) if url.scheme() == "https" => {
            host == "googleusercontent.com" || host.ends_with(".googleusercontent.com")
        }
        Some(url::Host::Ipv4(ip)) => local && ip.is_loopback(),
        Some(url::Host::Domain("localhost")) => local,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_googles_photo_hosts_are_asked() {
        let photo = "https://lh3.googleusercontent.com/a-/ALV-UjX=s100";
        assert!(is_google_photo(photo, false));
        assert!(!is_google_photo(
            "http://lh3.googleusercontent.com/a-/ALV-UjX=s100",
            false
        ));
        assert!(!is_google_photo(
            "https://googleusercontent.com.example.com/a=s100",
            false
        ));
        assert!(!is_google_photo("https://example.com/me.jpg", false));
        assert!(!is_google_photo(
            "http://127.0.0.1:8787/__mock/photo/1",
            false
        ));
        assert!(is_google_photo(
            "http://127.0.0.1:8787/__mock/photo/1",
            true
        ));
    }

    #[test]
    fn photos_are_asked_for_at_twice_the_largest_avatar() {
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a-/ALV-UjX=s100"),
            "https://lh3.googleusercontent.com/a-/ALV-UjX=s112"
        );
        assert_eq!(
            sized("https://lh3.googleusercontent.com/a/photo.jpg"),
            "https://lh3.googleusercontent.com/a/photo.jpg"
        );
    }
}
