//! The `"database"` settings section: results-grid font, pagination, and
//! per-surface density knobs (results grid, sidebar tree, query console
//! toolbar).

use gpui::{App, Pixels, SharedString};
use settings::{FontFamilyName, IntoGpui, RegisterSetting, Settings, SettingsContent};
use theme_settings::{ThemeSettings, UiLineHeight};

/// Rows fetched per results page when `page_size` is not configured.
pub const DEFAULT_PAGE_SIZE: usize = 500;
/// Lower bound applied to a configured `page_size`.
pub const MIN_PAGE_SIZE: usize = 50;
/// Horizontal cell padding (px, per side) when `results_row_padding` is not
/// configured.
pub const DEFAULT_RESULTS_ROW_PADDING: f32 = 6.0;
/// Upper bound applied to a configured `results_row_padding`.
pub const MAX_RESULTS_ROW_PADDING: f32 = 24.0;
/// Default executed-statement history retention.
pub const DEFAULT_HISTORY_LIMIT: usize = 1000;
/// Lower bound applied to a configured `history_limit`.
pub const MIN_HISTORY_LIMIT: usize = 1;

/// Density settings for the sidebar connection/database/schema/table tree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DatabaseSidebarSettings {
    /// Vertical padding (px) added to each tree row. `None` = current row
    /// height, unchanged.
    pub padding: Option<f32>,
    /// Row label font size (px). `None` = the UI font size.
    pub font_size: Option<f32>,
    /// Node/disclosure icon size (px). `None` = 14 (`IconSize::Small`).
    pub icon_size: Option<f32>,
}

/// Density settings for the query console toolbar (Run button, status
/// labels).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DatabaseToolbarSettings {
    /// Vertical bar padding (px). `None` = the current fixed padding.
    pub padding: Option<f32>,
    /// Toolbar label/button font size (px). `None` = the current fixed
    /// sizes.
    pub font_size: Option<f32>,
    /// Run button icon size (px). `None` = the current icon size.
    pub icon_size: Option<f32>,
}

/// Typed view of the `"database"` settings section.
#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct DatabaseClientSettings {
    /// Results-grid font family; `None` falls back to the buffer font family.
    pub results_font_family: Option<FontFamilyName>,
    /// Results-grid font size; `None` falls back to the buffer font size.
    pub results_font_size: Option<Pixels>,
    /// Rows fetched per results page; always at least [`MIN_PAGE_SIZE`].
    pub page_size: usize,
    /// Results-grid cell horizontal padding (px, per side); always clamped
    /// to `[0, MAX_RESULTS_ROW_PADDING]`.
    pub results_row_padding: f32,
    /// Results-grid line height; `None` falls back to the global
    /// `ui_line_height`.
    pub results_line_height: Option<UiLineHeight>,
    /// Sidebar tree density knobs.
    pub sidebar: DatabaseSidebarSettings,
    /// Query console toolbar density knobs.
    pub toolbar: DatabaseToolbarSettings,
    /// Whether to populate the autocomplete metadata cache on connection.
    pub metadata_cache: bool,
    /// CSV representation of SQL NULL on export; default is the empty string.
    pub export_null_string: String,
    /// Confirm before submitting grid DML. Consumed by
    /// `QueryView::submit_edits`, which prompts before running the DML when
    /// this is `true`.
    pub confirm_edits: bool,
    /// Global read-only default (disables all DML). OR'd with the active
    /// profile's own `read_only` flag by `QueryView::effective_read_only`.
    pub read_only: bool,
    /// Default commit mode for new query tabs: "auto" | "manual". Used by
    /// `QueryView::new` for a brand-new tab; a restored tab's persisted
    /// commit mode always wins over this default.
    pub commit_mode: String,
    /// Executed-statement retention; always at least [`MIN_HISTORY_LIMIT`].
    /// Consumed by `QueryView::record_history`'s prune call.
    pub history_limit: usize,
}

impl Settings for DatabaseClientSettings {
    fn from_settings(content: &SettingsContent) -> Self {
        let database = content.database.as_ref();
        Self {
            results_font_family: database.and_then(|database| database.results_font_family.clone()),
            results_font_size: database
                .and_then(|database| database.results_font_size)
                .map(|size| size.into_gpui()),
            page_size: database
                .and_then(|database| database.page_size)
                .unwrap_or(DEFAULT_PAGE_SIZE)
                .max(MIN_PAGE_SIZE),
            results_row_padding: database
                .and_then(|database| database.results_row_padding)
                .map(|padding| padding.clamp(0.0, MAX_RESULTS_ROW_PADDING))
                .unwrap_or(DEFAULT_RESULTS_ROW_PADDING),
            results_line_height: database
                .and_then(|database| database.results_line_height)
                .map(UiLineHeight::from),
            sidebar: database
                .and_then(|database| database.sidebar.clone())
                .map(|sidebar| DatabaseSidebarSettings {
                    padding: sidebar.padding,
                    font_size: sidebar.font_size,
                    icon_size: sidebar.icon_size,
                })
                .unwrap_or_default(),
            toolbar: database
                .and_then(|database| database.toolbar.clone())
                .map(|toolbar| DatabaseToolbarSettings {
                    padding: toolbar.padding,
                    font_size: toolbar.font_size,
                    icon_size: toolbar.icon_size,
                })
                .unwrap_or_default(),
            metadata_cache: database
                .and_then(|database| database.metadata_cache)
                .unwrap_or(true),
            export_null_string: database
                .and_then(|database| database.export_null_string.clone())
                .unwrap_or_default(),
            confirm_edits: database
                .and_then(|database| database.confirm_edits)
                .unwrap_or(true),
            read_only: database
                .and_then(|database| database.read_only)
                .unwrap_or(false),
            commit_mode: database
                .and_then(|database| database.commit_mode.clone())
                .map(|mode| mode.to_ascii_lowercase())
                .filter(|mode| mode == "auto" || mode == "manual")
                .unwrap_or_else(|| "auto".to_string()),
            history_limit: database
                .and_then(|database| database.history_limit)
                .unwrap_or(DEFAULT_HISTORY_LIMIT)
                .max(MIN_HISTORY_LIMIT),
        }
    }
}

impl DatabaseClientSettings {
    /// The results-grid font family, falling back to the buffer font family.
    pub fn resolved_results_font_family(&self, cx: &App) -> SharedString {
        self.results_font_family.as_ref().map_or_else(
            || ThemeSettings::get_global(cx).buffer_font.family.clone(),
            |family| family.0.clone().into(),
        )
    }

    /// The results-grid font size, falling back to the buffer font size.
    pub fn resolved_results_font_size(&self, cx: &App) -> Pixels {
        self.results_font_size
            .map(theme_settings::clamp_font_size)
            .unwrap_or_else(|| ThemeSettings::get_global(cx).buffer_font_size(cx))
    }

    /// The results-grid line height, falling back to the global
    /// `ui_line_height`.
    pub fn resolved_results_line_height(&self, cx: &App) -> f32 {
        self.results_line_height
            .map(|line_height| line_height.value())
            .unwrap_or_else(|| ThemeSettings::get_global(cx).ui_line_height())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{BorrowAppContext, px};
    use settings::SettingsStore;

    fn init_settings(cx: &mut App) {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
    }

    fn set_database_settings(
        cx: &mut App,
        update: impl FnOnce(&mut settings::DatabaseSettingsContent),
    ) {
        cx.update_global::<SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |content| {
                update(content.database.get_or_insert_default());
            });
        });
    }

    #[gpui::test]
    fn defaults_match_default_json(cx: &mut App) {
        init_settings(cx);
        let settings = DatabaseClientSettings::get_global(cx);
        assert_eq!(settings.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(settings.results_font_family, None);
        assert_eq!(settings.results_font_size, None);
        assert_eq!(settings.results_row_padding, DEFAULT_RESULTS_ROW_PADDING);
        assert_eq!(settings.results_line_height, None);
        assert_eq!(settings.sidebar, DatabaseSidebarSettings::default());
        assert_eq!(settings.toolbar, DatabaseToolbarSettings::default());
        assert!(settings.metadata_cache);
        assert_eq!(settings.export_null_string, "");
        assert!(settings.confirm_edits);
        assert!(!settings.read_only);
        assert_eq!(settings.commit_mode, "auto");
        assert_eq!(settings.history_limit, DEFAULT_HISTORY_LIMIT);
    }

    #[gpui::test]
    fn behavioral_settings_defaults(cx: &mut App) {
        init_settings(cx);
        let settings = DatabaseClientSettings::get_global(cx);
        assert!(settings.confirm_edits);
        assert!(!settings.read_only);
        assert_eq!(settings.commit_mode, "auto");
        assert_eq!(settings.history_limit, DEFAULT_HISTORY_LIMIT);
        assert!(settings.metadata_cache);
        assert_eq!(settings.export_null_string, "");
    }

    #[gpui::test]
    fn behavioral_settings_pass_through(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| {
            database.confirm_edits = Some(false);
            database.read_only = Some(true);
            database.commit_mode = Some("Manual".into());
            database.history_limit = Some(250);
            database.metadata_cache = Some(false);
            database.export_null_string = Some("\\N".into());
        });
        let settings = DatabaseClientSettings::get_global(cx);
        assert!(!settings.confirm_edits);
        assert!(settings.read_only);
        assert_eq!(settings.commit_mode, "manual"); // normalized to lowercase
        assert_eq!(settings.history_limit, 250);
        assert!(!settings.metadata_cache);
        assert_eq!(settings.export_null_string, "\\N");
    }

    #[gpui::test]
    fn history_limit_is_clamped_and_commit_mode_falls_back(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| database.history_limit = Some(0));
        assert_eq!(
            DatabaseClientSettings::get_global(cx).history_limit,
            MIN_HISTORY_LIMIT
        );
        set_database_settings(cx, |database| database.commit_mode = Some("bogus".into()));
        assert_eq!(DatabaseClientSettings::get_global(cx).commit_mode, "auto");
    }

    #[gpui::test]
    fn export_null_string_defaults_empty_and_passes_through(cx: &mut App) {
        init_settings(cx);
        assert_eq!(
            DatabaseClientSettings::get_global(cx).export_null_string,
            ""
        );

        set_database_settings(cx, |database| {
            database.export_null_string = Some("\\N".to_string());
        });
        assert_eq!(
            DatabaseClientSettings::get_global(cx).export_null_string,
            "\\N"
        );
    }

    #[gpui::test]
    fn metadata_cache_defaults_on_and_can_be_disabled(cx: &mut App) {
        init_settings(cx);
        assert!(
            DatabaseClientSettings::get_global(cx).metadata_cache,
            "metadata_cache must default to true"
        );
        set_database_settings(cx, |database| database.metadata_cache = Some(false));
        assert!(!DatabaseClientSettings::get_global(cx).metadata_cache);
    }

    #[gpui::test]
    fn results_row_padding_is_clamped(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| database.results_row_padding = Some(-5.0));
        assert_eq!(
            DatabaseClientSettings::get_global(cx).results_row_padding,
            0.0
        );

        set_database_settings(cx, |database| database.results_row_padding = Some(100.0));
        assert_eq!(
            DatabaseClientSettings::get_global(cx).results_row_padding,
            MAX_RESULTS_ROW_PADDING
        );

        set_database_settings(cx, |database| database.results_row_padding = Some(10.0));
        assert_eq!(
            DatabaseClientSettings::get_global(cx).results_row_padding,
            10.0
        );
    }

    #[gpui::test]
    fn results_line_height_falls_back_to_global_and_passes_through(cx: &mut App) {
        init_settings(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);

        let global_ui_line_height = ThemeSettings::get_global(cx).ui_line_height();
        assert_eq!(
            DatabaseClientSettings::get_global(cx).resolved_results_line_height(cx),
            global_ui_line_height
        );

        set_database_settings(cx, |database| {
            database.results_line_height = Some(settings::UiLineHeight::Custom(2.0));
        });
        assert_eq!(
            DatabaseClientSettings::get_global(cx).resolved_results_line_height(cx),
            2.0
        );
    }

    #[gpui::test]
    fn sidebar_settings_pass_through(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| {
            database.sidebar = Some(settings::DatabaseSidebarSettingsContent {
                padding: Some(4.0),
                font_size: Some(13.0),
                icon_size: Some(12.0),
            });
        });
        let settings = DatabaseClientSettings::get_global(cx);
        assert_eq!(settings.sidebar.padding, Some(4.0));
        assert_eq!(settings.sidebar.font_size, Some(13.0));
        assert_eq!(settings.sidebar.icon_size, Some(12.0));
    }

    #[gpui::test]
    fn toolbar_settings_pass_through(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| {
            database.toolbar = Some(settings::DatabaseToolbarSettingsContent {
                padding: Some(2.0),
                font_size: Some(11.0),
                icon_size: Some(10.0),
            });
        });
        let settings = DatabaseClientSettings::get_global(cx);
        assert_eq!(settings.toolbar.padding, Some(2.0));
        assert_eq!(settings.toolbar.font_size, Some(11.0));
        assert_eq!(settings.toolbar.icon_size, Some(10.0));
    }

    #[gpui::test]
    fn page_size_is_clamped_to_minimum(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| database.page_size = Some(10));
        assert_eq!(
            DatabaseClientSettings::get_global(cx).page_size,
            MIN_PAGE_SIZE
        );

        set_database_settings(cx, |database| database.page_size = Some(2000));
        assert_eq!(DatabaseClientSettings::get_global(cx).page_size, 2000);
    }

    #[gpui::test]
    fn results_font_settings_pass_through(cx: &mut App) {
        init_settings(cx);
        set_database_settings(cx, |database| {
            database.results_font_family = Some(settings::FontFamilyName("Zed Plex Mono".into()));
            database.results_font_size = Some(settings::FontSize::from(13.0));
        });
        let settings = DatabaseClientSettings::get_global(cx);
        assert_eq!(
            settings.results_font_family,
            Some(settings::FontFamilyName("Zed Plex Mono".into()))
        );
        assert_eq!(settings.results_font_size, Some(px(13.0)));
    }

    #[gpui::test]
    fn resolved_fonts_fall_back_to_buffer_font(cx: &mut App) {
        init_settings(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);

        let theme_settings = ThemeSettings::get_global(cx);
        let expected_family = theme_settings.buffer_font.family.clone();
        let expected_size = theme_settings.buffer_font_size(cx);

        let settings = DatabaseClientSettings::get_global(cx).clone();
        assert_eq!(settings.resolved_results_font_family(cx), expected_family);
        assert_eq!(settings.resolved_results_font_size(cx), expected_size);
    }

    #[gpui::test]
    fn resolved_fonts_use_overrides_when_set(cx: &mut App) {
        init_settings(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        set_database_settings(cx, |database| {
            database.results_font_family = Some(settings::FontFamilyName("Zed Plex Mono".into()));
            database.results_font_size = Some(settings::FontSize::from(13.0));
        });

        let settings = DatabaseClientSettings::get_global(cx).clone();
        assert_eq!(
            settings.resolved_results_font_family(cx),
            SharedString::from("Zed Plex Mono")
        );
        assert_eq!(settings.resolved_results_font_size(cx), px(13.0));
    }
}
