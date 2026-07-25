use crate::connection::client::{Connection, QueryResult};
use crate::connection::introspect::RelationKind;
use anyhow::Result;
use gpui::{App, AppContext, Task};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub data_type: String,
    // Populated by the sweep so Plan 5's `detect_editability` can find PK columns
    // without a second round-trip. False when the relation has no PK.
    pub is_primary_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationMeta {
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<ColumnMeta>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataCache {
    pub relations: Vec<RelationMeta>,
}

/// What the caret position calls for. Derived from a lexer-lite scan of the
/// text before the caret — no full SQL parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// After FROM / JOIN / UPDATE / INTO: schemas + tables/views.
    AfterFrom,
    /// After `qualifier.`: that relation's columns. `qualifier` is a table
    /// name or an alias to be resolved against the statement's FROM clause.
    AfterDot { qualifier: String },
    /// Anything else: columns + tables + keywords.
    General,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits trailing whitespace-delimited words; used to look at the last one or
/// two tokens before the caret.
#[allow(dead_code)] // Used by classify_context, wired up in Task 4.
fn preceding_words(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == '(')
        .filter(|w| !w.is_empty())
        .collect()
}

/// Classifies the caret context from the SQL text before the caret.
/// Wired up in Task 4 of the autocomplete plan.
#[allow(dead_code)]
pub fn classify_context(text_before_caret: &str) -> CompletionContext {
    // `qualifier.` (optionally with a partial column after the dot).
    let trimmed_tail = text_before_caret
        .rsplit(|c: char| c.is_whitespace() || c == ',' || c == '(')
        .next()
        .unwrap_or("");
    if let Some(dot) = trimmed_tail.rfind('.') {
        let qualifier = &trimmed_tail[..dot];
        // Take the last dotted segment (`public.users.` -> `users`).
        let qualifier = qualifier.rsplit('.').next().unwrap_or("");
        if !qualifier.is_empty() && qualifier.chars().all(is_ident_char) {
            return CompletionContext::AfterDot {
                qualifier: qualifier.to_string(),
            };
        }
    }

    // Whether the *most recent* relation-introducing keyword still governs the
    // caret (no relation token typed after it yet).
    let words = preceding_words(text_before_caret);
    let ends_mid_word = text_before_caret.chars().last().is_some_and(is_ident_char);

    // Index (from the end) of the current partial word, if any.
    let relation_keywords = ["from", "join", "update", "into"];
    let general_keywords = [
        "where", "select", "set", "on", "and", "or", "group", "order", "having",
    ];

    // Walk backwards over the completed words (excluding a partial trailing one).
    let completed = if ends_mid_word {
        &words[..words.len().saturating_sub(1)]
    } else {
        &words[..]
    };
    #[allow(clippy::never_loop)] // Loop body returns early; logic is correct.
    for word in completed.iter().rev() {
        let lower = word.to_ascii_lowercase();
        if relation_keywords.contains(&lower.as_str()) {
            return CompletionContext::AfterFrom;
        }
        if general_keywords.contains(&lower.as_str()) {
            return CompletionContext::General;
        }
        // A non-keyword token after the keyword means a relation/expression was
        // already named; keep scanning for an earlier governing keyword only if
        // this token is itself not a relation name following FROM. To stay
        // simple and predictable, a completed relation token ends the AfterFrom
        // scope: return General.
        return CompletionContext::General;
    }
    CompletionContext::General
}

/// Resolves a `qualifier` (alias or table name) to a bare relation name by
/// scanning `full_sql` for `... <relation> <qualifier>` or a bare `<qualifier>`
/// table reference. Returns the (possibly schema-stripped) relation name.
/// Wired up in Task 4 of the autocomplete plan.
#[allow(dead_code)]
pub(crate) fn resolve_alias(qualifier: &str, full_sql: &str) -> Option<String> {
    let tokens: Vec<&str> = full_sql
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .filter(|t| !t.is_empty())
        .collect();
    for window in tokens.windows(2) {
        let relation = window[0];
        let alias = window[1];
        if alias.eq_ignore_ascii_case(qualifier) && !is_sql_keyword(relation) {
            return Some(bare_relation(relation));
        }
    }
    // Bare table reference: the qualifier *is* a relation name in the statement.
    for token in &tokens {
        if bare_relation(token).eq_ignore_ascii_case(qualifier) {
            return Some(bare_relation(token));
        }
    }
    None
}

#[allow(dead_code)] // Used by resolve_alias, wired up in Task 4.
fn bare_relation(token: &str) -> String {
    token.rsplit('.').next().unwrap_or(token).to_string()
}

#[allow(dead_code)] // Used by resolve_alias, wired up in Task 4.
fn is_sql_keyword(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "select"
            | "from"
            | "join"
            | "where"
            | "on"
            | "and"
            | "or"
            | "as"
            | "update"
            | "insert"
            | "into"
            | "set"
            | "group"
            | "order"
            | "by"
            | "having"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "full"
            | "cross"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Wired up in Task 4 of the autocomplete plan.
pub enum CandidateKind {
    Schema,
    Table,
    View,
    Column,
    Keyword,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Wired up in Task 4 of the autocomplete plan.
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    pub detail: Option<String>,
}

#[allow(dead_code)] // Wired up in Task 4 of the autocomplete plan.
pub(crate) const SQL_KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "join",
    "left join",
    "right join",
    "inner join",
    "on",
    "group by",
    "order by",
    "having",
    "limit",
    "offset",
    "insert into",
    "values",
    "update",
    "set",
    "delete from",
    "and",
    "or",
    "not",
    "null",
    "is null",
    "is not null",
    "as",
    "distinct",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "case",
    "when",
    "then",
    "else",
    "end",
    "asc",
    "desc",
    "returning",
];

#[allow(dead_code)] // Wired up in Task 4 of the autocomplete plan.
fn matches_prefix(text: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || text
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
}

/// Context-sensitive suggestion list. Ranking order (best first): exact-prefix
/// matches, then in-scope columns, then tables/views, then schemas, then keywords.
/// `full_sql` is the whole statement text (for alias resolution); `prefix` is the
/// partial word under the caret.
///
/// Wired up in Task 4 of the autocomplete plan.
#[allow(dead_code)]
pub fn candidates(
    meta: &MetadataCache,
    context: &CompletionContext,
    prefix: &str,
    full_sql: &str,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    match context {
        CompletionContext::AfterFrom => {
            let mut schemas: Vec<&str> = meta.relations.iter().map(|r| r.schema.as_str()).collect();
            schemas.sort_unstable();
            schemas.dedup();
            for schema in schemas {
                if matches_prefix(schema, prefix) {
                    out.push(Candidate {
                        text: schema.to_string(),
                        kind: CandidateKind::Schema,
                        detail: None,
                    });
                }
            }
            for relation in &meta.relations {
                if matches_prefix(&relation.name, prefix) {
                    out.push(Candidate {
                        text: relation.name.clone(),
                        kind: match relation.kind {
                            RelationKind::View => CandidateKind::View,
                            _ => CandidateKind::Table,
                        },
                        detail: Some(relation.schema.clone()),
                    });
                }
            }
        }
        CompletionContext::AfterDot { qualifier } => {
            let resolved = resolve_alias(qualifier, full_sql);
            let target = resolved.as_deref().unwrap_or(qualifier);
            for relation in &meta.relations {
                if relation.name.eq_ignore_ascii_case(target) {
                    for column in &relation.columns {
                        if matches_prefix(&column.name, prefix) {
                            out.push(Candidate {
                                text: column.name.clone(),
                                kind: CandidateKind::Column,
                                detail: Some(column.data_type.clone()),
                            });
                        }
                    }
                }
            }
        }
        CompletionContext::General => {
            for relation in &meta.relations {
                for column in &relation.columns {
                    if matches_prefix(&column.name, prefix) {
                        out.push(Candidate {
                            text: column.name.clone(),
                            kind: CandidateKind::Column,
                            detail: Some(column.data_type.clone()),
                        });
                    }
                }
            }
            for relation in &meta.relations {
                if matches_prefix(&relation.name, prefix) {
                    out.push(Candidate {
                        text: relation.name.clone(),
                        kind: match relation.kind {
                            RelationKind::View => CandidateKind::View,
                            _ => CandidateKind::Table,
                        },
                        detail: Some(relation.schema.clone()),
                    });
                }
            }
            for keyword in SQL_KEYWORDS {
                if matches_prefix(keyword, prefix) {
                    out.push(Candidate {
                        text: keyword.to_string(),
                        kind: CandidateKind::Keyword,
                        detail: None,
                    });
                }
            }
        }
    }
    // Promote exact-prefix matches to the front while preserving kind-based ordering.
    // When prefix is empty, nothing is "exact" so ordering is unchanged.
    if !prefix.is_empty() {
        out.sort_by_key(|c| !c.text.eq_ignore_ascii_case(prefix));
    }
    out
}

fn cell(r: &QueryResult, row: &[Option<String>], name: &str) -> String {
    r.columns
        .iter()
        .position(|c| c == name)
        .and_then(|i| row.get(i).cloned().flatten())
        .unwrap_or_default()
}

/// Groups the flat schema/table/column sweep into per-relation metadata.
/// Rows are ordered by (schema, table, ordinal_position), so consecutive
/// rows of the same relation extend the last entry.
pub(crate) fn parse_metadata(r: &QueryResult) -> MetadataCache {
    let mut relations: Vec<RelationMeta> = Vec::new();
    for row in &r.rows {
        let schema = cell(r, row, "table_schema");
        let name = cell(r, row, "table_name");
        let kind = if cell(r, row, "table_type") == "VIEW" {
            RelationKind::View
        } else {
            RelationKind::Table
        };
        let column = ColumnMeta {
            name: cell(r, row, "column_name"),
            data_type: cell(r, row, "data_type"),
            is_primary_key: cell(r, row, "is_primary_key") == "YES",
        };
        match relations.last_mut() {
            Some(last) if last.schema == schema && last.name == name => {
                last.columns.push(column);
            }
            _ => relations.push(RelationMeta {
                schema,
                name,
                kind,
                columns: vec![column],
            }),
        }
    }
    MetadataCache { relations }
}

impl Connection {
    /// One cheap sweep of user-schema tables/views and their columns, feeding
    /// autocomplete. Independent of the tree's lazy loading.
    #[allow(dead_code)] // Wired up by Plan 3's caller in a later task.
    pub fn load_metadata(&self, cx: &App) -> Task<Result<MetadataCache>> {
        let sql = "select c.table_schema, c.table_name, t.table_type, \
                          c.column_name, c.data_type, \
                          case when pk.column_name is not null then 'YES' else 'NO' end \
                              as is_primary_key \
                   from information_schema.columns c \
                   join information_schema.tables t \
                     on t.table_schema = c.table_schema \
                    and t.table_name = c.table_name \
                   left join ( \
                       select kcu.table_schema, kcu.table_name, kcu.column_name \
                       from information_schema.table_constraints tc \
                       join information_schema.key_column_usage kcu \
                         on kcu.constraint_schema = tc.constraint_schema \
                        and kcu.constraint_name = tc.constraint_name \
                       where tc.constraint_type = 'PRIMARY KEY' \
                   ) pk \
                     on pk.table_schema = c.table_schema \
                    and pk.table_name = c.table_name \
                    and pk.column_name = c.column_name \
                   where c.table_schema not like 'pg\\_%' \
                     and c.table_schema <> 'information_schema' \
                   order by c.table_schema, c.table_name, c.ordinal_position"
            .to_string();
        let task = self.execute(sql, 100_000, cx);
        cx.background_spawn(async move { Ok(parse_metadata(&task.await?)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::client::QueryResult;
    use crate::connection::introspect::RelationKind;

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
    fn after_from_keyword_expects_relations() {
        assert_eq!(
            classify_context("select * from "),
            CompletionContext::AfterFrom
        );
        assert_eq!(
            classify_context("select * from us"),
            CompletionContext::AfterFrom
        );
        assert_eq!(
            classify_context("SELECT 1 JOIN "),
            CompletionContext::AfterFrom
        );
        assert_eq!(classify_context("update "), CompletionContext::AfterFrom);
        assert_eq!(
            classify_context("insert into "),
            CompletionContext::AfterFrom
        );
    }

    #[test]
    fn dot_after_identifier_is_a_qualifier() {
        assert_eq!(
            classify_context("select u."),
            CompletionContext::AfterDot {
                qualifier: "u".to_string()
            }
        );
        assert_eq!(
            classify_context("select u.na"),
            CompletionContext::AfterDot {
                qualifier: "u".to_string()
            }
        );
        assert_eq!(
            classify_context("select public.users."),
            CompletionContext::AfterDot {
                qualifier: "users".to_string()
            }
        );
    }

    #[test]
    fn bare_identifier_is_general() {
        assert_eq!(classify_context(""), CompletionContext::General);
        assert_eq!(classify_context("select "), CompletionContext::General);
        assert_eq!(
            classify_context("select id, na"),
            CompletionContext::General
        );
        assert_eq!(
            classify_context("select * from users where "),
            CompletionContext::General
        );
    }

    #[test]
    fn from_then_another_keyword_returns_to_general() {
        // A relation is already named after FROM; WHERE moves back to general.
        assert_eq!(
            classify_context("select * from users where id = 1 and "),
            CompletionContext::General
        );
    }

    #[test]
    fn resolves_alias_to_its_relation() {
        // `u` aliases public.users; a `u.` qualifier resolves to `users`.
        let sql = "select u. from public.users u";
        assert_eq!(resolve_alias("u", sql), Some("users".to_string()));
        // Bare table name maps to itself.
        assert_eq!(
            resolve_alias("users", "select users. from users"),
            Some("users".to_string())
        );
        assert_eq!(resolve_alias("missing", "select x. from users u"), None);
    }

    #[test]
    fn groups_rows_into_relations_preserving_column_order() {
        let r = qr(
            &[
                "table_schema",
                "table_name",
                "table_type",
                "column_name",
                "data_type",
                "is_primary_key",
            ],
            vec![
                vec![
                    Some("public"),
                    Some("users"),
                    Some("BASE TABLE"),
                    Some("id"),
                    Some("integer"),
                    Some("YES"),
                ],
                vec![
                    Some("public"),
                    Some("users"),
                    Some("BASE TABLE"),
                    Some("email"),
                    Some("text"),
                    Some("NO"),
                ],
                vec![
                    Some("public"),
                    Some("v_users"),
                    Some("VIEW"),
                    Some("id"),
                    Some("integer"),
                    Some("NO"),
                ],
            ],
        );
        let cache = parse_metadata(&r);
        assert_eq!(cache.relations.len(), 2);
        assert_eq!(cache.relations[0].schema, "public");
        assert_eq!(cache.relations[0].name, "users");
        assert_eq!(cache.relations[0].kind, RelationKind::Table);
        let names: Vec<_> = cache.relations[0]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["id", "email"]);
        assert_eq!(cache.relations[0].columns[1].data_type, "text");
        assert!(
            cache.relations[0].columns[0].is_primary_key,
            "users.id is PK"
        );
        assert!(
            !cache.relations[0].columns[1].is_primary_key,
            "users.email is not PK"
        );
        assert_eq!(cache.relations[1].name, "v_users");
        assert_eq!(cache.relations[1].kind, RelationKind::View);
    }

    #[test]
    fn same_table_name_in_two_schemas_stays_distinct() {
        let r = qr(
            &[
                "table_schema",
                "table_name",
                "table_type",
                "column_name",
                "data_type",
            ],
            vec![
                vec![
                    Some("public"),
                    Some("t"),
                    Some("BASE TABLE"),
                    Some("a"),
                    Some("integer"),
                ],
                vec![
                    Some("app"),
                    Some("t"),
                    Some("BASE TABLE"),
                    Some("b"),
                    Some("integer"),
                ],
            ],
        );
        let cache = parse_metadata(&r);
        assert_eq!(cache.relations.len(), 2);
        assert_eq!(cache.relations[0].schema, "public");
        assert_eq!(cache.relations[1].schema, "app");
    }

    // Live introspection test against a real Postgres. Run with:
    // DATABASE_CLIENT_TEST_PG_URL=postgres://postgres:secret@localhost:55432/testdb?sslmode=disable \
    //   cargo test -p database_client --lib -- --ignored live_
    #[cfg(test)]
    async fn live_connection(cx: &mut gpui::TestAppContext) -> Option<Connection> {
        use std::str::FromStr as _;
        let url = std::env::var("DATABASE_CLIENT_TEST_PG_URL").ok()?;
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = crate::connection::profile::ConnectionProfile::from_url(&url)?;
        let password = tokio_postgres::Config::from_str(&url).ok().and_then(|cfg| {
            cfg.get_password()
                .and_then(|p| String::from_utf8(p.to_vec()).ok())
        });
        cx.update(|cx| Connection::connect(profile, password, cx))
            .await
            .ok()
    }

    #[gpui::test]
    #[ignore]
    async fn live_load_metadata_reports_users_columns_and_pk(cx: &mut gpui::TestAppContext) {
        let Some(conn) = live_connection(cx).await else {
            return;
        };
        let cache = cx.update(|cx| conn.load_metadata(cx)).await.unwrap();
        let users = cache
            .relations
            .iter()
            .find(|r| r.schema == "public" && r.name == "users")
            .expect("expected public.users relation in metadata cache");
        let names: Vec<_> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "email"]);
        assert!(
            users.columns[0].is_primary_key,
            "users.id is the seeded PRIMARY KEY"
        );
        assert!(!users.columns[1].is_primary_key);
        assert!(!users.columns[2].is_primary_key);
    }

    fn sample_cache() -> MetadataCache {
        MetadataCache {
            relations: vec![
                RelationMeta {
                    schema: "public".into(),
                    name: "users".into(),
                    kind: RelationKind::Table,
                    columns: vec![
                        ColumnMeta {
                            name: "id".into(),
                            data_type: "integer".into(),
                            is_primary_key: false,
                        },
                        ColumnMeta {
                            name: "email".into(),
                            data_type: "text".into(),
                            is_primary_key: false,
                        },
                    ],
                },
                RelationMeta {
                    schema: "public".into(),
                    name: "v_users".into(),
                    kind: RelationKind::View,
                    columns: vec![ColumnMeta {
                        name: "id".into(),
                        data_type: "integer".into(),
                        is_primary_key: false,
                    }],
                },
            ],
        }
    }

    #[test]
    fn after_from_offers_tables_and_views() {
        let cache = sample_cache();
        let out = candidates(
            &cache,
            &CompletionContext::AfterFrom,
            "us",
            "select * from us",
        );
        let texts: Vec<_> = out.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"users"));
        // Prefix filter drops the view.
        assert!(!texts.contains(&"v_users"));
        assert!(out.iter().all(|c| matches!(
            c.kind,
            CandidateKind::Table | CandidateKind::View | CandidateKind::Schema
        )));
    }

    #[test]
    fn after_dot_offers_resolved_relation_columns_with_type_detail() {
        let cache = sample_cache();
        let ctx = CompletionContext::AfterDot {
            qualifier: "u".into(),
        };
        let out = candidates(&cache, &ctx, "", "select u. from public.users u");
        let by_name: std::collections::HashMap<_, _> =
            out.iter().map(|c| (c.text.as_str(), c)).collect();
        assert!(by_name.contains_key("id"));
        assert_eq!(by_name["id"].detail.as_deref(), Some("integer"));
        assert!(matches!(by_name["email"].kind, CandidateKind::Column));
    }

    #[test]
    fn general_offers_columns_tables_and_keywords() {
        let cache = sample_cache();
        let out = candidates(&cache, &CompletionContext::General, "", "select ");
        let kinds: std::collections::HashSet<_> = out
            .iter()
            .map(|c| std::mem::discriminant(&c.kind))
            .collect();
        assert!(kinds.contains(&std::mem::discriminant(&CandidateKind::Column)));
        assert!(kinds.contains(&std::mem::discriminant(&CandidateKind::Table)));
        assert!(kinds.contains(&std::mem::discriminant(&CandidateKind::Keyword)));
    }

    #[test]
    fn empty_cache_still_offers_keywords() {
        let cache = MetadataCache::default();
        let out = candidates(&cache, &CompletionContext::General, "sel", "sel");
        assert!(out.iter().any(|c| c.text.eq_ignore_ascii_case("select")));
        assert!(out.iter().all(|c| matches!(c.kind, CandidateKind::Keyword)));
    }

    #[test]
    fn exact_prefix_matches_rank_first() {
        // Cache with two columns: "id" (exact) and "identifier" (prefix-only)
        // with prefix "id", the exact match should come first.
        let cache = MetadataCache {
            relations: vec![RelationMeta {
                schema: "public".into(),
                name: "users".into(),
                kind: RelationKind::Table,
                columns: vec![
                    ColumnMeta {
                        name: "id".into(),
                        data_type: "integer".into(),
                        is_primary_key: false,
                    },
                    ColumnMeta {
                        name: "identifier".into(),
                        data_type: "text".into(),
                        is_primary_key: false,
                    },
                ],
            }],
        };
        let out = candidates(&cache, &CompletionContext::General, "id", "select ");
        // Both columns should match the prefix "id", but "id" (exact) should come first.
        assert_eq!(out[0].text, "id");
        assert_eq!(out[1].text, "identifier");
    }

    #[test]
    fn exact_match_in_lower_priority_kind_ranks_ahead_of_non_exact_higher_priority() {
        // A cache where a table with exact name "users" and a column "userscount" (prefix-only)
        // exist. With prefix "users", the exact-matching table should rank ahead even though
        // columns normally rank before tables by kind order.
        let cache = MetadataCache {
            relations: vec![RelationMeta {
                schema: "public".into(),
                name: "users".into(),
                kind: RelationKind::Table,
                columns: vec![ColumnMeta {
                    name: "userscount".into(),
                    data_type: "integer".into(),
                    is_primary_key: false,
                }],
            }],
        };
        let out = candidates(&cache, &CompletionContext::General, "users", "select ");
        // "users" table is exact match, "userscount" column is prefix-only (starts with "users" but not equal).
        // Even though columns rank before tables by kind, exact-prefix sort should come first overall.
        let positions: Vec<_> = out.iter().map(|c| c.text.as_str()).collect();
        let users_pos = positions.iter().position(|&t| t == "users");
        let userscount_pos = positions.iter().position(|&t| t == "userscount");
        // Both candidates must be present and the exact-match table must come before the non-exact column.
        assert!(
            users_pos.is_some(),
            "exact-match 'users' table should be present"
        );
        assert!(
            userscount_pos.is_some(),
            "'userscount' column should be present (it matches prefix 'users')"
        );
        assert!(
            users_pos.unwrap() < userscount_pos.unwrap(),
            "exact-match 'users' table should rank ahead of non-exact 'userscount' column"
        );
    }

    #[test]
    fn empty_prefix_does_not_reorder() {
        // With an empty prefix, all candidates match and should keep their original order
        // (columns first, then tables, then keywords in General context).
        let cache = MetadataCache {
            relations: vec![RelationMeta {
                schema: "public".into(),
                name: "users".into(),
                kind: RelationKind::Table,
                columns: vec![ColumnMeta {
                    name: "id".into(),
                    data_type: "integer".into(),
                    is_primary_key: false,
                }],
            }],
        };
        let out = candidates(&cache, &CompletionContext::General, "", "select ");
        let texts: Vec<_> = out.iter().map(|c| c.text.as_str()).collect();
        // Columns should come before tables.
        let id_pos = texts.iter().position(|&t| t == "id").unwrap();
        let users_pos = texts.iter().position(|&t| t == "users").unwrap();
        assert!(
            id_pos < users_pos,
            "columns should come before tables when no prefix"
        );
    }
}
