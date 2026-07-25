//! Full-cell value viewer: a read-only pane showing the complete selected cell
//! value (plain text, pretty-printed JSON, or a hex dump for bytea) with a Copy
//! button. Editing stays in-grid; this pane never mutates data.

use gpui::{ClipboardItem, Context, IntoElement, ParentElement, Render, Styled, Window, div};
use settings::Settings as _;
use theme::ActiveTheme;
use theme_settings::ThemeSettings;
use ui::Tooltip;
use ui::prelude::*;

/// Read-only pane showing the full value of the selected grid cell. Formatting
/// follows the column's data type: pretty-printed JSON for `json`/`jsonb`, a
/// hex dump for `bytea`, otherwise the raw text. `NULL` renders as the literal
/// "NULL". Copy always writes the raw (unformatted) value.
#[allow(dead_code)]
pub struct ValueViewer {
    value: Option<String>,
    data_type: String,
}

impl ValueViewer {
    pub fn new() -> Self {
        ValueViewer {
            value: None,
            data_type: String::new(),
        }
    }

    pub fn set_value(&mut self, value: Option<String>, data_type: String, cx: &mut Context<Self>) {
        self.value = value;
        self.data_type = data_type;
        cx.notify();
    }

    /// The formatted text shown in the pane (NOT what Copy writes).
    fn display_text(&self) -> String {
        let Some(text) = &self.value else {
            return "NULL".to_string();
        };
        let lowered = self.data_type.to_ascii_lowercase();
        if lowered == "json" || lowered == "jsonb" {
            if let Some(pretty) = pretty_json(text) {
                return pretty;
            }
        } else if lowered == "bytea" {
            return hex_dump(text);
        }
        text.clone()
    }

    fn copy_value(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.value.clone().unwrap_or_default(),
        ));
    }

    /// Test-only: `value` is private so other modules' tests (`query_view`'s
    /// `CellSelected` wiring test) can only assert on it through this
    /// accessor.
    #[cfg(test)]
    pub(crate) fn value_for_test(&self) -> Option<String> {
        self.value.clone()
    }
}

impl Default for ValueViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for ValueViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.display_text();
        let theme_settings = ThemeSettings::get_global(cx);
        let font_family = theme_settings.buffer_font.family.clone();
        let font_size = theme_settings.buffer_font_size(cx);

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new("Value")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("copy-cell-value", IconName::Copy)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Copy value"))
                            .on_click(cx.listener(|this, _, _window, cx| this.copy_value(cx))),
                    ),
            )
            .child(
                div()
                    .id("value-viewer-content")
                    .flex_1()
                    .overflow_scroll()
                    .p_2()
                    .font_family(font_family)
                    .text_size(font_size)
                    .text_color(cx.theme().colors().text)
                    .child(content),
            )
    }
}

/// Pretty-prints `s` when it is a valid JSON object or array. Returns `None`
/// for invalid JSON and for bare scalars (numbers/strings/bools/null), where
/// re-indenting adds nothing over the raw cell text.
pub fn pretty_json(s: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(s).ok()?;
    if !value.is_object() && !value.is_array() {
        return None;
    }
    serde_json::to_string_pretty(&value).ok()
}

/// Decodes Postgres bytea text (`\xDEADBEEF`, or bare hex) and renders a
/// classic 16-bytes-per-line hex dump: `offset  hex...  |ascii|`. Non-hex or
/// truncated input decodes what it can and never panics.
pub fn hex_dump(bytea_text: &str) -> String {
    let hex = bytea_text.strip_prefix(r"\x").unwrap_or(bytea_text);
    let digits: Vec<u8> = hex
        .bytes()
        .filter_map(|byte| (byte as char).to_digit(16).map(|digit| digit as u8))
        .collect();
    let bytes: Vec<u8> = digits
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect();

    let mut out = String::new();
    for (line_index, chunk) in bytes.chunks(16).enumerate() {
        let offset = line_index * 16;
        out.push_str(&format!("{offset:08x}  "));
        for byte in chunk {
            out.push_str(&format!("{byte:02x} "));
        }
        for _ in chunk.len()..16 {
            out.push_str("   ");
        }
        out.push_str(" |");
        for byte in chunk {
            let printable = if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            };
            out.push(printable);
        }
        out.push_str("|\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_json_indents_valid_object_and_array() {
        let pretty = pretty_json(r#"{"b":1,"a":[2,3]}"#).expect("valid json must format");
        assert!(pretty.contains('\n'), "output must be multi-line: {pretty}");
        assert!(pretty.contains("  "), "output must be indented: {pretty}");
        // Round-trips to the same value.
        let reparsed: serde_json::Value = serde_json::from_str(&pretty).expect("re-parses");
        assert_eq!(reparsed, serde_json::json!({"b": 1, "a": [2, 3]}));
        assert!(pretty_json("[1, 2, 3]").is_some());
    }

    #[test]
    fn pretty_json_returns_none_for_invalid_or_bare_scalar_text() {
        assert_eq!(pretty_json("not json"), None);
        assert_eq!(pretty_json("{unterminated"), None);
        assert_eq!(pretty_json(""), None);
        // A bare number/string is technically valid JSON but adds nothing to
        // pretty-print; we only format objects and arrays.
        assert_eq!(pretty_json("42"), None);
        assert_eq!(pretty_json("\"plain\""), None);
    }

    #[test]
    fn hex_dump_formats_pg_bytea_hex_with_offsets_and_ascii() {
        // Postgres text protocol renders bytea as `\x` + hex digits.
        let dump = hex_dump(r"\x48656c6c6f21");
        assert!(dump.contains("48 65 6c 6c 6f 21"), "hex bytes: {dump}");
        assert!(dump.contains("Hello!"), "ascii gutter: {dump}");
        assert!(dump.starts_with("00000000"), "offset column: {dump}");
    }

    #[test]
    fn hex_dump_handles_non_prefixed_and_odd_input_without_panicking() {
        // No `\x` prefix: treat the whole string as hex digits.
        assert!(hex_dump("deadbeef").contains("de ad be ef"));
        // Odd digit count / stray non-hex chars must not panic; unusable input
        // yields an empty dump rather than a crash.
        let _ = hex_dump(r"\xdeadbee");
        let _ = hex_dump("zz");
    }
}

#[cfg(test)]
mod viewer_tests {
    use super::*;

    fn init_test(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    #[gpui::test]
    fn set_value_updates_content_and_display_per_data_type(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let viewer = cx.new(|_| ValueViewer::new());

        // Plain text: display is the raw value.
        viewer.update(cx, |viewer, cx| {
            viewer.set_value(Some("hello".into()), "text".into(), cx)
        });
        viewer.read_with(cx, |viewer, _| {
            assert_eq!(viewer.value.as_deref(), Some("hello"));
            assert_eq!(viewer.display_text(), "hello");
        });

        // jsonb: pretty-printed.
        viewer.update(cx, |viewer, cx| {
            viewer.set_value(Some(r#"{"a":1}"#.into()), "jsonb".into(), cx)
        });
        viewer.read_with(cx, |viewer, _| {
            assert!(
                viewer.display_text().contains('\n'),
                "json must pretty-print"
            );
        });

        // bytea: hex dump.
        viewer.update(cx, |viewer, cx| {
            viewer.set_value(Some(r"\x4869".into()), "bytea".into(), cx)
        });
        viewer.read_with(cx, |viewer, _| {
            assert!(
                viewer.display_text().contains("48 69"),
                "bytea must hex-dump"
            );
        });

        // NULL: display is the literal NULL, value cleared.
        viewer.update(cx, |viewer, cx| viewer.set_value(None, "text".into(), cx));
        viewer.read_with(cx, |viewer, _| {
            assert_eq!(viewer.value, None);
            assert_eq!(viewer.display_text(), "NULL");
        });
    }

    #[gpui::test]
    fn copy_writes_raw_value_not_formatted(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let viewer = cx.new(|_| ValueViewer::new());
        viewer.update(cx, |viewer, cx| {
            viewer.set_value(Some(r#"{"a":1}"#.into()), "jsonb".into(), cx);
            viewer.copy_value(cx);
        });
        let copied = cx.update(|cx| cx.read_from_clipboard());
        assert_eq!(
            copied.and_then(|item| item.text()),
            Some(r#"{"a":1}"#.to_string())
        );
    }
}
