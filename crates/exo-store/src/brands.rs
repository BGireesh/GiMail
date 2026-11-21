//! What the app last found of each sending domain's brand (its BIMI record): when to look again,
//! the logo and certificate the record names, and what came of checking them. A JSON map in `kv`,
//! so it needs no schema change, and Clear Cache drops it, since DNS has it.

use std::collections::HashMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::Result;
use crate::store::Store;

/// Where the records are kept in `kv`.
const KEY: &str = "brands";

/// A domain's brand, as last looked up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandRecord {
    /// When to look it up again.
    pub next: Timestamp,
    /// The `l=` logo's HTTPS URL, when the record names one.
    pub logo: Option<String>,
    /// The `a=` certificate's HTTPS URL, when the record names one.
    pub certificate: Option<String>,
    /// What came of fetching and checking the logo and certificate. `None` until they're fetched.
    pub checked: Option<BrandCheck>,
}

/// What came of fetching a brand's logo and certificate and checking them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrandCheck {
    /// The certificate vouches for the logo until it expires.
    Valid { until: Timestamp },
    /// It doesn't, or the logo or certificate couldn't be fetched. Either is tried again when the
    /// record is next looked up.
    Invalid,
}

impl Store {
    /// The domains' brands, by domain, as [`save_brand`](Self::save_brand) left them.
    ///
    /// # Errors
    /// On database failures.
    pub async fn brands(&self) -> Result<HashMap<String, BrandRecord>> {
        let mut db = self.inner.db.clone();
        Ok(db::kv_get(&mut db, KEY).await?.unwrap_or_default())
    }

    /// Keeps `record` as `domain`'s brand.
    ///
    /// # Errors
    /// On database failures.
    pub async fn save_brand(&self, domain: &str, record: &BrandRecord) -> Result<()> {
        let mut db = self.inner.db.clone();
        let mut write = self.inner.begin(&mut db).await?;
        let mut brands: HashMap<String, BrandRecord> =
            db::kv_get(write.exec(), KEY).await?.unwrap_or_default();
        brands.insert(domain.to_owned(), record.clone());
        db::kv_set(write.exec(), KEY, &brands).await?;
        write.commit().await
    }
}
