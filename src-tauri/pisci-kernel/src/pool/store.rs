//! Thin async facade around the shared `Database`.
//!
//! Services never hold the raw `Arc<Mutex<Database>>`; they go through a
//! `PoolStore` so the lock noise stays in one place and so a future
//! "fake" or "in-memory" store can stand in for tests.

use crate::store::Database;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared handle to the database that pool services mutate.
///
/// Cloning is free (it only bumps the `Arc` refcount). Every method
/// takes `&self` so callers can hand the same handle to multiple
/// concurrent services.
#[derive(Clone)]
pub struct PoolStore {
    db: Arc<Mutex<Database>>,
}

impl PoolStore {
    pub fn new(db: Arc<Mutex<Database>>) -> Self {
        Self { db }
    }

    /// Expose the inner handle for call sites that need to pass it to
    /// legacy APIs (agent loop, memory tool, …). Prefer the `read` /
    /// `write` combinators below for new code.
    pub fn inner(&self) -> Arc<Mutex<Database>> {
        self.db.clone()
    }

    /// Run a synchronous closure under the DB mutex. Both `read` and
    /// `write` currently share the same body — `rusqlite::Connection`
    /// uses interior mutability, so the mutex is the only locking
    /// primitive we need. They are kept as distinct methods because
    /// call sites read more clearly when intent is explicit.
    pub async fn read<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Database) -> anyhow::Result<R>,
    {
        let guard = self.db.lock().await;
        f(&guard)
    }

    pub async fn write<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Database) -> anyhow::Result<R>,
    {
        let guard = self.db.lock().await;
        f(&guard)
    }
}
