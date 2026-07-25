//! Approach-A DML generation for the editable results grid. Every value is
//! emitted as a text literal cast back to its column type
//! (`'text'::pgtype`) — the text from `simple_query` is a valid input literal
//! for that type, so this round-trips correctly for all types (arrays, json,
//! bytea, enums, …). `None` is a real SQL `NULL`; `Some("")` is the empty
//! string. WHERE clauses are built from primary-key columns only.

use std::collections::HashMap;

use crate::connection::introspect::{quote_ident, quote_literal};

/// A staged edit of one existing row: its primary-key identity and the set of
/// changed columns. `set` values are `None` for a SQL `NULL`, `Some(text)`
/// (including `Some("")`) for a concrete value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RowEdit {
    pub pk: Vec<(String, String)>,
    pub set: Vec<(String, Option<String>)>,
}

/// A staged new row: one value per column, `None` ⇒ `NULL`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct InsertRow {
    pub values: Vec<(String, Option<String>)>,
}

/// `None ⇒ "NULL"`; `Some(text) ⇒ quote_literal(text)::pg_type`. The type is
/// interpolated verbatim (not identifier-quoted) so container/qualified types
/// like `integer[]` or `timestamp without time zone` cast correctly.
#[allow(dead_code)]
pub fn null_or_literal(text: Option<&str>, pg_type: &str) -> String {
    match text {
        None => "NULL".to_string(),
        Some(value) => format!("{}::{}", quote_literal(value), pg_type),
    }
}

#[allow(dead_code)]
fn column_type<'a>(col_types: &'a HashMap<String, String>, col: &str) -> &'a str {
    col_types.get(col).map(String::as_str).unwrap_or("text")
}

/// `col1 = literal1 AND col2 = literal2 …` from primary-key columns, whose
/// values are never `NULL`.
#[allow(dead_code)]
fn pk_predicate(pk: &[(String, String)], col_types: &HashMap<String, String>) -> String {
    pk.iter()
        .map(|(col, value)| {
            format!(
                "{} = {}",
                quote_ident(col),
                null_or_literal(Some(value.as_str()), column_type(col_types, col))
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Approach-A `UPDATE` for a staged [`RowEdit`]: sets each edited column and
/// restricts to the row identified by its primary-key columns.
#[allow(dead_code)]
pub fn generate_update(
    schema: &str,
    table: &str,
    edit: &RowEdit,
    col_types: &HashMap<String, String>,
) -> String {
    let assignments = edit
        .set
        .iter()
        .map(|(col, value)| {
            format!(
                "{} = {}",
                quote_ident(col),
                null_or_literal(value.as_deref(), column_type(col_types, col))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "UPDATE {}.{} SET {} WHERE {}",
        quote_ident(schema),
        quote_ident(table),
        assignments,
        pk_predicate(&edit.pk, col_types)
    )
}

/// Approach-A `INSERT` for a staged [`InsertRow`]: one column list and one
/// value list, in the same order as `row.values`.
#[allow(dead_code)]
pub fn generate_insert(
    schema: &str,
    table: &str,
    row: &InsertRow,
    col_types: &HashMap<String, String>,
) -> String {
    let columns = row
        .values
        .iter()
        .map(|(col, _)| quote_ident(col))
        .collect::<Vec<_>>()
        .join(", ");
    let values = row
        .values
        .iter()
        .map(|(col, value)| null_or_literal(value.as_deref(), column_type(col_types, col)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {}.{} ({}) VALUES ({})",
        quote_ident(schema),
        quote_ident(table),
        columns,
        values
    )
}

/// Approach-A `DELETE` for a row identified by its primary-key columns only.
#[allow(dead_code)]
pub fn generate_delete(
    schema: &str,
    table: &str,
    pk: &[(String, String)],
    col_types: &HashMap<String, String>,
) -> String {
    format!(
        "DELETE FROM {}.{} WHERE {}",
        quote_ident(schema),
        quote_ident(table),
        pk_predicate(pk, col_types)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_or_literal_maps_none_to_bare_null() {
        assert_eq!(null_or_literal(None, "integer"), "NULL");
        assert_eq!(null_or_literal(None, "text"), "NULL");
    }

    #[test]
    fn null_or_literal_casts_some_including_empty_string() {
        // empty string is a real value, NOT null.
        assert_eq!(null_or_literal(Some(""), "text"), "''::text");
        assert_eq!(null_or_literal(Some("42"), "integer"), "'42'::integer");
    }

    #[test]
    fn null_or_literal_escapes_apostrophes_and_keeps_type_verbatim() {
        assert_eq!(null_or_literal(Some("o'brien"), "text"), "'o''brien'::text");
        // Container/qualified types pass through unquoted so arrays/timestamps cast.
        assert_eq!(
            null_or_literal(Some("{1,2}"), "integer[]"),
            "'{1,2}'::integer[]"
        );
        assert_eq!(
            null_or_literal(Some("2020-01-01 00:00:00"), "timestamp without time zone"),
            "'2020-01-01 00:00:00'::timestamp without time zone"
        );
        assert_eq!(
            null_or_literal(Some("{\"a\":1}"), "jsonb"),
            "'{\"a\":1}'::jsonb"
        );
        assert_eq!(
            null_or_literal(Some("\\x00ff"), "bytea"),
            "'\\x00ff'::bytea"
        );
    }

    fn types() -> HashMap<String, String> {
        [
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
            ("note".to_string(), "text".to_string()),
            ("tenant".to_string(), "uuid".to_string()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn update_sets_edited_columns_with_casts_and_pk_where() {
        let edit = RowEdit {
            pk: vec![("id".into(), "7".into())],
            set: vec![("name".into(), Some("ann".into()))],
        };
        assert_eq!(
            generate_update("app", "users", &edit, &types()),
            "UPDATE \"app\".\"users\" SET \"name\" = 'ann'::text WHERE \"id\" = '7'::integer"
        );
    }

    #[test]
    fn update_emits_null_for_none_and_empty_string_for_some_empty() {
        let edit = RowEdit {
            pk: vec![("id".into(), "7".into())],
            set: vec![("name".into(), None), ("note".into(), Some("".into()))],
        };
        assert_eq!(
            generate_update("app", "users", &edit, &types()),
            "UPDATE \"app\".\"users\" SET \"name\" = NULL, \"note\" = ''::text WHERE \"id\" = '7'::integer"
        );
    }

    #[test]
    fn update_uses_all_pk_columns_and_quotes_identifiers() {
        let edit = RowEdit {
            pk: vec![("id".into(), "7".into()), ("tenant".into(), "t-1".into())],
            set: vec![("name".into(), Some("o'brien".into()))],
        };
        assert_eq!(
            generate_update("app", "users", &edit, &types()),
            "UPDATE \"app\".\"users\" SET \"name\" = 'o''brien'::text \
             WHERE \"id\" = '7'::integer AND \"tenant\" = 't-1'::uuid"
        );
    }

    #[test]
    fn insert_lists_columns_and_casts_values() {
        let row = InsertRow {
            values: vec![
                ("id".into(), Some("9".into())),
                ("name".into(), Some("kai".into())),
            ],
        };
        assert_eq!(
            generate_insert("app", "users", &row, &types()),
            "INSERT INTO \"app\".\"users\" (\"id\", \"name\") VALUES ('9'::integer, 'kai'::text)"
        );
    }

    #[test]
    fn insert_emits_null_for_none_and_empty_for_some_empty() {
        let row = InsertRow {
            values: vec![
                ("id".into(), Some("9".into())),
                ("name".into(), None),
                ("note".into(), Some("".into())),
            ],
        };
        assert_eq!(
            generate_insert("app", "users", &row, &types()),
            "INSERT INTO \"app\".\"users\" (\"id\", \"name\", \"note\") \
             VALUES ('9'::integer, NULL, ''::text)"
        );
    }

    #[test]
    fn delete_targets_row_by_full_pk() {
        assert_eq!(
            generate_delete("app", "users", &[("id".into(), "7".into())], &types()),
            "DELETE FROM \"app\".\"users\" WHERE \"id\" = '7'::integer"
        );
    }

    #[test]
    fn delete_uses_composite_pk_and_quotes() {
        let pk = vec![("id".into(), "7".into()), ("tenant".into(), "t'1".into())];
        assert_eq!(
            generate_delete("app", "users", &pk, &types()),
            "DELETE FROM \"app\".\"users\" \
             WHERE \"id\" = '7'::integer AND \"tenant\" = 't''1'::uuid"
        );
    }
}
