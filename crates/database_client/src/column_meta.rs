//! Maps Postgres type names (`information_schema.columns.data_type`) to
//! column icons and numeric-ness for the schema tree and results grid.

use ui::IconName;

/// Lowercases and drops a `(...)` modifier ("character varying(255)",
/// "numeric(10,2)[]"); array notation is preserved for the caller to decide.
fn base_type(data_type: &str) -> String {
    let lowered = data_type.trim().to_ascii_lowercase();
    let stripped = match (lowered.find('('), lowered.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            format!("{}{}", &lowered[..open], &lowered[close + 1..])
        }
        _ => lowered,
    };
    stripped.trim().to_string()
}

/// Reduces array notation ("integer[]", "_int4") to the element type name.
/// Bare "array" (what information_schema reports) carries no element info and
/// becomes "" so it falls through to the unknown-type fallback.
fn element_type(base: &str) -> &str {
    let mut name = base;
    while let Some(stripped) = name.strip_suffix("[]") {
        name = stripped.trim_end();
    }
    name = name.strip_prefix('_').unwrap_or(name);
    if name == "array" { "" } else { name }
}

fn is_numeric_name(name: &str) -> bool {
    matches!(
        name,
        "smallint"
            | "integer"
            | "bigint"
            | "int"
            | "int2"
            | "int4"
            | "int8"
            | "serial"
            | "smallserial"
            | "bigserial"
            | "numeric"
            | "decimal"
            | "real"
            | "double precision"
            | "float4"
            | "float8"
            | "money"
            | "oid"
    )
}

/// Whether values of this type should be right-aligned in the results grid.
/// Arrays are excluded: an `integer[]` cell renders as `{1,2}`, not a number.
// Pinned cross-task API: the results-grid polish task is its first non-test
// consumer, so dead_code fires until that task lands.
#[allow(dead_code)]
pub fn is_numeric_type(data_type: &str) -> bool {
    let base = base_type(data_type);
    if base.ends_with("[]") || base.starts_with('_') || base == "array" {
        return false;
    }
    is_numeric_name(&base)
}

pub fn column_type_icon(data_type: &str) -> IconName {
    let base = base_type(data_type);
    let name = element_type(&base);
    if is_numeric_name(name) {
        return IconName::Hash;
    }
    match name {
        "boolean" | "bool" => IconName::ToggleLeft,
        "date" | "interval" | "timetz" => IconName::Calendar,
        name if name == "time" || name.starts_with("time ") || name.starts_with("timestamp") => {
            IconName::Calendar
        }
        "uuid" => IconName::Fingerprint,
        // json.svg is the Lucide braces glyph; reused instead of a duplicate
        // Braces asset.
        "json" | "jsonb" => IconName::Json,
        "bytea" => IconName::Binary,
        _ => IconName::TextSnippet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_types_map_to_hash() {
        for t in [
            "smallint",
            "integer",
            "bigint",
            "numeric",
            "decimal",
            "real",
            "double precision",
            "money",
            "serial",
            "bigserial",
        ] {
            assert_eq!(column_type_icon(t), IconName::Hash, "{t}");
            assert!(is_numeric_type(t), "{t}");
        }
    }

    #[test]
    fn text_types_map_to_text_snippet() {
        for t in [
            "character varying",
            "character",
            "text",
            "name",
            "\"char\"",
            "citext",
        ] {
            assert_eq!(column_type_icon(t), IconName::TextSnippet, "{t}");
            assert!(!is_numeric_type(t), "{t}");
        }
    }

    #[test]
    fn boolean_maps_to_toggle() {
        assert_eq!(column_type_icon("boolean"), IconName::ToggleLeft);
        assert_eq!(column_type_icon("bool"), IconName::ToggleLeft);
        assert!(!is_numeric_type("boolean"));
    }

    #[test]
    fn temporal_types_map_to_calendar() {
        for t in [
            "date",
            "time without time zone",
            "time with time zone",
            "time",
            "timestamp without time zone",
            "timestamp with time zone",
            "timestamp",
            "timestamptz",
            "timetz",
            "interval",
        ] {
            assert_eq!(column_type_icon(t), IconName::Calendar, "{t}");
        }
    }

    #[test]
    fn uuid_maps_to_fingerprint() {
        assert_eq!(column_type_icon("uuid"), IconName::Fingerprint);
    }

    #[test]
    fn json_types_map_to_json_braces_glyph() {
        assert_eq!(column_type_icon("json"), IconName::Json);
        assert_eq!(column_type_icon("jsonb"), IconName::Json);
    }

    #[test]
    fn bytea_maps_to_binary() {
        assert_eq!(column_type_icon("bytea"), IconName::Binary);
    }

    #[test]
    fn mapping_is_case_insensitive() {
        assert_eq!(column_type_icon("INTEGER"), IconName::Hash);
        assert_eq!(
            column_type_icon("Timestamp With Time Zone"),
            IconName::Calendar
        );
        assert_eq!(column_type_icon("JSONB"), IconName::Json);
        assert!(is_numeric_type("NUMERIC"));
    }

    #[test]
    fn type_modifiers_are_ignored() {
        assert_eq!(
            column_type_icon("character varying(255)"),
            IconName::TextSnippet
        );
        assert_eq!(column_type_icon("numeric(10,2)"), IconName::Hash);
        assert!(is_numeric_type("numeric(10,2)"));
    }

    #[test]
    fn arrays_take_the_element_type_icon() {
        assert_eq!(column_type_icon("integer[]"), IconName::Hash);
        assert_eq!(column_type_icon("text[]"), IconName::TextSnippet);
        assert_eq!(column_type_icon("uuid[]"), IconName::Fingerprint);
        assert_eq!(column_type_icon("numeric(10,2)[]"), IconName::Hash);
        // udt-style array names
        assert_eq!(column_type_icon("_int4"), IconName::Hash);
        assert_eq!(column_type_icon("_text"), IconName::TextSnippet);
        // information_schema reports bare "ARRAY" with no element info
        assert_eq!(column_type_icon("ARRAY"), IconName::TextSnippet);
    }

    #[test]
    fn arrays_are_never_numeric() {
        // an int[] cell renders as "{1,2}" — right-aligning it would be wrong
        assert!(!is_numeric_type("integer[]"));
        assert!(!is_numeric_type("_int4"));
        assert!(!is_numeric_type("ARRAY"));
    }

    #[test]
    fn unknown_types_fall_back_to_text_snippet() {
        assert_eq!(column_type_icon("USER-DEFINED"), IconName::TextSnippet);
        assert_eq!(column_type_icon("tsvector"), IconName::TextSnippet);
        assert_eq!(column_type_icon(""), IconName::TextSnippet);
        assert!(!is_numeric_type("USER-DEFINED"));
    }
}
