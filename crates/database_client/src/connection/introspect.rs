use crate::connection::client::{Connection, QueryResult};
use anyhow::Result;
use gpui::{App, AppContext, Task};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaInfo {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Table,
    View,
    MaterializedView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationInfo {
    pub name: String,
    pub kind: RelationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub is_unique: bool,
    pub is_primary: bool,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub definition: String,
    pub referenced_table: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    pub name: String,
    pub kind: String,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionInfo {
    pub name: String,
    pub signature: String,
    pub returns: String,
    pub language: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceInfo {
    pub name: String,
    pub last_value: Option<String>,
    pub increment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerInfo {
    pub name: String,
    pub definition: String,
}

fn col(r: &QueryResult, row: &[Option<String>], name: &str) -> String {
    r.columns
        .iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i).cloned().flatten())
        .unwrap_or_default()
}

fn col_opt(r: &QueryResult, row: &[Option<String>], name: &str) -> Option<String> {
    r.columns
        .iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i).cloned().flatten())
}

pub(crate) fn parse_schemas(r: &QueryResult) -> Vec<SchemaInfo> {
    r.rows
        .iter()
        .map(|row| SchemaInfo {
            name: col(r, row, "schema_name"),
        })
        .collect()
}

pub(crate) fn parse_databases(r: &QueryResult) -> Vec<DatabaseInfo> {
    r.rows
        .iter()
        .map(|row| DatabaseInfo {
            name: col(r, row, "datname"),
        })
        .collect()
}

pub(crate) fn parse_relations(r: &QueryResult) -> Vec<RelationInfo> {
    r.rows
        .iter()
        .map(|row| {
            let kind = if col(r, row, "table_type") == "VIEW" {
                RelationKind::View
            } else {
                RelationKind::Table
            };
            RelationInfo {
                name: col(r, row, "table_name"),
                kind,
            }
        })
        .collect()
}

pub(crate) fn parse_materialized_views(r: &QueryResult) -> Vec<RelationInfo> {
    r.rows
        .iter()
        .map(|row| RelationInfo {
            name: col(r, row, "table_name"),
            kind: RelationKind::MaterializedView,
        })
        .collect()
}

pub(crate) fn parse_columns(r: &QueryResult) -> Vec<ColumnInfo> {
    r.rows
        .iter()
        .map(|row| ColumnInfo {
            name: col(r, row, "column_name"),
            data_type: col(r, row, "data_type"),
            is_primary_key: col(r, row, "is_primary_key") == "YES",
        })
        .collect()
}

pub(crate) fn parse_indexes(r: &QueryResult) -> Vec<IndexInfo> {
    r.rows
        .iter()
        .map(|row| IndexInfo {
            name: col(r, row, "index_name"),
            is_unique: col(r, row, "is_unique") == "t",
            is_primary: col(r, row, "is_primary") == "t",
            definition: col(r, row, "definition"),
        })
        .collect()
}

pub(crate) fn parse_foreign_keys(r: &QueryResult) -> Vec<ForeignKeyInfo> {
    r.rows
        .iter()
        .map(|row| ForeignKeyInfo {
            name: col(r, row, "fk_name"),
            definition: col(r, row, "definition"),
            referenced_table: col(r, row, "referenced_table"),
        })
        .collect()
}

pub(crate) fn parse_constraints(r: &QueryResult) -> Vec<ConstraintInfo> {
    r.rows
        .iter()
        .map(|row| ConstraintInfo {
            name: col(r, row, "constraint_name"),
            kind: col(r, row, "kind"),
            definition: col(r, row, "definition"),
        })
        .collect()
}

pub(crate) fn parse_functions(r: &QueryResult) -> Vec<FunctionInfo> {
    r.rows
        .iter()
        .map(|row| FunctionInfo {
            name: col(r, row, "function_name"),
            signature: col(r, row, "signature"),
            returns: col(r, row, "returns"),
            language: col(r, row, "language"),
        })
        .collect()
}

pub(crate) fn parse_sequences(r: &QueryResult) -> Vec<SequenceInfo> {
    r.rows
        .iter()
        .map(|row| SequenceInfo {
            name: col(r, row, "sequence_name"),
            last_value: col_opt(r, row, "last_value"),
            increment: col(r, row, "increment"),
        })
        .collect()
}

pub(crate) fn parse_triggers(r: &QueryResult) -> Vec<TriggerInfo> {
    r.rows
        .iter()
        .map(|row| TriggerInfo {
            name: col(r, row, "trigger_name"),
            definition: col(r, row, "definition"),
        })
        .collect()
}

pub(crate) fn parse_row_count(r: &QueryResult) -> i64 {
    r.rows
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(-1)
}

pub(crate) fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub(crate) use crate::sql_paging::quote_ident;

impl Connection {
    pub fn list_databases(&self, cx: &App) -> Task<Result<Vec<DatabaseInfo>>> {
        let sql = "select datname from pg_database \
                   where datistemplate = false and datallowconn \
                   order by datname"
            .to_string();
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_databases(&task.await?)) })
    }

    pub fn list_schemas(&self, cx: &App) -> Task<Result<Vec<SchemaInfo>>> {
        let sql = "select schema_name from information_schema.schemata \
                   where schema_name not like 'pg\\_%' and schema_name <> 'information_schema' \
                   order by schema_name"
            .to_string();
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_schemas(&task.await?)) })
    }

    pub fn list_relations(&self, schema: String, cx: &App) -> Task<Result<Vec<RelationInfo>>> {
        let sql = format!(
            "select table_name, table_type from information_schema.tables \
             where table_schema = {} order by table_name",
            quote_literal(&schema)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_relations(&task.await?)) })
    }

    pub fn list_materialized_views(
        &self,
        schema: String,
        cx: &App,
    ) -> Task<Result<Vec<RelationInfo>>> {
        let sql = format!(
            "select matviewname as table_name from pg_matviews \
             where schemaname = {} order by matviewname",
            quote_literal(&schema)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_materialized_views(&task.await?)) })
    }

    pub fn list_columns(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<ColumnInfo>>> {
        let sql = format!(
            "select c.column_name, c.data_type, \
                    case when kcu.column_name is not null then 'YES' else 'NO' end \
                        as is_primary_key \
             from information_schema.columns c \
             left join information_schema.table_constraints tc \
               on tc.table_schema = c.table_schema \
              and tc.table_name = c.table_name \
              and tc.constraint_type = 'PRIMARY KEY' \
             left join information_schema.key_column_usage kcu \
               on kcu.constraint_schema = tc.constraint_schema \
              and kcu.constraint_name = tc.constraint_name \
              and kcu.table_schema = c.table_schema \
              and kcu.table_name = c.table_name \
              and kcu.column_name = c.column_name \
             where c.table_schema = {} and c.table_name = {} \
             order by c.ordinal_position",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_columns(&task.await?)) })
    }

    pub fn list_indexes(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<IndexInfo>>> {
        let sql = format!(
            "select i.relname as index_name, \
                    ix.indisunique as is_unique, \
                    ix.indisprimary as is_primary, \
                    pg_get_indexdef(ix.indexrelid) as definition \
             from pg_index ix \
             join pg_class i on i.oid = ix.indexrelid \
             join pg_class t on t.oid = ix.indrelid \
             join pg_namespace n on n.oid = t.relnamespace \
             where n.nspname = {} and t.relname = {} \
             order by i.relname",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_indexes(&task.await?)) })
    }

    pub fn list_foreign_keys(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<ForeignKeyInfo>>> {
        let sql = format!(
            "select con.conname as fk_name, \
                    pg_get_constraintdef(con.oid) as definition, \
                    ref.relname as referenced_table \
             from pg_constraint con \
             join pg_class rel on rel.oid = con.conrelid \
             join pg_namespace n on n.oid = rel.relnamespace \
             join pg_class ref on ref.oid = con.confrelid \
             where con.contype = 'f' and n.nspname = {} and rel.relname = {} \
             order by con.conname",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_foreign_keys(&task.await?)) })
    }

    pub fn list_constraints(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<ConstraintInfo>>> {
        let sql = format!(
            "select con.conname as constraint_name, \
                    case con.contype \
                        when 'c' then 'CHECK' \
                        when 'u' then 'UNIQUE' \
                        when 'p' then 'PRIMARY KEY' \
                    end as kind, \
                    pg_get_constraintdef(con.oid) as definition \
             from pg_constraint con \
             join pg_class rel on rel.oid = con.conrelid \
             join pg_namespace n on n.oid = rel.relnamespace \
             where con.contype in ('c', 'u', 'p') and n.nspname = {} and rel.relname = {} \
             order by con.conname",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_constraints(&task.await?)) })
    }

    pub fn list_functions(&self, schema: String, cx: &App) -> Task<Result<Vec<FunctionInfo>>> {
        let sql = format!(
            "select p.proname as function_name, \
                    pg_get_function_arguments(p.oid) as signature, \
                    pg_get_function_result(p.oid) as returns, \
                    l.lanname as language \
             from pg_proc p \
             join pg_namespace n on n.oid = p.pronamespace \
             join pg_language l on l.oid = p.prolang \
             where n.nspname = {} \
             order by p.proname",
            quote_literal(&schema)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_functions(&task.await?)) })
    }

    pub fn list_sequences(&self, schema: String, cx: &App) -> Task<Result<Vec<SequenceInfo>>> {
        let sql = format!(
            "select sequencename as sequence_name, \
                    last_value::text as last_value, \
                    increment_by::text as increment \
             from pg_sequences \
             where schemaname = {} \
             order by sequencename",
            quote_literal(&schema)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_sequences(&task.await?)) })
    }

    pub fn list_triggers(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<TriggerInfo>>> {
        let sql = format!(
            "select t.tgname as trigger_name, \
                    pg_get_triggerdef(t.oid) as definition \
             from pg_trigger t \
             join pg_class c on c.oid = t.tgrelid \
             join pg_namespace n on n.oid = c.relnamespace \
             where not t.tgisinternal and n.nspname = {} and c.relname = {} \
             order by t.tgname",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_triggers(&task.await?)) })
    }

    pub fn approx_row_count(&self, schema: String, table: String, cx: &App) -> Task<Result<i64>> {
        let sql = format!(
            "select c.reltuples::bigint as row_count \
             from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = {} and c.relname = {}",
            quote_literal(&schema),
            quote_literal(&table)
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_row_count(&task.await?)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::client::QueryResult;

    fn qr(columns: &[&str], rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|c| c.map(String::from)).collect())
                .collect(),
            command_tag: None,
            truncated: false,
        }
    }

    #[test]
    fn parses_schemas() {
        let r = qr(
            &["schema_name"],
            vec![vec![Some("public")], vec![Some("app")]],
        );
        let s = parse_schemas(&r);
        assert_eq!(
            s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["public", "app"]
        );
    }

    #[test]
    fn parses_databases() {
        let r = qr(
            &["datname"],
            vec![vec![Some("app")], vec![Some("postgres")]],
        );
        let d = parse_databases(&r);
        assert_eq!(
            d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["app", "postgres"]
        );
    }

    #[test]
    fn parses_relations_kind() {
        let r = qr(
            &["table_name", "table_type"],
            vec![
                vec![Some("users"), Some("BASE TABLE")],
                vec![Some("v_users"), Some("VIEW")],
            ],
        );
        let rels = parse_relations(&r);
        assert_eq!(rels[0].kind, RelationKind::Table);
        assert_eq!(rels[1].kind, RelationKind::View);
    }

    #[test]
    fn parses_columns() {
        let r = qr(
            &["column_name", "data_type", "is_primary_key"],
            vec![
                vec![Some("id"), Some("integer"), Some("YES")],
                vec![Some("email"), Some("text"), Some("NO")],
            ],
        );
        let cols = parse_columns(&r);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].data_type, "integer");
        assert!(cols[0].is_primary_key);
        assert_eq!(cols[1].name, "email");
        assert!(!cols[1].is_primary_key);
    }

    #[test]
    fn parses_columns_missing_pk_column_defaults_to_false() {
        let r = qr(
            &["column_name", "data_type"],
            vec![vec![Some("id"), Some("integer")]],
        );
        let cols = parse_columns(&r);
        assert!(!cols[0].is_primary_key);

        let r = qr(
            &["column_name", "data_type", "is_primary_key"],
            vec![
                vec![Some("id"), Some("integer")],
                vec![Some("name"), Some("text")],
            ],
        );
        // Short row: `col` returns "" for the missing cell.
        assert!(!parse_columns(&r)[1].is_primary_key);
    }

    #[test]
    fn parses_materialized_views() {
        let r = qr(
            &["table_name"],
            vec![vec![Some("daily_totals")], vec![Some("weekly_totals")]],
        );
        let rels = parse_materialized_views(&r);
        assert_eq!(
            rels.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["daily_totals", "weekly_totals"]
        );
        assert!(
            rels.iter()
                .all(|r| r.kind == RelationKind::MaterializedView)
        );
    }

    #[test]
    fn quote_literal_escapes_apostrophes() {
        assert_eq!(quote_literal("o'brien"), "'o''brien'");
    }

    #[test]
    fn quote_ident_double_quotes_and_escapes() {
        assert_eq!(quote_ident("order"), "\"order\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    // Live introspection tests against a real Postgres. Run with:
    // DATABASE_CLIENT_TEST_PG_URL=postgres://postgres:secret@localhost:55432/testdb?sslmode=disable \
    //   cargo test -p database_client --lib -- --ignored live_
    #[cfg(test)]
    async fn live_connection(cx: &mut gpui::TestAppContext) -> Option<Connection> {
        use std::str::FromStr as _;
        let url = std::env::var("DATABASE_CLIENT_TEST_PG_URL").ok()?;
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = crate::connection::profile::ConnectionProfile::from_url(&url).unwrap();
        let password = tokio_postgres::Config::from_str(&url).ok().and_then(|cfg| {
            cfg.get_password()
                .and_then(|p| String::from_utf8(p.to_vec()).ok())
        });
        Some(
            cx.update(|cx| Connection::connect(profile, password, cx))
                .await
                .unwrap(),
        )
    }

    #[gpui::test]
    #[ignore]
    async fn live_list_schemas(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let schemas = cx.update(|cx| conn.list_schemas(cx)).await.unwrap();
        let names: Vec<_> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"public"),
            "expected public schema, got {names:?}"
        );
        assert!(names.contains(&"app"), "expected app schema, got {names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("pg_")),
            "internal pg_* schemas should be filtered out, got {names:?}"
        );
    }

    #[gpui::test]
    #[ignore]
    async fn live_list_databases(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let dbs = cx.update(|cx| conn.list_databases(cx)).await.unwrap();
        let names: Vec<_> = dbs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"testdb"), "expected testdb, got {names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("template")),
            "template databases should be filtered out, got {names:?}"
        );
    }

    #[gpui::test]
    #[ignore]
    async fn live_list_relations(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let relations = cx
            .update(|cx| conn.list_relations("public".to_string(), cx))
            .await
            .unwrap();
        let by_name: std::collections::HashMap<_, _> = relations
            .iter()
            .map(|r| (r.name.as_str(), r.kind))
            .collect();
        assert_eq!(by_name.get("users"), Some(&RelationKind::Table));
        assert_eq!(by_name.get("orders"), Some(&RelationKind::Table));
        assert_eq!(by_name.get("user_emails"), Some(&RelationKind::View));
    }

    #[gpui::test]
    #[ignore]
    async fn live_list_columns(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let columns = cx
            .update(|cx| conn.list_columns("public".to_string(), "users".to_string(), cx))
            .await
            .unwrap();
        let names: Vec<_> = columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "email"]);
    }

    #[gpui::test]
    #[ignore]
    async fn live_list_columns_reports_primary_key(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let columns = cx
            .update(|cx| conn.list_columns("public".to_string(), "users".to_string(), cx))
            .await
            .unwrap();
        let pk_flags: Vec<_> = columns
            .iter()
            .map(|c| (c.name.as_str(), c.is_primary_key))
            .collect();
        assert_eq!(
            pk_flags,
            vec![("id", true), ("name", false), ("email", false)],
            "users.id is the seeded PRIMARY KEY"
        );
    }

    #[test]
    fn parses_indexes_with_flags() {
        let r = qr(
            &["index_name", "is_unique", "is_primary", "definition"],
            vec![
                vec![
                    Some("users_pkey"),
                    Some("t"),
                    Some("t"),
                    Some("CREATE UNIQUE INDEX users_pkey ON public.users USING btree (id)"),
                ],
                vec![
                    Some("users_email_idx"),
                    Some("f"),
                    Some("f"),
                    Some("CREATE INDEX users_email_idx ON public.users USING btree (email)"),
                ],
            ],
        );
        let indexes = parse_indexes(&r);
        assert_eq!(indexes[0].name, "users_pkey");
        assert!(indexes[0].is_unique);
        assert!(indexes[0].is_primary);
        assert!(indexes[0].definition.contains("USING btree (id)"));
        assert!(!indexes[1].is_unique);
        assert!(!indexes[1].is_primary);
    }

    #[test]
    fn parses_foreign_keys() {
        let r = qr(
            &["fk_name", "definition", "referenced_table"],
            vec![vec![
                Some("orders_user_id_fkey"),
                Some("FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE"),
                Some("users"),
            ]],
        );
        let fks = parse_foreign_keys(&r);
        assert_eq!(fks[0].name, "orders_user_id_fkey");
        assert_eq!(fks[0].referenced_table, "users");
        assert!(fks[0].definition.contains("REFERENCES users(id)"));
    }

    #[test]
    fn parses_constraints_with_kind() {
        let r = qr(
            &["constraint_name", "kind", "definition"],
            vec![
                vec![
                    Some("users_pkey"),
                    Some("PRIMARY KEY"),
                    Some("PRIMARY KEY (id)"),
                ],
                vec![
                    Some("users_email_key"),
                    Some("UNIQUE"),
                    Some("UNIQUE (email)"),
                ],
                vec![
                    Some("users_age_check"),
                    Some("CHECK"),
                    Some("CHECK ((age >= 0))"),
                ],
            ],
        );
        let cons = parse_constraints(&r);
        assert_eq!(cons[0].kind, "PRIMARY KEY");
        assert_eq!(cons[1].kind, "UNIQUE");
        assert_eq!(cons[2].kind, "CHECK");
        assert_eq!(cons[2].definition, "CHECK ((age >= 0))");
    }

    #[test]
    fn parses_functions() {
        let r = qr(
            &["function_name", "signature", "returns", "language"],
            vec![vec![
                Some("add_numbers"),
                Some("a integer, b integer"),
                Some("integer"),
                Some("sql"),
            ]],
        );
        let funcs = parse_functions(&r);
        assert_eq!(funcs[0].name, "add_numbers");
        assert_eq!(funcs[0].signature, "a integer, b integer");
        assert_eq!(funcs[0].returns, "integer");
        assert_eq!(funcs[0].language, "sql");
    }

    #[test]
    fn parses_sequences_with_nullable_last_value() {
        let r = qr(
            &["sequence_name", "last_value", "increment"],
            vec![
                vec![Some("users_id_seq"), Some("42"), Some("1")],
                vec![Some("fresh_seq"), None, Some("1")],
            ],
        );
        let seqs = parse_sequences(&r);
        assert_eq!(seqs[0].name, "users_id_seq");
        assert_eq!(seqs[0].last_value.as_deref(), Some("42"));
        assert_eq!(seqs[0].increment, "1");
        assert_eq!(
            seqs[1].last_value, None,
            "never-called sequence has NULL last_value"
        );
    }

    #[test]
    fn parses_triggers() {
        let r = qr(
            &["trigger_name", "definition"],
            vec![vec![
                Some("users_audit"),
                Some(
                    "CREATE TRIGGER users_audit AFTER UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION log_change()",
                ),
            ]],
        );
        let triggers = parse_triggers(&r);
        assert_eq!(triggers[0].name, "users_audit");
        assert!(triggers[0].definition.contains("AFTER UPDATE"));
    }

    #[test]
    fn parses_row_count_and_defaults_negative_when_unknown() {
        let r = qr(&["row_count"], vec![vec![Some("1234")]]);
        assert_eq!(parse_row_count(&r), 1234);

        // Never-analyzed table: reltuples is -1 in PG14+.
        let never = qr(&["row_count"], vec![vec![Some("-1")]]);
        assert_eq!(parse_row_count(&never), -1);

        // No row / unparsable falls back to -1 (treated as "unknown").
        let empty = qr(&["row_count"], vec![]);
        assert_eq!(parse_row_count(&empty), -1);
    }
}
