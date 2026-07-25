use crate::connection::client::Connection;
use anyhow::Result;
use gpui::{App, AppContext as _, Task};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)]
pub struct TransactionSession {
    conn: Connection,
    open: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl TransactionSession {
    pub fn new(conn: Connection) -> Self {
        TransactionSession {
            conn,
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A clone of the transaction-bound connection; Manual-mode statements must
    /// run on THIS connection so they land inside the open transaction.
    pub fn connection(&self) -> Connection {
        self.conn.clone()
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// Test-only seam: forces `is_open()` to `true` on a session built from a
    /// connection that was never actually contacted (no test double exists for
    /// `Connection`, so its `BEGIN` cannot be exercised without a live DB).
    #[cfg(test)]
    pub(crate) fn mark_open_for_test(&mut self) {
        self.open.store(true, Ordering::SeqCst);
    }

    /// Only flips `open` to `true` once `BEGIN` has actually succeeded; a
    /// failed `BEGIN` leaves the flag `false` so later statements aren't
    /// mistakenly routed onto a connection with no real open transaction.
    pub fn begin(&mut self, cx: &App) -> Task<Result<()>> {
        let open = self.open.clone();
        let task = self.conn.execute("BEGIN".to_string(), 1, cx);
        cx.background_spawn(async move {
            task.await.map(|_result| {
                open.store(true, Ordering::SeqCst);
            })
        })
    }

    /// Only flips `open` to `false` once `COMMIT` has actually succeeded; a
    /// failed `COMMIT` may still leave the transaction open, so the flag is
    /// left unchanged on error.
    pub fn commit(&mut self, cx: &App) -> Task<Result<()>> {
        let open = self.open.clone();
        let task = self.conn.execute("COMMIT".to_string(), 1, cx);
        cx.background_spawn(async move {
            task.await.map(|_result| {
                open.store(false, Ordering::SeqCst);
            })
        })
    }

    /// Only flips `open` to `false` once `ROLLBACK` has actually succeeded; a
    /// failed `ROLLBACK` may still leave the transaction open, so the flag is
    /// left unchanged on error.
    pub fn rollback(&mut self, cx: &App) -> Task<Result<()>> {
        let open = self.open.clone();
        let task = self.conn.execute("ROLLBACK".to_string(), 1, cx);
        cx.background_spawn(async move {
            task.await.map(|_result| {
                open.store(false, Ordering::SeqCst);
            })
        })
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    Auto,
    Manual,
}

#[allow(dead_code)]
impl CommitMode {
    pub fn as_stored_str(self) -> &'static str {
        match self {
            CommitMode::Auto => "auto",
            CommitMode::Manual => "manual",
        }
    }

    /// Unknown/legacy/NULL values load as the safe default (`Auto`).
    pub fn from_stored_str(value: &str) -> CommitMode {
        match value {
            "manual" => CommitMode::Manual,
            _ => CommitMode::Auto,
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn commit_mode_round_trips_stored_string() {
        assert_eq!(CommitMode::Auto.as_stored_str(), "auto");
        assert_eq!(CommitMode::Manual.as_stored_str(), "manual");
        assert_eq!(CommitMode::from_stored_str("auto"), CommitMode::Auto);
        assert_eq!(CommitMode::from_stored_str("manual"), CommitMode::Manual);
    }

    #[test]
    fn commit_mode_unknown_string_falls_back_to_auto() {
        // NULL/legacy/garbage persisted values load as the safe default.
        assert_eq!(CommitMode::from_stored_str(""), CommitMode::Auto);
        assert_eq!(CommitMode::from_stored_str("MANUAL"), CommitMode::Auto);
        assert_eq!(CommitMode::from_stored_str("committed"), CommitMode::Auto);
    }

    // Not `#[ignore]`: without `DATABASE_CLIENT_TEST_PG_URL` set this early-returns
    // and passes trivially, so `cargo test -p database_client --lib` needs no DB.
    // `TransactionSession::new` requires a real `Connection` (no test double exists
    // for it — see `Connection::connect`), so exercising `is_open()`'s transitions
    // still needs one live connection when a DB happens to be available.
    #[gpui::test]
    async fn transaction_session_is_open_transitions_with_begin_commit_rollback(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::connection::client::Connection;
        use crate::connection::profile::ConnectionProfile;
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = ConnectionProfile::from_url(&url).unwrap();
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();
        let mut session = TransactionSession::new(conn);

        assert!(!session.is_open());
        cx.update(|cx| session.begin(cx)).await.unwrap();
        assert!(session.is_open());
        cx.update(|cx| session.rollback(cx)).await.unwrap();
        assert!(!session.is_open());
        cx.update(|cx| session.begin(cx)).await.unwrap();
        assert!(session.is_open());
        cx.update(|cx| session.commit(cx)).await.unwrap();
        assert!(!session.is_open());
    }

    #[gpui::test]
    #[ignore]
    async fn live_rollback_discards_insert_and_commit_persists(cx: &mut gpui::TestAppContext) {
        use crate::connection::client::Connection;
        use crate::connection::profile::ConnectionProfile;
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = ConnectionProfile::from_url(&url).unwrap();
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();

        // Fresh scratch table on the shared connection.
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_tx_probe (id int)".to_string(),
                1,
                cx,
            )
        })
        .await
        .unwrap();

        let mut session = TransactionSession::new(conn.clone());
        assert!(!session.is_open());

        // begin → insert → rollback ⇒ row absent.
        cx.update(|cx| session.begin(cx)).await.unwrap();
        assert!(session.is_open());
        cx.update(|cx| {
            session.connection().execute(
                "insert into zedium_tx_probe values (1)".to_string(),
                1,
                cx,
            )
        })
        .await
        .unwrap();
        cx.update(|cx| session.rollback(cx)).await.unwrap();
        assert!(!session.is_open());
        let after_rollback = cx
            .update(|cx| conn.execute("select count(*) from zedium_tx_probe".to_string(), 1, cx))
            .await
            .unwrap();
        assert_eq!(after_rollback.rows, vec![vec![Some("0".to_string())]]);

        // begin → insert → commit ⇒ row present.
        cx.update(|cx| session.begin(cx)).await.unwrap();
        cx.update(|cx| {
            session.connection().execute(
                "insert into zedium_tx_probe values (2)".to_string(),
                1,
                cx,
            )
        })
        .await
        .unwrap();
        cx.update(|cx| session.commit(cx)).await.unwrap();
        assert!(!session.is_open());
        let after_commit = cx
            .update(|cx| conn.execute("select count(*) from zedium_tx_probe".to_string(), 1, cx))
            .await
            .unwrap();
        assert_eq!(after_commit.rows, vec![vec![Some("1".to_string())]]);
    }
}
