//! Pure helpers for server-side pagination: decide whether a SQL text is a
//! single wrappable SELECT/WITH statement, and build the
//! `select * from (…) zedium_page … limit … offset …` wrapper around it.
//!
//! Known limitations (safe direction: misclassification falls back to the
//! client-side truncation path): `E'\''` backslash escape strings are not
//! special-cased; parenthesized selects and bare `VALUES` are treated as
//! non-wrappable; a `WITH` statement containing a bare `insert`, `update`,
//! `delete`, `merge`, or `into` keyword anywhere (e.g. `… for update`, or a
//! quoted-in-spirit identifier written unquoted) is treated as non-wrappable
//! even when the statement would actually be legal inside a subquery.

/// Sort direction for a paged query's `ORDER BY` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// Quotes a SQL identifier: wraps in double quotes, doubling embedded quotes.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[derive(Default)]
struct ScanInfo {
    /// Byte offset of the first significant (non-comment, non-whitespace,
    /// non-semicolon) character.
    first_code: Option<usize>,
    /// Byte offset of the first semicolon outside strings and comments.
    first_semicolon: Option<usize>,
    /// Byte offset of the last significant character that is not a semicolon.
    last_code_non_semicolon: Option<usize>,
    /// A bare `INTO` keyword appeared outside strings and comments.
    /// `SELECT … INTO table` is forbidden inside a subquery.
    has_into: bool,
    /// A bare data-modifying keyword (`INSERT`/`UPDATE`/`DELETE`/`MERGE`)
    /// appeared outside strings and comments. A `WITH` clause containing a
    /// data-modifying statement is forbidden inside a subquery.
    has_dml: bool,
}

fn mark(info: &mut ScanInfo, i: usize) {
    if info.first_code.is_none() {
        info.first_code = Some(i);
    }
    info.last_code_non_semicolon = Some(i);
}

/// If `bytes[start]` begins a dollar-quote delimiter (`$$` or `$tag$`),
/// returns the offset just past the delimiter's closing `$`.
fn dollar_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start + 1;
    // `$1`, `$2`, … are positional parameters, not quote tags.
    if bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    while j < bytes.len() {
        match bytes[j] {
            b'$' => return Some(j + 1),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' => j += 1,
            _ => return None,
        }
    }
    None
}

fn scan(sql: &str) -> ScanInfo {
    let bytes = sql.as_bytes();
    let mut info = ScanInfo::default();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b if b.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                // Postgres block comments nest.
                let mut depth = 1_usize;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            quote @ (b'\'' | b'"') => {
                // Single-quoted string or double-quoted identifier. Doubled
                // quotes ('' / "") fall out of the toggle naturally: the pair
                // reads as close-then-reopen with identical net effect.
                mark(&mut info, i);
                i += 1;
                while i < bytes.len() {
                    let done = bytes[i] == quote;
                    mark(&mut info, i);
                    i += 1;
                    if done {
                        break;
                    }
                }
            }
            b'$' => {
                if let Some(tag_end) = dollar_tag_end(bytes, i) {
                    mark(&mut info, i);
                    let tag = &sql[i..tag_end];
                    match sql[tag_end..].find(tag) {
                        Some(rel) => {
                            i = tag_end + rel + tag.len();
                            mark(&mut info, i - 1);
                        }
                        None => {
                            // Unterminated dollar quote: consume the rest.
                            mark(&mut info, bytes.len() - 1);
                            i = bytes.len();
                        }
                    }
                } else {
                    mark(&mut info, i);
                    i += 1;
                }
            }
            b';' => {
                if info.first_semicolon.is_none() {
                    info.first_semicolon = Some(i);
                }
                i += 1;
            }
            b if b.is_ascii_alphanumeric() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                mark(&mut info, start);
                mark(&mut info, i - 1);
                let word = &sql[start..i];
                if word.eq_ignore_ascii_case("into") {
                    info.has_into = true;
                } else if ["insert", "update", "delete", "merge"]
                    .iter()
                    .any(|keyword| word.eq_ignore_ascii_case(keyword))
                {
                    info.has_dml = true;
                }
            }
            _ => {
                mark(&mut info, i);
                i += 1;
            }
        }
    }
    info
}

fn leading_keyword(code: &str) -> &str {
    let end = code
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(code.len());
    &code[..end]
}

/// True iff `sql` is a single SELECT/WITH statement that can safely be
/// wrapped in a paging subquery.
pub fn wrappable(sql: &str) -> bool {
    let info = scan(sql);
    let Some(first) = info.first_code else {
        return false;
    };
    // Any significant code at or after a top-level semicolon means a second
    // statement (this also rejects `; select 1`, where the semicolon precedes
    // the first code byte).
    if let (Some(semicolon), Some(last)) = (info.first_semicolon, info.last_code_non_semicolon) {
        if last > semicolon {
            return false;
        }
    }
    let keyword = leading_keyword(&sql[first..]);
    if keyword.eq_ignore_ascii_case("select") {
        // `SELECT … INTO table` creates a table; Postgres forbids it inside a
        // subquery ("SELECT ... INTO is not allowed here").
        !info.has_into
    } else if keyword.eq_ignore_ascii_case("with") {
        // A WITH clause containing a data-modifying statement must be at the
        // top level; a WITH statement can also end in `SELECT … INTO`. Bare
        // `update` here also rejects `… for update` — a safe false negative.
        !info.has_into && !info.has_dml
    } else {
        false
    }
}

/// The statement text with the trailing semicolon (and anything after it) and
/// trailing whitespace removed.
fn statement_body(sql: &str) -> &str {
    let end = scan(sql).first_semicolon.unwrap_or(sql.len());
    sql[..end].trim_end()
}

/// Wraps a wrappable statement in a paging subquery. Callers must check
/// [`wrappable`] first; wrapping anything else produces SQL the server
/// rejects.
///
/// `sort` is a **1-based output-column ordinal** plus direction. `ORDER BY n`
/// is positional, so result sets with duplicate column names (any join
/// exposing two `id`s) stay unambiguous where `ORDER BY "name"` would error.
pub fn wrap(
    sql: &str,
    sort: Option<(usize, SortDirection)>,
    limit: usize,
    offset: usize,
) -> String {
    let body = statement_body(sql);
    let mut wrapped = format!("select * from (\n{body}\n) zedium_page");
    if let Some((ordinal, direction)) = sort {
        let direction = match direction {
            SortDirection::Ascending => "asc",
            SortDirection::Descending => "desc",
        };
        wrapped.push_str(&format!(" order by {ordinal} {direction}"));
    }
    wrapped.push_str(&format!(" limit {limit} offset {offset}"));
    wrapped
}

/// Wraps a wrappable statement in a filtering subquery
/// (`select * from (<body>) zedium_filter where <where_expr>`). Callers must
/// check [`wrappable`] on the original `sql` first. The output is itself a
/// plain `SELECT`, so it can be handed to [`wrap`] for page-0/sorted paging.
/// `where_expr` is raw user SQL (a Postgres error on an invalid expression is
/// surfaced to the caller); it is never quoted here.
pub fn filter_subquery(sql: &str, where_expr: &str) -> String {
    let body = statement_body(sql);
    format!("select * from (\n{body}\n) zedium_filter where {where_expr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- wrappable: positives ---

    #[test]
    fn plain_select_is_wrappable() {
        assert!(wrappable("select * from users"));
        assert!(wrappable("SELECT 1"));
    }

    #[test]
    fn with_cte_is_wrappable() {
        assert!(wrappable(
            "with t as (select id from users) select * from t"
        ));
        assert!(wrappable("WITH t AS (SELECT 1) SELECT * FROM t"));
    }

    #[test]
    fn leading_comments_are_skipped() {
        assert!(wrappable("-- heading\nselect 1"));
        assert!(wrappable("/* block */ select 1"));
        assert!(wrappable("/* outer /* nested */ still outer */\nselect 1"));
        assert!(wrappable(
            "  \n\t-- a\n/* b */ -- c\nwith t as (select 1) select * from t"
        ));
    }

    #[test]
    fn semicolon_inside_single_quoted_string_is_ignored() {
        assert!(wrappable("select 'a;b' as x"));
        assert!(wrappable("select 'it''s; fine' as x")); // '' doubling
    }

    #[test]
    fn semicolon_inside_double_quoted_identifier_is_ignored() {
        assert!(wrappable("select 1 as \";\""));
    }

    #[test]
    fn semicolon_inside_dollar_quoted_body_is_ignored() {
        assert!(wrappable("select $$a;b$$ as x"));
        assert!(wrappable("select $tag$ one; two $tag$ as x"));
    }

    #[test]
    fn positional_parameter_is_not_a_dollar_quote() {
        // `$1` must not open a dollar quote that swallows the rest.
        assert!(wrappable("select $1 + $2 as sum"));
    }

    #[test]
    fn semicolon_inside_line_and_block_comments_is_ignored() {
        assert!(wrappable("select 1 -- not two; statements\n"));
        assert!(wrappable("select /* ; */ 1"));
    }

    #[test]
    fn trailing_semicolons_whitespace_and_comments_are_allowed() {
        assert!(wrappable("select 1;"));
        assert!(wrappable("select 1 ;  \n\t"));
        assert!(wrappable("select 1; -- done"));
        assert!(wrappable("select 1;;"));
    }

    // --- wrappable: negatives ---

    #[test]
    fn multi_statement_is_not_wrappable() {
        assert!(!wrappable("select 1; select 2"));
        assert!(!wrappable("select 1;\n-- gap\nselect 2;"));
        assert!(!wrappable("; select 1")); // leading empty statement
    }

    #[test]
    fn non_select_first_keyword_is_not_wrappable() {
        assert!(!wrappable("update users set name = 'x'"));
        assert!(!wrappable("explain select 1"));
        assert!(!wrappable("insert into users values (1)"));
    }

    #[test]
    fn keyword_requires_token_boundary() {
        assert!(!wrappable("withdraw funds"));
        assert!(!wrappable("selection"));
    }

    #[test]
    fn select_into_is_not_wrappable() {
        // Postgres: "SELECT ... INTO is not allowed here" inside a subquery.
        assert!(!wrappable("select 1 as x into my_backup"));
        assert!(!wrappable("SELECT * INTO backup FROM users"));
        assert!(!wrappable("select 1 as a into temp t2"));
        assert!(!wrappable(
            "with t as (select 1) select * into backup from t"
        ));
    }

    #[test]
    fn data_modifying_cte_is_not_wrappable() {
        // Postgres: "WITH clause containing a data-modifying statement must
        // be at the top level" when wrapped in a subquery.
        assert!(!wrappable(
            "with ins as (insert into t values (1) returning x) select * from ins"
        ));
        assert!(!wrappable(
            "WITH upd AS (UPDATE t SET x = 1 RETURNING x) SELECT * FROM upd"
        ));
        assert!(!wrappable(
            "with del as (delete from t returning x) select * from del"
        ));
        assert!(!wrappable(
            "with m as (merge into t using s on t.id = s.id when matched then do nothing returning *) select * from m"
        ));
    }

    #[test]
    fn forbidden_keyword_detection_respects_token_boundaries_and_quoting() {
        // `into`/`update` as substrings of longer identifiers must not match.
        assert!(wrappable("select intonation from into_table"));
        assert!(wrappable("select * from updates"));
        // Inside strings, quoted identifiers, and comments they are inert.
        assert!(wrappable("select 'insert into t' as x"));
        assert!(wrappable("select \"into\" from t"));
        assert!(wrappable("select 1 -- into t\n"));
        assert!(wrappable(
            "with t as (select $$delete$$ as x) select * from t"
        ));
        // A plain SELECT with `for update` is still wrappable: only the
        // WITH-leading scan treats DML keywords as disqualifying.
        assert!(wrappable("select * from t for update"));
    }

    #[test]
    fn empty_and_comment_only_inputs_are_not_wrappable() {
        assert!(!wrappable(""));
        assert!(!wrappable("   \n\t"));
        assert!(!wrappable("-- just a comment"));
        assert!(!wrappable("/* only */"));
    }

    // --- wrap ---

    #[test]
    fn wrap_strips_trailing_semicolon_and_whitespace() {
        assert_eq!(
            wrap("select * from users;  \n", None, 500, 0),
            "select * from (\nselect * from users\n) zedium_page limit 500 offset 0"
        );
    }

    #[test]
    fn wrap_strips_everything_after_the_trailing_semicolon() {
        assert_eq!(
            wrap("select 1; -- done", None, 501, 500),
            "select * from (\nselect 1\n) zedium_page limit 501 offset 500"
        );
    }

    #[test]
    fn wrap_keeps_trailing_line_comment_harmless() {
        // No semicolon: the trailing comment stays in the body; the newline
        // before `)` keeps it from commenting out the paging clause.
        assert_eq!(
            wrap("select 1 -- note", None, 500, 0),
            "select * from (\nselect 1 -- note\n) zedium_page limit 500 offset 0"
        );
    }

    #[test]
    fn wrap_sorts_by_positional_ordinal() {
        // Ordinals (not names) keep duplicate-named columns unambiguous.
        assert_eq!(
            wrap(
                "select * from users",
                Some((1, SortDirection::Ascending)),
                500,
                0
            ),
            "select * from (\nselect * from users\n) zedium_page order by 1 asc limit 500 offset 0"
        );
        assert_eq!(
            wrap(
                "select * from users",
                Some((3, SortDirection::Descending)),
                501,
                1000,
            ),
            "select * from (\nselect * from users\n) zedium_page order by 3 desc limit 501 offset 1000"
        );
    }

    #[test]
    fn quote_ident_doubles_embedded_quotes() {
        assert_eq!(quote_ident("order"), "\"order\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    // --- filter_subquery ---

    #[test]
    fn filter_subquery_wraps_body_and_strips_trailing_semicolon() {
        assert_eq!(
            filter_subquery("select * from users;  \n", "id > 10"),
            "select * from (\nselect * from users\n) zedium_filter where id > 10"
        );
    }

    #[test]
    fn filter_subquery_output_is_itself_wrappable_and_pages() {
        let filtered = filter_subquery("select id, name from users", "name is not null");
        assert!(
            wrappable(&filtered),
            "filtered subquery must page like any select"
        );
        let paged = wrap(&filtered, Some((1, SortDirection::Descending)), 501, 0);
        assert_eq!(
            paged,
            "select * from (\nselect * from (\nselect id, name from users\n) \
zedium_filter where name is not null\n) zedium_page order by 1 desc limit 501 offset 0"
        );
    }
}
