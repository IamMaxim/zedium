//! DataGrip-style results grid: resizable columns, row-number gutter,
//! cell selection/copy, and a cell context menu.

use std::collections::{HashMap, HashSet};

use editor::Editor;
use gpui::{
    AbsoluteLength, App, ClickEvent, ClipboardItem, Context, DismissEvent, Entity, EventEmitter,
    Focusable as _, FontWeight, KeyBinding, MouseButton, MouseDownEvent, Pixels, Point, Render,
    SharedString, Subscription, Window, actions, anchored, deferred, px,
};
use settings::Settings as _;
use theme::ActiveTheme;
use ui::prelude::*;
use ui::{
    ColumnWidthConfig, ContextMenu, ContextMenuEntry, ResizableColumnsState, Table,
    TableInteractionState, TableResizeBehavior,
};

use crate::connection::client::QueryResult;
use crate::connection::introspect::quote_literal;
use crate::query_view::Editability;
use crate::settings::DatabaseClientSettings;
use crate::sql_paging::{SortDirection, quote_ident};

actions!(
    database_client,
    [
        /// Copies the selected results-grid cell's raw value to the clipboard.
        CopyCellValue,
        /// Commits the in-progress inline cell edit.
        CommitCellEdit,
        /// Cancels the in-progress inline cell edit.
        CancelCellEdit,
        /// Appends a blank pending-insert row to the results grid.
        AddRow,
        /// Toggles the pending delete-mark on the selected committed row(s).
        /// No-op on pending-insert rows; those are only removed via Revert.
        DeleteRow,
        /// Submits all pending grid edits as a DML transaction.
        SubmitEdits,
        /// Discards all pending grid edits.
        RevertEdits
    ]
);

pub(crate) fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopyCellValue, Some("DatabaseResultsGrid")),
        KeyBinding::new("ctrl-c", CopyCellValue, Some("DatabaseResultsGrid")),
        KeyBinding::new("enter", CommitCellEdit, Some("DatabaseResultsGrid")),
        KeyBinding::new("escape", CancelCellEdit, Some("DatabaseResultsGrid")),
        KeyBinding::new("cmd-+", AddRow, Some("DatabaseResultsGrid")),
        KeyBinding::new("cmd-backspace", DeleteRow, Some("DatabaseResultsGrid")),
        KeyBinding::new("cmd-enter", SubmitEdits, Some("DatabaseResultsGrid")),
        KeyBinding::new("ctrl-enter", SubmitEdits, Some("DatabaseResultsGrid")),
    ]);
}

/// Approximate advance width of one glyph as a fraction of the font size.
/// The results font defaults to the (monospace) buffer font, where ~0.6 is typical.
const CHAR_WIDTH_FACTOR: f32 = 0.62;
pub(crate) const MIN_COLUMN_WIDTH: f32 = 60.0;
pub(crate) const MAX_COLUMN_WIDTH: f32 = 480.0;
/// Only the first N rows participate in initial width estimation.
const WIDTH_SAMPLE_ROWS: usize = 50;

fn display_len(cell: &Option<String>) -> usize {
    match cell {
        Some(text) => text.chars().count(),
        None => 4, // rendered as the literal "NULL"
    }
}

pub(crate) fn estimate_column_widths(
    columns: &[String],
    rows: &[Vec<Option<String>>],
    font_size: f32,
    cell_padding: f32,
) -> Vec<f32> {
    columns
        .iter()
        .enumerate()
        .map(|(col, name)| {
            let mut chars = name.chars().count();
            for row in rows.iter().take(WIDTH_SAMPLE_ROWS) {
                if let Some(cell) = row.get(col) {
                    chars = chars.max(display_len(cell));
                }
            }
            (chars as f32 * font_size * CHAR_WIDTH_FACTOR + 2.0 * cell_padding)
                .clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
        })
        .collect()
}

pub(crate) fn row_number_column_width(row_count: usize, font_size: f32, cell_padding: f32) -> f32 {
    let digits = row_count.max(1).ilog10() as f32 + 1.0;
    (digits * font_size * CHAR_WIDTH_FACTOR + 2.0 * cell_padding).max(28.0)
}

pub(crate) fn detect_numeric_columns(cols: usize, rows: &[Vec<Option<String>>]) -> Vec<bool> {
    (0..cols)
        .map(|col| {
            let mut saw_value = false;
            for row in rows {
                if let Some(Some(text)) = row.get(col) {
                    if text.trim().parse::<f64>().is_err() {
                        return false;
                    }
                    saw_value = true;
                }
            }
            saw_value
        })
        .collect()
}

pub(crate) fn row_to_tsv(row: &[Option<String>]) -> String {
    row.iter()
        .map(|cell| cell.as_deref().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\t")
}

pub(crate) fn row_to_json(columns: &[String], row: &[Option<String>]) -> String {
    let mut object = serde_json::Map::new();
    for (name, cell) in columns.iter().zip(row) {
        let value = match cell {
            Some(text) => serde_json::Value::String(text.clone()),
            None => serde_json::Value::Null,
        };
        object.insert(name.clone(), value);
    }
    // Value's Display is infallible — no Result to unwrap.
    serde_json::Value::Object(object).to_string()
}

/// Builds a single-column WHERE predicate for the header "Filter by this
/// value" / "Exclude this value" quick filters. `None` (a NULL cell) yields an
/// `IS NULL` / `IS NOT NULL` test — never `= ''`, which would miss real NULLs.
pub(crate) fn filter_predicate(column: &str, value: Option<&str>, exclude: bool) -> String {
    match value {
        None => format!(
            "{} {}",
            quote_ident(column),
            if exclude { "IS NOT NULL" } else { "IS NULL" }
        ),
        Some(text) => format!(
            "{} {} {}",
            quote_ident(column),
            if exclude { "<>" } else { "=" },
            quote_literal(text)
        ),
    }
}

/// A requested/applied server-side sort. Keyed by the 0-based data-column
/// index — never by name — so results with duplicate column names (any join
/// exposing two `id`s) sort unambiguously via a positional `ORDER BY`; the
/// name is carried only for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSort {
    /// 0-based index into the result's data columns (the SQL ordinal is
    /// `column + 1`).
    pub column: usize,
    /// Header display name of the sorted column.
    pub name: SharedString,
    pub direction: SortDirection,
}

/// Events emitted by [`ResultsGrid`] for its owning `QueryView` to handle.
pub enum ResultsGridEvent {
    /// User clicked a sortable header. The grid has already applied the
    /// none→asc→desc→none cycling decision ([`next_sort`]); the payload is the
    /// requested new sort state, or `None` to clear sorting. The owner stores
    /// it verbatim, re-queries, then calls [`ResultsGrid::set_result`] +
    /// [`ResultsGrid::set_sort`].
    SortRequested(Option<ColumnSort>),
    /// User picked a header quick-filter; the payload is the new WHERE
    /// expression built by [`filter_predicate`]. The owner re-queries page 0
    /// preserving sort.
    FilterRequested(String),
    /// User asked to submit all pending edits as a DML transaction. Emitted
    /// only when the grid is editable and has pending changes.
    SubmitRequested,
    /// User asked to discard all pending edits. Emitted only when the grid
    /// is editable.
    RevertRequested,
    /// A cell was selected (or the selection cleared). The owner pushes the
    /// value + data type into its `ValueViewer`.
    CellSelected {
        value: Option<String>,
        data_type: String,
    },
}

/// Uncommitted grid edits. `updates` keys are `(row, data-column)` into the
/// *original* result rows; the value is `None` for a staged SQL NULL and
/// `Some(text)` (incl. `Some("")`) for a concrete value. `inserts` holds new
/// rows (one `Option` per data column). `deletes` are original-row indices.
#[derive(Debug, Default, Clone)]
pub struct PendingChanges {
    pub updates: HashMap<(usize, usize), Option<String>>,
    pub inserts: Vec<Vec<Option<String>>>,
    pub deletes: HashSet<usize>,
}

impl PendingChanges {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.inserts.is_empty() && self.deletes.is_empty()
    }

    pub fn count(&self) -> usize {
        self.updates.len() + self.inserts.len() + self.deletes.len()
    }

    pub fn stage_update(&mut self, row: usize, col: usize, value: Option<String>) {
        self.updates.insert((row, col), value);
    }

    pub fn toggle_delete(&mut self, row: usize) {
        if !self.deletes.remove(&row) {
            self.deletes.insert(row);
        }
    }

    pub fn add_insert_row(&mut self, columns: usize) {
        self.inserts.push(vec![None; columns]);
    }

    pub fn set_insert_cell(&mut self, insert_index: usize, col: usize, value: Option<String>) {
        if let Some(row) = self.inserts.get_mut(insert_index) {
            if let Some(cell) = row.get_mut(col) {
                *cell = value;
            }
        }
    }

    pub fn clear(&mut self) {
        self.updates.clear();
        self.inserts.clear();
        self.deletes.clear();
    }
}

/// DataGrip-style grid over one [`QueryResult`]: pinned row-number gutter,
/// resizable columns, cell selection/copy, and a cell context menu.
pub struct ResultsGrid {
    interaction: Entity<TableInteractionState>,
    columns_state: Option<Entity<ResizableColumnsState>>,
    result: Option<QueryResult>,
    numeric_columns: Vec<bool>,
    /// Postgres data type per data column (by index), used to pick the
    /// `ValueViewer`'s display format for a selected cell (pretty JSON, hex
    /// for bytea, …). Empty when the caller hasn't supplied types yet — the
    /// viewer then falls back to plain text / JSON auto-detection.
    column_types: Vec<String>,
    /// (row index, data-column index) — the gutter is not selectable.
    selected_cell: Option<(usize, usize)>,
    sortable: bool,
    sort: Option<ColumnSort>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    /// Uncommitted edits staged against `result`.
    pending: PendingChanges,
    /// `Some` when `result`'s rows map to a single base table with a primary
    /// key, i.e. edits can be reconciled to `UPDATE`/`DELETE`/`INSERT`.
    /// `None` means read-only (joins, aggregates, views, …).
    editability: Option<Editability>,
    /// User-facing override that disables editing even when `editability`
    /// permits it (e.g. a read-only connection).
    read_only: bool,
    /// Row count of `result` at the time it was last (re)built, used to
    /// distinguish original rows (< this) from staged inserts (>= this).
    original_row_count: usize,
    /// The cell currently open for inline editing: `(row, data-column,
    /// overlay editor)`. `row` indexes into the rendered row list (originals
    /// followed by pending inserts), matching `original_row_count`'s split.
    editing_cell: Option<(usize, usize, Entity<Editor>)>,
    /// The single-line WHERE-expression editor backing the filter bar.
    /// Lazily created by [`Self::ensure_filter_editor`] (needs a `Window`),
    /// so it starts `None` until the owner's first render.
    filter_editor: Option<Entity<Editor>>,
    /// A filter-editor text sync requested by [`Self::set_filter_text`]
    /// before a `Window` was available (e.g. from the owner's event
    /// subscription); applied to `filter_editor` on the next render.
    pending_filter_text: Option<String>,
    /// Postgres error from the last failed filter re-query, shown under the
    /// filter bar. Cleared by a fresh (non-empty) filter request.
    filter_error: Option<String>,
    /// Test-only: the last overlay editor built by [`Self::begin_cell_edit`],
    /// kept alive so [`Self::begin_cell_edit_at`] can start a further edit
    /// session without a live `Window` — plain `Entity::update` (used by
    /// `grid.update(cx, ...)` in tests) never provides one; only
    /// `update_in`/the window-construction closure do.
    #[cfg(test)]
    last_editor_for_tests: Option<Entity<Editor>>,
}

impl EventEmitter<ResultsGridEvent> for ResultsGrid {}

impl ResultsGrid {
    pub fn new(cx: &mut Context<Self>) -> Self {
        ResultsGrid {
            interaction: cx.new(|cx| TableInteractionState::new(cx)),
            columns_state: None,
            result: None,
            numeric_columns: Vec::new(),
            column_types: Vec::new(),
            selected_cell: None,
            sortable: false,
            sort: None,
            context_menu: None,
            pending: PendingChanges::default(),
            editability: None,
            read_only: false,
            original_row_count: 0,
            editing_cell: None,
            filter_editor: None,
            pending_filter_text: None,
            filter_error: None,
            #[cfg(test)]
            last_editor_for_tests: None,
        }
    }

    /// Sets the Postgres data type shown per data column, used to format a
    /// selected cell's value in the `ValueViewer`. Pass an empty `Vec` (or
    /// leave unset) to fall back to plain text / JSON auto-detection.
    pub fn set_column_types(&mut self, types: Vec<String>, cx: &mut Context<Self>) {
        self.column_types = types;
        cx.notify();
    }

    /// Replaces the displayed result and recomputes numeric columns. When the
    /// column list is unchanged (Load more / sort page of the same session),
    /// the existing column-widths state — including the user's manual resizes —
    /// and the cell selection are preserved; otherwise the widths are rebuilt
    /// from estimates and the selection cleared. Always clears the context menu.
    /// `sortable`: headers clickable (caller passes `sql_paging::wrappable(&sql)`).
    pub fn set_result(
        &mut self,
        result: Option<QueryResult>,
        sortable: bool,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.sortable = sortable;
        match &result {
            Some(new_result) if !new_result.columns.is_empty() => {
                self.numeric_columns =
                    detect_numeric_columns(new_result.columns.len(), &new_result.rows);
                let font_size = f32::from(Self::results_font_size(cx));
                let cell_padding = Self::results_row_padding(cx);
                let gutter =
                    row_number_column_width(new_result.rows.len(), font_size, cell_padding);
                let same_columns = self
                    .result
                    .as_ref()
                    .is_some_and(|old| old.columns == new_result.columns);
                self.original_row_count = new_result.rows.len();
                match &self.columns_state {
                    Some(state) if same_columns => {
                        // Only the row-number gutter can need more room (the
                        // row count grew); data-column widths stay untouched.
                        state.update(cx, |state, cx| {
                            state.set_column_configuration(
                                0,
                                px(gutter),
                                TableResizeBehavior::None,
                            );
                            cx.notify();
                        });
                        self.selected_cell = self
                            .selected_cell
                            .filter(|(row, _)| *row < new_result.rows.len());
                    }
                    _ => {
                        // A different (or first) column list invalidates any
                        // staged edits — they were keyed against the old shape.
                        self.pending.clear();
                        self.editing_cell = None;
                        self.selected_cell = None;
                        let data_widths = estimate_column_widths(
                            &new_result.columns,
                            &new_result.rows,
                            font_size,
                            cell_padding,
                        );
                        let mut initial_widths: Vec<AbsoluteLength> =
                            Vec::with_capacity(data_widths.len() + 1);
                        initial_widths.push(AbsoluteLength::Pixels(px(gutter)));
                        initial_widths.extend(
                            data_widths
                                .iter()
                                .map(|width| AbsoluteLength::Pixels(px(*width))),
                        );
                        let mut behaviors = vec![TableResizeBehavior::None];
                        behaviors.extend(std::iter::repeat_n(
                            // MinSize is in rems: 60px at the default 16px-per-rem.
                            TableResizeBehavior::MinSize(MIN_COLUMN_WIDTH / 16.0),
                            data_widths.len(),
                        ));
                        let cols = data_widths.len() + 1;
                        self.columns_state =
                            Some(cx.new(|_| {
                                ResizableColumnsState::new(cols, initial_widths, behaviors)
                            }));
                    }
                }
            }
            _ => {
                self.pending.clear();
                self.editing_cell = None;
                self.selected_cell = None;
                self.numeric_columns.clear();
                self.columns_state = None;
                self.original_row_count = 0;
            }
        }
        self.result = result;
        cx.notify();
    }

    /// Overrides whether the grid may accept inline edits. `editability` is
    /// `None` for read-only results (joins, aggregates, views, …);
    /// `read_only` is a user-facing override (e.g. a read-only connection)
    /// that disables editing even when `editability` permits it.
    #[allow(dead_code)] // Wired up by a later task of the editable-grid plan.
    pub fn set_editability(
        &mut self,
        editability: Option<Editability>,
        read_only: bool,
        cx: &mut Context<Self>,
    ) {
        self.editability = editability;
        self.read_only = read_only;
        cx.notify();
    }

    #[allow(dead_code)] // Wired up by a later task of the editable-grid plan.
    pub fn pending_changes(&self) -> &PendingChanges {
        &self.pending
    }

    #[allow(dead_code)] // Wired up by a later task of the editable-grid plan.
    pub fn clear_pending(&mut self, cx: &mut Context<Self>) {
        self.pending.clear();
        cx.notify();
    }

    /// Requests submission of all pending edits. A no-op unless the grid is
    /// editable and has pending changes to submit.
    pub fn request_submit(&mut self, cx: &mut Context<Self>) {
        if self.is_editable() && !self.pending.is_empty() {
            cx.emit(ResultsGridEvent::SubmitRequested);
        }
    }

    /// Requests discarding all pending edits. A no-op unless the grid is
    /// editable.
    pub fn request_revert(&mut self, cx: &mut Context<Self>) {
        if self.is_editable() {
            cx.emit(ResultsGridEvent::RevertRequested);
        }
    }

    pub(crate) fn is_editable(&self) -> bool {
        self.editability.is_some() && !self.read_only
    }

    #[cfg(test)]
    pub(crate) fn pending_mut_for_test(&mut self) -> &mut PendingChanges {
        &mut self.pending
    }

    /// Appends a blank pending-insert row (one `None` per data column) to
    /// [`PendingChanges::inserts`]. A no-op when the grid isn't editable or
    /// has no result to size the row against.
    pub fn add_pending_row(&mut self, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let Some(columns) = self.result.as_ref().map(|result| result.columns.len()) else {
            return;
        };
        self.pending.add_insert_row(columns);
        cx.notify();
    }

    /// Toggles the pending delete-mark on the selected original row. Pending
    /// insert rows (not yet committed) aren't marked for delete; the later
    /// Revert action is responsible for dropping those.
    pub fn delete_selected_row(&mut self, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let Some((row, _)) = self.selected_cell else {
            return;
        };
        if row < self.original_row_count {
            self.pending.toggle_delete(row);
            cx.notify();
        }
    }

    /// The text currently displayed for `(row, col)`: a staged update takes
    /// priority over the original/inserted value so re-opening an edit (or
    /// building the overlay's seed text) reflects prior edits in this
    /// session, not the original result.
    fn cell_text(&self, row: usize, col: usize) -> Option<String> {
        if let Some(staged) = self.pending.updates.get(&(row, col)) {
            return staged.clone();
        }
        let row_values = if row < self.original_row_count {
            self.result.as_ref()?.rows.get(row)?
        } else {
            self.pending.inserts.get(row - self.original_row_count)?
        };
        row_values.get(col)?.clone()
    }

    /// Stages `value` for `(row, col)`, routing to `pending.updates` for an
    /// original row or `pending.inserts` for a row appended past
    /// `original_row_count`.
    fn stage_cell(&mut self, row: usize, col: usize, value: Option<String>) {
        if row < self.original_row_count {
            self.pending.stage_update(row, col, value);
        } else {
            self.pending
                .set_insert_cell(row - self.original_row_count, col, value);
        }
    }

    /// Opens the inline-edit overlay for `(row, col)`, seeded with the
    /// current cell text (empty for NULL). No-ops when the grid isn't
    /// editable. Live keystroke behavior of the overlay editor is verified
    /// by manual smoke, not this unit-testable state transition.
    fn begin_cell_edit(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable() {
            return;
        }
        let current_text = self.cell_text(row, col);
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            if let Some(text) = current_text.as_deref() {
                editor.set_text(text, window, cx);
            }
            editor
        });
        window.focus(&editor.focus_handle(cx), cx);
        #[cfg(test)]
        {
            self.last_editor_for_tests = Some(editor.clone());
        }
        self.editing_cell = Some((row, col, editor));
        cx.notify();
    }

    /// Test-only: opens the overlay for `(row, col)` by reusing the editor
    /// entity from the most recent [`Self::begin_cell_edit`] call, since a
    /// plain `Context<Self>` (as given to `Entity::update` in tests) never
    /// carries a `Window`. See `last_editor_for_tests` for why.
    #[cfg(test)]
    fn begin_cell_edit_at(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let Some(editor) = self.last_editor_for_tests.clone() else {
            return;
        };
        self.editing_cell = Some((row, col, editor));
        cx.notify();
    }

    /// Reads the overlay editor's current text and stages it, closing the
    /// overlay.
    fn commit_cell_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, _, editor)) = &self.editing_cell else {
            return;
        };
        let text = editor.read(cx).text(cx);
        self.commit_cell_edit_with(text, cx);
    }

    /// Stages `text` for the cell under edit and closes the overlay.
    fn commit_cell_edit_with(&mut self, text: String, cx: &mut Context<Self>) {
        let Some((row, col, _)) = self.editing_cell.take() else {
            return;
        };
        self.stage_cell(row, col, Some(text));
        cx.notify();
    }

    /// Stages a SQL NULL for the cell under edit and closes the overlay.
    #[allow(dead_code)] // Wired to a "Set to NULL" trigger by a later task of the editable-grid plan.
    fn set_editing_cell_null(&mut self, cx: &mut Context<Self>) {
        let Some((row, col, _)) = self.editing_cell.take() else {
            return;
        };
        self.stage_cell(row, col, None);
        cx.notify();
    }

    /// Closes the overlay without staging anything.
    fn cancel_cell_edit(&mut self, cx: &mut Context<Self>) {
        self.editing_cell = None;
        cx.notify();
    }

    /// Confirmed sort state to display as a header chevron (set by the owner
    /// after the sorted page arrives).
    pub fn set_sort(&mut self, sort: Option<ColumnSort>, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.notify();
    }

    fn results_font_size(cx: &App) -> Pixels {
        DatabaseClientSettings::get_global(cx).resolved_results_font_size(cx)
    }

    fn results_font_family(cx: &App) -> SharedString {
        DatabaseClientSettings::get_global(cx).resolved_results_font_family(cx)
    }

    fn results_row_padding(cx: &App) -> f32 {
        DatabaseClientSettings::get_global(cx).results_row_padding
    }

    fn cycle_sort(&mut self, column: usize, name: SharedString, cx: &mut Context<Self>) {
        cx.emit(ResultsGridEvent::SortRequested(next_sort(
            self.sort.as_ref(),
            column,
            &name,
        )));
    }

    fn select_cell(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_cell = Some((row, col));
        let value = self
            .result
            .as_ref()
            .and_then(|result| result.rows.get(row))
            .and_then(|row| row.get(col))
            .cloned()
            .flatten();
        let data_type = self.column_types.get(col).cloned().unwrap_or_default();
        cx.emit(ResultsGridEvent::CellSelected { value, data_type });
        let focus_handle = self.interaction.read(cx).focus_handle.clone();
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    /// Lazily creates the single-line WHERE-expression editor backing the
    /// filter bar. A no-op once it already exists.
    pub fn ensure_filter_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_editor.is_none() {
            let editor = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text(
                    "WHERE expression (e.g. status = 'active')",
                    window,
                    cx,
                );
                editor
            });
            self.filter_editor = Some(editor);
        }
    }

    /// Requests that the filter editor's text be set to `text`. Setting an
    /// `Editor`'s text needs a `Window`, which isn't available from the
    /// owner's event subscription (only a plain `Context`), so the request is
    /// staged here and applied on the next render.
    pub fn set_filter_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.pending_filter_text = Some(text.to_string());
        cx.notify();
    }

    pub fn set_filter_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.filter_error = error;
        cx.notify();
    }

    /// Reads the filter editor's current text and requests a re-query.
    fn submit_filter(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.filter_editor else {
            return;
        };
        let text = editor.read(cx).text(cx);
        cx.emit(ResultsGridEvent::FilterRequested(text));
    }

    /// Clears the filter editor and requests the filter be dropped.
    fn clear_filter(&mut self, cx: &mut Context<Self>) {
        cx.emit(ResultsGridEvent::FilterRequested(String::new()));
    }

    fn copy_selected_cell(&mut self, cx: &mut Context<Self>) {
        let Some((row, col)) = self.selected_cell else {
            return;
        };
        let Some(result) = &self.result else {
            return;
        };
        let Some(cell) = result.rows.get(row).and_then(|row| row.get(col)) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(cell.clone().unwrap_or_default()));
    }

    fn deploy_cell_context_menu(
        &mut self,
        row: usize,
        col: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = &self.result else {
            return;
        };
        let Some(row_values) = result.rows.get(row).cloned() else {
            return;
        };
        let Some(column_name) = result.columns.get(col).cloned() else {
            return;
        };
        let cell_value = row_values.get(col).cloned().flatten().unwrap_or_default();
        let row_tsv = row_to_tsv(&row_values);
        let row_json = row_to_json(&result.columns, &row_values);

        let context_menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.item(
                ContextMenuEntry::new("Copy Value").handler(move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(cell_value.clone()));
                }),
            )
            .item(
                ContextMenuEntry::new("Copy Row").handler(move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(row_tsv.clone()));
                }),
            )
            .item(
                ContextMenuEntry::new("Copy Row as JSON").handler(move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(row_json.clone()));
                }),
            )
            .item(
                ContextMenuEntry::new("Copy Column Name").handler(move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(column_name.clone()));
                }),
            )
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu.take();
            cx.notify();
        });
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    /// Deploys the header quick-filter menu ("Filter by this value" / "Exclude
    /// this value") for `col`. The value comes from the *selected* cell in
    /// that column, if any — with nothing selected in the column, the entries
    /// are shown disabled rather than hidden, since the column itself is
    /// still a valid filter target once a value is picked.
    fn deploy_header_context_menu(
        &mut self,
        col: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = &self.result else {
            return;
        };
        let Some(column_name) = result.columns.get(col).cloned() else {
            return;
        };
        let selected_value = self
            .selected_cell
            .filter(|(_, selected_col)| *selected_col == col)
            .and_then(|(row, _)| result.rows.get(row).and_then(|row| row.get(col)).cloned());
        let has_value = selected_value.is_some();
        let value_for_include = selected_value.clone().flatten();
        let value_for_exclude = selected_value.flatten();
        let include_column = column_name.clone();
        let exclude_column = column_name;
        let entity = cx.entity();

        let context_menu = ContextMenu::build(window, cx, |menu, window, _| {
            menu.item(
                ContextMenuEntry::new("Filter by this value")
                    .disabled(!has_value)
                    .handler(window.handler_for(&entity, move |_this, _window, cx| {
                        cx.emit(ResultsGridEvent::FilterRequested(filter_predicate(
                            &include_column,
                            value_for_include.as_deref(),
                            false,
                        )));
                    })),
            )
            .item(
                ContextMenuEntry::new("Exclude this value")
                    .disabled(!has_value)
                    .handler(window.handler_for(&entity, move |_this, _window, cx| {
                        cx.emit(ResultsGridEvent::FilterRequested(filter_predicate(
                            &exclude_column,
                            value_for_exclude.as_deref(),
                            true,
                        )));
                    })),
            )
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu.take();
            cx.notify();
        });
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cell(
        &self,
        row_ix: usize,
        col_ix: usize,
        cell: &Option<String>,
        is_insert_row: bool,
        is_delete_row: bool,
        is_modified: bool,
        row_height: Pixels,
        cell_padding: Pixels,
        selected: Option<(usize, usize)>,
        numeric: &[bool],
        editable: bool,
        editing: Option<&Entity<Editor>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_selected = selected == Some((row_ix, col_ix));
        let is_null = cell.is_none();
        let text: SharedString = match cell {
            Some(value) => SharedString::from(value.clone()),
            None => "NULL".into(),
        };
        div()
            .id(("db-cell", row_ix * numeric.len().max(1) + col_ix))
            .h(row_height)
            .w_full()
            .px(cell_padding)
            .whitespace_nowrap()
            .text_ellipsis()
            .overflow_hidden()
            .when(numeric.get(col_ix).copied().unwrap_or(false), |this| {
                this.text_right()
            })
            .when(is_null && editing.is_none(), |this| {
                this.italic().text_color(cx.theme().colors().text_muted)
            })
            .when(is_insert_row, |this| {
                this.bg(cx.theme().status().created_background)
            })
            .when(is_delete_row, |this| {
                this.line_through().text_color(cx.theme().status().deleted)
            })
            .when(is_modified && !is_insert_row, |this| {
                this.bg(cx.theme().status().modified_background)
                    .border_1()
                    .border_color(cx.theme().status().modified)
            })
            .when(is_selected, |this| {
                this.bg(cx.theme().colors().element_selected)
                    .border_1()
                    .border_color(cx.theme().colors().border_focused)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.select_cell(row_ix, col_ix, window, cx);
                    if editable && event.click_count >= 2 {
                        this.begin_cell_edit(row_ix, col_ix, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.select_cell(row_ix, col_ix, window, cx);
                    this.deploy_cell_context_menu(row_ix, col_ix, event.position, window, cx);
                }),
            )
            .when_some(editing, |this, editor| {
                // The overlay editor's live keystroke behavior (typing,
                // caret, IME) is covered by manual smoke, not unit tests.
                this.child(div().size_full().child(editor.clone()))
            })
            .when(editing.is_none(), |this| this.child(text))
            .into_any_element()
    }
}

impl Render for ResultsGrid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filter_editor(window, cx);
        if let Some(text) = self.pending_filter_text.take() {
            if let Some(editor) = &self.filter_editor {
                editor.update(cx, |editor, cx| editor.set_text(text, window, cx));
            }
        }
        let filter_bar = self.filter_editor.clone().map(|filter_editor| {
            h_flex()
                .w_full()
                .flex_shrink_0()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .child(div().flex_1().min_w_0().child(filter_editor))
                .child(
                    Button::new("apply-filter", "Apply")
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _window, cx| this.submit_filter(cx))),
                )
                .child(
                    Button::new("clear-filter", "Clear")
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _window, cx| this.clear_filter(cx))),
                )
                .when_some(self.filter_error.clone(), |bar, error| {
                    bar.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                })
        });

        let (Some(result), Some(columns_state)) = (&self.result, &self.columns_state) else {
            return div().size_full().into_any_element();
        };
        let data_cols = result.columns.len();
        let cols = data_cols + 1;
        let original_row_count = self.original_row_count;
        let row_count = original_row_count + self.pending.inserts.len();

        let settings = DatabaseClientSettings::get_global(cx);
        let font_family = Self::results_font_family(cx);
        let font_size = Self::results_font_size(cx);
        let cell_padding = px(settings.results_row_padding);
        let row_height: Pixels = font_size * settings.resolved_results_line_height(cx);
        // py_0p5 (2px top + bottom) + the 1px bottom border of the header row.
        let header_height = row_height + px(5.);

        let text_muted = cx.theme().colors().text_muted;
        let created_background = cx.theme().status().created_background;
        let rows = result.rows.clone();
        let numeric = self.numeric_columns.clone();
        let selected = self.selected_cell;
        let editable = self.is_editable();
        let pending = self.pending.clone();
        let editing_cell = self.editing_cell.clone();

        let mut header_cells: Vec<AnyElement> = Vec::with_capacity(cols);
        header_cells.push(div().into_any_element()); // gutter header
        for (col, name) in result.columns.iter().enumerate() {
            let name = SharedString::from(name.clone());
            // Match by column index, not name: duplicate-named columns must
            // not all grow a chevron when one of them is sorted.
            let sort_icon = self
                .sort
                .as_ref()
                .filter(|sort| sort.column == col)
                .map(|sort| match sort.direction {
                    SortDirection::Ascending => IconName::ChevronUp,
                    SortDirection::Descending => IconName::ChevronDown,
                });
            header_cells.push(
                div()
                    .id(("db-header", col))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .flex_1()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(name.clone()),
                    )
                    .when_some(sort_icon, |this, icon| {
                        this.child(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted))
                    })
                    .when(self.sortable, |this| {
                        let name = name.clone();
                        this.cursor_pointer().on_click(cx.listener(
                            move |this, _: &ClickEvent, _window, cx| {
                                // Also keeps a double-click from triggering the Table's
                                // built-in header width-reset.
                                cx.stop_propagation();
                                this.cycle_sort(col, name.clone(), cx);
                            },
                        ))
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.deploy_header_context_menu(col, event.position, window, cx);
                        }),
                    )
                    .into_any_element(),
            );
        }

        let table = Table::new(cols)
            .interactable(&self.interaction)
            .striped()
            .no_ui_font()
            .disable_base_style()
            .pin_cols(1)
            .width_config(ColumnWidthConfig::Resizable(columns_state.clone()))
            .header(header_cells)
            .uniform_list(
                "database-results",
                row_count,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    range
                        .filter_map(|row_ix| {
                            let is_insert_row = row_ix >= original_row_count;
                            let row = if is_insert_row {
                                pending.inserts.get(row_ix - original_row_count)?
                            } else {
                                rows.get(row_ix)?
                            };
                            let is_delete_row = !is_insert_row && pending.deletes.contains(&row_ix);
                            let mut cells: Vec<AnyElement> = Vec::with_capacity(cols);
                            cells.push(
                                div()
                                    .h(row_height)
                                    .w_full()
                                    .px(cell_padding)
                                    .text_right()
                                    .text_color(text_muted)
                                    .overflow_hidden()
                                    .when(is_insert_row, |this| this.bg(created_background))
                                    .child(SharedString::from((row_ix + 1).to_string()))
                                    .into_any_element(),
                            );
                            // Multi-statement batches can yield ragged rows
                            // (cell counts differing from the column list);
                            // pad with NULL / truncate to the column count so
                            // Table's rectangular row invariant holds instead
                            // of panicking.
                            for col_ix in 0..data_cols {
                                let staged = pending.updates.get(&(row_ix, col_ix));
                                let is_modified = staged.is_some();
                                let original = row.get(col_ix).unwrap_or(&None);
                                let cell = staged.unwrap_or(original);
                                let editing = editing_cell
                                    .as_ref()
                                    .filter(|(r, c, _)| *r == row_ix && *c == col_ix)
                                    .map(|(_, _, editor)| editor);
                                cells.push(this.render_cell(
                                    row_ix,
                                    col_ix,
                                    cell,
                                    is_insert_row,
                                    is_delete_row,
                                    is_modified,
                                    row_height,
                                    cell_padding,
                                    selected,
                                    &numeric,
                                    editable,
                                    editing,
                                    cx,
                                ));
                            }
                            Some(cells)
                        })
                        .collect()
                }),
            )
            .empty_table_callback(|_window, _cx| {
                Label::new("No rows returned.")
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element()
            });

        div()
            .key_context("DatabaseResultsGrid")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(font_family)
            .text_size(font_size)
            .line_height(row_height)
            .text_color(cx.theme().colors().text)
            .on_action(cx.listener(|this, _: &CopyCellValue, _window, cx| {
                this.copy_selected_cell(cx);
            }))
            .on_action(cx.listener(|this, _: &CommitCellEdit, window, cx| {
                if this.editing_cell.is_some() {
                    this.commit_cell_edit(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CancelCellEdit, _window, cx| {
                if this.editing_cell.is_some() {
                    this.cancel_cell_edit(cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &AddRow, _window, cx| {
                this.add_pending_row(cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteRow, _window, cx| {
                this.delete_selected_row(cx);
            }))
            .on_action(cx.listener(|this, _: &SubmitEdits, _window, cx| {
                this.request_submit(cx);
            }))
            .on_action(cx.listener(|this, _: &RevertEdits, _window, cx| {
                this.request_revert(cx);
            }))
            .children(filter_bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        // Muted strip behind the (transparent) header cells; the Table
                        // itself draws the header's bottom border.
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(header_height)
                            .bg(cx.theme().colors().surface_background),
                    )
                    .child(table),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
            .into_any_element()
    }
}

/// The single source of the none→asc→desc→none sort-cycling decision, keyed
/// by column index. `cycle_sort` emits its output verbatim in `SortRequested`;
/// QueryView stores it verbatim — no duplicate cycle logic anywhere.
pub(crate) fn next_sort(
    current: Option<&ColumnSort>,
    column: usize,
    name: &SharedString,
) -> Option<ColumnSort> {
    let direction = match current {
        Some(sort) if sort.column == column => match sort.direction {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => return None,
        },
        _ => SortDirection::Ascending,
    };
    Some(ColumnSort {
        column,
        name: name.clone(),
        direction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn width_estimation_uses_header_and_cells_with_clamps() {
        let columns = vec!["id".to_string(), "a_rather_long_header_name".to_string()];
        let rows = vec![vec![cell("1"), cell("x")], vec![cell("22"), cell("yy")]];
        let widths = estimate_column_widths(
            &columns,
            &rows,
            14.0,
            crate::settings::DEFAULT_RESULTS_ROW_PADDING,
        );
        assert_eq!(widths.len(), 2);
        // "id" and its cells are tiny -> clamped up to the minimum.
        assert_eq!(widths[0], MIN_COLUMN_WIDTH);
        // header longer than cells -> header drives the width.
        assert!(widths[1] > MIN_COLUMN_WIDTH && widths[1] < MAX_COLUMN_WIDTH);
        assert!(
            widths[1] >= 25.0 * 14.0 * 0.5,
            "header length must influence width, got {}",
            widths[1]
        );
    }

    #[test]
    fn width_estimation_clamps_long_cells_and_ignores_rows_past_sample() {
        let columns = vec!["c".to_string()];
        let rows = vec![vec![cell(&"x".repeat(500))]]; // way over max
        let widths = estimate_column_widths(
            &columns,
            &rows,
            14.0,
            crate::settings::DEFAULT_RESULTS_ROW_PADDING,
        );
        assert_eq!(widths[0], MAX_COLUMN_WIDTH);

        // Row 51 must not affect the estimate.
        let mut rows_past_sample: Vec<Vec<Option<String>>> =
            (0..50).map(|_| vec![cell("ab")]).collect();
        rows_past_sample.push(vec![cell(&"y".repeat(500))]);
        let widths = estimate_column_widths(
            &columns,
            &rows_past_sample,
            14.0,
            crate::settings::DEFAULT_RESULTS_ROW_PADDING,
        );
        assert!(widths[0] < MAX_COLUMN_WIDTH);
    }

    #[test]
    fn width_estimation_treats_null_as_four_chars() {
        // NULL renders as the 4-char literal "NULL"; it must count as 4, not 0.
        let columns = vec!["c".to_string()];
        let with_null = estimate_column_widths(
            &columns,
            &[vec![None]],
            14.0,
            crate::settings::DEFAULT_RESULTS_ROW_PADDING,
        );
        let with_text = estimate_column_widths(
            &columns,
            &[vec![cell("NULL")]],
            14.0,
            crate::settings::DEFAULT_RESULTS_ROW_PADDING,
        );
        assert_eq!(with_null, with_text);
    }

    #[test]
    fn width_estimation_grows_with_cell_padding() {
        // A column name long enough that the un-padded estimate already
        // clears MIN_COLUMN_WIDTH, so the padding delta isn't swallowed by
        // the clamp.
        let columns = vec!["a_medium_length_column_name".to_string()];
        let rows = vec![vec![cell("1")]];
        let narrow = estimate_column_widths(&columns, &rows, 14.0, 0.0);
        let wide = estimate_column_widths(&columns, &rows, 14.0, 20.0);
        assert!(wide[0] > narrow[0]);
    }

    #[test]
    fn row_number_width_grows_with_digit_count() {
        let narrow = row_number_column_width(9, 14.0, crate::settings::DEFAULT_RESULTS_ROW_PADDING);
        let wide =
            row_number_column_width(12_345, 14.0, crate::settings::DEFAULT_RESULTS_ROW_PADDING);
        assert!(wide > narrow);
        assert!(narrow >= 1.0 * 14.0 * 0.5);
    }

    #[test]
    fn numeric_detection_scans_non_null_cells() {
        let rows = vec![
            vec![cell("1"), cell("abc"), None, cell("-1.5e3")],
            vec![cell("-2.5"), cell("2"), None, cell("42")],
        ];
        assert_eq!(
            detect_numeric_columns(4, &rows),
            vec![true, false, false, true] // all-NULL column is NOT numeric
        );
        // No rows loaded -> nothing is numeric.
        assert_eq!(detect_numeric_columns(2, &[]), vec![false, false]);
    }

    #[test]
    fn tsv_uses_empty_string_for_null() {
        let row = vec![cell("a"), None, cell("b\tc")];
        assert_eq!(row_to_tsv(&row), "a\t\tb\tc");
    }

    #[test]
    fn json_keys_by_column_and_uses_null() {
        let columns = vec!["id".to_string(), "name".to_string(), "note".to_string()];
        let row = vec![cell("1"), None, cell("say \"hi\"")];
        let json = row_to_json(&columns, &row);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("copy-row JSON must parse");
        assert_eq!(parsed["id"], serde_json::json!("1"));
        assert_eq!(parsed["name"], serde_json::Value::Null);
        assert_eq!(parsed["note"], serde_json::json!("say \"hi\""));
    }

    #[test]
    fn filter_predicate_quotes_identifier_and_literal_for_equality() {
        assert_eq!(
            filter_predicate("first name", Some("o'brien"), false),
            "\"first name\" = 'o''brien'"
        );
        assert_eq!(filter_predicate("id", Some("42"), true), "\"id\" <> '42'");
    }

    #[test]
    fn filter_predicate_uses_is_null_for_null_cells() {
        // A NULL cell has no literal: build `IS NULL` / `IS NOT NULL`, never `= ''`.
        assert_eq!(filter_predicate("note", None, false), "\"note\" IS NULL");
        assert_eq!(filter_predicate("note", None, true), "\"note\" IS NOT NULL");
        // Identifier quoting still applies for NULL predicates.
        assert_eq!(
            filter_predicate("we\"ird", None, false),
            "\"we\"\"ird\" IS NULL"
        );
    }

    fn sort(column: usize, name: &str, direction: SortDirection) -> ColumnSort {
        ColumnSort {
            column,
            name: name.into(),
            direction,
        }
    }

    #[test]
    fn sort_cycles_none_asc_desc_none_by_column_index() {
        let name: SharedString = "id".into();
        let first = next_sort(None, 0, &name);
        assert_eq!(first, Some(sort(0, "id", SortDirection::Ascending)));
        let second = next_sort(first.as_ref(), 0, &name);
        assert_eq!(second, Some(sort(0, "id", SortDirection::Descending)));
        assert_eq!(next_sort(second.as_ref(), 0, &name), None);
    }

    #[test]
    fn sort_switching_column_starts_ascending() {
        let current = Some(sort(0, "id", SortDirection::Descending));
        assert_eq!(
            next_sort(current.as_ref(), 1, &"name".into()),
            Some(sort(1, "name", SortDirection::Ascending))
        );
    }

    #[test]
    fn duplicate_column_names_cycle_independently_by_index() {
        // A join can expose two `id` columns; clicking the second must not
        // continue the first one's cycle.
        let name: SharedString = "id".into();
        let current = Some(sort(0, "id", SortDirection::Ascending));
        assert_eq!(
            next_sort(current.as_ref(), 1, &name),
            Some(sort(1, "id", SortDirection::Ascending))
        );
    }

    fn init_test(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    #[gpui::test]
    fn set_result_builds_gutter_plus_data_column_state(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let grid = cx.new(ResultsGrid::new);
        let result = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), None]],
            command_tag: None,
            truncated: false,
        };
        grid.update(cx, |grid, cx| grid.set_result(Some(result), true, cx));
        grid.read_with(cx, |grid, cx| {
            let widths = grid
                .columns_state
                .as_ref()
                .expect("columns state must exist once a result is set");
            let widths = widths.read(cx);
            assert_eq!(widths.cols(), 3, "gutter + 2 data columns");
            assert_eq!(
                widths.resize_behavior().as_slice()[0],
                ui::TableResizeBehavior::None,
                "gutter must not be resizable"
            );
            assert_eq!(grid.numeric_columns, vec![true, false]);
            assert!(grid.sortable);
        });
    }

    fn number_rows(n: usize) -> Vec<Vec<Option<String>>> {
        (0..n).map(|i| vec![cell(&i.to_string())]).collect()
    }

    #[gpui::test]
    fn set_result_with_same_columns_keeps_widths_state_and_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test(cx);
        let grid = cx.new(ResultsGrid::new);
        let page = |rows: usize| QueryResult {
            columns: vec!["id".into()],
            rows: number_rows(rows),
            command_tag: None,
            truncated: false,
        };
        grid.update(cx, |grid, cx| {
            grid.set_result(Some(page(3)), true, cx);
            grid.selected_cell = Some((2, 0));
        });
        let state_before = grid.read_with(cx, |grid, _| {
            grid.columns_state
                .as_ref()
                .expect("columns state must exist")
                .entity_id()
        });

        // Load more / a sorted page arrives: same column list, more rows.
        grid.update(cx, |grid, cx| grid.set_result(Some(page(6)), true, cx));
        grid.read_with(cx, |grid, _| {
            let state_after = grid
                .columns_state
                .as_ref()
                .expect("columns state must survive an append")
                .entity_id();
            assert_eq!(
                state_before, state_after,
                "manual column widths (the state entity) must survive paging"
            );
            assert_eq!(
                grid.selected_cell,
                Some((2, 0)),
                "selection must survive an append of the same columns"
            );
        });

        // A different column list rebuilds the state and clears the selection.
        let other = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![cell("1"), cell("a")]],
            command_tag: None,
            truncated: false,
        };
        grid.update(cx, |grid, cx| grid.set_result(Some(other), true, cx));
        grid.read_with(cx, |grid, _| {
            let state_after = grid
                .columns_state
                .as_ref()
                .expect("columns state must exist")
                .entity_id();
            assert_ne!(state_before, state_after);
            assert_eq!(grid.selected_cell, None);
        });
    }

    #[gpui::test]
    fn set_result_clears_selection_when_selected_row_vanishes(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let grid = cx.new(ResultsGrid::new);
        let page = |rows: usize| QueryResult {
            columns: vec!["id".into()],
            rows: number_rows(rows),
            command_tag: None,
            truncated: false,
        };
        grid.update(cx, |grid, cx| {
            grid.set_result(Some(page(5)), true, cx);
            grid.selected_cell = Some((4, 0));
            // A re-sorted page 0 can shrink the row count below the selection.
            grid.set_result(Some(page(2)), true, cx);
        });
        grid.read_with(cx, |grid, _| assert_eq!(grid.selected_cell, None));
    }

    #[gpui::test]
    fn copy_selected_cell_writes_raw_value_and_empty_for_null(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let grid = cx.new(ResultsGrid::new);
        let result = QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![vec![Some("hello".into()), None]],
            command_tag: None,
            truncated: false,
        };
        grid.update(cx, |grid, cx| {
            grid.set_result(Some(result), false, cx);
            grid.selected_cell = Some((0, 0));
            grid.copy_selected_cell(cx);
        });
        let copied = cx.update(|cx| cx.read_from_clipboard());
        assert_eq!(
            copied.and_then(|item| item.text()),
            Some("hello".to_string())
        );

        grid.update(cx, |grid, cx| {
            grid.selected_cell = Some((0, 1));
            grid.copy_selected_cell(cx);
        });
        let copied = cx
            .update(|cx| cx.read_from_clipboard())
            .expect("copying a NULL cell must still write an (empty) clipboard item");
        // ClipboardItem::text() concatenates string entries and returns None
        // when the result is empty — an empty string has no text to return.
        assert_eq!(copied.text(), None);
    }

    #[gpui::test]
    async fn grid_renders_without_panicking(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (_grid, cx) = cx.add_window_view(|_window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into(), "name".into()],
                rows: vec![
                    vec![Some("1".into()), Some("alice".into())],
                    vec![Some("2".into()), None],
                ],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid
        });
        cx.run_until_parked();
    }

    #[test]
    fn pending_stages_update_revert_and_delete() {
        let mut pending = PendingChanges::default();
        assert!(pending.is_empty());

        pending.stage_update(2, 1, Some("ann".into()));
        pending.stage_update(2, 3, None); // set-NULL
        assert_eq!(pending.updates.get(&(2, 1)), Some(&Some("ann".to_string())));
        assert_eq!(pending.updates.get(&(2, 3)), Some(&None));
        assert_eq!(pending.count(), 2);

        pending.toggle_delete(2);
        assert!(pending.deletes.contains(&2));
        pending.toggle_delete(2); // toggles back off
        assert!(!pending.deletes.contains(&2));

        pending.clear();
        assert!(pending.is_empty());
        assert_eq!(pending.count(), 0);
    }

    #[test]
    fn pending_add_insert_row_seeds_null_cells() {
        let mut pending = PendingChanges::default();
        pending.add_insert_row(3);
        assert_eq!(pending.inserts, vec![vec![None, None, None]]);
        pending.set_insert_cell(0, 1, Some("x".into()));
        assert_eq!(pending.inserts[0], vec![None, Some("x".into()), None]);
        assert_eq!(pending.count(), 1);
    }

    #[gpui::test]
    async fn grid_renders_ragged_rows_without_panicking(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        // Multi-statement batches (e.g. `SELECT 1; SELECT 1, 2;`) can produce
        // rows whose cell count differs from the column list; the grid must
        // pad/truncate such rows instead of panicking in Table's row builder.
        let (_grid, cx) = cx.add_window_view(|_window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into()],
                rows: vec![
                    vec![Some("1".into())],
                    vec![Some("1".into()), Some("2".into())],
                    vec![],
                ],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn committing_a_cell_edit_stages_an_update(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (grid, cx) = cx.add_window_view(|window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into(), "name".into()],
                rows: vec![vec![Some("1".into()), Some("al".into())]],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid.editability = Some(crate::query_view::Editability {
                schema: "public".into(),
                table: "users".into(),
                pk_cols: vec!["id".into()],
                col_types: Default::default(),
            });
            grid.begin_cell_edit(0, 1, window, cx);
            grid
        });
        grid.update(cx, |grid, cx| {
            grid.commit_cell_edit_with("ann".to_string(), cx);
            assert_eq!(
                grid.pending.updates.get(&(0, 1)),
                Some(&Some("ann".to_string())),
                "committing seeds an update entry"
            );
            assert!(grid.editing_cell.is_none(), "commit closes the overlay");
        });
    }

    #[gpui::test]
    fn set_null_stages_none_and_cancel_leaves_pending_untouched(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (grid, cx) = cx.add_window_view(|window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into(), "name".into()],
                rows: vec![vec![Some("1".into()), Some("al".into())]],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid.editability = Some(crate::query_view::Editability {
                schema: "public".into(),
                table: "users".into(),
                pk_cols: vec!["id".into()],
                col_types: Default::default(),
            });
            grid.begin_cell_edit(0, 1, window, cx);
            grid
        });
        grid.update(cx, |grid, cx| {
            grid.set_editing_cell_null(cx);
            assert_eq!(grid.pending.updates.get(&(0, 1)), Some(&None));
        });
        grid.update(cx, |grid, cx| {
            grid.begin_cell_edit_at(0, 0, cx); // helper that skips window focus in tests
            grid.cancel_cell_edit(cx);
            assert!(
                !grid.pending.updates.contains_key(&(0, 0)),
                "cancel stages nothing"
            );
        });
    }

    #[gpui::test]
    fn add_row_appends_blank_insert_and_delete_marks_selected(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (grid, cx) = cx.add_window_view(|_window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into(), "name".into()],
                rows: vec![vec![Some("1".into()), Some("al".into())]],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid.editability = Some(crate::query_view::Editability {
                schema: "public".into(),
                table: "users".into(),
                pk_cols: vec!["id".into()],
                col_types: Default::default(),
            });
            grid
        });
        grid.update(cx, |grid, cx| {
            grid.add_pending_row(cx);
            assert_eq!(grid.pending.inserts, vec![vec![None, None]]);

            grid.selected_cell = Some((0, 0));
            grid.delete_selected_row(cx);
            assert!(grid.pending.deletes.contains(&0));
        });
    }

    #[gpui::test]
    fn add_and_delete_are_no_ops_when_not_editable(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (grid, cx) = cx.add_window_view(|_window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into()],
                rows: vec![vec![Some("1".into())]],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx); // no editability set
            grid
        });
        grid.update(cx, |grid, cx| {
            grid.add_pending_row(cx);
            grid.selected_cell = Some((0, 0));
            grid.delete_selected_row(cx);
            assert!(
                grid.pending.is_empty(),
                "read-only/non-editable grid stages nothing"
            );
        });
    }

    #[gpui::test]
    fn request_submit_and_revert_emit_events(cx: &mut gpui::TestAppContext) {
        init_test(cx);
        let (grid, cx) = cx.add_window_view(|_window, cx| {
            let mut grid = ResultsGrid::new(cx);
            let result = QueryResult {
                columns: vec!["id".into()],
                rows: vec![vec![Some("1".into())]],
                command_tag: None,
                truncated: false,
            };
            grid.set_result(Some(result), true, cx);
            grid.editability = Some(crate::query_view::Editability {
                schema: "public".into(),
                table: "users".into(),
                pk_cols: vec!["id".into()],
                col_types: Default::default(),
            });
            grid.pending.stage_update(0, 0, Some("2".into()));
            grid
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _sub = cx.update(|_window, cx| {
            let events = events.clone();
            cx.subscribe(
                &grid,
                move |_grid, event: &ResultsGridEvent, _cx| match event {
                    ResultsGridEvent::SubmitRequested => events.borrow_mut().push("submit"),
                    ResultsGridEvent::RevertRequested => events.borrow_mut().push("revert"),
                    ResultsGridEvent::SortRequested(_) => {}
                    ResultsGridEvent::FilterRequested(_) => {}
                    ResultsGridEvent::CellSelected { .. } => {}
                },
            )
        });
        grid.update(cx, |grid, cx| {
            grid.request_submit(cx);
            grid.request_revert(cx);
        });
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec!["submit", "revert"]);
    }
}
