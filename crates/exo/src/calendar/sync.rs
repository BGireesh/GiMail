//! The calendar's data off the UI thread: a task on the account's runtime lists the user's
//! calendars, reads the events of the days on screen from the store, fetches those days from
//! Google Calendar, sends the changes made to events, and publishes what it has for the UI to
//! read each frame.
//!
//! Exo has no server for Calendar to push changes through, so it polls. It reads the days on
//! screen every minute while the window is focused and every five minutes otherwise, and the
//! calendar lists every ten, for the mail account and the calendar accounts signed in.
//!
//! Changes go into the store's calendar queue ([`CalendarChange`]), which shows them on the
//! calendar at once, keeps them across a quit and sends them once the connection allows. New
//! events, edits and deletions are held ten seconds first, so Undo can take one back before anyone
//! hears of it. Undo also takes back a change still waiting for the connection. After a panic the
//! task runs again (`recovery`), and finds its queue as it was.
//!
//! The task also keeps the cache of colleagues' calendars (`settings.calendar.cache`): on the
//! schedule's days and hours, at its frequency, it reads each listed person's events over the
//! next week's weekdays into the store, and it forgets whomever the list drops. It looks at the
//! clock at least every minute, so a Mac that wakes past a refresh refreshes then.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::Range,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use exo_store::{
    AccountCalendar, CachedGuest, CalendarCache, CalendarChange, CalendarOutcome, Connection,
    InviteLinks, Outage, Refusal, Store, StoredEvent,
};
use futures::StreamExt;
use gmail::calendar::Event;
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
use tokio::sync::{mpsc, oneshot, watch};

use crate::Clock;
use crate::account::{Loader, Presence, Repaint};
use crate::calendar::cache;
use crate::recovery::{self, Restarts};

/// How long fetched days stay fresh before they're fetched again.
const FOCUSED: Duration = Duration::from_secs(60);
const UNFOCUSED: Duration = Duration::from_secs(300);
/// How long the calendar list stays fresh.
const LIST_FRESH: Duration = Duration::from_secs(600);
/// How soon to try again when Google can't be reached.
const RETRY: Duration = Duration::from_secs(15);
/// How long a change waits for Undo before it's sent.
const HOLD: SignedDuration = SignedDuration::from_secs(10);
/// How often the cache of colleagues' calendars looks at the clock, at most.
const CACHE_CHECK: Duration = Duration::from_secs(60);
/// How many colleagues' calendars a refresh of the cache reads at a time.
const CACHE_READS: usize = 4;

/// Days of events, from some calendars, in a time zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The days.
    pub days: Range<Date>,
    /// Calendars whose events show.
    pub shown: Vec<String>,
    /// Calendars whose events are fetched (a superset of `shown`).
    pub synced: Vec<String>,
    /// Where days start and end.
    pub tz: TimeZone,
}

/// What the calendar has, as the task last read it.
#[derive(Clone, Debug, Default)]
pub struct Data {
    /// The calendars of every calendar account read, each once with the account it's read
    /// through, once they're listed (the store's copy, then Google's).
    pub calendars: Option<Vec<AccountCalendar>>,
    /// Calendar accounts just signed in whose list hasn't come yet.
    pub loading_accounts: BTreeSet<String>,
    /// The window `events` hold, once read.
    pub window: Option<Window>,
    /// The shown calendars' events in the window, with the changes still queued for Calendar.
    pub events: Vec<StoredEvent>,
    /// The `.ics` of the last thread asked about that has an invite, by thread.
    pub invite: Option<(String, Vec<u8>)>,
    /// A new window's events are being fetched.
    pub loading: bool,
    /// Free/busy as last asked for (my shown calendars, for Find Time): each calendar's busy
    /// spans, or `None` when Calendar won't say, and the time it covers.
    pub busy: BTreeMap<String, Option<Vec<(Timestamp, Timestamp)>>>,
    pub busy_range: Option<(Timestamp, Timestamp)>,
    /// Series' `recurrence` lines, by `{calendar}/{series}`, as asked for this session.
    pub series: BTreeMap<String, Vec<String>>,
    /// Guests' calendars as last read, by address.
    pub guests: BTreeMap<String, GuestCalendar>,
    /// When each listed colleague's calendar was last cached, for those cached (Calendar
    /// Settings shows it).
    pub cache_synced: BTreeMap<String, Timestamp>,
    /// Listed colleagues whose calendars a refresh of the cache is reading now.
    pub cache_syncing: BTreeSet<String>,
}

/// A guest's calendar as last read.
#[derive(Clone, Debug)]
pub struct GuestCalendar {
    /// The time it was last asked for.
    pub range: (Timestamp, Timestamp),
    /// Being read. What it had before stays meanwhile.
    pub loading: bool,
    pub events: GuestEvents,
}

/// What Calendar said about a guest's calendar.
#[derive(Clone, Debug)]
pub enum GuestEvents {
    /// Nothing yet.
    Pending,
    /// Their events (instances) over the range.
    Shown(Vec<Event>),
    /// Calendar won't show them.
    Hidden,
    /// Calendar couldn't be reached. It's asked again once the connection is back.
    Failed,
    /// A cached colleague's events (the cache's window only), shown until Calendar answers: while
    /// the latest loads, or when it couldn't be reached, which counts as [`GuestEvents::Failed`]
    /// otherwise (asked again once the connection is back).
    Stale(Vec<Event>),
}

impl GuestEvents {
    /// The events to draw: Calendar's, or a cached copy's.
    pub fn events(&self) -> Option<&[Event]> {
        match self {
            GuestEvents::Shown(events) | GuestEvents::Stale(events) => Some(events),
            GuestEvents::Pending | GuestEvents::Hidden | GuestEvents::Failed => None,
        }
    }
}

/// A change Calendar refused, so the calendar went back to how it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// An answer to an event.
    Respond,
    /// Answer `status` to the invite with this UID, from conversation `thread`, refused for
    /// `refusal`. `links` are the invitation mail's for a sender Calendar doesn't know.
    RespondToInvite {
        uid: String,
        thread: Option<String>,
        status: String,
        refusal: Refusal,
        links: Option<InviteLinks>,
    },
    /// A new event, queued under `token`.
    Create {
        token: u64,
        calendar: String,
        event: Box<Event>,
    },
    /// An edit.
    Edit,
    /// A deletion.
    Delete,
}

impl Failure {
    /// What it means for the UI that Calendar refused `change`, queued under `token`, for
    /// `refusal`.
    fn of(token: u64, change: CalendarChange, refusal: Refusal) -> Self {
        match change {
            CalendarChange::Create { calendar, event } => Self::Create {
                token,
                calendar,
                event: Box::new(event),
            },
            CalendarChange::Edit { .. } | CalendarChange::Split { .. } => Self::Edit,
            CalendarChange::Delete { .. } | CalendarChange::Truncate { .. } => Self::Delete,
            CalendarChange::Respond { .. } => Self::Respond,
            CalendarChange::RespondToInvite {
                invite,
                thread,
                status,
                links,
                ..
            } => Self::RespondToInvite {
                uid: invite.ical_uid.unwrap_or_default(),
                thread,
                status,
                refusal,
                links,
            },
        }
    }
}

enum Request {
    Show(Window),
    /// Queue `change` under `token`, held for Undo first when `held`.
    Queue {
        token: u64,
        change: Box<CalendarChange>,
        held: bool,
    },
    /// Take back the change queued under this token.
    Undo(u64),
    /// Read these calendars' free/busy over this time.
    Busy(Vec<String>, Timestamp, Timestamp),
    /// Send every queued change now, held ones too, then say so (quitting).
    SendNow(oneshot::Sender<()>),
    /// Read this thread's invite.
    Invite(String),
    /// The calendar accounts read changed, so list them again, with `added` (just signed in)
    /// first.
    Accounts {
        added: Option<String>,
    },
    /// Stop reading calendar account `email` (signed out), forgetting its list when `removed`.
    Forget {
        email: String,
        removed: bool,
    },
    /// Read this series' rule: its calendar and id.
    Series(String, String),
    /// Read these guests' calendars over this time.
    Guests(Vec<String>, Timestamp, Timestamp),
    /// From the task fetching it: a guest's calendar over a time, `None` when it couldn't be
    /// reached.
    GuestLoaded(String, (Timestamp, Timestamp), Option<Option<Vec<Event>>>),
    /// From a refresh of the cache of colleagues' calendars: one person's answer.
    CacheLoaded(Box<CacheAnswer>),
    /// Refresh this listed colleague's cached calendar now, whatever the schedule says.
    ResyncCache(String),
    /// From the task fetching it: a thread's invite, if it has one.
    InviteLoaded(String, Option<Vec<u8>>),
    /// Panics the task, as a bug would.
    #[cfg(test)]
    Panic,
}

/// What Calendar said about one person in a refresh of the cache, and the window it was asked
/// about.
struct CacheAnswer {
    email: String,
    today: Date,
    tz: TimeZone,
    range: (Timestamp, Timestamp),
    /// Their events, `None` when Calendar won't show them, or why they couldn't be read.
    found: Result<Option<Vec<Event>>, String>,
}

/// The changes of this session Undo can take back.
#[derive(Default)]
struct Tokens {
    /// Queued, and not going out right now.
    undoable: HashSet<u64>,
    /// Taken back, and not dropped from the queue yet.
    undone: HashSet<u64>,
}

/// What the UI and the task share, which outlives the task's runs.
struct Shared {
    data: watch::Sender<Arc<Data>>,
    busy: AtomicUsize,
    failures: Mutex<Vec<Failure>>,
    tokens: Mutex<Tokens>,
    /// The window on screen, as last asked for.
    window: Mutex<Option<Window>>,
    /// Series whose rule was asked for (`{calendar}/{series}`).
    series_asked: Mutex<HashSet<String>>,
    /// The time each guest's calendar was last asked for.
    guests_asked: Mutex<BTreeMap<String, (Timestamp, Timestamp)>>,
    /// The cache's settings, `None` until the app says them, and each nudge to look at them and
    /// the clock again.
    cache: watch::Sender<Option<CalendarCache>>,
    /// A nudge of `cache` is counted busy and not taken up by the task yet. While it's set, the
    /// busy count holds one for it, which the task takes over.
    cache_nudged: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn tokens(&self) -> MutexGuard<'_, Tokens> {
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The UI's handle on the calendar task.
pub struct CalendarSync {
    requests: mpsc::UnboundedSender<Request>,
    data: watch::Receiver<Arc<Data>>,
    shared: Arc<Shared>,
    /// Which calendar accounts the store reads is said here, at once.
    store: Store,
}

impl CalendarSync {
    /// Starts the task on the current runtime, and again after a panic as `restarts` says,
    /// fetching invites with `loader`. `repaint` asks for a frame when data arrives. The cache of
    /// colleagues' calendars goes by `clock`, once [`CalendarSync::set_cache`] says its settings.
    pub(crate) fn start(
        store: Store,
        loader: Loader,
        presence: watch::Receiver<Presence>,
        repaint: Repaint,
        restarts: &Arc<Restarts>,
        clock: Clock,
    ) -> Self {
        let (requests, receiver) = mpsc::unbounded_channel();
        let (sender, data) = watch::channel(Arc::new(Data::default()));
        let (cache, cache_receiver) = watch::channel(None);
        // The first run's opening counts as busy from now.
        let shared = Arc::new(Shared {
            data: sender,
            busy: AtomicUsize::new(1),
            failures: Mutex::default(),
            tokens: Mutex::default(),
            window: Mutex::default(),
            series_asked: Mutex::default(),
            guests_asked: Mutex::default(),
            cache,
            cache_nudged: std::sync::atomic::AtomicBool::new(false),
        });
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        let mut resumed = false;
        let handle = store.clone();
        recovery::spawn(restarts, {
            let (requests, shared) = (requests.clone(), shared.clone());
            move || {
                if std::mem::replace(&mut resumed, true) {
                    shared.busy.fetch_add(1, Ordering::SeqCst);
                }
                let task = Task {
                    store: store.clone(),
                    loader: loader.clone(),
                    requests: requests.clone(),
                    presence: presence.clone(),
                    connection: store.connection(),
                    repaint: repaint.clone(),
                    shared: shared.clone(),
                    window: None,
                    fetched: Vec::new(),
                    listed: None,
                    retry_at: None,
                    clock: clock.clone(),
                    cache: cache_receiver.clone(),
                    cache_last: None,
                    cache_covered: BTreeSet::new(),
                    cache_reading: 0,
                    cache_in_flight: BTreeSet::new(),
                    cache_check_at: tokio::time::Instant::now() + CACHE_CHECK,
                };
                task.run(receiver.clone(), resumed)
            }
        });
        Self {
            requests,
            data,
            shared,
            store: handle,
        }
    }

    /// Queues `change`, shown on the calendar at once and sent once Undo's hold is over.
    /// Returns its token, for [`CalendarSync::undo`].
    pub fn hold(&self, change: CalendarChange) -> u64 {
        let token = token();
        self.shared.tokens().undoable.insert(token);
        self.post(Request::Queue {
            token,
            change: Box::new(change),
            held: true,
        });
        token
    }

    /// Queues `change` to go at once, shown on the calendar meanwhile, with no Undo.
    pub fn queue(&self, change: CalendarChange) {
        self.post(Request::Queue {
            token: token(),
            change: Box::new(change),
            held: false,
        });
    }

    /// Takes back the change `token`. Returns whether it hadn't gone yet.
    pub fn undo(&self, token: u64) -> bool {
        let mut tokens = self.shared.tokens();
        if !tokens.undoable.remove(&token) {
            return false;
        }
        tokens.undone.insert(token);
        drop(tokens);
        self.post(Request::Undo(token));
        true
    }

    /// How many of this session's changes Undo could still take back: held, or waiting for the
    /// connection.
    pub fn held(&self) -> usize {
        self.shared.tokens().undoable.len()
    }

    /// Sends the queued changes now, held ones too, waiting at most `grace` (Exo is quitting).
    pub async fn send_now(&self, grace: Duration) {
        let (done, wait) = oneshot::channel();
        self.post(Request::SendNow(done));
        let _ = tokio::time::timeout(grace, wait).await;
    }

    /// What the calendar has now.
    pub fn data(&self) -> Arc<Data> {
        self.data.borrow().clone()
    }

    /// Reads (and keeps fresh) `window`'s events from now on.
    pub fn show(&self, window: Window) {
        *self
            .shared
            .window
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(window.clone());
        self.post(Request::Show(window));
    }

    /// Reads `calendars`' free/busy from `from` to `to` into the data.
    pub fn busy(&self, calendars: Vec<String>, from: Timestamp, to: Timestamp) {
        self.post(Request::Busy(calendars, from, to));
    }

    /// Reads `guests`' calendars from `from` to `to` into the data, each unless it was asked for
    /// over that time already.
    pub fn load_guests(&self, guests: &[String], from: Timestamp, to: Timestamp) {
        let mut asked = self
            .shared
            .guests_asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let wanted: Vec<String> = guests
            .iter()
            .filter(|email| {
                !asked
                    .get(*email)
                    .is_some_and(|(start, end)| *start <= from && *end >= to)
            })
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        for email in &wanted {
            asked.insert(email.clone(), (from, to));
        }
        drop(asked);
        self.post(Request::Guests(wanted, from, to));
    }

    /// The changes Calendar refused since the last call.
    pub fn failures(&self) -> Vec<Failure> {
        std::mem::take(
            &mut *self
                .shared
                .failures
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Reads series `id`'s rule on `calendar` into the data (`series`), once a session.
    pub fn load_series(&self, calendar: &str, id: &str) {
        let key = format!("{calendar}/{id}");
        if self
            .shared
            .series_asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key)
        {
            self.post(Request::Series(calendar.to_owned(), id.to_owned()));
        }
    }

    /// Reads the invite of thread `id` into the data, if it carries one.
    pub fn load_invite(&self, id: &str) {
        self.post(Request::Invite(id.to_owned()));
    }

    /// Reads calendar account `email`'s calendars with `tokens` from now on, because it signed in
    /// (`added`), which shows `Loading calendars` until its list comes, or the account opened.
    pub fn read_account(
        &self,
        email: &str,
        tokens: impl gmail::TokenProvider + 'static,
        added: bool,
    ) {
        self.store.add_calendar_account(email, tokens);
        if added {
            self.shared.data.send_modify(|data| {
                Arc::make_mut(data)
                    .loading_accounts
                    .insert(email.to_owned());
            });
        }
        self.post(Request::Accounts {
            added: added.then(|| email.to_owned()),
        });
    }

    /// Stops reading calendar account `email`, because Google signed it out or it was `removed`.
    /// Removing it forgets its list and the events only it had.
    pub fn forget_account(&self, email: &str, removed: bool) {
        self.post(Request::Forget {
            email: email.to_owned(),
            removed,
        });
    }

    /// Keeps the cache of colleagues' calendars by `cache` from now on: the app's settings, once
    /// at start and whenever they change. Whomever it no longer lists is forgotten at once.
    pub fn set_cache(&self, cache: CalendarCache) {
        self.nudge_cache(|current| *current = Some(cache));
    }

    /// Refreshes `email`'s cached calendar now, outside the schedule too (Calendar Settings'
    /// re-sync). They show as syncing at once, however long the task takes to get to it, until
    /// their answer lands, or the task finds nothing to do (they aren't listed, or Calendar
    /// can't be reached).
    pub fn resync_cache(&self, email: &str) {
        let email = email.to_lowercase();
        self.shared.data.send_modify(|data| {
            Arc::make_mut(data).cache_syncing.insert(email.clone());
        });
        self.post(Request::ResyncCache(email));
    }

    /// Has the cache of colleagues' calendars look at the clock now, for when the minute changed,
    /// maybe by more than the task slept (a Mac asleep).
    pub fn check_cache(&self) {
        self.nudge_cache(|_| {});
    }

    /// Changes the cache's settings by `change` and wakes the task to look at them, counted busy
    /// until it has.
    fn nudge_cache(&self, change: impl FnOnce(&mut Option<CalendarCache>)) {
        // Counted before the task can see the nudge, so it always finds the count to take over.
        if !self.shared.cache_nudged.swap(true, Ordering::SeqCst) {
            self.shared.busy.fetch_add(1, Ordering::SeqCst);
        }
        self.shared.cache.send_modify(change);
    }

    fn post(&self, request: Request) {
        self.shared.busy.fetch_add(1, Ordering::SeqCst);
        if self.requests.send(request).is_err() {
            self.shared.busy.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Nothing is being read or fetched.
    pub fn is_idle(&self) -> bool {
        self.shared.busy.load(Ordering::SeqCst) == 0
    }
}

/// A token for a change. It's random, so this session's tokens never meet one an earlier session
/// queued.
fn token() -> u64 {
    // SQLite's integers are signed.
    getrandom::u64().unwrap_or_else(|_| Timestamp::now().as_nanosecond().unsigned_abs() as u64) >> 1
}

struct Task {
    store: Store,
    loader: Loader,
    /// The task's own requests, for invites fetched off the task.
    requests: mpsc::UnboundedSender<Request>,
    presence: watch::Receiver<Presence>,
    /// The connection, whose return sends what's queued.
    connection: watch::Receiver<Connection>,
    repaint: Repaint,
    shared: Arc<Shared>,
    /// The window on screen.
    window: Option<Window>,
    /// Calendar and days fetched, and when.
    fetched: Vec<(String, Range<Date>, Instant)>,
    /// When the calendar list was last fetched.
    listed: Option<Instant>,
    /// When the next queued change is due, once its hold or its backoff is over.
    retry_at: Option<Timestamp>,
    /// The app's clock, which the cache's schedule goes by.
    clock: Clock,
    /// The cache's settings, once the app has said them.
    cache: watch::Receiver<Option<CalendarCache>>,
    /// When the cache was last refreshed, once known: at first, the oldest of the listed people's
    /// cached copies (`None` while someone listed has none).
    cache_last: Option<Option<Timestamp>>,
    /// The people `cache_last` covers. Anyone listed since is read at the next look inside the
    /// schedule.
    cache_covered: BTreeSet<String>,
    /// How many answers of the refresh under way haven't been handled yet. No refresh starts
    /// until they have.
    cache_reading: usize,
    /// Whose calendars the cache is reading now.
    cache_in_flight: BTreeSet<String>,
    /// When the cache next looks at the clock unprompted. A deadline, so that the task's other
    /// wakes don't put it off.
    cache_check_at: tokio::time::Instant,
}

/// Counts as busy until dropped.
struct Busy(Arc<Shared>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Task {
    /// Handles requests until the UI's handle is gone. A run after a panic (`resumed`) takes up
    /// the window on screen again; every run sends what's queued.
    async fn run(
        mut self,
        requests: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Request>>>,
        resumed: bool,
    ) {
        let mut requests = requests.lock().await;
        // `start`, or the restart, counted this.
        {
            let _busy = Busy(self.shared.clone());
            match self.store.calendars().await {
                Ok(stored) if !stored.is_empty() => self.publish_calendars(stored),
                Ok(_) => {}
                Err(error) => log::error!("reading calendars: {error}"),
            }
            self.list().await;
            let window = self
                .shared
                .window
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if resumed && let Some(window) = window {
                self.window = Some(window);
                self.fetch(false).await;
                self.read().await;
            }
            self.flush(false).await;
        }
        let mut watching = true;
        let mut cache_watching = true;
        loop {
            let wait = self.next_poll();
            let retry = self.retry_at.map(|at| {
                Duration::try_from(at.duration_since(Timestamp::now())).unwrap_or_default()
            });
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { return };
                    let _busy = Busy(self.shared.clone());
                    self.handle(request).await;
                }
                () = tokio::time::sleep(retry.unwrap_or_default()), if retry.is_some() => {
                    let _busy = self.busy();
                    self.flush(false).await;
                }
                changed = self.connection.changed(), if watching => match changed {
                    Ok(()) => {
                        let _busy = self.busy();
                        self.retry_guests();
                        self.flush(false).await;
                    }
                    Err(_) => watching = false,
                },
                changed = self.cache.changed(), if cache_watching => {
                    if changed.is_err() {
                        cache_watching = false;
                        continue;
                    }
                    let _busy = self.busy();
                    self.check_cache().await;
                }
                () = tokio::time::sleep_until(self.cache_check_at), if self.cache.borrow().is_some() => {
                    let _busy = self.busy();
                    self.check_cache().await;
                }
                () = tokio::time::sleep(wait) => {
                    let _busy = self.busy();
                    if self.listed.is_none_or(|at| at.elapsed() >= LIST_FRESH) {
                        self.list().await;
                    }
                    if self.fetch(true).await {
                        self.read().await;
                    }
                }
            }
        }
    }

    /// Forgets the cached calendars of people the cache doesn't list (everyone's, with the
    /// cache off), then, unless a refresh is still being read or Calendar can't be reached,
    /// refreshes everyone listed if it's time to, or, inside the schedule, whoever was listed
    /// since the last refresh.
    async fn check_cache(&mut self) {
        self.cache_check_at = tokio::time::Instant::now() + CACHE_CHECK;
        let settings = self.cache.borrow_and_update().clone();
        // The settings are taken up here, whichever wake brought them, so a nudge's busy count is
        // taken over here too. A nudge after this wakes the task again.
        let _nudge = self
            .shared
            .cache_nudged
            .swap(false, Ordering::SeqCst)
            .then(|| Busy(self.shared.clone()));
        let Some(settings) = settings else { return };
        let keep = if settings.enabled {
            settings.people.as_slice()
        } else {
            &[]
        };
        if let Err(error) = self.store.forget_cached_guests(keep).await {
            log::error!("forgetting colleagues' cached calendars: {error}");
        }
        self.note_listed(keep).await;
        if !settings.enabled || settings.people.is_empty() {
            // Switched on again, it starts from what's cached then.
            self.cache_last = None;
            self.cache_covered.clear();
            return;
        }
        // Someone taken off the list and put back is read again, as anyone newly listed is.
        self.cache_covered
            .retain(|email| settings.people.contains(email));
        // The last answer of the refresh being read looks again.
        if self.cache_reading > 0 {
            return;
        }
        let last = match self.cache_last {
            Some(last) => last,
            None => {
                let last = self.oldest_cached(&settings.people).await;
                self.cache_last = Some(last);
                self.cache_covered = settings.people.iter().cloned().collect();
                last
            }
        };
        if self.down() {
            return;
        }
        let now = self.clock.now();
        if cache::due(&settings, &now, last) {
            self.cache_last = Some(Some(now.timestamp()));
            self.cache_covered = settings.people.iter().cloned().collect();
            self.refresh_cache(settings.people.clone(), &now);
            return;
        }
        let added: Vec<String> = settings
            .people
            .iter()
            .filter(|email| !self.cache_covered.contains(*email))
            .cloned()
            .collect();
        if !added.is_empty() && cache::due(&settings, &now, None) {
            self.cache_covered.extend(added.iter().cloned());
            self.refresh_cache(added, &now);
        }
    }

    /// Keeps [`Data::cache_synced`] to `keep`, the people listed: whoever isn't any more is
    /// dropped, and whoever is but isn't there yet is looked up in the store.
    async fn note_listed(&self, keep: &[String]) {
        let (known, stale): (Vec<String>, bool) = {
            let data = self.shared.data.borrow();
            let known = keep
                .iter()
                .filter(|email| data.cache_synced.contains_key(*email))
                .cloned()
                .collect();
            let stale = data.cache_synced.keys().any(|email| !keep.contains(email))
                || data.cache_syncing.iter().any(|email| !keep.contains(email));
            (known, stale)
        };
        let mut found = Vec::new();
        for email in keep.iter().filter(|email| !known.contains(email)) {
            match self.store.cached_guest(email).await {
                Ok(Some(cached)) => found.push((email.clone(), cached.fetched_at)),
                Ok(None) => {}
                Err(error) => log::error!("reading a colleague's cached calendar: {error}"),
            }
        }
        if !stale && found.is_empty() {
            return;
        }
        self.edit(|data| {
            data.cache_synced.retain(|email, _| keep.contains(email));
            data.cache_syncing.retain(|email| keep.contains(email));
            data.cache_synced.extend(found);
        });
    }

    /// Notes that `email`'s calendar was cached at `at`, or is no longer cached (`None`).
    fn note_cached(&self, email: &str, at: Option<Timestamp>) {
        self.edit(|data| match at {
            Some(at) => {
                data.cache_synced.insert(email.to_owned(), at);
            }
            None => {
                data.cache_synced.remove(email);
            }
        });
    }

    /// Changes the data the UI reads, and asks for a frame.
    fn edit(&self, change: impl FnOnce(&mut Data)) {
        self.shared
            .data
            .send_modify(|data| change(Arc::make_mut(data)));
        self.repaint.request();
    }

    /// When the least recently cached of `people` was cached, or `None` when one of them isn't.
    async fn oldest_cached(&self, people: &[String]) -> Option<Timestamp> {
        let mut oldest: Option<Timestamp> = None;
        for email in people {
            match self.store.cached_guest(email).await {
                Ok(Some(cached)) => {
                    oldest = Some(oldest.map_or(cached.fetched_at, |at| at.min(cached.fetched_at)));
                }
                Ok(None) => return None,
                Err(error) => {
                    log::error!("reading a colleague's cached calendar: {error}");
                    return None;
                }
            }
        }
        oldest
    }

    /// Reads `people`'s calendars over the cache's window from `now` off the task, a few at a
    /// time. Each answer comes back as a request ([`Task::cache_answer`]), counted busy until it's
    /// handled, so the task answers others meanwhile.
    fn refresh_cache(&mut self, people: Vec<String>, now: &jiff::Zoned) {
        let (today, tz) = (now.date(), now.time_zone().clone());
        let (from, to) = cache::window_range(today, &tz);
        self.cache_reading += people.len();
        self.shared.busy.fetch_add(people.len(), Ordering::SeqCst);
        self.cache_in_flight.extend(people.iter().cloned());
        self.edit(|data| data.cache_syncing.extend(people.iter().cloned()));
        let (store, requests, shared) = (
            self.store.clone(),
            self.requests.clone(),
            self.shared.clone(),
        );
        tokio::spawn(async move {
            let mut answers = futures::stream::iter(people)
                .map(|email| {
                    let store = store.clone();
                    async move {
                        let found = store.attendee_events(&email, from, to).await;
                        (email, found)
                    }
                })
                .buffer_unordered(CACHE_READS);
            while let Some((email, found)) = answers.next().await {
                let answer = CacheAnswer {
                    email,
                    today,
                    tz: tz.clone(),
                    range: (from, to),
                    found: found.map_err(|error| error.to_string()),
                };
                if requests
                    .send(Request::CacheLoaded(Box::new(answer)))
                    .is_err()
                {
                    shared.busy.fetch_sub(1, Ordering::SeqCst);
                }
            }
        });
    }

    /// Puts a refresh's answer for one person in the cache, unless they were taken off the list
    /// or the cache switched off meanwhile. Google's answer replaces what's cached, and a
    /// calendar Google no longer shows is forgotten. One that couldn't be read stays as it was
    /// until the next refresh.
    async fn cache_answer(&mut self, answer: CacheAnswer) {
        let CacheAnswer {
            email,
            today,
            tz,
            range: (from, to),
            found,
        } = answer;
        self.cache_in_flight.remove(&email);
        self.edit(|data| {
            data.cache_syncing.remove(&email);
        });
        let listed = self
            .cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.enabled && cache.people.contains(&email));
        if !listed {
            return;
        }
        let outcome = match found {
            Ok(Some(events)) => {
                let cached = CachedGuest {
                    fetched_at: self.clock.now().timestamp(),
                    from,
                    to,
                    events: cache::keep_window(events, today, &tz),
                };
                let outcome = self.store.cache_guest(&email, &cached).await;
                if outcome.is_ok() {
                    self.note_cached(&email, Some(cached.fetched_at));
                }
                outcome
            }
            Ok(None) => self.forget_cached(&email).await,
            Err(error) => {
                log::warn!("refreshing a colleague's cached calendar: {error}");
                Ok(())
            }
        };
        if let Err(error) = outcome {
            log::error!("caching a colleague's calendar: {error}");
        }
    }

    /// Forgets `email`'s cached calendar, keeping everyone else's.
    async fn forget_cached(&self, email: &str) -> exo_store::Result<()> {
        self.note_cached(email, None);
        let keep: Vec<String> = self
            .cache
            .borrow()
            .iter()
            .flat_map(|cache| cache.people.iter())
            .filter(|person| !person.eq_ignore_ascii_case(email))
            .cloned()
            .collect();
        self.store.forget_cached_guests(&keep).await
    }

    /// Whether the cache is on and lists `email`.
    fn caches(&self, email: &str) -> bool {
        self.cache.borrow().as_ref().is_some_and(|cache| {
            cache.enabled
                && cache
                    .people
                    .iter()
                    .any(|person| person.eq_ignore_ascii_case(email))
        })
    }

    /// `email`'s cached events, while the cache lists them.
    async fn cached_events(&self, email: &str) -> Option<Vec<Event>> {
        if !self.caches(email) {
            return None;
        }
        match self.store.cached_guest(email).await {
            Ok(cached) => cached.map(|cached| cached.events),
            Err(error) => {
                log::error!("reading a colleague's cached calendar: {error}");
                None
            }
        }
    }

    /// Puts what a live read of a listed colleague's calendar over `range` found in the cache, as
    /// a refresh would: their events replace what's cached, and a calendar Google no longer shows
    /// is forgotten. A read that couldn't reach Google leaves the cache as it was, and so does one
    /// that doesn't cover the cache's whole window (Meet a week ahead), which the next refresh
    /// reads instead.
    async fn cache_live(
        &self,
        email: &str,
        range: (Timestamp, Timestamp),
        found: &Option<Option<Vec<Event>>>,
    ) {
        if !self.caches(email) {
            return;
        }
        let outcome = match found {
            Some(Some(events)) => {
                let now = self.clock.now();
                let (today, tz) = (now.date(), now.time_zone().clone());
                let (from, to) = cache::window_range(today, &tz);
                if range.0 > from || range.1 < to {
                    return;
                }
                let cached = CachedGuest {
                    fetched_at: now.timestamp(),
                    from,
                    to,
                    events: cache::keep_window(events.clone(), today, &tz),
                };
                let outcome = self.store.cache_guest(email, &cached).await;
                if outcome.is_ok() {
                    self.note_cached(email, Some(cached.fetched_at));
                }
                outcome
            }
            Some(None) => self.forget_cached(email).await,
            None => Ok(()),
        };
        if let Err(error) = outcome {
            log::error!("caching a colleague's calendar: {error}");
        }
    }

    /// Counts the task as busy until the guard drops.
    fn busy(&self) -> Busy {
        self.shared.busy.fetch_add(1, Ordering::SeqCst);
        Busy(self.shared.clone())
    }

    async fn handle(&mut self, request: Request) {
        match request {
            Request::Show(window) => {
                self.window = Some(window);
                self.set_loading(true);
                self.read().await;
                if self.fetch(false).await {
                    self.read().await;
                }
                self.set_loading(false);
            }
            Request::Queue {
                token,
                change,
                held,
            } => {
                // Undo took it back before it was queued.
                if !self.shared.tokens().undone.remove(&token) {
                    let hold = held.then(|| Timestamp::now() + HOLD);
                    if let Err(error) = self.store.queue_calendar_change(token, *change, hold).await
                    {
                        log::error!("queueing a calendar change: {error}");
                    }
                    self.read().await;
                }
                self.flush(false).await;
            }
            Request::Undo(token) => {
                if let Err(error) = self.store.unqueue_calendar_change(token).await {
                    log::error!("taking back a calendar change: {error}");
                }
                self.shared.tokens().undone.remove(&token);
                self.read().await;
            }
            Request::Busy(calendars, from, to) => {
                match self.store.free_busy(&calendars, from, to).await {
                    Ok(found) => {
                        let mut data = Data::clone(&self.shared.data.borrow());
                        data.busy = found.into_iter().collect();
                        data.busy_range = Some((from, to));
                        self.shared.data.send_replace(Arc::new(data));
                        self.repaint.request();
                    }
                    Err(error) => log::warn!("reading free/busy: {error}"),
                }
            }
            Request::SendNow(done) => {
                self.flush(true).await;
                let _ = done.send(());
            }
            Request::Invite(thread) => self.load_invite(thread).await,
            Request::Accounts { added } => {
                if let Some(email) = &added {
                    self.list_account(email).await;
                    self.shared.data.send_modify(|data| {
                        Arc::make_mut(data).loading_accounts.remove(email);
                    });
                }
                self.list().await;
                if self.fetch(false).await {
                    self.read().await;
                }
                self.flush(false).await;
            }
            Request::Forget { email, removed } => {
                if let Err(error) = self.store.remove_calendar_account(&email, removed).await {
                    log::error!("forgetting a calendar account's calendars: {error}");
                }
                match self.store.calendars().await {
                    Ok(calendars) => self.publish_calendars(calendars),
                    Err(error) => log::error!("reading calendars: {error}"),
                }
                self.read().await;
            }
            Request::Series(calendar, id) => match self.store.series_rules(&calendar, &id).await {
                Ok(rules) => {
                    let mut data = Data::clone(&self.shared.data.borrow());
                    data.series.insert(format!("{calendar}/{id}"), rules);
                    self.shared.data.send_replace(Arc::new(data));
                    self.repaint.request();
                }
                Err(error) => {
                    // Asked again next time the details open.
                    self.shared
                        .series_asked
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&format!("{calendar}/{id}"));
                    log::warn!("reading a series' rule: {error}");
                }
            },
            Request::Guests(guests, from, to) => self.load_guests(guests, (from, to)).await,
            Request::GuestLoaded(email, range, found) => {
                self.cache_live(&email, range, &found).await;
                // Google out of reach, a cached colleague's copy stands in, marked stale.
                let cached = match found {
                    None => self.cached_events(&email).await,
                    Some(_) => None,
                };
                let mut data = Data::clone(&self.shared.data.borrow());
                // A later ask's answer stands.
                if data
                    .guests
                    .get(&email)
                    .is_some_and(|calendar| calendar.range == range)
                {
                    let events = match found {
                        Some(Some(events)) => GuestEvents::Shown(events),
                        Some(None) => GuestEvents::Hidden,
                        None => cached.map_or(GuestEvents::Failed, GuestEvents::Stale),
                    };
                    data.guests.insert(
                        email,
                        GuestCalendar {
                            range,
                            loading: false,
                            events,
                        },
                    );
                    self.shared.data.send_replace(Arc::new(data));
                    self.repaint.request();
                }
            }
            Request::CacheLoaded(answer) => {
                self.cache_reading = self.cache_reading.saturating_sub(1);
                self.cache_answer(*answer).await;
                if self.cache_reading == 0 {
                    // Whoever was listed, or the time that passed, while it was read.
                    self.check_cache().await;
                }
            }
            Request::ResyncCache(email) => {
                if self.cache_in_flight.contains(&email) {
                    // Already being read; its answer ends the syncing.
                } else if self.caches(&email) && !self.down() {
                    let now = self.clock.now();
                    self.refresh_cache(vec![email], &now);
                } else {
                    self.edit(|data| {
                        data.cache_syncing.remove(&email);
                    });
                }
            }
            Request::InviteLoaded(thread, ics) => {
                let mut data = Data::clone(&self.shared.data.borrow());
                data.invite = ics.map(|ics| (thread, ics));
                self.shared.data.send_replace(Arc::new(data));
                self.repaint.request();
            }
            #[cfg(test)]
            Request::Panic => panic!("a test panic in the calendar's task"),
        }
    }

    /// How long the days on screen stay fresh.
    fn freshness(&self) -> Duration {
        if *self.presence.borrow() == Presence::Focused {
            FOCUSED
        } else {
            UNFOCUSED
        }
    }

    /// When to look at the days on screen again.
    fn next_poll(&self) -> Duration {
        let fresh = self.freshness();
        let oldest = self.fetched.iter().map(|(_, _, at)| *at).min();
        match (self.window.as_ref(), oldest) {
            (Some(_), Some(at)) => fresh
                .saturating_sub(at.elapsed())
                .max(Duration::from_secs(1)),
            (Some(_), None) => RETRY,
            (None, _) => LIST_FRESH,
        }
    }

    /// Finds thread `id`'s invite in the store and fetches its `.ics` off the task. The bytes
    /// come back as a request.
    async fn load_invite(&self, id: String) {
        let detail = match self
            .store
            .thread(&exo_store::ThreadId::new(id.clone()))
            .await
        {
            Ok(detail) => detail,
            Err(error) => return log::error!("reading thread {id}: {error}"),
        };
        let attachment = detail
            .as_ref()
            .and_then(|detail| {
                detail
                    .messages
                    .iter()
                    .find_map(crate::thread::invite::attachment)
            })
            .cloned();
        let shared = self.shared.clone();
        shared.busy.fetch_add(1, Ordering::SeqCst);
        let Some(attachment) = attachment else {
            if self.requests.send(Request::InviteLoaded(id, None)).is_err() {
                shared.busy.fetch_sub(1, Ordering::SeqCst);
            }
            return;
        };
        let requests = self.requests.clone();
        self.loader.attachment(&attachment, move |bytes| {
            if requests
                .send(Request::InviteLoaded(id, bytes.ok()))
                .is_err()
            {
                shared.busy.fetch_sub(1, Ordering::SeqCst);
            }
        });
    }

    /// Marks `guests`' calendars as loading over `range` and reads each off the task, at once and
    /// side by side. Meanwhile a cached colleague's copy shows (stale), if nothing of theirs is
    /// shown yet. With the connection down, they fail, or show their cached copy (stale).
    async fn load_guests(&self, guests: Vec<String>, range: (Timestamp, Timestamp)) {
        let down = self.down();
        let mut cached = BTreeMap::new();
        let wanted = {
            let data = self.shared.data.borrow();
            guests
                .iter()
                .filter(|email| {
                    down || data
                        .guests
                        .get(*email)
                        .is_none_or(|calendar| matches!(calendar.events, GuestEvents::Pending))
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        if !down {
            // Loading shows at once (Meet's spinner), not after the cache is looked at.
            self.edit(|data| {
                for email in &guests {
                    let events = data
                        .guests
                        .remove(email)
                        .map_or(GuestEvents::Pending, |calendar| calendar.events);
                    data.guests.insert(
                        email.clone(),
                        GuestCalendar {
                            range,
                            loading: true,
                            events,
                        },
                    );
                }
            });
        }
        for email in wanted {
            if let Some(events) = self.cached_events(&email).await {
                cached.insert(email, events);
            }
        }
        let mut data = Data::clone(&self.shared.data.borrow());
        for email in &guests {
            let events = data
                .guests
                .remove(email)
                .map_or(GuestEvents::Pending, |calendar| calendar.events);
            let events = match (events, cached.remove(email)) {
                (_, cached) if down => cached.map_or(GuestEvents::Failed, GuestEvents::Stale),
                (GuestEvents::Pending, Some(cached)) => GuestEvents::Stale(cached),
                (events, _) => events,
            };
            data.guests.insert(
                email.clone(),
                GuestCalendar {
                    range,
                    loading: !down,
                    events,
                },
            );
        }
        self.shared.data.send_replace(Arc::new(data));
        self.repaint.request();
        if down {
            return;
        }
        for email in guests {
            let (store, requests, shared) = (
                self.store.clone(),
                self.requests.clone(),
                self.shared.clone(),
            );
            shared.busy.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let found = match store.attendee_events(&email, range.0, range.1).await {
                    Ok(found) => Some(found),
                    Err(error) => {
                        log::warn!("reading a guest's calendar: {error}");
                        None
                    }
                };
                if requests
                    .send(Request::GuestLoaded(email, range, found))
                    .is_err()
                {
                    shared.busy.fetch_sub(1, Ordering::SeqCst);
                }
            });
        }
    }

    /// Once the connection is back, asks again for guests' calendars that couldn't be read.
    fn retry_guests(&self) {
        if self.down() {
            return;
        }
        let data = self.shared.data.borrow();
        let failed = data.guests.iter().filter(|(_, calendar)| {
            matches!(calendar.events, GuestEvents::Failed | GuestEvents::Stale(_))
        });
        let mut asked = self
            .shared
            .guests_asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = asked.len();
        for (email, _) in failed {
            asked.remove(email);
        }
        // The UI asks again on its next frame.
        if asked.len() < before {
            self.repaint.request();
        }
    }

    /// Whether Calendar is out of reach too, because the network or Google's sign-in is down or
    /// nothing of Google's answers. Gmail's own limits and a disabled Gmail don't hold the
    /// calendar.
    fn down(&self) -> bool {
        matches!(
            self.connection.borrow().outage,
            Some(Outage::Offline | Outage::Auth | Outage::Gmail)
        )
    }

    /// Sends each queued change whose turn it is (held ones too when `all`), in turn, unless
    /// the connection is down, whose return sends them. A change Calendar refused says so, and
    /// the calendar shows Calendar's copy again.
    async fn flush(&mut self, all: bool) {
        let mut changed = Vec::new();
        let mut progressed = true;
        while progressed {
            progressed = false;
            if self.down() {
                self.retry_at = None;
                break;
            }
            let (due, next) = match self.store.due_calendar_changes(all).await {
                Ok(found) => found,
                Err(error) => {
                    log::error!("reading the calendar's queue: {error}");
                    break;
                }
            };
            self.retry_at = next;
            for queued in due {
                // Once it's going out, a change can't be taken back. A change taken back since it
                // was read waits for Undo's request, which drops it.
                let undoable = {
                    let mut tokens = self.shared.tokens();
                    if tokens.undone.contains(&queued.token) {
                        continue;
                    }
                    tokens.undoable.remove(&queued.token)
                };
                let calendar = queued.change.calendar().to_owned();
                match self.store.send_calendar_change(&queued).await {
                    Ok(CalendarOutcome::Sent) => changed.push(calendar),
                    Ok(CalendarOutcome::Refused { refusal, .. }) => {
                        self.fail(Failure::of(queued.token, queued.change, refusal));
                        changed.push(calendar);
                    }
                    outcome => {
                        if let Err(error) = outcome {
                            log::error!("sending a calendar change: {error}");
                        }
                        if undoable {
                            self.shared.tokens().undoable.insert(queued.token);
                        }
                        continue;
                    }
                }
                progressed = true;
            }
        }
        if changed.is_empty() {
            return;
        }
        // A series' instances, or what a refusal left, come with a fetch.
        self.fetched.retain(|(id, ..)| !changed.contains(id));
        self.fetch(false).await;
        self.read().await;
    }

    fn fail(&self, failure: Failure) {
        self.shared
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(failure);
        self.repaint.request();
    }

    /// Lists every calendar account's calendars from Google, and publishes them.
    async fn list(&mut self) {
        let mut listed = false;
        for account in self.store.calendar_accounts() {
            listed |= self.list_account(&account).await;
        }
        if listed {
            self.listed = Some(Instant::now());
        }
        match self.store.calendars().await {
            Ok(calendars) => self.publish_calendars(calendars),
            Err(error) => log::error!("reading calendars: {error}"),
        }
    }

    /// Lists `account`'s calendars from Google. Returns whether Google answered.
    async fn list_account(&self, account: &str) -> bool {
        match self.store.sync_calendars(account).await {
            Ok(_) => true,
            Err(error) => {
                log::warn!("listing calendars: {error}");
                false
            }
        }
    }

    fn set_loading(&self, loading: bool) {
        let mut data = Data::clone(&self.shared.data.borrow());
        data.loading = loading;
        self.shared.data.send_replace(Arc::new(data));
        self.repaint.request();
    }

    fn publish_calendars(&self, calendars: Vec<AccountCalendar>) {
        let mut data = Data::clone(&self.shared.data.borrow());
        data.calendars = Some(calendars);
        self.shared.data.send_replace(Arc::new(data));
        self.repaint.request();
    }

    /// Fetches the window's days of each synced calendar that hasn't been (or, when
    /// `refresh`, was fetched longer ago than the freshness allows). Returns whether anything
    /// came.
    async fn fetch(&mut self, refresh: bool) -> bool {
        let Some(window) = self.window.clone() else {
            return false;
        };
        let fresh = self.freshness();
        let mut fetched_any = false;
        for calendar in &window.synced {
            let covered = self.fetched.iter().any(|(id, days, at)| {
                id == calendar
                    && days.start <= window.days.start
                    && days.end >= window.days.end
                    && (!refresh || at.elapsed() < fresh)
            });
            if covered {
                continue;
            }
            match self
                .store
                .sync_events(calendar, window.days.clone(), &window.tz)
                .await
            {
                Ok(()) => {
                    self.fetched.retain(|(id, days, _)| {
                        !(id == calendar
                            && days.start >= window.days.start
                            && days.end <= window.days.end)
                    });
                    self.fetched
                        .push((calendar.clone(), window.days.clone(), Instant::now()));
                    fetched_any = true;
                }
                Err(error) => log::warn!("fetching a calendar's events: {error}"),
            }
        }
        fetched_any
    }

    /// Reads the window's events and the queued changes made to them from the store, and
    /// publishes them.
    async fn read(&self) {
        let Some(window) = &self.window else { return };
        match self
            .store
            .events(&window.shown, window.days.clone(), &window.tz)
            .await
        {
            Ok(events) => {
                let mut data = Data::clone(&self.shared.data.borrow());
                data.window = Some(window.clone());
                data.events = events;
                self.shared.data.send_replace(Arc::new(data));
                self.repaint.request();
            }
            Err(error) => log::error!("reading events: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use exo_store::Cut;
    use gmail::calendar::EventDateTime;

    use super::*;

    const ME: &str = "maya@brightmoor.co";

    /// An account on a new gmail-mock, its store in a folder of its own.
    fn account(
        name: &str,
    ) -> (
        tokio::runtime::Runtime,
        gmail_mock::MockServer,
        gmail::Client,
        std::path::PathBuf,
        crate::account::Account,
    ) {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let server = runtime
            .block_on(gmail_mock::MockServer::start(gmail_mock::Config::default()))
            .expect("mock starts");
        let tokens = Arc::new(gmail::StaticToken(gmail_mock::DEV_ACCESS_TOKEN.into()));
        let gmail = gmail::Client::new(server.endpoints(), tokens.as_ref().clone());
        let dir = std::env::temp_dir().join(format!("exo-calendar-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let credentials = crate::account::Credentials {
            email: ME.into(),
            endpoints: server.endpoints(),
            tokens,
        };
        let account = crate::account::Account::open(&dir, credentials, egui::Context::default())
            .expect("opens");
        (runtime, server, gmail, dir, account)
    }

    /// Waits until the task is idle and `done` holds, for a few seconds at most.
    fn until(calendar: &CalendarSync, done: impl Fn(&Data) -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if calendar.is_idle() && done(&calendar.data()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn this_and_following_ends_an_all_day_series_on_the_day_before() {
        let (runtime, _server, gmail, dir, account) = account("truncate-all-day");
        let day = |date: &str| EventDateTime {
            date: Some(date.into()),
            ..EventDateTime::default()
        };
        let series = Event {
            summary: Some("Recycling out".into()),
            start: Some(day("2026-11-03")),
            end: Some(day("2026-11-04")),
            recurrence: vec!["RRULE:FREQ=WEEKLY".into()],
            ..Event::default()
        };
        let series = runtime
            .block_on(
                gmail
                    .calendar()
                    .insert_event(ME, &series, None)
                    .into_future(),
            )
            .expect("inserts")
            .id
            .expect("an id");
        // Deleting the third, on the 17th, and the ones after.
        account.calendar().hold(CalendarChange::Truncate {
            calendar: ME.into(),
            series: series.clone(),
            from: Cut::Day(Date::constant(2026, 11, 17)),
            notify: false,
        });
        // Quitting sends what's held.
        drop(account);
        let instances = runtime
            .block_on(
                gmail
                    .calendar()
                    .instances(ME, &series, None, Some("2027-01-01T00:00:00Z"))
                    .into_future(),
            )
            .expect("instances");
        let days: Vec<String> = instances
            .items
            .iter()
            .filter_map(|event| event.start.as_ref()?.date.clone())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(days, ["2026-11-03", "2026-11-10"]);
    }

    #[test]
    fn the_task_runs_again_after_a_panic_and_keeps_its_window_and_held_changes() {
        let (runtime, _server, gmail, dir, account) = account("calendar-panic");
        let calendar = account.calendar();
        let tz = TimeZone::get("America/New_York").expect("zone");
        let friday = Date::constant(2026, 11, 20);
        calendar.show(Window {
            days: friday..friday.tomorrow().expect("a day"),
            shown: vec![ME.into()],
            synced: vec![ME.into()],
            tz: tz.clone(),
        });
        let at = |hour: i8| EventDateTime {
            date_time: Some(
                friday
                    .at(hour, 0, 0, 0)
                    .to_zoned(tz.clone())
                    .expect("exists")
                    .strftime("%Y-%m-%dT%H:%M:%S%:z")
                    .to_string(),
            ),
            ..EventDateTime::default()
        };
        let lunch = Event {
            id: Some("e0lunch0009".into()),
            summary: Some("Lunch".into()),
            start: Some(at(12)),
            end: Some(at(13)),
            ..Event::default()
        };
        let token = calendar.hold(CalendarChange::Create {
            calendar: ME.into(),
            event: lunch,
        });
        let has_lunch = |data: &Data| {
            data.events
                .iter()
                .any(|stored| stored.event.summary.as_deref() == Some("Lunch"))
        };
        assert!(until(calendar, has_lunch), "held, it shows");

        calendar.post(Request::Panic);
        // The task that runs again has the window, and Undo still takes the change back.
        assert!(calendar.undo(token));
        assert!(
            until(calendar, |data| !has_lunch(data)),
            "the run after the panic took it back"
        );
        drop(account);
        let query = gmail::calendar::EventsQuery {
            q: Some("Lunch".into()),
            ..Default::default()
        };
        let found = runtime
            .block_on(gmail.calendar().events(ME, &query).into_future())
            .expect("lists");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found.items.is_empty(), "Calendar never heard of it");
    }
}
