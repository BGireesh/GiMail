//! The account's log of bulk actions, a week of them. The Bulk Actions dialog lists them after a
//! relaunch, each with what it changed, so it can still be put back, and one a quit stopped
//! carries on. Gmail has no record of them, so Clear Cache and Restart keeps the log.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use toasty::Json;

use crate::bulk::{BulkTarget, Relabelled};
use crate::db::BulkRow;
use crate::error::Result;
use crate::mutation::Mutation;
use crate::store::Store;

/// How long the log keeps a bulk action.
const KEPT: SignedDuration = SignedDuration::from_hours(7 * 24);

/// A bulk action as the log keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkRecord {
    /// What it does, as the Bulk Actions dialog names it.
    pub name: String,
    /// What it's on, for the dialog.
    pub query: Option<String>,
    /// What it does to each thread.
    pub mutation: Mutation,
    /// The threads it's for.
    pub target: BulkTarget,
    /// Where it is.
    pub state: BulkState,
    /// How many threads it changed, or put back once undone.
    pub count: usize,
}

/// Where a logged bulk action is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkState {
    /// Still to run, or to finish running.
    Pending,
    /// Every thread changed.
    Done,
    /// Still to be put back, or to finish being put back.
    Undoing,
    /// Put back, or cancelled before it ran.
    Undone,
    /// Gmail failed it. What it changed by then can still be put back.
    Failed,
}

/// A bulk action read back from the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoggedBulk {
    /// Its entry, which [`Store::relog_bulk`] updates.
    pub id: u64,
    /// What the log keeps of it.
    pub record: BulkRecord,
    /// What it has changed, which undoing it puts back.
    pub changed: Relabelled,
}

impl Store {
    /// The bulk actions of the last week, oldest first. Older ones are forgotten.
    ///
    /// # Errors
    /// On database failures.
    pub async fn bulk_log(&self) -> Result<Vec<LoggedBulk>> {
        let mut db = self.inner.db.clone();
        let since = (Timestamp::now() - KEPT).as_millisecond();
        let mut write = self.inner.begin(&mut db).await?;
        BulkRow::filter(BulkRow::fields().started_at().lt(since))
            .delete()
            .exec(write.exec())
            .await?;
        write.commit().await?;
        let mut rows = BulkRow::all().exec(&mut db).await?;
        rows.sort_by_key(|row| row.id);
        Ok(rows
            .into_iter()
            .map(|row| LoggedBulk {
                id: row.id,
                record: row.record.0,
                changed: row.changed.0,
            })
            .collect())
    }

    /// Logs `record`, a bulk action starting now that has changed `changed` so far. Returns its
    /// entry's id.
    ///
    /// # Errors
    /// On database failures.
    pub async fn log_bulk(&self, record: &BulkRecord, changed: &Relabelled) -> Result<u64> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let row = BulkRow::create()
            .started_at(Timestamp::now().as_millisecond())
            .record(Json(record.clone()))
            .changed(Json(changed.clone()))
            .exec(write.exec())
            .await?;
        write.commit().await?;
        Ok(row.id)
    }

    /// Records where logged bulk action `id` is, and what it has changed.
    ///
    /// # Errors
    /// On database failures.
    pub async fn relog_bulk(
        &self,
        id: u64,
        record: &BulkRecord,
        changed: &Relabelled,
    ) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        BulkRow::filter_by_id(id)
            .update()
            .record(Json(record.clone()))
            .changed(Json(changed.clone()))
            .exec(write.exec())
            .await?;
        write.commit().await
    }

    /// Records what logged bulk action `id` has changed so far, as it runs.
    pub(crate) async fn checkpoint_bulk(&self, id: u64, changed: &Relabelled) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        BulkRow::filter_by_id(id)
            .update()
            .changed(Json(changed.clone()))
            .exec(write.exec())
            .await?;
        write.commit().await
    }
}
