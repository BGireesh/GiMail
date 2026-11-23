//! Keeps the notes folder ([`super::markdown`]) up to date, in a loop of its own so the account's
//! worker never waits on it: a folder on a slow or stalled disk delays the copy, never a save.
//!
//! The worker queues a job after each note change, and when the folder is turned on or moved, or
//! `Rebuild Notes Folder` asks. The loop takes every job queued by the time it gets to them, so
//! changes to one thread are written once and a rebuild stands for them all, and reads the notes
//! from the store as it writes them, so the copy is the latest.

use std::{collections::BTreeSet, panic::AssertUnwindSafe, path::PathBuf, sync::Arc};

use exo_store::{DeviceSettings, NotedThread, Store, ThreadId};
use futures::FutureExt as _;
use tokio::sync::{mpsc, watch};

use super::markdown;

/// What the loop is asked to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    /// A thread's notes, which changed.
    Thread(ThreadId),
    /// Every thread's: the folder was turned on or moved, or the user `asked`.
    Rebuild { asked: bool },
}

/// How a job went, for the UI, when it has something to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The folder couldn't be written, on a rebuild the user `asked` for or otherwise.
    Failed { asked: bool },
    /// The rebuild the user asked for is done.
    Rebuilt,
}

/// A job, with what keeps the account busy until it's done (so tests settle after it).
type Queued = (Job, Box<dyn Send>);

/// The loop's queue.
pub(crate) struct Exports {
    sender: mpsc::UnboundedSender<Queued>,
    /// While true the loop takes no jobs, which wait (tests).
    paused: watch::Sender<bool>,
}

impl Exports {
    /// Starts the loop over `store` on the current runtime, supervised by `restarts`. `report`
    /// tells the UI how a job went.
    pub(crate) fn start(
        store: Store,
        restarts: &Arc<crate::recovery::Restarts>,
        report: impl Fn(Outcome) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (paused, paused_receiver) = watch::channel(false);
        crate::recovery::spawn_once(
            restarts,
            run(store, receiver, paused_receiver, Box::new(report)),
        );
        Arc::new(Self { sender, paused })
    }

    /// Queues `job`, the account busy with `busy` until it's done.
    pub(crate) fn queue(&self, job: Job, busy: impl Send + 'static) {
        if self.sender.send((job, Box::new(busy))).is_err() {
            log::warn!("the notes folder's loop has stopped");
        }
    }

    /// Holds jobs (`true`) or lets them run again. Jobs queued meanwhile wait, and keep the
    /// account busy.
    pub(crate) fn pause(&self, paused: bool) {
        self.paused.send_replace(paused);
    }
}

/// Takes jobs as they come, every one queued by then at once, and writes them.
async fn run(
    store: Store,
    mut receiver: mpsc::UnboundedReceiver<Queued>,
    mut paused: watch::Receiver<bool>,
    report: Box<dyn Fn(Outcome) + Send + Sync>,
) {
    while let Some(first) = receiver.recv().await {
        let _ = paused.wait_for(|paused| !*paused).await;
        let mut batch = vec![first];
        while let Ok(more) = receiver.try_recv() {
            batch.push(more);
        }
        let (jobs, busy): (Vec<Job>, Vec<Box<dyn Send>>) = batch.into_iter().unzip();
        let work = AssertUnwindSafe(write(&store, coalesce(jobs), &*report)).catch_unwind();
        if work.await.is_err() {
            log::error!("writing the notes folder panicked");
        }
        drop(busy);
    }
}

/// `jobs` as few as do the same: one rebuild for any (asked when any was), else each thread
/// once.
fn coalesce(jobs: Vec<Job>) -> Vec<Job> {
    let rebuild = jobs.iter().fold(None, |rebuild, job| match job {
        Job::Rebuild { asked } => Some(rebuild.unwrap_or(false) || *asked),
        Job::Thread(_) => rebuild,
    });
    if let Some(asked) = rebuild {
        return vec![Job::Rebuild { asked }];
    }
    let mut seen = BTreeSet::new();
    jobs.into_iter()
        .filter(|job| match job {
            Job::Thread(thread) => seen.insert(thread.as_str().to_owned()),
            Job::Rebuild { .. } => true,
        })
        .collect()
}

/// Writes `jobs`, with the notes as the store has them now, and reports how it went.
async fn write(store: &Store, jobs: Vec<Job>, report: &(dyn Fn(Outcome) + Send + Sync)) {
    let asked = jobs.contains(&Job::Rebuild { asked: true });
    let Some(folder) = folder(store).await else {
        return;
    };
    let folder = match folder {
        Ok(folder) => folder,
        Err(error) => {
            log::error!("the notes folder: {error}");
            report(Outcome::Failed { asked });
            return;
        }
    };
    let mut threads = Vec::new();
    for job in jobs {
        let read = match job {
            Job::Rebuild { .. } => store.all_notes().await,
            Job::Thread(thread) => store
                .noted_thread(&thread)
                .await
                .map(|noted| noted.into_iter().collect()),
        };
        match read {
            Ok(read) => threads.extend(read),
            Err(error) => {
                log::error!("reading the notes to copy them: {error}");
                report(Outcome::Failed { asked });
                return;
            }
        }
    }
    let written = write_threads(store, folder, threads).await;
    match written {
        Ok(()) if asked => report(Outcome::Rebuilt),
        Ok(()) => {}
        Err(error) => {
            log::error!("writing the notes folder: {error}");
            report(Outcome::Failed { asked });
        }
    }
}

/// The folder this device keeps the notes' copy in, or `None` while it keeps none. A folder that
/// isn't a full path is an error.
async fn folder(store: &Store) -> Option<std::io::Result<PathBuf>> {
    match store.settings::<DeviceSettings>().await {
        Ok(device) => device.notes_folder.as_deref().map(markdown::expand),
        Err(error) => {
            log::error!("reading the notes folder's setting: {error}");
            None
        }
    }
}

/// Writes `threads` to `folder` off the runtime's threads. The folder is made even when there's
/// nothing to write, so one that can't be is found out at once.
async fn write_threads(
    store: &Store,
    folder: PathBuf,
    threads: Vec<NotedThread>,
) -> std::io::Result<()> {
    let messages: Vec<String> = threads
        .iter()
        .flat_map(|(_, notes, _)| notes.iter().filter_map(|note| note.message_id.clone()))
        .collect();
    let gone = store
        .missing_messages(&messages)
        .await
        .unwrap_or_else(|error| {
            log::error!("reading which noted messages are gone: {error}");
            BTreeSet::new()
        });
    let files = store.note_files().to_owned();
    tokio::task::spawn_blocking(move || markdown::export_all(&folder, &files, &threads, &gone))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_come_together() {
        let thread = |id: &str| Job::Thread(ThreadId::new(id));
        assert_eq!(
            coalesce(vec![thread("a"), thread("b"), thread("a")]),
            [thread("a"), thread("b")]
        );
        assert_eq!(
            coalesce(vec![
                thread("a"),
                Job::Rebuild { asked: false },
                thread("b")
            ]),
            [Job::Rebuild { asked: false }]
        );
        assert_eq!(
            coalesce(vec![
                Job::Rebuild { asked: true },
                Job::Rebuild { asked: false }
            ]),
            [Job::Rebuild { asked: true }]
        );
    }
}
