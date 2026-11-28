//! A note's files: images shown in its text and files listed under it as cards. Pasting,
//! dropping or picking a file copies it into the account's note files (`{account}.notes`), named
//! by a new uuid, and the note lists it. An image that decodes goes in the text, shown by a new
//! Content-ID; anything else, an image that doesn't decode too, is listed.
//!
//! What's added is read, decoded and copied off the UI thread, and goes into the editor it was
//! added to when done. Until then the editor lists it as a dimmed card, and an editor asked to
//! close waits for it, so the save has it. One whose editor has gone by then (the thread was
//! left) is removed again, and the user is told. The images the editors show (the open one's,
//! and each card's) are read and decoded off the UI thread too, and kept while the thread is
//! open.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
};

use egui::{CornerRadius, Rect, Sense, Ui, pos2, vec2};
use exo_editor::{ImageLoad, ImageSource, Picture};
use exo_store::NoteFile;

use super::Notes;

/// The largest file a note takes.
pub const MAX_FILE: u64 = 100 * 1024 * 1024;

/// Why a file wasn't added to a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It's over [`MAX_FILE`].
    TooLarge,
    /// It has no bytes.
    Empty,
    /// It couldn't be read, or kept.
    Unreadable,
}

impl Refused {
    /// The error toast that says so.
    pub fn text(self) -> &'static str {
        match self {
            Self::TooLarge => "That file is over 100 MB.",
            Self::Empty => "That file is empty.",
            Self::Unreadable => "Couldn't read that file.",
        }
    }
}

/// Keeps `bytes`, a `mime` file named `name`, in `folder` under a new uuid, as a file of a note.
/// `content_id` is set for an image the note's text shows.
pub fn add_file(
    folder: &Path,
    name: &str,
    mime: &str,
    bytes: &[u8],
    content_id: Option<String>,
) -> Result<NoteFile, Refused> {
    let size = bytes.len() as u64;
    check(size)?;
    let uuid = exo_editor::uuid_v4();
    if let Err(error) =
        std::fs::create_dir_all(folder).and_then(|()| std::fs::write(folder.join(&uuid), bytes))
    {
        log::warn!("keeping a note's file: {error}");
        return Err(Refused::Unreadable);
    }
    Ok(NoteFile {
        uuid,
        name: name.to_owned(),
        mime_type: mime.to_owned(),
        size,
        content_id,
    })
}

/// Copies the file at `path` into `folder` as a file of a note. An image (by its name) that
/// decodes comes back with a new Content-ID, to go in the note's text; anything else is listed.
pub fn add_path(folder: &Path, path: &Path) -> Result<NoteFile, Refused> {
    check_path(path)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "untitled".to_owned());
    let mime = crate::compose::mime_type(&name);
    if crate::compose::is_inline_image(&name) {
        let bytes = std::fs::read(path).map_err(|error| {
            log::warn!("reading an image for a note: {error}");
            Refused::Unreadable
        })?;
        let content_id = decodes(&bytes).then(exo_editor::uuid_v4);
        return add_file(folder, &name, mime, &bytes, content_id);
    }
    // Copied rather than read, so a big file never sits in memory.
    let uuid = exo_editor::uuid_v4();
    let size = std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::copy(path, folder.join(&uuid)))
        .map_err(|error| {
            log::warn!("copying a file for a note: {error}");
            Refused::Unreadable
        })?;
    Ok(NoteFile {
        uuid,
        name,
        mime_type: mime.to_owned(),
        size,
        content_id: None,
    })
}

/// Keeps an image's `bytes` (a pasted screenshot), named `name`: in the note's text when it
/// decodes, listed when it doesn't.
pub fn add_image(folder: &Path, name: &str, bytes: &[u8]) -> Result<NoteFile, Refused> {
    let content_id = decodes(bytes).then(exo_editor::uuid_v4);
    add_file(
        folder,
        name,
        crate::compose::mime_type(name),
        bytes,
        content_id,
    )
}

/// Turns away the file at `path` when it's a folder, can't be read, or is empty or too large: what
/// can be told without reading it.
fn check_path(path: &Path) -> Result<(), Refused> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        log::warn!("reading a file for a note: {error}");
        Refused::Unreadable
    })?;
    // A folder can't be kept.
    if metadata.is_dir() {
        return Err(Refused::Unreadable);
    }
    check(metadata.len())
}

/// Turns away a file of `size` bytes that's empty or too large.
fn check(size: u64) -> Result<(), Refused> {
    match size {
        0 => Err(Refused::Empty),
        size if size > MAX_FILE => Err(Refused::TooLarge),
        _ => Ok(()),
    }
}

/// Whether `bytes` are an image the editor can show: decoded whole, by the decoder that shows it.
fn decodes(bytes: &[u8]) -> bool {
    matches!(crate::compose::decode(bytes), ImageLoad::Loaded(_))
}

/// What's being added to a note, which the worker reads, decodes and keeps.
enum Adding {
    /// A file, by its path.
    Path(PathBuf),
    /// A pasted image's bytes, and its name.
    Image(String, Vec<u8>),
}

/// A file kept for editor `editor`, or why it couldn't be.
struct Added {
    /// Which of the files being added it is.
    token: u64,
    editor: String,
    file: Result<NoteFile, Refused>,
}

/// A file on its way into a note.
struct Pending {
    token: u64,
    editor: String,
    /// Its name, as its card shows it meanwhile.
    name: String,
}

/// What became of the files added since last asked ([`Notes::take_added`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// Why the last one that couldn't be kept wasn't.
    pub refused: Option<Refused>,
    /// The names of those whose editor had gone, which aren't on its note.
    pub left_out: Vec<String>,
}

/// The files being added to notes off the UI thread, which come back to their editors.
pub(super) struct Adder {
    sender: Sender<Added>,
    receiver: Receiver<Added>,
    /// Those not back yet, oldest first.
    pending: Vec<Pending>,
    next: u64,
    /// While set, files being added wait before they're kept (tests).
    held: Arc<AtomicBool>,
}

impl Default for Adder {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver,
            pending: Vec::new(),
            next: 0,
            held: Arc::default(),
        }
    }
}

impl Adder {
    /// Keeps `adding` in `folder` on another thread, for editor `editor`.
    fn start(&mut self, ctx: &egui::Context, folder: &Path, editor: &str, adding: Adding) {
        let token = self.next;
        self.next += 1;
        let name = match &adding {
            Adding::Path(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Adding::Image(name, _) => name.clone(),
        };
        self.pending.push(Pending {
            token,
            editor: editor.to_owned(),
            name,
        });
        let (sender, ctx, folder, editor, held) = (
            self.sender.clone(),
            ctx.clone(),
            folder.to_owned(),
            editor.to_owned(),
            self.held.clone(),
        );
        std::thread::spawn(move || {
            while held.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let file = match adding {
                Adding::Path(path) => add_path(&folder, &path),
                Adding::Image(name, bytes) => add_image(&folder, &name, &bytes),
            };
            let _ = sender.send(Added {
                token,
                editor,
                file,
            });
            ctx.request_repaint();
        });
    }

    /// The names of the files on their way into editor `editor`'s note, oldest first.
    pub(super) fn pending_for<'a>(&'a self, editor: &'a str) -> impl Iterator<Item = &'a str> {
        self.pending
            .iter()
            .filter(move |pending| pending.editor == editor)
            .map(|pending| pending.name.as_str())
    }
}

/// The note's files listed under it: those its text doesn't show.
pub(super) fn listed(files: &[NoteFile]) -> impl Iterator<Item = &NoteFile> {
    files.iter().filter(|file| file.content_id.is_none())
}

/// What a note's file cards ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Chip {
    /// Open the file, previewed unless asked not to.
    Open(NoteFile, bool),
    /// Take the file (by uuid) off the note being edited.
    Remove(String),
}

/// The gap between a note's text and its file cards.
const CHIPS_GAP: f32 = 8.0;
/// The remove cross's size, and how far it is from a card's top and right.
const CROSS: f32 = 12.0;
const CROSS_TOP: f32 = 9.0;
const CROSS_RIGHT: f32 = 8.0;

/// How tall the cards of `files`' listed files are in `width`, with the gap above them (0 for
/// none).
pub(super) fn chips_height(files: &[NoteFile], width: f32) -> f32 {
    let count = listed(files).count();
    if count == 0 {
        return 0.0;
    }
    let (card, per_row) = exo_ui::attachment_grid(width + 2.0 * exo_ui::CARD_MARGIN_X);
    let rows = count.div_ceil(per_row);
    CHIPS_GAP + rows as f32 * (card.y + exo_ui::CARD_MARGIN_BOTTOM)
}

/// A card for a file still on its way into the note, which [`chips`] draws dimmed and takes no
/// click on: it has a name and nothing else yet.
pub(super) fn pending_file(at: usize, name: &str) -> NoteFile {
    NoteFile {
        uuid: format!("{PENDING}{at}"),
        name: name.to_owned(),
        mime_type: crate::compose::mime_type(name).to_owned(),
        size: 0,
        content_id: None,
    }
}

/// What starts the uuid of a [`pending_file`], which no kept file's does.
const PENDING: &str = "pending:";

/// Whether `file` is a [`pending_file`].
fn is_pending(file: &NoteFile) -> bool {
    file.uuid.starts_with(PENDING)
}

/// Draws the cards of `files`' listed files at the top of `area` (which [`chips_height`] sized),
/// of which `clip` shows. With `removable`, a hovered card shows its remove cross. A file still
/// being added ([`pending_file`]) is dimmed and takes no click. `salt` tells one note's cards
/// from another's.
pub(super) fn chips(
    ui: &Ui,
    files: &[NoteFile],
    area: Rect,
    clip: Rect,
    removable: bool,
    salt: &str,
) -> Option<Chip> {
    let ctx = ui.ctx().clone();
    let palette = exo_ui::palette(&ctx);
    let (card, per_row) = exo_ui::attachment_grid(area.width() + 2.0 * exo_ui::CARD_MARGIN_X);
    let mut clicked = None;
    for (at, file) in listed(files).enumerate() {
        let (row, column) = (at / per_row, at % per_row);
        let rect = Rect::from_min_size(
            pos2(
                area.left() + column as f32 * (card.x + 2.0 * exo_ui::CARD_MARGIN_X),
                area.top() + CHIPS_GAP + row as f32 * (card.y + exo_ui::CARD_MARGIN_BOTTOM),
            ),
            card,
        );
        let id = exo_ui::id(&ctx, ("note-file", salt, &file.uuid));
        if is_pending(file) {
            // Not there to open or remove yet: its card under a wash of the note's own fill.
            exo_ui::paint_attachment_card(ui, rect, &file.name, &file.mime_type, 0.0);
            ui.painter().rect_filled(
                rect.expand(1.0),
                CornerRadius::same(4),
                palette.note_background_default.gamma_multiply(0.6),
            );
            ctx.request_repaint_after_secs(0.1);
            continue;
        }
        let response = ui
            .interact(rect.intersect(clip), id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = exo_ui::motion::transition(
            &ctx,
            id.with("hover"),
            response.hovered(),
            0.25,
            exo_ui::motion::CubicBezier::EASE,
        );
        exo_ui::paint_attachment_card(ui, rect, &file.name, &file.mime_type, hover);
        let cross = Rect::from_min_size(
            pos2(rect.right() - CROSS_RIGHT - CROSS, rect.top() + CROSS_TOP),
            vec2(CROSS, CROSS),
        );
        if removable && response.hovered() {
            let on_cross = response.hover_pos().is_some_and(|at| cross.contains(at));
            ui.painter().rect_filled(
                cross.expand(2.0),
                CornerRadius::same(3),
                palette.background_elevated_accent,
            );
            exo_ui::paint_icon(
                ui,
                "css/isInvalid-cross",
                Rect::from_center_size(cross.center(), vec2(CROSS, CROSS * 8.0 / 9.0)),
                if on_cross {
                    palette.ink(0.9, 0.9)
                } else {
                    palette.ink(0.6, 0.6)
                },
            );
        }
        if !response.clicked() {
            continue;
        }
        let on_cross = response
            .interact_pointer_pos()
            .is_some_and(|at| cross.contains(at));
        clicked = Some(if removable && on_cross {
            Chip::Remove(file.uuid.clone())
        } else {
            let modifiers = ui.input(|input| input.modifiers);
            Chip::Open(file.clone(), !(modifiers.command || modifiers.shift))
        });
    }
    clicked
}

/// An image read and decoded for note `note`'s editors, under the editor's `key`, and the file
/// it came from (none for a `data:` URL).
struct Loaded {
    note: String,
    key: String,
    uuid: Option<String>,
    load: ImageLoad,
}

/// The images the notes' editors asked for: the loads in flight, and what came of those done,
/// by file, while the thread is open.
pub(super) struct Pictures {
    sender: Sender<Loaded>,
    receiver: Receiver<Loaded>,
    kept: HashMap<String, ImageLoad>,
}

impl Default for Pictures {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            sender,
            receiver,
            kept: HashMap::new(),
        }
    }
}

impl Pictures {
    /// Forgets the kept images: another thread opened.
    pub(super) fn clear(&mut self) {
        self.kept.clear();
    }
}

/// A copy of `load`.
fn copy(load: &ImageLoad) -> ImageLoad {
    match load {
        ImageLoad::Loaded(picture) => ImageLoad::Loaded(Picture {
            pixels: picture.pixels.clone(),
            natural: picture.natural,
            retina: picture.retina,
            file: picture.file.clone(),
        }),
        ImageLoad::Blocked => ImageLoad::Blocked,
        ImageLoad::Broken => ImageLoad::Broken,
    }
}

/// The file of `files` that Content-ID `cid` names (compared without case).
fn by_cid<'a>(files: &'a [NoteFile], cid: &str) -> Option<&'a NoteFile> {
    files.iter().find(|file| {
        file.content_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(cid))
    })
}

impl Notes {
    /// Adds the files at `paths` to the open editor, kept in `folder`: images in the text at the
    /// caret, the rest listed. Returns why the last one turned away was, if one was.
    /// They're read and kept off the UI thread, and go in when done ([`Self::take_added`]); what
    /// can be told at once (an empty file, one too large) is refused here.
    pub fn add_paths(
        &mut self,
        ctx: &egui::Context,
        folder: &Path,
        paths: &[PathBuf],
    ) -> Option<Refused> {
        let editor = self.editing.as_ref()?.id.clone();
        let mut refused = None;
        for path in paths {
            match check_path(path) {
                Ok(()) => self
                    .adder
                    .start(ctx, folder, &editor, Adding::Path(path.clone())),
                Err(why) => refused = Some(why),
            }
        }
        refused
    }

    /// The names of the files still on their way into the open editor's note, oldest first.
    pub fn adding(&self) -> Vec<String> {
        self.editing.as_ref().map_or_else(Vec::new, |open| {
            self.adder
                .pending_for(&open.id)
                .map(str::to_owned)
                .collect()
        })
    }

    /// Whether no file is on its way into a note, or the ones that are are held on purpose
    /// ([`Self::hold_adding`]). For tests and screenshots, which wait for the app to go quiet.
    pub fn files_idle(&self) -> bool {
        self.adder.pending.is_empty() || self.adder.held.load(Ordering::SeqCst)
    }

    /// Holds the files being added (`true`) where they are, before they're kept, until called
    /// with `false` (tests).
    pub fn hold_adding(&self, held: bool) {
        self.adder.held.store(held, Ordering::SeqCst);
    }

    /// Adds a pasted image's `bytes`, named `name`, to the open editor, kept in `folder` off the
    /// UI thread.
    pub fn add_image(
        &mut self,
        ctx: &egui::Context,
        folder: &Path,
        name: &str,
        bytes: Vec<u8>,
    ) -> Option<Refused> {
        let editor = self.editing.as_ref()?.id.clone();
        if let Err(why) = check(bytes.len() as u64) {
            return Some(why);
        }
        self.adder
            .start(ctx, folder, &editor, Adding::Image(name.to_owned(), bytes));
        None
    }

    /// Puts the files kept in `folder` since the last call in the editors they were added to,
    /// and says what became of the rest: why the last one that couldn't be kept wasn't, and
    /// which had no editor left to go to. Those are named, and their bytes removed, as nothing
    /// lists them.
    pub fn take_added(&mut self, folder: &Path) -> Taken {
        let mut taken = Taken::default();
        while let Ok(added) = self.adder.receiver.try_recv() {
            self.adder
                .pending
                .retain(|pending| pending.token != added.token);
            match added.file {
                Ok(file) => {
                    if let Err(file) = self.add(&added.editor, file) {
                        if let Err(error) = std::fs::remove_file(folder.join(&file.uuid)) {
                            log::warn!("removing a file its note never got: {error}");
                        }
                        taken.left_out.push(file.name);
                    }
                }
                Err(why) => taken.refused = Some(why),
            }
        }
        taken
    }

    /// Puts `file` on editor `editor`'s note, and in its text at the caret when it's an image to
    /// show. Hands it back when that editor isn't open any more.
    fn add(&mut self, editor: &str, file: NoteFile) -> Result<(), NoteFile> {
        let time = self.time;
        let Some(open) = self.editing.as_mut().filter(|open| open.id == editor) else {
            return Err(file);
        };
        if let Some(cid) = &file.content_id {
            open.editor.insert_image(cid);
        }
        open.files.push(file);
        open.changed = Some(time);
        Ok(())
    }

    /// Takes file `uuid` off the open editor's note. Its bytes stay: a version may list it.
    fn remove_file(&mut self, uuid: &str) {
        let time = self.time;
        if let Some(open) = &mut self.editing {
            open.files.retain(|file| file.uuid != uuid);
            open.changed = Some(time);
        }
    }

    /// Carries out a click on a file card: opening waits for [`Self::take_opening`].
    pub(super) fn chip(&mut self, chip: Chip) {
        match chip {
            Chip::Open(file, preview) => self.opening = Some((file, preview)),
            Chip::Remove(uuid) => self.remove_file(&uuid),
        }
    }

    /// The file a card was clicked to open, and whether to preview it, since this was last asked.
    pub fn take_opening(&mut self) -> Option<(NoteFile, bool)> {
        self.opening.take()
    }

    /// Keeps the `data:` images pasted into the open editor as the note's images, in `folder`.
    /// One that can't be kept takes the paste back. Returns why, if one couldn't. They're kept as
    /// they come: the editor has them in its text already, and one that doesn't decode shows as
    /// broken there.
    pub fn take_uploads(&mut self, folder: &Path) -> Option<Refused> {
        let time = self.time;
        let open = self.editing.as_mut()?;
        let uploads = open.editor.take_uploads();
        for upload in &uploads {
            let extension = upload
                .mime_type
                .rsplit_once('/')
                .map_or("png", |(_, subtype)| subtype);
            match add_file(
                folder,
                &format!("image.{extension}"),
                &upload.mime_type,
                &upload.bytes,
                Some(upload.cid.clone()),
            ) {
                Ok(file) => open.files.push(file),
                Err(why) => {
                    open.editor.undo();
                    return Some(why);
                }
            }
        }
        if !uploads.is_empty() {
            open.changed = Some(time);
        }
        None
    }

    /// Hands the editors the images that came, and starts loading what each asks for: an image
    /// of the note's from `folder`, or a `data:` URL. Remote images aren't fetched for notes.
    pub fn load_images(&mut self, folder: &Path, ctx: &egui::Context) {
        while let Ok(loaded) = self.pictures.receiver.try_recv() {
            self.set_image(&loaded.note, &loaded.key, &loaded.load);
            if let Some(uuid) = loaded.uuid {
                self.pictures.kept.insert(uuid, loaded.load);
            }
        }
        let mut wanted = Vec::new();
        if let Some(open) = &mut self.editing {
            for (source, _) in open.editor.image_requests() {
                let file = cid_file(&source, &open.files);
                wanted.push((open.id.clone(), source, file));
            }
        }
        if let Some(history) = &mut self.history {
            let files = history.files().to_vec();
            for (source, _) in history.editor.image_requests() {
                let file = cid_file(&source, &files);
                wanted.push((history.id.clone(), source, file));
            }
        }
        for note in self.notes.notes.iter() {
            if let Some(body) = self.bodies.get_mut(&note.id) {
                for (source, _) in body.editor.image_requests() {
                    let file = cid_file(&source, &note.files);
                    wanted.push((note.id.clone(), source, file));
                }
            }
        }
        for (note, source, file) in wanted {
            let key = source.key();
            let finish = {
                let (sender, ctx, note, key) = (
                    self.pictures.sender.clone(),
                    ctx.clone(),
                    note.clone(),
                    key.clone(),
                );
                let uuid = file.as_ref().map(|file| file.uuid.clone());
                move |load: ImageLoad| {
                    let _ = sender.send(Loaded {
                        note,
                        key,
                        uuid,
                        load,
                    });
                    ctx.request_repaint();
                }
            };
            match source {
                ImageSource::Cid(_) => match file {
                    Some(file) => match self.pictures.kept.get(&file.uuid).map(copy) {
                        Some(load) => self.set_image(&note, &key, &load),
                        None => {
                            let path = folder.join(&file.uuid);
                            std::thread::spawn(move || {
                                finish(std::fs::read(path).map_or(ImageLoad::Broken, |bytes| {
                                    crate::compose::decode(&bytes)
                                }))
                            });
                        }
                    },
                    None => self.set_image(&note, &key, &ImageLoad::Broken),
                },
                ImageSource::Data(url) => {
                    std::thread::spawn(move || {
                        finish(
                            exo_mail::decode_data_url(&url)
                                .map_or(ImageLoad::Broken, |bytes| crate::compose::decode(&bytes)),
                        )
                    });
                }
                ImageSource::Remote(_) => self.set_image(&note, &key, &ImageLoad::Blocked),
            }
        }
    }

    /// Gives note `note`'s editors (the open one, its card's, its history's) what loading `key`
    /// came to.
    fn set_image(&mut self, note: &str, key: &str, load: &ImageLoad) {
        if let Some(open) = self.editing.as_mut().filter(|open| open.id == note) {
            open.editor.set_image(key, copy(load));
        }
        if let Some(history) = self.history.as_mut().filter(|open| open.id == note) {
            history.editor.set_image(key, copy(load));
        }
        if let Some(body) = self.bodies.get_mut(note) {
            body.editor.set_image(key, copy(load));
        }
    }
}

/// The note's file a `cid:` image source names, of `files`.
fn cid_file(source: &ImageSource, files: &[NoteFile]) -> Option<NoteFile> {
    match source {
        ImageSource::Cid(cid) => by_cid(files, cid).cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("exo-note-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a folder");
        dir
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 4) as u8, ((x ^ y) * 4) as u8, 255])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encoded");
        out
    }

    #[test]
    fn a_truncated_image_is_kept_as_a_file() {
        let dir = folder("truncated");
        let whole = png();
        let half = &whole[..whole.len() / 2];
        // Its header still reads: the format and the size.
        assert!(
            image::ImageReader::new(std::io::Cursor::new(half))
                .with_guessed_format()
                .expect("a format")
                .into_dimensions()
                .is_ok()
        );
        let path = dir.join("cut.png");
        std::fs::write(&path, half).expect("written");
        let file = add_path(&dir.join("kept"), &path).expect("kept");
        assert_eq!(file.content_id, None, "listed, not shown in the text");
        assert_eq!(
            add_image(&dir.join("kept"), "image.png", half)
                .expect("kept")
                .content_id,
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_over_the_limit_is_refused() {
        let dir = folder("over");
        let path = dir.join("huge.bin");
        std::fs::File::create(&path)
            .and_then(|file| file.set_len(MAX_FILE + 1))
            .expect("a sparse file");
        let kept = dir.join("kept");
        assert_eq!(add_path(&kept, &path), Err(Refused::TooLarge));
        assert!(!kept.exists(), "nothing is copied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_undecodable_image_is_kept_as_a_file() {
        let dir = folder("undecodable");
        let path = dir.join("photo.png");
        std::fs::write(&path, b"not a png at all").expect("written");
        let kept = dir.join("kept");
        let file = add_path(&kept, &path).expect("kept");
        assert_eq!(
            (file.name.as_str(), file.mime_type.as_str(), file.content_id),
            ("photo.png", "image/png", None),
            "listed, not shown in the text"
        );
        assert_eq!(
            std::fs::read(kept.join(&file.uuid)).expect("its bytes"),
            b"not a png at all"
        );

        // One that decodes goes in the text.
        std::fs::write(&path, png()).expect("written");
        let shown = add_path(&kept, &path).expect("kept");
        assert!(shown.content_id.is_some(), "an image to show");
        assert!(
            add_image(&kept, "image.png", b"junk")
                .expect("kept")
                .content_id
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_file_is_refused() {
        let dir = folder("empty");
        let path = dir.join("empty.txt");
        std::fs::write(&path, b"").expect("written");
        assert_eq!(add_path(&dir, &path), Err(Refused::Empty));
        assert_eq!(
            add_file(&dir, "x", "text/plain", b"", None),
            Err(Refused::Empty)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
