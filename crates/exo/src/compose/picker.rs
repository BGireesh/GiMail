//! The file pickers behind Attach and Insert Image, the second limited to the image types. Both
//! pick any number of files and no folders.

use std::path::PathBuf;

/// The files the user picks, of the file `extensions` given (any file when there are none).
/// Empty when the user cancels. Runs the panel modally, so the window waits until the panel
/// closes.
#[cfg(target_os = "macos")]
pub(crate) fn pick_files(extensions: &[&str]) -> Vec<PathBuf> {
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString};

    let Some(main_thread) = objc2::MainThreadMarker::new() else {
        log::warn!("the file picker opens on the main thread only");
        return Vec::new();
    };
    let panel = NSOpenPanel::openPanel(main_thread);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(true);
    if !extensions.is_empty() {
        let types: Vec<_> = extensions
            .iter()
            .map(|extension| NSString::from_str(extension))
            .collect();
        // `allowedContentTypes` takes UTTypes, which come from a framework Exo doesn't link.
        // Extensions pick the same files.
        #[expect(deprecated, reason = "the replacement needs UniformTypeIdentifiers")]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
    }
    if panel.runModal() != NSModalResponseOK {
        return Vec::new();
    }
    panel
        .URLs()
        .iter()
        .filter_map(|url| url.to_file_path())
        .collect()
}

/// No picker off macOS yet, but files can still be dropped.
#[cfg(not(target_os = "macos"))]
pub(crate) fn pick_files(_extensions: &[&str]) -> Vec<PathBuf> {
    log::warn!("no file picker on this platform");
    Vec::new()
}
