//! Searching an Outlook account: Exo's query as KQL for Graph's `$search`, Graph's hits narrowed
//! by the labels KQL can't ask for, and a search's select-all.

use crate::support;

use std::collections::BTreeSet;

use exo_store::{
    BulkTarget, ListCutoff, ListId, Mutation, Progress, Relabelled, Remote, Scope, Search, Store,
};
use mock_servers::{Config, DEV_ACCESS_TOKEN, MockServer};
use outlook::well_known;
use support::{TempDir, options};

struct Outlook {
    _server: MockServer,
    _dir: TempDir,
    store: Store,
    graph: outlook::Client,
}

impl Outlook {
    async fn synced() -> Self {
        let server = MockServer::start(Config::default())
            .await
            .expect("mock starts");
        let dir = TempDir::new();
        let store = Store::open_with(
            dir.path(),
            "microsoft maya@brightmoor.co",
            Remote::outlook(
                outlook::Endpoints::mock(&server.url()),
                outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
            ),
            options(),
        )
        .await
        .expect("store opens");
        let graph = outlook::Client::new(
            outlook::Endpoints::mock(&server.url()),
            outlook::StaticToken(DEV_ACCESS_TOKEN.into()),
        );
        for _ in 0..80 {
            if !store.sync().await.expect("sync").backfilling {
                break;
            }
        }
        Self {
            _server: server,
            _dir: dir,
            store,
            graph,
        }
    }

    /// The conversations Graph's own search finds for `kql`, in `folder` when given.
    async fn graph_finds(&self, kql: &str, folder: Option<&str>) -> BTreeSet<String> {
        let mail = self.graph.mail();
        let call = match folder {
            Some(folder) => mail.folder_messages(folder),
            None => mail.messages(),
        };
        call.search(&format!("\"{kql}\""))
            .select("conversationId,isDraft")
            .top(500)
            .all()
            .await
            .expect("searches")
            .value
            .into_iter()
            .filter(|message| message.is_draft != Some(true))
            .filter_map(|message| message.conversation_id)
            .collect()
    }

    /// The conversations Exo's search of Outlook finds for `raw`, every page.
    async fn exo_finds(&self, raw: &str) -> BTreeSet<String> {
        let mut search = Search::new(raw);
        loop {
            let page = self.store.search_gmail(&search).await.expect("searches");
            search.add_gmail(page);
            if search.gmail_done() {
                break;
            }
        }
        self.store
            .search(&mut search, 500, true, 15)
            .await
            .expect("reads")
            .results
            .into_iter()
            .map(|result| result.thread.id.as_str().to_owned())
            .collect()
    }

    /// How many messages Exo's search of Outlook finds for `raw`, every page.
    async fn hits(&self, raw: &str) -> usize {
        let mut search = Search::new(raw);
        loop {
            let page = self.store.search_gmail(&search).await.expect("searches");
            search.add_gmail(page);
            if search.gmail_done() {
                return search.gmail_hits();
            }
        }
    }

    /// A word from the newest Inbox conversation's subject.
    async fn word(&self) -> String {
        let page = self
            .store
            .list(&ListId::Inbox, None, 1)
            .await
            .expect("lists");
        let subject = &page.threads.first().expect("mail").subject;
        subject
            .split(|c: char| !c.is_alphanumeric())
            .max_by_key(|word| word.len())
            .expect("a word")
            .to_lowercase()
    }
}

#[tokio::test]
async fn a_word_search_finds_what_graph_finds() {
    let outlook = Outlook::synced().await;
    let word = outlook.word().await;
    let found = outlook.exo_finds(&word).await;
    assert!(!found.is_empty());
    assert!(
        outlook.graph_finds(&word, None).await.is_subset(&found),
        "every conversation Graph finds for {word:?}"
    );
}

#[tokio::test]
async fn labels_and_read_state_narrow_graphs_hits() {
    let outlook = Outlook::synced().await;
    let word = outlook.word().await;
    let messages = |folder: Option<&'static str>, kql: Option<String>| {
        let graph = outlook.graph.clone();
        async move {
            let mail = graph.mail();
            let mut call = match folder {
                Some(folder) => mail.folder_messages(folder),
                None => mail.messages(),
            };
            if let Some(kql) = &kql {
                call = call.search(&format!("\"{kql}\""));
            }
            call.select("id,isDraft,parentFolderId")
                .top(1000)
                .all()
                .await
                .expect("lists")
                .value
                .into_iter()
                .filter(|message| message.is_draft != Some(true))
                .count()
        }
    };
    // The word in the Inbox alone: Graph's hits anywhere, less those outside it.
    assert_eq!(
        outlook.hits(&format!("in:inbox {word}")).await,
        messages(Some(well_known::INBOX), Some(word.clone())).await
    );
    // Nothing KQL can ask: every message is a candidate, and the read state picks, Junk Email
    // and Deleted Items left out.
    let junk = outlook
        .graph
        .mail()
        .folder(well_known::JUNK_EMAIL)
        .await
        .expect("Junk");
    let deleted = outlook
        .graph
        .mail()
        .folder(well_known::DELETED_ITEMS)
        .await
        .expect("Trash");
    let unread_elsewhere = outlook
        .graph
        .mail()
        .messages()
        .filter("isRead eq false")
        .select("id,isDraft,parentFolderId")
        .top(1000)
        .all()
        .await
        .expect("lists")
        .value
        .into_iter()
        .filter(|message| message.is_draft != Some(true))
        .filter(|message| {
            message.parent_folder_id != junk.id && message.parent_folder_id != deleted.id
        })
        .count();
    assert!(unread_elsewhere > 0);
    assert_eq!(outlook.hits("is:unread").await, unread_elsewhere);
}

#[tokio::test]
async fn a_searchs_select_all_reaches_what_it_finds() {
    let outlook = Outlook::synced().await;
    let word = outlook.word().await;
    let page = outlook
        .store
        .list(&ListId::Inbox, None, 1)
        .await
        .expect("lists");
    let newest = page.threads.first().expect("mail").date;
    let target = BulkTarget {
        list: Some(ListCutoff {
            scope: Scope::Search(word.clone()),
            filter: None,
            keep: Vec::new(),
            before: newest,
        }),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let mut done = Relabelled::default();
    outlook
        .store
        .bulk(&target, &Mutation::Star, &Progress::default(), &mut done)
        .await
        .expect("stars what the search finds");
    let starred: BTreeSet<String> = outlook
        .graph
        .mail()
        .messages()
        .filter("flag/flagStatus eq 'flagged'")
        .select("conversationId")
        .top(500)
        .all()
        .await
        .expect("lists")
        .value
        .into_iter()
        .filter_map(|message| message.conversation_id)
        .collect();
    assert!(
        outlook.graph_finds(&word, None).await.is_subset(&starred),
        "every conversation the search finds is starred"
    );
}
