//! The account's bulk actions, queued and run one at a time on the account's runtime, straight
//! against Gmail (exo-store's `Store::bulk`). Each is undoable while it waits (which cancels it),
//! while it runs (which stops it and puts back what it did) and after. The store's log keeps a
//! week of them with what each changed, so after a relaunch they're listed and can still be put
//! back, and one that a quit stopped carries on.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, MutexGuard},
};

use exo_store::{
    BulkRecord, BulkState, BulkTarget, LoggedBulk, Mutation, Progress, Relabelled, Store,
};
use tokio::sync::Notify;

use crate::account::{Repaint, UndoToken};

/// The tokens of the bulk actions read back from the log: the entry's id with the top bit set,
/// which this session's own tokens, counting up from 0, never reach.
const LOGGED: UndoToken = 1 << 63;

/// Where a bulk action is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for the one before it.
    Pending,
    /// Changing threads.
    Running,
    /// Every thread changed.
    Done,
    /// Being put back, or waiting to be.
    Undoing,
    /// Put back, or cancelled before it started.
    Undone,
    /// Gmail failed it. What it changed by then can still be put back.
    Failed,
}

/// A bulk action as the UI shows it.
#[derive(Clone, Debug)]
pub struct Summary {
    /// The toast's undo token, which also names it.
    pub token: UndoToken,
    /// What it does, such as `Mark Done`.
    pub name: String,
    /// What it's on, as the Bulk Actions dialog shows it.
    pub query: Option<String>,
    /// Where it is.
    pub status: Status,
    /// Whether its work is under way rather than waiting, which is what the footer reports.
    pub active: bool,
    /// The loaded rows it's for, which show `Pending:` while it waits or runs.
    pub threads: Arc<HashSet<String>>,
    /// Threads changed (or put back, while undoing) so far.
    pub done: usize,
    /// Out of how many. None until they're all found.
    pub total: usize,
}

/// A bulk action to queue.
#[derive(Clone, Debug)]
pub struct Request {
    /// What it does.
    pub name: &'static str,
    /// What it's on, for the Bulk Actions dialog.
    pub query: Option<String>,
    /// The action.
    pub mutation: Mutation,
    /// Its threads.
    pub target: BulkTarget,
    /// The loaded rows it's for.
    pub threads: HashSet<String>,
}

struct Job {
    summary: Summary,
    mutation: Mutation,
    target: BulkTarget,
    progress: Arc<Progress>,
    /// What it changed, to put back.
    changed: Relabelled,
    /// Its entry in the log, and the state the entry has, once it's written.
    logged: Option<(u64, BulkState)>,
}

impl Job {
    /// One read back from the log.
    fn read(entry: LoggedBulk) -> Self {
        let LoggedBulk {
            id,
            record,
            changed,
        } = entry;
        let status = match record.state {
            BulkState::Pending => Status::Pending,
            BulkState::Done => Status::Done,
            BulkState::Undoing => Status::Undoing,
            BulkState::Undone => Status::Undone,
            BulkState::Failed => Status::Failed,
        };
        let progress = match record.state {
            BulkState::Pending | BulkState::Undoing => Progress::default(),
            BulkState::Done | BulkState::Undone | BulkState::Failed => {
                Progress::ended(record.count)
            }
        };
        Self {
            summary: Summary {
                token: LOGGED | id,
                name: record.name,
                query: record.query,
                status,
                active: false,
                threads: Arc::default(),
                done: 0,
                total: 0,
            },
            mutation: record.mutation,
            target: record.target,
            progress: Arc::new(progress),
            changed,
            logged: Some((id, record.state)),
        }
    }

    /// Where the log has it. One running is still to finish, should a quit stop it.
    fn state(&self) -> BulkState {
        match self.summary.status {
            Status::Pending | Status::Running => BulkState::Pending,
            Status::Done => BulkState::Done,
            Status::Undoing => BulkState::Undoing,
            Status::Undone => BulkState::Undone,
            Status::Failed => BulkState::Failed,
        }
    }

    /// What the log keeps of it.
    fn record(&self) -> BulkRecord {
        let state = self.state();
        BulkRecord {
            name: self.summary.name.clone(),
            query: self.summary.query.clone(),
            mutation: self.mutation.clone(),
            target: self.target.clone(),
            state,
            count: match state {
                // What its undo put back.
                BulkState::Undone => self.progress.total(),
                _ => self.changed.len(),
            },
        }
    }
}

/// The bulk actions, shared between the UI and the task running them.
#[derive(Clone)]
pub struct Jobs {
    shared: Arc<Shared>,
}

struct Shared {
    jobs: Mutex<Vec<Job>>,
    wake: Notify,
    repaint: Repaint,
}

/// What the runner does next.
enum Work {
    /// Run it, adding to what it has changed already (before a quit stopped it).
    Run(BulkTarget, Mutation, Arc<Progress>, Relabelled),
    Undo(Relabelled, Arc<Progress>),
}

impl Jobs {
    /// Starts the task that runs `store`'s bulk actions on the current runtime, and starts it
    /// again after a panic as `restarts` says. `repaint` asks for frames as they move on. The
    /// bulk actions `logged` in an earlier session come first, and those still to run or to put
    /// back carry on.
    pub(crate) fn start(
        store: Store,
        repaint: Repaint,
        restarts: &Arc<crate::recovery::Restarts>,
        logged: Vec<LoggedBulk>,
    ) -> Self {
        let jobs = Self {
            shared: Arc::new(Shared {
                jobs: Mutex::new(logged.into_iter().map(Job::read).collect()),
                wake: Notify::new(),
                repaint,
            }),
        };
        crate::recovery::spawn(restarts, {
            let jobs = jobs.clone();
            move || jobs.clone().run(store.clone())
        });
        jobs
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Job>> {
        self.shared
            .jobs
            .lock()
            .expect("bulk jobs lock is never poisoned")
    }

    /// Queues `request`. [`Jobs::undo`] with `token` undoes it.
    pub fn submit(&self, token: UndoToken, request: Request) {
        let Request {
            name,
            query,
            mutation,
            target,
            threads,
        } = request;
        let summary = Summary {
            token,
            name: name.to_owned(),
            query,
            status: Status::Pending,
            active: false,
            threads: Arc::new(threads),
            done: 0,
            total: 0,
        };
        self.lock().push(Job {
            summary,
            mutation,
            target,
            progress: Arc::default(),
            changed: Relabelled::default(),
            logged: None,
        });
        self.shared.wake.notify_one();
    }

    /// Undoes the bulk action `token`: a waiting one is dropped, a running one stops and is put
    /// back, a finished one is put back. Returns whether `token` names one.
    pub fn undo(&self, token: UndoToken) -> bool {
        let mut jobs = self.lock();
        let Some(job) = jobs.iter_mut().find(|job| job.summary.token == token) else {
            return false;
        };
        match job.summary.status {
            Status::Pending => {
                job.summary.status = Status::Undone;
                // For the log.
                self.shared.wake.notify_one();
            }
            Status::Running => {
                job.progress.stop();
                job.summary.status = Status::Undoing;
            }
            Status::Done | Status::Failed => {
                job.summary.status = Status::Undoing;
                self.shared.wake.notify_one();
            }
            Status::Undoing | Status::Undone => {}
        }
        true
    }

    /// Every bulk action of this session and those logged in the last week, oldest first.
    pub fn summaries(&self) -> Vec<Summary> {
        self.lock()
            .iter()
            .map(|job| Summary {
                done: job.progress.done(),
                total: job.progress.total(),
                ..job.summary.clone()
            })
            .collect()
    }

    /// Whether no bulk action is waiting or under way.
    pub fn is_idle(&self) -> bool {
        self.lock().iter().all(|job| {
            matches!(
                job.summary.status,
                Status::Done | Status::Undone | Status::Failed
            )
        })
    }

    /// The next piece of work, marked under way: the oldest waiting action, or undo.
    fn next(&self) -> Option<(UndoToken, Work)> {
        let mut jobs = self.lock();
        let job = jobs.iter_mut().find(|job| {
            !job.summary.active && matches!(job.summary.status, Status::Pending | Status::Undoing)
        })?;
        job.summary.active = true;
        let work = if job.summary.status == Status::Pending {
            job.summary.status = Status::Running;
            // A logged one's entry keeps what it changes as it goes.
            job.progress = Arc::new(
                job.logged
                    .map_or_else(Progress::default, |(id, _)| Progress::logged(id)),
            );
            Work::Run(
                job.target.clone(),
                job.mutation.clone(),
                job.progress.clone(),
                job.changed.clone(),
            )
        } else {
            job.progress = Arc::default();
            Work::Undo(job.changed.clone(), job.progress.clone())
        };
        Some((job.summary.token, work))
    }

    /// Writes to the store's log what it doesn't have yet: the new bulk actions, and the others'
    /// new states with what they changed.
    async fn log(&self, store: &Store) {
        let due: Vec<(UndoToken, Option<u64>, BulkRecord, Relabelled)> = self
            .lock()
            .iter()
            .filter(|job| job.logged.map(|(_, state)| state) != Some(job.state()))
            .map(|job| {
                (
                    job.summary.token,
                    job.logged.map(|(id, _)| id),
                    job.record(),
                    job.changed.clone(),
                )
            })
            .collect();
        for (token, id, record, changed) in due {
            let written = match id {
                Some(id) => store.relog_bulk(id, &record, &changed).await.map(|()| id),
                None => store.log_bulk(&record, &changed).await,
            };
            match written {
                Ok(id) => {
                    if let Some(job) = self
                        .lock()
                        .iter_mut()
                        .find(|job| job.summary.token == token)
                    {
                        job.logged = Some((id, record.state));
                    }
                }
                Err(error) => log::error!("logging a bulk action: {error}"),
            }
        }
    }

    async fn run(self, store: Store) {
        loop {
            self.log(&store).await;
            let Some((token, work)) = self.next() else {
                self.shared.wake.notified().await;
                continue;
            };
            self.shared.repaint.request();
            // What it changed (still to put back, when an undo failed), and whether it worked.
            let (changed, ran, ok) = match work {
                Work::Run(target, mutation, progress, mut changed) => {
                    let result = store
                        .bulk(&target, &mutation, &progress, &mut changed)
                        .await;
                    if let Err(error) = &result {
                        log::error!("bulk {mutation:?}: {error}");
                    }
                    (changed, true, result.is_ok())
                }
                Work::Undo(changed, progress) => match store.undo_bulk(&changed, &progress).await {
                    Ok(()) => (Relabelled::default(), false, true),
                    Err(error) => {
                        log::error!("undoing a bulk action: {error}");
                        (changed, false, false)
                    }
                },
            };
            let mut jobs = self.lock();
            if let Some(job) = jobs.iter_mut().find(|job| job.summary.token == token) {
                job.summary.active = false;
                job.changed = changed;
                job.summary.status = match (job.summary.status, ran, ok) {
                    // Undone while it ran, so the next round puts back what it did.
                    (Status::Undoing, true, _) => Status::Undoing,
                    (_, true, true) => Status::Done,
                    (_, false, true) => Status::Undone,
                    (_, _, false) => Status::Failed,
                };
            }
            drop(jobs);
            self.shared.repaint.request();
        }
    }
}
