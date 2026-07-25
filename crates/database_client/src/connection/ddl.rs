use crate::connection::client::{Connection, QueryResult};
use crate::connection::introspect::{
    ColumnInfo, ConstraintInfo, IndexInfo, quote_ident, quote_literal,
};
use anyhow::{Context as _, Result};
use gpui::{App, AppContext as _, Task};
use std::collections::HashSet;

/// Reconstructs a `CREATE TABLE` statement (plus trailing `CREATE INDEX` and
/// `COMMENT ON` lines) from introspected metadata. Postgres has no
/// `pg_get_tabledef`, so this is assembled by hand and re-parses on the server
/// (see the live round-trip test).
///
/// Column NOT NULL / DEFAULT clauses are not represented in `ColumnInfo` and are
/// therefore omitted; PK/unique/check come from `constraints`. Indexes backing a
/// constraint (primary, or same-named as a constraint) are skipped to avoid
/// emitting a redundant `CREATE INDEX`.
pub fn reconstruct_create_table(
    schema: &str,
    table: &str,
    columns: &[ColumnInfo],
    constraints: &[ConstraintInfo],
    indexes: &[IndexInfo],
    comments: &[(String, String)],
) -> String {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(table));

    let mut lines: Vec<String> = Vec::new();
    for column in columns {
        lines.push(format!(
            "    {} {}",
            quote_ident(&column.name),
            column.data_type
        ));
    }
    for constraint in constraints {
        lines.push(format!(
            "    CONSTRAINT {} {}",
            quote_ident(&constraint.name),
            constraint.definition
        ));
    }

    let mut ddl = format!("CREATE TABLE {qualified} (\n{}\n);", lines.join(",\n"));

    let constraint_names: HashSet<&str> = constraints
        .iter()
        .map(|constraint| constraint.name.as_str())
        .collect();
    for index in indexes {
        if index.is_primary || constraint_names.contains(index.name.as_str()) {
            continue;
        }
        ddl.push('\n');
        let definition = index.definition.trim_end();
        ddl.push_str(definition);
        if !definition.ends_with(';') {
            ddl.push(';');
        }
    }

    for (column_name, body) in comments {
        ddl.push('\n');
        if column_name.is_empty() {
            ddl.push_str(&format!(
                "COMMENT ON TABLE {qualified} IS {};",
                quote_literal(body)
            ));
        } else {
            ddl.push_str(&format!(
                "COMMENT ON COLUMN {qualified}.{} IS {};",
                quote_ident(column_name),
                quote_literal(body)
            ));
        }
    }

    ddl
}

/// Which server object a DDL request targets. Tables have no `pg_get_*def`
/// function, so they are reconstructed (see `reconstruct_create_table`); the
/// rest come straight from Postgres.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlTarget {
    Table,
    View,
    MaterializedView,
    Function,
    Index,
    Sequence,
    Trigger,
}

/// Maps a relation's kind onto its DDL target.
pub fn ddl_target_for_relation(kind: crate::connection::introspect::RelationKind) -> DdlTarget {
    use crate::connection::introspect::RelationKind;
    match kind {
        RelationKind::Table => DdlTarget::Table,
        RelationKind::View => DdlTarget::View,
        RelationKind::MaterializedView => DdlTarget::MaterializedView,
    }
}

/// Maps an object tree node onto its DDL target, if any. Structural nodes
/// (schema, group folders, columns, relations) return `None`; relations use
/// `ddl_target_for_relation` instead (they carry a `RelationKind`).
pub fn ddl_target_for_node_kind(kind: crate::tree::NodeKind) -> Option<DdlTarget> {
    use crate::tree::NodeKind;
    match kind {
        NodeKind::Function => Some(DdlTarget::Function),
        NodeKind::Sequence => Some(DdlTarget::Sequence),
        NodeKind::Index => Some(DdlTarget::Index),
        NodeKind::Trigger => Some(DdlTarget::Trigger),
        _ => None,
    }
}

/// Builds a query returning a single column `ddl` holding the object's full
/// `CREATE …` text. Returns `None` for `Table` (no server function exists).
///
/// `format('… %I …', schema, name, …)` lets Postgres do identifier quoting for
/// the emitted DDL, while the `WHERE` clauses embed name literals via
/// `quote_literal`. The regclass cast uses a pre-quoted qualified name so a
/// mixed-case or reserved-word object still resolves.
fn server_ddl_sql(target: DdlTarget, schema: &str, name: &str) -> Option<String> {
    let schema_literal = quote_literal(schema);
    let name_literal = quote_literal(name);
    let qualified = quote_literal(&format!("{}.{}", quote_ident(schema), quote_ident(name)));
    let sql = match target {
        DdlTarget::Table => return None,
        DdlTarget::View => format!(
            "select format('CREATE OR REPLACE VIEW %I.%I AS\n%s', \
             {schema_literal}, {name_literal}, \
             pg_get_viewdef({qualified}::regclass, true)) as ddl"
        ),
        DdlTarget::MaterializedView => format!(
            "select format('CREATE MATERIALIZED VIEW %I.%I AS\n%s', \
             {schema_literal}, {name_literal}, \
             pg_get_viewdef({qualified}::regclass, true)) as ddl"
        ),
        DdlTarget::Function => format!(
            "select pg_get_functiondef(p.oid) as ddl from pg_proc p \
             join pg_namespace n on n.oid = p.pronamespace \
             where n.nspname = {schema_literal} and p.proname = {name_literal} \
             limit 1"
        ),
        DdlTarget::Index => format!(
            "select pg_get_indexdef(c.oid) as ddl from pg_class c \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = {schema_literal} and c.relname = {name_literal} \
             and c.relkind = 'i' limit 1"
        ),
        DdlTarget::Sequence => format!(
            "select format('CREATE SEQUENCE %I.%I\n  START WITH %s\n  INCREMENT BY %s\n  \
             MINVALUE %s\n  MAXVALUE %s\n  CACHE %s%s;', \
             schemaname, sequencename, start_value, increment_by, min_value, max_value, \
             cache_size, case when cycle then E'\n  CYCLE' else '' end) as ddl \
             from pg_sequences \
             where schemaname = {schema_literal} and sequencename = {name_literal}"
        ),
        DdlTarget::Trigger => format!(
            "select pg_get_triggerdef(t.oid) as ddl from pg_trigger t \
             join pg_class c on c.oid = t.tgrelid \
             join pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = {schema_literal} and t.tgname = {name_literal} \
             and not t.tgisinternal limit 1"
        ),
    };
    Some(sql)
}

/// Parses `list_comments` rows into `(column_name, body)` pairs, dropping rows
/// with a NULL comment. An empty `column_name` denotes the table comment.
pub(crate) fn parse_comments(result: &QueryResult) -> Vec<(String, String)> {
    let name_index = result.columns.iter().position(|c| c == "column_name");
    let comment_index = result.columns.iter().position(|c| c == "comment");
    let (Some(name_index), Some(comment_index)) = (name_index, comment_index) else {
        return Vec::new();
    };
    result
        .rows
        .iter()
        .filter_map(|row| {
            let body = row.get(comment_index).cloned().flatten()?;
            let column_name = row.get(name_index).cloned().flatten().unwrap_or_default();
            Some((column_name, body))
        })
        .collect()
}

impl Connection {
    /// Table and column comments for a relation. The first row (empty
    /// `column_name`) is the table comment; the rest are per-column.
    pub fn list_comments(
        &self,
        schema: String,
        table: String,
        cx: &App,
    ) -> Task<Result<Vec<(String, String)>>> {
        let qualified = quote_literal(&format!("{}.{}", quote_ident(&schema), quote_ident(&table)));
        let sql = format!(
            "select '' as column_name, \
                    obj_description({qualified}::regclass, 'pg_class') as comment \
             union all \
             select a.attname as column_name, \
                    col_description({qualified}::regclass, a.attnum) as comment \
             from pg_attribute a \
             where a.attrelid = {qualified}::regclass and a.attnum > 0 and not a.attisdropped \
             order by 1"
        );
        let task = self.execute(sql, 10_000, cx);
        cx.background_spawn(async move { Ok(parse_comments(&task.await?)) })
    }

    /// Gathers columns, constraints, indexes, and comments, then reconstructs
    /// the `CREATE TABLE` DDL.
    fn table_ddl(&self, schema: String, table: String, cx: &App) -> Task<Result<String>> {
        let connection = self.clone();
        cx.spawn(async move |cx| {
            let columns = cx
                .update(|cx| connection.list_columns(schema.clone(), table.clone(), cx))
                .await?;
            let constraints = cx
                .update(|cx| connection.list_constraints(schema.clone(), table.clone(), cx))
                .await?;
            let indexes = cx
                .update(|cx| connection.list_indexes(schema.clone(), table.clone(), cx))
                .await?;
            let comments = cx
                .update(|cx| connection.list_comments(schema.clone(), table.clone(), cx))
                .await?;
            Ok(reconstruct_create_table(
                &schema,
                &table,
                &columns,
                &constraints,
                &indexes,
                &comments,
            ))
        })
    }

    /// Returns the object's `CREATE …` DDL. Server objects run one query; tables
    /// are reconstructed by gathering columns, constraints, indexes, and
    /// comments (no `pg_get_tabledef` exists).
    pub fn object_ddl(
        &self,
        target: DdlTarget,
        schema: String,
        name: String,
        cx: &App,
    ) -> Task<Result<String>> {
        if target == DdlTarget::Table {
            return self.table_ddl(schema, name, cx);
        }
        let Some(sql) = server_ddl_sql(target, &schema, &name) else {
            return Task::ready(Err(anyhow::anyhow!("no server DDL for {target:?}")));
        };
        let task = self.execute(sql, 1, cx);
        cx.background_spawn(async move {
            let result = task.await?;
            result
                .rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().next().flatten())
                .context("DDL query returned no rows")
        })
    }
}

#[cfg(test)]
mod reconstruct_tests {
    use super::*;
    use crate::connection::introspect::{ColumnInfo, ConstraintInfo, IndexInfo};

    fn column(name: &str, data_type: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            is_primary_key: pk,
        }
    }
    fn constraint(name: &str, kind: &str, definition: &str) -> ConstraintInfo {
        ConstraintInfo {
            name: name.into(),
            kind: kind.into(),
            definition: definition.into(),
        }
    }
    fn index(name: &str, is_unique: bool, is_primary: bool, definition: &str) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            is_unique,
            is_primary,
            definition: definition.into(),
        }
    }

    #[test]
    fn columns_types_and_constraints_render() {
        let ddl = reconstruct_create_table(
            "public",
            "users",
            &[
                column("id", "integer", true),
                column("email", "text", false),
                column("age", "integer", false),
            ],
            &[
                constraint("users_pkey", "PRIMARY KEY", "PRIMARY KEY (id)"),
                constraint("users_email_key", "UNIQUE", "UNIQUE (email)"),
                constraint("users_age_check", "CHECK", "CHECK ((age > 0))"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            ddl,
            "CREATE TABLE \"public\".\"users\" (\n\
             \x20   \"id\" integer,\n\
             \x20   \"email\" text,\n\
             \x20   \"age\" integer,\n\
             \x20   CONSTRAINT \"users_pkey\" PRIMARY KEY (id),\n\
             \x20   CONSTRAINT \"users_email_key\" UNIQUE (email),\n\
             \x20   CONSTRAINT \"users_age_check\" CHECK ((age > 0))\n\
             );"
        );
    }

    #[test]
    fn trailing_indexes_skip_primary_and_constraint_backed() {
        let ddl = reconstruct_create_table(
            "public",
            "users",
            &[column("id", "integer", true)],
            &[constraint("users_email_key", "UNIQUE", "UNIQUE (email)")],
            &[
                // Primary index — covered by the PK constraint; skipped.
                index(
                    "users_pkey",
                    true,
                    true,
                    "CREATE UNIQUE INDEX users_pkey ON public.users USING btree (id)",
                ),
                // Backs the unique constraint (same name); skipped.
                index(
                    "users_email_key",
                    true,
                    false,
                    "CREATE UNIQUE INDEX users_email_key ON public.users USING btree (email)",
                ),
                // A plain secondary index — emitted.
                index(
                    "users_email_lower_idx",
                    false,
                    false,
                    "CREATE INDEX users_email_lower_idx ON public.users USING btree (lower(email))",
                ),
            ],
            &[],
        );
        assert!(ddl.contains(
            "\nCREATE INDEX users_email_lower_idx ON public.users USING btree (lower(email));"
        ));
        assert!(!ddl.contains("users_pkey ON"));
        assert!(!ddl.contains("users_email_key ON"));
    }

    #[test]
    fn table_and_column_comments_render() {
        let ddl = reconstruct_create_table(
            "public",
            "users",
            &[column("id", "integer", true)],
            &[],
            &[],
            &[
                (String::new(), "app users".into()),
                ("id".into(), "primary key it's".into()),
            ],
        );
        assert!(ddl.contains("\nCOMMENT ON TABLE \"public\".\"users\" IS 'app users';"));
        assert!(
            ddl.contains("\nCOMMENT ON COLUMN \"public\".\"users\".\"id\" IS 'primary key it''s';")
        );
    }

    #[test]
    fn index_definition_gets_a_semicolon_only_when_missing() {
        let already = reconstruct_create_table(
            "public",
            "t",
            &[column("id", "integer", false)],
            &[],
            &[index(
                "t_idx",
                false,
                false,
                "CREATE INDEX t_idx ON public.t (id);",
            )],
            &[],
        );
        assert!(already.contains("CREATE INDEX t_idx ON public.t (id);"));
        assert!(!already.contains("(id);;"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_ddl_sql_builds_view_and_index_and_sequence_queries() {
        let view = server_ddl_sql(DdlTarget::View, "public", "v_users").unwrap();
        assert!(view.contains("pg_get_viewdef('\"public\".\"v_users\"'::regclass, true)"));
        assert!(view.contains("CREATE OR REPLACE VIEW"));

        let matview = server_ddl_sql(DdlTarget::MaterializedView, "app", "m_stats").unwrap();
        assert!(matview.contains("CREATE MATERIALIZED VIEW"));
        assert!(matview.contains("pg_get_viewdef('\"app\".\"m_stats\"'::regclass, true)"));

        let index = server_ddl_sql(DdlTarget::Index, "public", "users_pkey").unwrap();
        assert!(index.contains("pg_get_indexdef(c.oid)"));
        assert!(index.contains("n.nspname = 'public'"));
        assert!(index.contains("c.relname = 'users_pkey'"));

        let function = server_ddl_sql(DdlTarget::Function, "public", "add").unwrap();
        assert!(function.contains("pg_get_functiondef(p.oid)"));
        assert!(function.contains("p.proname = 'add'"));

        let sequence = server_ddl_sql(DdlTarget::Sequence, "public", "users_id_seq").unwrap();
        assert!(sequence.contains("CREATE SEQUENCE"));
        assert!(sequence.contains("from pg_sequences"));
        assert!(sequence.contains("sequencename = 'users_id_seq'"));

        let trigger = server_ddl_sql(DdlTarget::Trigger, "public", "audit_trg").unwrap();
        assert!(trigger.contains("pg_get_triggerdef(t.oid)"));
        assert!(trigger.contains("not t.tgisinternal"));

        // Table has no server function; it routes to reconstruction instead.
        assert!(server_ddl_sql(DdlTarget::Table, "public", "users").is_none());
    }

    #[test]
    fn server_ddl_sql_escapes_apostrophes_in_names() {
        let sql = server_ddl_sql(DdlTarget::Index, "public", "o'brien_idx").unwrap();
        assert!(sql.contains("c.relname = 'o''brien_idx'"));
    }
}

#[cfg(test)]
mod ddl_target_tests {
    use super::*;
    use crate::connection::introspect::RelationKind;

    #[test]
    fn ddl_target_for_relation_maps_each_kind() {
        assert_eq!(
            ddl_target_for_relation(RelationKind::Table),
            DdlTarget::Table
        );
        assert_eq!(ddl_target_for_relation(RelationKind::View), DdlTarget::View);
        assert_eq!(
            ddl_target_for_relation(RelationKind::MaterializedView),
            DdlTarget::MaterializedView
        );
    }

    #[test]
    fn ddl_target_for_node_kind_covers_object_nodes() {
        use crate::tree::NodeKind;
        assert_eq!(
            ddl_target_for_node_kind(NodeKind::Function),
            Some(DdlTarget::Function)
        );
        assert_eq!(
            ddl_target_for_node_kind(NodeKind::Sequence),
            Some(DdlTarget::Sequence)
        );
        assert_eq!(
            ddl_target_for_node_kind(NodeKind::Index),
            Some(DdlTarget::Index)
        );
        assert_eq!(
            ddl_target_for_node_kind(NodeKind::Trigger),
            Some(DdlTarget::Trigger)
        );
        // Structural nodes have no direct DDL target.
        assert_eq!(ddl_target_for_node_kind(NodeKind::Schema), None);
        assert_eq!(ddl_target_for_node_kind(NodeKind::Column), None);
    }
}

#[cfg(test)]
mod comment_tests {
    use super::*;
    use crate::connection::client::QueryResult;

    fn qr(rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: vec!["column_name".into(), "comment".into()],
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|c| c.map(String::from)).collect())
                .collect(),
            command_tag: None,
            truncated: false,
        }
    }

    #[test]
    fn parse_comments_keeps_table_and_column_and_drops_nulls() {
        let result = qr(vec![
            vec![Some(""), Some("app users")],
            vec![Some("id"), Some("the pk")],
            vec![Some("email"), None],
        ]);
        let comments = parse_comments(&result);
        assert_eq!(
            comments,
            vec![
                (String::new(), "app users".to_string()),
                ("id".to_string(), "the pk".to_string()),
            ],
            "null-comment rows are dropped"
        );
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::connection::profile::ConnectionProfile;
    use std::str::FromStr as _;

    async fn live_connection(cx: &mut gpui::TestAppContext) -> Option<Connection> {
        let url = std::env::var("DATABASE_CLIENT_TEST_PG_URL").ok()?;
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = ConnectionProfile::from_url(&url).unwrap();
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
    async fn live_reconstructed_table_ddl_reparses(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let ddl = cx
            .update(|cx| conn.object_ddl(DdlTarget::Table, "public".into(), "users".into(), cx))
            .await
            .unwrap();
        assert!(ddl.starts_with("CREATE TABLE \"public\".\"users\""));

        // The generated DDL must re-parse: run it on a throwaway schema.
        cx.update(|cx| {
            conn.execute(
                "drop schema if exists zedium_ddl cascade; create schema zedium_ddl".into(),
                1,
                cx,
            )
        })
        .await
        .unwrap();
        let relocated = ddl.replace("\"public\".", "\"zedium_ddl\".");
        cx.update(|cx| conn.execute(relocated, 1, cx))
            .await
            .unwrap();
        cx.update(|cx| conn.execute("drop schema zedium_ddl cascade".into(), 1, cx))
            .await
            .unwrap();
    }

    #[gpui::test]
    #[ignore]
    async fn live_view_ddl_is_a_create_view(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let ddl = cx
            .update(|cx| {
                conn.object_ddl(DdlTarget::View, "public".into(), "user_emails".into(), cx)
            })
            .await
            .unwrap();
        assert!(ddl.contains("CREATE OR REPLACE VIEW"));
        assert!(ddl.contains("user_emails"));
    }
}
