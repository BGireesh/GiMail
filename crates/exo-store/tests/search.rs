//! Search: the local index, Gmail's search on top, and suggestions from local mail.

mod support;

use exo_store::{
    Highlighted, ListId, Mutation, SaveNote, Search, SearchPage, ThreadId, ThreadSummary,
};
use gmail::ListQuery;
use support::Harness;

/// The window the first sync stores (All Mail's 31 days).
const WINDOW_MS: i64 = 31 * 24 * 60 * 60 * 1000;

async fn search(harness: &Harness, query: &str) -> SearchPage {
    let mut search = Search::new(query);
    harness
        .store
        .search(&mut search, 100, true, 15)
        .await
        .expect("search")
}

fn subjects(page: &SearchPage) -> Vec<&str> {
    page.results
        .iter()
        .map(|result| result.thread.subject.as_str())
        .collect()
}

fn bold(text: &Highlighted) -> Vec<&str> {
    text.bold
        .iter()
        .map(|range| &text.text[range.clone()])
        .collect()
}

#[tokio::test]
async fn the_index_finds_words_in_bodies_and_shows_them_in_bold() {
    let harness = Harness::synced().await;
    // Emeka's mother: "…can you make your sweet potato thing again?"
    let page = search(&harness, "Potato").await;
    let thanksgiving = page
        .results
        .iter()
        .find(|result| result.thread.subject.starts_with("Thanksgiving"))
        .expect("found locally");
    let snippet = thanksgiving.snippet.as_ref().expect("an excerpt");
    assert_eq!(bold(snippet), ["potato"], "{snippet:?}");
    assert!(thanksgiving.subject.is_none(), "the subject has no match");
    assert!(
        thanksgiving.thread.messages > 1,
        "the row counts its messages"
    );
    assert!(
        !thanksgiving.thread.removed_by.contains(&Mutation::MarkDone),
        "a plain search keeps done threads"
    );
    assert!(
        thanksgiving.thread.removed_by.contains(&Mutation::Trash),
        "but not trashed ones"
    );

    let page = search(&harness, "subject:thanksgiving").await;
    let subject = page.results[0]
        .subject
        .as_ref()
        .expect("a highlighted subject");
    assert_eq!(bold(subject), ["Thanksgiving"]);
}

#[tokio::test]
async fn phrases_are_checked_as_typed_and_highlighted_whole() {
    let harness = Harness::synced().await;
    let page = search(&harness, "\"sweet potato\"").await;
    let thanksgiving = page
        .results
        .iter()
        .find(|result| result.thread.subject.starts_with("Thanksgiving"))
        .expect("found locally");
    assert_eq!(
        bold(thanksgiving.snippet.as_ref().expect("an excerpt")),
        ["sweet potato"],
        "the phrase in bold, not its words"
    );
    // The index stems `potatoes` to `potato`, so "sweet potato thing" matches it in the index.
    let page = search(&harness, "\"potatoes thing\"").await;
    assert!(
        !subjects(&page)
            .iter()
            .any(|subject| subject.starts_with("Thanksgiving")),
        "{:?}",
        subjects(&page)
    );
}

#[tokio::test]
async fn operators_narrow_the_index() {
    let harness = Harness::synced().await;
    let from = search(&harness, "from:emekao@gmail.com").await;
    assert!(
        subjects(&from).contains(&"Thanksgiving 🦃"),
        "{:?}",
        subjects(&from)
    );
    assert_eq!(
        subjects(&search(&harness, "potato label:family").await),
        ["Thanksgiving 🦃"]
    );
    let not_family = search(&harness, "potato -label:family").await;
    assert!(not_family.results.is_empty(), "{:?}", subjects(&not_family));
    assert!(search(&harness, "potato in:spam").await.results.is_empty());
    let inbox = search(&harness, "in:inbox").await;
    assert!(!inbox.results.is_empty());
    assert!(
        inbox
            .results
            .iter()
            .all(|result| result.thread.removed_by.contains(&Mutation::MarkDone)),
        "Done takes a thread out of in:inbox"
    );
    let local = harness.ids(&ListId::Inbox).await;
    assert!(
        inbox
            .results
            .iter()
            .all(|result| local.contains(result.thread.id.as_str()))
    );
    let attachments = search(&harness, "has:attachment").await;
    assert!(
        !attachments.results.is_empty()
            && attachments
                .results
                .iter()
                .all(|result| result.thread.attachments != exo_store::Attachments::None)
    );
    let none = search(&harness, "zzqx").await;
    assert!(none.results.is_empty());
    let dates = search(&harness, "before:2000/01/01").await;
    assert!(dates.results.is_empty());
    let results = search(&harness, "-in:inbox").await.results;
    assert!(
        results
            .windows(2)
            .all(|pair| pair[0].thread.date >= pair[1].thread.date),
        "newest first"
    );
}

#[tokio::test]
async fn the_index_follows_triage() {
    let harness = Harness::synced().await;
    let before = search(&harness, "in:inbox").await;
    let first = before.results[0].thread.id.clone();
    harness
        .store
        .apply(std::slice::from_ref(&first), Mutation::MarkDone)
        .await
        .expect("applies");
    let after = search(&harness, "in:inbox").await;
    assert!(after.results.iter().all(|result| result.thread.id != first));
    assert!(
        search(&harness, "in:done")
            .await
            .results
            .iter()
            .any(|result| result.thread.id == first)
    );
}

#[tokio::test]
async fn gmail_finds_what_the_window_left_out() {
    let harness = Harness::synced().await;
    let query = "from:newsletter@freightsignals.com";
    let mut session = Search::new(query);
    let local = harness
        .store
        .search(&mut session, 200, true, 15)
        .await
        .expect("search");
    let gmail = harness
        .gmail_ids(ListQuery {
            q: Some(query.into()),
            ..ListQuery::default()
        })
        .await;
    assert!(
        local.results.len() < gmail.len(),
        "{} local of {}",
        local.results.len(),
        gmail.len()
    );
    let cutoff = jiff::Timestamp::now().as_millisecond() - WINDOW_MS;
    while !session.gmail_done() {
        let page = harness.store.search_gmail(&session).await.expect("gmail");
        session.add_gmail(page);
    }
    let merged = harness
        .store
        .search(&mut session, 200, true, 15)
        .await
        .expect("search");
    let found: std::collections::BTreeSet<String> = merged
        .results
        .iter()
        .map(|result| result.thread.id.as_str().to_owned())
        .collect();
    assert_eq!(found, gmail);
    assert!(
        merged
            .results
            .iter()
            .any(|result| result.thread.date.as_millisecond() < cutoff),
        "older than the window"
    );
    assert!(
        merged
            .results
            .windows(2)
            .all(|pair| pair[0].thread.date >= pair[1].thread.date)
    );
}

#[tokio::test]
async fn hostile_queries_are_only_searched_for() {
    let harness = Harness::synced().await;
    let inbox = harness.ids(&ListId::Inbox).await;
    for query in [
        "'); DROP TABLE threads; --",
        "\" OR 1=1 --",
        "NEAR(a b",
        "\"unterminated",
        "subject:\"a\"\"b\"",
        "from:\"x\" AND OR NOT",
        "label:\"); DELETE FROM search_fts; --",
        "* ^ :",
        "from:%_\\",
        "a:b:c in:\"",
        "after:'; --",
    ] {
        let page = search(&harness, query).await;
        assert!(page.results.len() <= 100, "{query}");
        let mut session = Search::new(query);
        harness
            .store
            .search_gmail(&session)
            .await
            .expect("Gmail is asked, not SQLite");
        session.add_gmail(exo_store::GmailPage::default());
    }
    assert_eq!(
        harness.ids(&ListId::Inbox).await,
        inbox,
        "nothing was dropped"
    );
    assert!(
        !search(&harness, "potato").await.results.is_empty(),
        "the index is intact"
    );
}

#[tokio::test]
async fn filters_narrow_a_list_and_keep_sticky_rows() {
    use exo_store::{Filter, ThreadId};
    let harness = Harness::synced().await;
    let inbox = harness
        .store
        .list(&ListId::Inbox, None, 500)
        .await
        .expect("list")
        .threads;
    let unread = harness
        .store
        .list_filtered(&ListId::Inbox, Some(Filter::Unread), &[], &[], None, 500)
        .await
        .expect("list")
        .threads;
    assert!(!unread.is_empty() && unread.len() < inbox.len());
    assert!(unread.iter().all(|thread| thread.unread));
    assert_eq!(
        unread.len(),
        inbox.iter().filter(|thread| thread.unread).count()
    );
    let starred = harness
        .store
        .list_filtered(&ListId::Inbox, Some(Filter::Starred), &[], &[], None, 500)
        .await
        .expect("list")
        .threads;
    assert!(starred.iter().all(|thread| thread.starred));
    // Paging through a filtered list sees each thread once.
    let first = harness
        .store
        .list_filtered(&ListId::Inbox, Some(Filter::Unread), &[], &[], None, 3)
        .await
        .expect("list");
    let rest = harness
        .store
        .list_filtered(
            &ListId::Inbox,
            Some(Filter::Unread),
            &[],
            &[],
            first.next.as_ref(),
            500,
        )
        .await
        .expect("list");
    let paged: Vec<&ThreadId> = first
        .threads
        .iter()
        .chain(&rest.threads)
        .map(|thread| &thread.id)
        .collect();
    assert_eq!(
        paged,
        unread.iter().map(|thread| &thread.id).collect::<Vec<_>>()
    );

    let read = unread[0].id.clone();
    harness
        .store
        .apply(std::slice::from_ref(&read), Mutation::MarkRead)
        .await
        .expect("applies");
    let gone = harness
        .store
        .list_filtered(&ListId::Inbox, Some(Filter::Unread), &[], &[], None, 500)
        .await
        .expect("list")
        .threads;
    assert!(gone.iter().all(|thread| thread.id != read));
    let kept = harness
        .store
        .list_filtered(
            &ListId::Inbox,
            Some(Filter::Unread),
            std::slice::from_ref(&read),
            &[],
            None,
            500,
        )
        .await
        .expect("list")
        .threads;
    assert!(
        kept.iter().any(|thread| thread.id == read),
        "a sticky row stays"
    );

    // A conversation unstarred in Starred stays where it was while it's held, even when paging.
    let starred = harness
        .store
        .list(&ListId::Starred, None, 500)
        .await
        .expect("list")
        .threads;
    assert!(starred.len() > 2, "the fixtures star a few");
    let unstarred = starred[1].id.clone();
    harness
        .store
        .apply(std::slice::from_ref(&unstarred), Mutation::Unstar)
        .await
        .expect("applies");
    let ids = |threads: &[exo_store::ThreadSummary]| {
        threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>()
    };
    assert!(
        !ids(&harness
            .store
            .list(&ListId::Starred, None, 500)
            .await
            .expect("list")
            .threads)
        .contains(&unstarred)
    );
    let held = std::slice::from_ref(&unstarred);
    let kept = harness
        .store
        .list_filtered(&ListId::Starred, None, &[], held, None, 500)
        .await
        .expect("list")
        .threads;
    assert_eq!(ids(&kept), ids(&starred), "in its place");
    assert!(!kept[1].starred, "showing it unstarred");
    let first = harness
        .store
        .list_filtered(&ListId::Starred, None, &[], held, None, 1)
        .await
        .expect("list");
    let second = harness
        .store
        .list_filtered(&ListId::Starred, None, &[], held, first.next.as_ref(), 1)
        .await
        .expect("list");
    let rest = harness
        .store
        .list_filtered(&ListId::Starred, None, &[], held, second.next.as_ref(), 500)
        .await
        .expect("list");
    assert_eq!(
        [first.threads, second.threads, rest.threads]
            .concat()
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>(),
        ids(&starred)
    );

    // Awaiting a reply lists Sent threads whose last message is the user's.
    let no_reply = harness
        .store
        .list_filtered(&ListId::All, Some(Filter::NoReply), &[], &[], None, 500)
        .await
        .expect("list")
        .threads;
    assert!(!no_reply.is_empty());
    let sent = harness.ids(&ListId::Sent).await;
    assert!(
        no_reply
            .iter()
            .all(|thread| sent.contains(thread.id.as_str()))
    );
}

/// A time `minutes` after a fixed start, so notes sort by when they were written.
fn at(minutes: i64) -> jiff::Timestamp {
    "2026-11-23T10:00:00Z"
        .parse::<jiff::Timestamp>()
        .expect("a time")
        + jiff::SignedDuration::from_mins(minutes)
}

/// Saves `text` as a new note on `thread` (on its first message when `message` is given), from
/// the editor `draft`.
async fn note(
    harness: &Harness,
    thread: &ThreadId,
    message: Option<&str>,
    draft: &str,
    text: &str,
    minutes: i64,
) -> exo_store::Note {
    harness
        .store
        .save_note(
            SaveNote {
                draft_id: draft.into(),
                note_id: None,
                thread_id: thread.clone(),
                message_id: message.map(str::to_owned),
                html: format!("<p>{text}</p>"),
                text: text.into(),
                files: Vec::new(),
            },
            at(minutes),
        )
        .await
        .expect("saves")
        .expect("a note")
}

/// The Thanksgiving conversation, whose mail says `potato`.
async fn thanksgiving(harness: &Harness) -> ThreadSummary {
    search(harness, "potato")
        .await
        .results
        .into_iter()
        .find(|result| result.thread.subject.starts_with("Thanksgiving"))
        .expect("found locally")
        .thread
}

/// An inbox conversation other than `not`.
async fn another(harness: &Harness, not: &ThreadId) -> ThreadSummary {
    harness
        .all(&ListId::Inbox)
        .await
        .into_iter()
        .find(|thread| thread.id != *not && thread.drafts == 0)
        .expect("another inbox thread")
}

fn ids(page: &SearchPage) -> Vec<&str> {
    page.results
        .iter()
        .map(|result| result.thread.id.as_str())
        .collect()
}

#[tokio::test]
async fn notes_terms_find_threads_by_their_notes() {
    let harness = Harness::synced().await;
    assert!(search(&harness, "overage").await.results.is_empty());
    let noted = thanksgiving(&harness).await;
    let other = another(&harness, &noted.id).await;
    let saved = note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    note(&harness, &other.id, None, "b", "Nothing to see", 1).await;

    let page = search(&harness, "notes:overage").await;
    assert_eq!(ids(&page), [noted.id.as_str()]);
    let result = &page.results[0];
    assert!(!result.deleted);
    assert_eq!(result.thread.notes, 1);
    assert_eq!(result.notes.len(), 1, "{:?}", result.notes);
    let hit = &result.notes[0];
    assert_eq!(hit.note_id, saved.id);
    assert_eq!(hit.label, "Thread note");
    assert_eq!(hit.date, at(0));
    assert_eq!(bold(&hit.snippet), ["overage"], "{:?}", hit.snippet);
    assert!(search(&harness, "notes:zzqx").await.results.is_empty());
}

#[tokio::test]
async fn a_bare_notes_term_lists_every_noted_thread() {
    let harness = Harness::synced().await;
    let first = thanksgiving(&harness).await;
    let second = another(&harness, &first.id).await;
    assert!(search(&harness, "notes:").await.results.is_empty());
    note(&harness, &first.id, None, "a", "One", 0).await;
    note(&harness, &second.id, None, "b", "Two", 1).await;

    let page = search(&harness, "notes:").await;
    let mut found = ids(&page);
    found.sort_unstable();
    let mut expected = vec![first.id.as_str(), second.id.as_str()];
    expected.sort_unstable();
    assert_eq!(found, expected);
    assert!(
        page.results.iter().all(|result| result.notes.is_empty()),
        "the row's own icon says it has notes"
    );
}

#[tokio::test]
async fn has_notes_is_the_same() {
    let harness = Harness::synced().await;
    let first = thanksgiving(&harness).await;
    let second = another(&harness, &first.id).await;
    note(&harness, &first.id, None, "a", "One", 0).await;
    note(&harness, &second.id, None, "b", "Two overage", 1).await;
    let bare = search(&harness, "notes:").await;
    for query in ["has:notes", "has:note", "note:"] {
        assert_eq!(ids(&search(&harness, query).await), ids(&bare), "{query}");
    }
    assert_eq!(
        ids(&search(&harness, "note:overage").await),
        [second.id.as_str()]
    );
}

#[tokio::test]
async fn plain_words_match_mail_or_notes() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;

    let page = search(&harness, "overage").await;
    assert_eq!(ids(&page), [noted.id.as_str()], "a word only in a note");
    let hits = &page.results[0].notes;
    assert_eq!(hits.len(), 1);
    assert_eq!(bold(&hits[0].snippet), ["overage"]);

    let page = search(&harness, "potato").await;
    let result = page
        .results
        .iter()
        .find(|result| result.thread.id == noted.id)
        .expect("a word only in mail");
    assert!(result.notes.is_empty(), "{:?}", result.notes);
    assert_eq!(
        bold(result.snippet.as_ref().expect("an excerpt")),
        ["potato"],
        "the mail's excerpt keeps its highlight"
    );

    let page = search(&harness, "potato overage").await;
    assert_eq!(ids(&page), [noted.id.as_str()], "one in each");
}

#[tokio::test]
async fn notes_and_plain_words_combine() {
    let harness = Harness::synced().await;
    let renewal = search(&harness, "renewal").await;
    let with_renewal = renewal.results[0].thread.id.clone();
    let without = harness
        .all(&ListId::Inbox)
        .await
        .into_iter()
        .find(|thread| !renewal.results.iter().any(|r| r.thread.id == thread.id))
        .expect("a thread without renewal");
    note(&harness, &with_renewal, None, "a", "Ping them on Slack", 0).await;
    note(&harness, &without.id, None, "b", "Slack them too", 1).await;

    let page = search(&harness, "notes:slack renewal").await;
    assert_eq!(ids(&page), [with_renewal.as_str()]);
    assert_eq!(page.results[0].notes.len(), 1);
    assert_eq!(bold(&page.results[0].notes[0].snippet), ["Slack"]);
}

#[tokio::test]
async fn from_combines_with_notes() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    assert_eq!(
        ids(&search(&harness, "from:emekao@gmail.com notes:overage").await),
        [noted.id.as_str()]
    );
    assert!(
        search(&harness, "from:nobody@nowhere.example notes:overage")
            .await
            .results
            .is_empty()
    );
}

#[tokio::test]
async fn a_phrase_matches_within_one_note() {
    let harness = Harness::synced().await;
    let split = thanksgiving(&harness).await;
    let whole = another(&harness, &split.id).await;
    note(&harness, &split.id, None, "a", "We should keep the cost", 0).await;
    note(&harness, &split.id, None, "b", "flat next year", 1).await;
    note(
        &harness,
        &whole.id,
        None,
        "c",
        "Please keep the cost flat",
        2,
    )
    .await;

    let page = search(&harness, "notes:\"keep the cost flat\"").await;
    assert_eq!(ids(&page), [whole.id.as_str()]);
    assert_eq!(
        bold(&page.results[0].notes[0].snippet),
        ["keep the cost flat"]
    );
    assert_eq!(
        ids(&search(&harness, "\"keep the cost flat\"").await),
        [whole.id.as_str()],
        "a plain phrase too"
    );
}

#[tokio::test]
async fn a_prefix_star_matches() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    assert_eq!(
        ids(&search(&harness, "notes:overa*").await),
        [noted.id.as_str()]
    );
    assert!(search(&harness, "notes:overa").await.results.is_empty());
}

#[tokio::test]
async fn a_deleted_note_is_not_found() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    let saved = note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    harness
        .store
        .delete_note(&saved.id, at(1))
        .await
        .expect("deletes");
    assert!(search(&harness, "notes:overage").await.results.is_empty());
    assert!(search(&harness, "overage").await.results.is_empty());
    assert!(search(&harness, "has:notes").await.results.is_empty());
}

#[tokio::test]
async fn negated_notes_excludes() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    let has = |page: &SearchPage| page.results.iter().any(|r| r.thread.id == noted.id);
    assert!(has(&search(&harness, "potato").await));
    assert!(!has(&search(&harness, "potato -notes:overage").await));
    assert!(!has(&search(&harness, "potato -has:notes").await));
    assert!(!has(&search(&harness, "potato -notes:").await));
    assert!(
        !has(&search(&harness, "potato -overage").await),
        "a word in a note"
    );
    assert!(has(&search(&harness, "potato -notes:zzqx").await));
    assert!(has(&search(&harness, "potato -notes:potato").await));
}

#[tokio::test]
async fn fts_syntax_in_a_notes_term_is_harmless() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    note(&harness, &noted.id, None, "a", "a-b OR (overage)", 0).await;
    for query in [
        "notes:\"",
        "notes:a-b",
        "notes:*",
        "notes:OR",
        "notes:(",
        "notes:\"x\"\" OR 1\"",
        "notes:NEAR(a",
        "-notes:\"",
        "notes:' OR 1=1 --",
        "overage*",
        "\"a-b\" notes:",
        "notes:x OR (",
        "-(notes:\" OR",
        "{notes:* from:\"}",
        "notes:a OR -{NEAR( notes:)}",
        "after:'; notes:x OR before:2000/01/01",
    ] {
        let mut session = Search::new(query);
        harness
            .store
            .search(&mut session, 100, true, 15)
            .await
            .unwrap_or_else(|error| panic!("{query}: {error}"));
    }
    assert_eq!(
        ids(&search(&harness, "notes:a-b").await),
        [noted.id.as_str()]
    );
}

#[tokio::test]
async fn notes_terms_never_ask_gmail() {
    let harness = Harness::synced().await;
    let session = Search::new("notes:overage from:ana");
    assert!(session.gmail_done(), "answered locally");
    let page = harness
        .store
        .search_gmail(&session)
        .await
        .expect("no call to make");
    assert!(page.hits.is_empty() && page.next.is_none());
    assert!(!Search::new("overage").gmail_done());
}

#[tokio::test]
async fn notes_on_a_deleted_thread_are_still_found() {
    let harness = Harness::synced().await;
    let noted = thanksgiving(&harness).await;
    let kept = another(&harness, &noted.id).await;
    note(
        &harness,
        &noted.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    note(&harness, &kept.id, None, "b", "Some overage here too", 1).await;
    harness
        .gmail
        .gmail()
        .delete_thread(noted.id.as_str())
        .await
        .expect("deletes");
    harness.store.sync().await.expect("syncs");
    assert!(
        harness
            .store
            .thread(&noted.id)
            .await
            .expect("reads")
            .is_none()
    );

    for query in ["notes:overage", "overage", "has:notes"] {
        let page = search(&harness, query).await;
        let gone = page
            .results
            .iter()
            .find(|result| result.thread.id == noted.id)
            .unwrap_or_else(|| panic!("{query}: {:?}", ids(&page)));
        assert!(gone.deleted, "{query}");
        assert_eq!(gone.thread.subject, noted.subject, "{query}");
        assert_eq!(gone.thread.notes, 1);
        assert!(!gone.thread.participants.is_empty());
        let other = page
            .results
            .iter()
            .find(|result| result.thread.id == kept.id)
            .expect("the stored one too");
        assert!(!other.deleted);
        assert!(
            page.results
                .windows(2)
                .all(|pair| pair[0].thread.date >= pair[1].thread.date),
            "{query}: by date"
        );
    }
    let page = search(&harness, "notes:overage").await;
    let gone = page
        .results
        .iter()
        .find(|result| result.thread.id == noted.id)
        .expect("found");
    assert_eq!(bold(&gone.notes[0].snippet), ["overage"]);
    assert!(
        !ids(&search(&harness, "notes:overage from:emekao@gmail.com").await)
            .contains(&noted.id.as_str()),
        "a mail term can't match a thread whose mail is gone"
    );
}

/// The ids of a page, as a set.
fn id_set(page: &SearchPage) -> std::collections::BTreeSet<String> {
    page.results
        .iter()
        .map(|result| result.thread.id.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn a_notes_phrase_matches_as_typed() {
    let harness = Harness::synced().await;
    let loose = thanksgiving(&harness).await;
    let exact = another(&harness, &loose.id).await;
    note(&harness, &loose.id, None, "a", "Keeping the costs flat", 0).await;
    let typed = note(&harness, &exact.id, None, "b", "We keep the cost flat", 1).await;
    note(&harness, &exact.id, None, "c", "Keeping the costs flat", 2).await;

    let page = search(&harness, "notes:\"keep the cost flat\"").await;
    assert_eq!(ids(&page), [exact.id.as_str()], "stemming doesn't stand in");
    let hits = &page.results[0].notes;
    assert_eq!(
        hits.iter()
            .map(|hit| hit.note_id.as_str())
            .collect::<Vec<_>>(),
        [typed.id.as_str()],
        "only the note that has it as typed"
    );
    assert_eq!(
        ids(&search(&harness, "\"keep the cost flat\"").await),
        [exact.id.as_str()],
        "the same as a plain phrase"
    );
}

/// The Thanksgiving thread with an `overage` note, and another inbox thread, not from Emeka, with
/// a `slack` note.
async fn two_noted(harness: &Harness) -> (ThreadSummary, ThreadSummary) {
    let first = thanksgiving(harness).await;
    let emeka = id_set(&search(harness, "from:emekao@gmail.com").await);
    let second = harness
        .all(&ListId::Inbox)
        .await
        .into_iter()
        .find(|thread| thread.id != first.id && !emeka.contains(thread.id.as_str()))
        .expect("a thread not from Emeka");
    note(
        harness,
        &first.id,
        None,
        "a",
        "Ask about the overage fee",
        0,
    )
    .await;
    note(harness, &second.id, None, "b", "Ping them on Slack", 1).await;
    (first, second)
}

fn set(ids: &[&ThreadId]) -> std::collections::BTreeSet<String> {
    ids.iter().map(|id| id.as_str().to_owned()).collect()
}

#[tokio::test]
async fn notes_or_notes_finds_either() {
    let harness = Harness::synced().await;
    let (first, second) = two_noted(&harness).await;
    let session = Search::new("notes:overage OR notes:slack");
    assert!(session.gmail_done(), "every match has a note");
    assert_eq!(
        id_set(&search(&harness, "notes:overage OR notes:slack").await),
        set(&[&first.id, &second.id])
    );
}

#[tokio::test]
async fn notes_or_a_word_finds_either() {
    let harness = Harness::synced().await;
    let (first, second) = two_noted(&harness).await;
    let mut expected = id_set(&search(&harness, "renewal").await);
    expected.insert(second.id.as_str().to_owned());
    assert!(!Search::new("notes:slack OR renewal").gmail_done());
    assert_eq!(
        id_set(&search(&harness, "notes:slack OR renewal").await),
        expected,
        "the word in mail"
    );
    assert_eq!(
        id_set(&search(&harness, "notes:zzqx OR overage").await),
        set(&[&first.id]),
        "the word in a note"
    );
}

#[tokio::test]
async fn notes_in_a_braced_or_with_from() {
    let harness = Harness::synced().await;
    let (_, second) = two_noted(&harness).await;
    let mut expected = id_set(&search(&harness, "from:emekao@gmail.com").await);
    expected.insert(second.id.as_str().to_owned());
    assert_eq!(
        id_set(&search(&harness, "{notes:slack from:emekao@gmail.com}").await),
        expected
    );
}

#[tokio::test]
async fn a_negated_group_with_notes() {
    let harness = Harness::synced().await;
    let (first, second) = two_noted(&harness).await;
    let found = id_set(&search(&harness, "-(notes:overage potato)").await);
    assert!(!found.contains(first.id.as_str()), "it has both");
    assert!(found.contains(second.id.as_str()), "it has neither");
    let potato = id_set(&search(&harness, "potato").await);
    assert!(
        potato
            .iter()
            .filter(|id| *id != first.id.as_str())
            .all(|id| found.contains(id)),
        "potato without the note"
    );
}

#[tokio::test]
async fn from_and_a_group_of_notes() {
    let harness = Harness::synced().await;
    let (first, _) = two_noted(&harness).await;
    assert_eq!(
        ids(&search(
            &harness,
            "from:emekao@gmail.com (notes:overage OR notes:slack)"
        )
        .await),
        [first.id.as_str()]
    );
}

/// Stores the freight newsletter's threads older than the window, as Gmail's search does, and
/// returns one of them.
async fn an_old_thread(harness: &Harness) -> ThreadId {
    let mut session = Search::new("from:newsletter@freightsignals.com");
    while !session.gmail_done() {
        let page = harness.store.search_gmail(&session).await.expect("gmail");
        session.add_gmail(page);
    }
    let cutoff = jiff::Timestamp::now().as_millisecond() - WINDOW_MS;
    harness
        .store
        .search(&mut session, 200, true, 15)
        .await
        .expect("search")
        .results
        .into_iter()
        .find(|result| result.thread.date.as_millisecond() < cutoff)
        .expect("older than the window")
        .thread
        .id
}

#[tokio::test]
async fn an_old_noted_thread_is_found_online_by_its_note() {
    let harness = Harness::synced().await;
    let old = an_old_thread(&harness).await;
    assert!(
        !ids(&search(&harness, "from:newsletter@freightsignals.com").await).contains(&old.as_str()),
        "online, the local results stop at the window"
    );
    note(&harness, &old, None, "a", "Ask about the overage fee", 0).await;
    assert_eq!(ids(&search(&harness, "overage").await), [old.as_str()]);
}

#[tokio::test]
async fn without_notes_gmail_still_adds_the_rest() {
    let harness = Harness::synced().await;
    let old = an_old_thread(&harness).await;
    note(&harness, &old, None, "a", "Ask about the overage fee", 0).await;
    let query = "from:newsletter@freightsignals.com -has:notes";
    let mut session = Search::new(query);
    assert!(
        !session.gmail_done(),
        "matches without notes may be only in Gmail"
    );
    let local = harness
        .store
        .search(&mut session, 200, true, 15)
        .await
        .expect("search");
    while !session.gmail_done() {
        let page = harness.store.search_gmail(&session).await.expect("gmail");
        session.add_gmail(page);
    }
    let merged = harness
        .store
        .search(&mut session, 200, true, 15)
        .await
        .expect("search");
    let found = id_set(&merged);
    assert!(!found.contains(old.as_str()), "Gmail's match has a note");
    assert!(found.len() > local.results.len(), "Gmail added older ones");
    let cutoff = jiff::Timestamp::now().as_millisecond() - WINDOW_MS;
    assert!(
        merged
            .results
            .iter()
            .any(|result| result.thread.date.as_millisecond() < cutoff)
    );
}

#[tokio::test]
async fn deleted_rows_share_the_limit_by_date() {
    let harness = Harness::synced().await;
    let (gone, kept) = two_noted(&harness).await;
    harness
        .gmail
        .gmail()
        .delete_thread(gone.id.as_str())
        .await
        .expect("deletes");
    harness.store.sync().await.expect("syncs");
    let all = search(&harness, "has:notes").await;
    assert_eq!(id_set(&all), set(&[&gone.id, &kept.id]));
    let mut session = Search::new("has:notes");
    let one = harness
        .store
        .search(&mut session, 1, true, 15)
        .await
        .expect("search");
    assert_eq!(ids(&one), [all.results[0].thread.id.as_str()], "the newer");
    assert!(one.more_local);
}
