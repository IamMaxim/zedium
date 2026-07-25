//! Pure serializers (CSV / JSON / SQL INSERT) for query results, plus a
//! paging `Connection::export_relation` that streams a whole relation.

use crate::connection::client::Connection;
use crate::connection::introspect::quote_literal;
use crate::settings::DatabaseClientSettings;
use crate::sql_paging::{quote_ident, wrap};
use anyhow::Result;
use gpui::{App, Task};
use settings::Settings as _;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Sql,
}

/// Serializes an in-memory result to the requested format. Pure and total:
/// short rows are padded to the column count, extra cells are ignored.
/// `table` names the target relation for `Sql` INSERTs (defaults to
/// `exported_table`); `null_string` is the CSV NULL token (unused by JSON/SQL,
/// which use `null` / the `NULL` keyword).
#[allow(dead_code)]
pub fn serialize_result(
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Option<String>>],
    table: Option<&str>,
    null_string: &str,
) -> String {
    match format {
        ExportFormat::Csv => serialize_csv(columns, rows, null_string),
        // Filled in by Tasks 2 and 3.
        ExportFormat::Json => serialize_json(columns, rows),
        ExportFormat::Sql => serialize_sql(columns, rows, table),
    }
}

/// One field, RFC-4180 quoted: fields containing `"`, `,`, CR, or LF are
/// wrapped in double quotes with embedded quotes doubled. `None` renders as
/// `null_string` (which is itself quoted if it contains a special character).
fn csv_field(cell: Option<&str>, null_string: &str) -> String {
    let raw = cell.unwrap_or(null_string);
    if raw.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_string()
    }
}

fn serialize_csv(columns: &[String], rows: &[Vec<Option<String>>], null_string: &str) -> String {
    let mut out = String::new();
    let header = columns
        .iter()
        .map(|name| csv_field(Some(name), null_string))
        .collect::<Vec<_>>()
        .join(",");
    out.push_str(&header);
    out.push_str("\r\n");
    for row in rows {
        let line = (0..columns.len())
            .map(|index| csv_field(row.get(index).and_then(|cell| cell.as_deref()), null_string))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push_str("\r\n");
    }
    out
}

fn serialize_json(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let mut array = Vec::with_capacity(rows.len());
    for row in rows {
        let mut object = serde_json::Map::new();
        for (index, name) in columns.iter().enumerate() {
            let value = match row.get(index).and_then(|cell| cell.as_ref()) {
                Some(text) => serde_json::Value::String(text.clone()),
                None => serde_json::Value::Null,
            };
            object.insert(name.clone(), value);
        }
        array.push(serde_json::Value::Object(object));
    }
    // `Value`'s Display is infallible — no Result to unwrap.
    serde_json::Value::Array(array).to_string()
}

fn serialize_sql(columns: &[String], rows: &[Vec<Option<String>>], table: Option<&str>) -> String {
    let table_name = table.unwrap_or("exported_table");
    let quoted_columns = columns
        .iter()
        .map(|name| quote_ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    let quoted_table = quote_ident(table_name);
    let mut out = String::new();
    for row in rows {
        let values = (0..columns.len())
            .map(
                |index| match row.get(index).and_then(|cell| cell.as_ref()) {
                    // Bare unknown-type literal: coerces to the target column type
                    // on INSERT. A `::text` cast (as `null_or_literal` would add)
                    // would break re-import into integer/array/json columns.
                    Some(text) => quote_literal(text),
                    None => "NULL".to_string(),
                },
            )
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "INSERT INTO {quoted_table} ({quoted_columns}) VALUES ({values});\n"
        ));
    }
    out
}

/// Rows fetched per internal page while streaming a whole relation.
const EXPORT_PAGE_SIZE: usize = 1000;

impl Connection {
    /// Streams every row of `schema.table` by paging through
    /// `sql_paging::wrap` (page size [`EXPORT_PAGE_SIZE`]), then serializes to
    /// `format`. Not capped at `DEFAULT_RESULT_LIMIT`, and never issues a
    /// single unbounded query.
    pub fn export_relation(
        &self,
        schema: String,
        table: String,
        format: ExportFormat,
        cx: &App,
    ) -> Task<Result<String>> {
        let connection = self.clone();
        let null_string = DatabaseClientSettings::get_global(cx)
            .export_null_string
            .clone();
        cx.spawn(async move |cx| {
            let base = format!(
                "select * from {}.{}",
                quote_ident(&schema),
                quote_ident(&table)
            );
            let mut offset = 0usize;
            let mut columns: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<Option<String>>> = Vec::new();
            loop {
                let wrapped = wrap(&base, None, EXPORT_PAGE_SIZE, offset);
                let connection = connection.clone();
                let page = cx
                    .update(|cx| connection.execute(wrapped, EXPORT_PAGE_SIZE, cx))
                    .await?;
                if columns.is_empty() {
                    columns = page.columns.clone();
                }
                let fetched = page.rows.len();
                rows.extend(page.rows);
                if fetched < EXPORT_PAGE_SIZE {
                    break;
                }
                offset += EXPORT_PAGE_SIZE;
            }
            Ok(serialize_result(
                format,
                &columns,
                &rows,
                Some(&table),
                &null_string,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[Option<&str>]) -> Vec<Option<String>> {
        cells.iter().map(|c| c.map(str::to_string)).collect()
    }

    #[test]
    fn csv_quotes_commas_quotes_and_newlines_and_uses_null_string() {
        let columns = vec!["id".to_string(), "note".to_string()];
        let rows = vec![
            row(&[Some("1"), Some("plain")]),
            row(&[Some("2"), Some("a,b")]),
            row(&[Some("3"), Some("say \"hi\"")]),
            row(&[Some("4"), Some("line1\nline2")]),
            row(&[Some("5"), None]),
        ];
        let csv = serialize_result(ExportFormat::Csv, &columns, &rows, None, "\\N");
        assert_eq!(
            csv,
            "id,note\r\n\
             1,plain\r\n\
             2,\"a,b\"\r\n\
             3,\"say \"\"hi\"\"\"\r\n\
             4,\"line1\nline2\"\r\n\
             5,\\N\r\n"
        );
    }

    #[test]
    fn csv_empty_null_string_renders_as_empty_field() {
        let columns = vec!["a".to_string()];
        let rows = vec![row(&[None])];
        assert_eq!(
            serialize_result(ExportFormat::Csv, &columns, &rows, None, ""),
            "a\r\n\r\n"
        );
    }

    #[test]
    fn csv_pads_ragged_rows_to_column_count() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![row(&[Some("1")])]; // short row
        assert_eq!(
            serialize_result(ExportFormat::Csv, &columns, &rows, None, ""),
            "a,b\r\n1,\r\n"
        );
    }

    #[test]
    fn json_is_array_of_objects_with_null_and_string_values() {
        let columns = vec!["id".to_string(), "name".to_string(), "note".to_string()];
        let rows = vec![
            row(&[Some("1"), None, Some("say \"hi\"")]),
            row(&[Some("2"), Some("bob"), Some("x")]),
        ];
        let json = serialize_result(ExportFormat::Json, &columns, &rows, None, "");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("export JSON must parse");
        assert_eq!(parsed[0]["id"], serde_json::json!("1"));
        assert_eq!(parsed[0]["name"], serde_json::Value::Null);
        assert_eq!(parsed[0]["note"], serde_json::json!("say \"hi\""));
        assert_eq!(parsed[1]["name"], serde_json::json!("bob"));
        // Numbers stay strings (the text protocol gives us no type info).
        assert!(parsed[1]["id"].is_string());
    }

    #[test]
    fn json_empty_rows_is_empty_array() {
        let columns = vec!["a".to_string()];
        assert_eq!(
            serialize_result(ExportFormat::Json, &columns, &[], None, ""),
            "[]"
        );
    }

    #[test]
    fn sql_emits_quoted_insert_per_row_with_null_keyword() {
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![row(&[Some("1"), Some("O'Brien")]), row(&[Some("2"), None])];
        let sql = serialize_result(ExportFormat::Sql, &columns, &rows, Some("users"), "");
        assert_eq!(
            sql,
            "INSERT INTO \"users\" (\"id\", \"name\") VALUES ('1', 'O''Brien');\n\
             INSERT INTO \"users\" (\"id\", \"name\") VALUES ('2', NULL);\n"
        );
    }

    #[test]
    fn sql_quotes_identifiers_and_defaults_table_name() {
        let columns = vec!["we\"ird".to_string()];
        let rows = vec![row(&[Some("x")])];
        let sql = serialize_result(ExportFormat::Sql, &columns, &rows, None, "");
        assert_eq!(
            sql,
            "INSERT INTO \"exported_table\" (\"we\"\"ird\") VALUES ('x');\n"
        );
    }

    #[test]
    fn sql_pads_short_rows_with_null() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![row(&[Some("1")])];
        assert_eq!(
            serialize_result(ExportFormat::Sql, &columns, &rows, Some("t"), ""),
            "INSERT INTO \"t\" (\"a\", \"b\") VALUES ('1', NULL);\n"
        );
    }
}

// Live export tests against a real Postgres. Run with:
// DATABASE_CLIENT_TEST_PG_URL=postgres://zedium:zedium@localhost:5544/zedium \
//   cargo test -p database_client --lib export::live_tests -- --ignored
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::connection::profile::ConnectionProfile;

    fn password_from_url(url: &str) -> Option<String> {
        use std::str::FromStr as _;
        tokio_postgres::Config::from_str(url)
            .ok()
            .and_then(|config| {
                config
                    .get_password()
                    .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
            })
    }

    // Seeds a >1-page table, exports it as SQL, re-runs the generated INSERTs
    // into a temp copy, and asserts the round-tripped row count matches.
    #[gpui::test]
    #[ignore]
    async fn export_relation_streams_all_rows_and_reimports(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = ConnectionProfile::from_url(&url).unwrap();
        let password = password_from_url(&url);
        let connection = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap();

        cx.update(|cx| {
            connection.execute(
                "drop table if exists zedium_export_src; \
                 create table zedium_export_src (id int primary key, note text); \
                 insert into zedium_export_src \
                   select generate_series(1, 2500), 'n,\"' || generate_series(1, 2500);"
                    .to_string(),
                10_000,
                cx,
            )
        })
        .await
        .unwrap();

        let sql = cx
            .update(|cx| {
                connection.export_relation(
                    "public".to_string(),
                    "zedium_export_src".to_string(),
                    ExportFormat::Sql,
                    cx,
                )
            })
            .await
            .unwrap();
        // All rows streamed, not capped at DEFAULT_RESULT_LIMIT (500).
        assert_eq!(sql.matches("INSERT INTO").count(), 2500);

        // Re-import into a temp copy; count must match.
        let reimport = format!(
            "drop table if exists zedium_export_dst; \
             create table zedium_export_dst (id int primary key, note text); \
             {}",
            sql.replace(
                "INSERT INTO \"zedium_export_src\"",
                "INSERT INTO zedium_export_dst"
            )
        );
        cx.update(|cx| connection.execute(reimport, 10_000, cx))
            .await
            .unwrap();
        let count = cx
            .update(|cx| {
                connection.execute(
                    "select count(*) as c from zedium_export_dst".to_string(),
                    10,
                    cx,
                )
            })
            .await
            .unwrap();
        assert_eq!(count.rows[0][0].as_deref(), Some("2500"));

        cx.update(|cx| {
            connection.execute(
                "drop table zedium_export_src; drop table zedium_export_dst;".to_string(),
                10,
                cx,
            )
        })
        .await
        .unwrap();
    }
}
