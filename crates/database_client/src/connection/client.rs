use crate::connection::profile::{ConnectionProfile, SslMode};
use anyhow::{Context as _, Result};
use gpui::{App, Task};
use gpui_tokio::Tokio;
use rustls_platform_verifier::ConfigVerifierExt as _;
use std::sync::Arc;

pub const DEFAULT_RESULT_LIMIT: usize = 500;

#[derive(Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub command_tag: Option<String>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct Connection {
    client: Arc<tokio_postgres::Client>,
}

impl Connection {
    pub fn connect(
        profile: ConnectionProfile,
        password: Option<String>,
        cx: &App,
    ) -> Task<Result<Connection>> {
        Tokio::spawn_result(cx, async move {
            let config = profile.pg_config(password.as_deref());
            let client = match profile.ssl_mode {
                SslMode::Disable => {
                    let (client, connection) = config
                        .connect(tokio_postgres::NoTls)
                        .await
                        .context("connect (no tls)")?;
                    tokio::spawn(async move {
                        if let Err(error) = connection.await {
                            log::error!("database connection error: {error}");
                        }
                    });
                    client
                }
                SslMode::Prefer | SslMode::Require => {
                    let tls = make_rustls_connector();
                    let (client, connection) =
                        config.connect(tls).await.context("connect (tls)")?;
                    tokio::spawn(async move {
                        if let Err(error) = connection.await {
                            log::error!("database connection error: {error}");
                        }
                    });
                    client
                }
            };
            Ok(Connection {
                client: Arc::new(client),
            })
        })
    }

    pub fn execute(&self, sql: String, limit: usize, cx: &App) -> Task<Result<QueryResult>> {
        let client = self.client.clone();
        Tokio::spawn_result(cx, async move {
            let messages = client.simple_query(&sql).await.context("simple_query")?;
            Ok(collect_result(messages, limit))
        })
    }
}

fn make_rustls_connector() -> tokio_postgres_rustls::MakeRustlsConnect {
    let config = rustls::ClientConfig::with_platform_verifier();
    tokio_postgres_rustls::MakeRustlsConnect::new(config)
}

pub(crate) fn truncate_rows(
    mut rows: Vec<Vec<Option<String>>>,
    limit: usize,
) -> (Vec<Vec<Option<String>>>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

pub(crate) fn collect_result(
    messages: Vec<tokio_postgres::SimpleQueryMessage>,
    limit: usize,
) -> QueryResult {
    use tokio_postgres::SimpleQueryMessage::*;
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut command_tag = None;
    for msg in messages {
        match msg {
            RowDescription(cols) => {
                // Until a data row arrives the latest statement's description
                // wins, so an all-empty multi-statement batch shows the last
                // statement's headers.
                if rows.is_empty() {
                    columns = cols.iter().map(|c| c.name().to_string()).collect();
                }
            }
            Row(row) => {
                if columns.is_empty() {
                    columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                }
                let cells = (0..row.len())
                    .map(|i| row.get(i).map(|s| s.to_string()))
                    .collect();
                rows.push(cells);
            }
            CommandComplete(n) => command_tag = Some(format!("{n} rows")),
            _ => {}
        }
    }
    let (rows, truncated) = truncate_rows(rows, limit);
    QueryResult {
        columns,
        rows,
        command_tag,
        truncated,
    }
}

#[cfg(test)]
fn profile_password_from_url(url: &str) -> Option<String> {
    use std::str::FromStr as _;
    tokio_postgres::Config::from_str(url).ok().and_then(|cfg| {
        cfg.get_password()
            .and_then(|p| String::from_utf8(p.to_vec()).ok())
    })
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn truncate_marks_when_over_limit() {
        let rows = vec![vec![Some("a".into())]; 3];
        let (out, truncated) = truncate_rows(rows, 2);
        assert_eq!(out.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn truncate_keeps_all_when_under_limit() {
        let rows = vec![vec![Some("a".into())]; 2];
        let (out, truncated) = truncate_rows(rows, 5);
        assert_eq!(out.len(), 2);
        assert!(!truncated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Run with: DATABASE_CLIENT_TEST_PG_URL=postgres://user:pass@localhost/db cargo test -p database_client -- --ignored
    #[gpui::test]
    #[ignore]
    async fn connect_and_select(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = crate::connection::profile::ConnectionProfile::from_url(&url).unwrap();
        let password = profile_password_from_url(&url);
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();
        let result = cx
            .update(|cx| conn.execute("select 1 as one, null::text as n".into(), 500, cx))
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["one".to_string(), "n".to_string()]);
        assert_eq!(result.rows, vec![vec![Some("1".to_string()), None]]);
    }

    // Zero-row results must still carry column headers (RowDescription arrives
    // before any data row, even when there are no rows).
    #[gpui::test]
    #[ignore]
    async fn live_empty_result_preserves_column_headers(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = crate::connection::profile::ConnectionProfile::from_url(&url).unwrap();
        let password = profile_password_from_url(&url);
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();
        let result = cx
            .update(|cx| conn.execute("select 1 as one, 'x' as label where false".into(), 500, cx))
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["one".to_string(), "label".to_string()]);
        assert!(result.rows.is_empty());
    }

    // With multiple statements, headers must match the statement that produced
    // the rows, even when an earlier statement returns zero rows.
    #[gpui::test]
    #[ignore]
    async fn live_multi_statement_headers_match_data(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = crate::connection::profile::ConnectionProfile::from_url(&url).unwrap();
        let password = profile_password_from_url(&url);
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();
        let result = cx
            .update(|cx| conn.execute("select 1 as a where false; select 2 as b".into(), 500, cx))
            .await
            .unwrap();
        assert_eq!(result.columns, vec!["b".to_string()]);
        assert_eq!(result.rows, vec![vec![Some("2".to_string())]]);
    }
}
