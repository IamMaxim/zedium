use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use editor::{
    Bias, CompletionProvider, Editor, EditorElement, EditorEvent, EditorStyle, RowHighlightOptions,
};
use gpui::{
    App, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyBinding, ParentElement, PromptLevel, Render, SharedString, Styled, Subscription, Task,
    TextStyle, WeakEntity, Window, actions, div, px, relative,
};
use language::{
    Anchor as LanguageAnchor, Buffer, CharScopeContext, CodeLabel, TextBufferSnapshot,
    ToOffset as _,
};
use project::lsp_store::CompletionDocumentation;
use project::{
    Completion, CompletionDisplayOptions, CompletionResponse, CompletionSource, Project,
};
use settings::Settings as _;
use theme::ActiveTheme;
use theme_settings::ThemeSettings;
use ui::prelude::*;
use ui::{ContextMenu, ContextMenuEntry, IconButtonShape, Indicator, PopoverMenu, Tooltip};
use util::ResultExt as _;
use workspace::{
    ItemId, SerializableItem, Workspace, WorkspaceId, delete_unloaded_items,
    item::{Item, ItemEvent},
};

use crate::{
    connection::{
        client::{Connection, DEFAULT_RESULT_LIMIT, QueryResult, truncate_rows},
        edit::{InsertRow, RowEdit, generate_delete, generate_insert, generate_update},
        introspect::RelationKind,
        metadata_cache::{Candidate, MetadataCache, candidates, classify_context},
        profile::ConnectionProfile,
        session::{CommitMode, TransactionSession},
        store,
    },
    export::{ExportFormat, serialize_result},
    persistence::QueryDb,
    query_history::{HistoryDb, HistoryEntry},
    results_grid::{ColumnSort, PendingChanges, ResultsGrid, ResultsGridEvent},
    settings::DatabaseClientSettings,
    value_viewer::ValueViewer,
};

actions!(database_client, [RunStatement, RunScript]);

pub(crate) const DEFAULT_EDITOR_RATIO: f32 = 0.3;
const MIN_EDITOR_RATIO: f32 = 0.1;
const MAX_EDITOR_RATIO: f32 = 0.9;
const RESIZE_HANDLE_HEIGHT: f32 = 8.0;

pub(crate) fn clamp_editor_ratio(ratio: f32) -> f32 {
    ratio.clamp(MIN_EDITOR_RATIO, MAX_EDITOR_RATIO)
}

/// Zero-size drag payload for the editor/results split handle. Carries the
/// view's `EntityId` because `on_drag_move` fires window-wide for the payload
/// type — two visible `QueryView`s must not react to each other's drags.
#[derive(Clone)]
struct DraggedQuerySplitHandle {
    view: gpui::EntityId,
}

pub(crate) fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-enter", RunScript, Some("QueryView > Editor")),
        KeyBinding::new("ctrl-shift-enter", RunScript, Some("QueryView > Editor")),
        KeyBinding::new("cmd-enter", RunStatement, Some("QueryView > Editor")),
        KeyBinding::new("ctrl-enter", RunStatement, Some("QueryView > Editor")),
    ]);
}

pub enum QueryViewEvent {
    Edit,
    ResultsChanged,
}

/// What a `dispatch_query` call is for; decides page-size snapshotting,
/// row appending, and how a failure is surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryIntent {
    /// Fresh run of the editor buffer: page 0, replaces the result; a failure
    /// replaces the result with the error.
    Run,
    /// Next page of the current session: appends rows; a failure keeps them.
    LoadMore,
    /// Page 0 of the current session with a changed sort: replaces the rows;
    /// a failure keeps the currently displayed ones.
    Sort,
    /// Page 0 with a changed WHERE filter: replaces the rows preserving sort;
    /// a failure keeps the displayed rows and reverts `filter_expr` to the
    /// filter those rows actually reflect (`applied_filter`).
    Filter,
}

/// Row-highlight type key for the statement currently running.
enum ActiveStatement {}

pub struct QueryView {
    editor: Entity<Editor>,
    connection: Option<Connection>,
    profile: Option<ConnectionProfile>,
    result: Option<QueryResult>,
    error: Option<String>,
    elapsed_ms: Option<u128>,
    /// Monotonically increasing counter; each `run` call captures its value so that
    /// stale async completions can detect they are no longer the most recent query.
    run_generation: usize,
    /// True while a query is in flight; cleared when the matching generation completes.
    running: bool,
    /// Zero-based index of the most recently loaded page of the current result session.
    page: usize,
    /// Requested server-side sort, stored verbatim from
    /// `ResultsGridEvent::SortRequested` — never cycled here.
    sort: Option<ColumnSort>,
    /// The sort confirmed by the last successful page — what the grid's rows
    /// actually reflect. A failed sort re-query reverts `sort` to this.
    applied_sort: Option<ColumnSort>,
    /// Requested WHERE filter, stored verbatim from
    /// `ResultsGridEvent::FilterRequested`. `None`/empty clears filtering.
    filter_expr: Option<String>,
    /// The filter confirmed by the last successful page — what the grid's
    /// rows actually reflect. A failed filter re-query reverts `filter_expr`
    /// to this, mirroring `applied_sort`.
    applied_filter: Option<String>,
    /// Page size captured when the current result session started (page 0);
    /// Load more keeps using it so a settings change mid-session cannot skew
    /// the `page * page_size` offsets against already-loaded rows.
    session_page_size: usize,
    /// Whether the server had at least one more row past the last fetched page.
    has_more: bool,
    /// The SQL that produced the current result; Load more / sort re-runs use it
    /// (not the editor buffer, which may have been edited since).
    last_sql: Option<String>,
    /// True when the SQL content or profile has changed and needs to be persisted.
    needs_serialize: bool,
    /// Committed editor/results split ratio (persisted). `visible_` is the live
    /// drag preview; they diverge only mid-drag.
    editor_ratio: f32,
    visible_editor_ratio: f32,
    /// Optional short label shown in the tab title (e.g. the table name for double-click opens).
    label: Option<SharedString>,
    /// Set by `run_statement` to a `"stmt N/M"` label naming the statement that
    /// just ran; cleared by `run_script` since a whole-buffer run has no single
    /// ordinal to show.
    last_run_label: Option<SharedString>,
    /// User-facing override that disables editing even when the grid's
    /// result is otherwise editable (e.g. a read-only connection). Computed
    /// from `DatabaseClientSettings::read_only` OR'd with the active
    /// profile's `read_only` flag (see `effective_read_only`), and pushed
    /// into the grid via `ResultsGrid::set_editability`.
    read_only: bool,
    /// `Some` when the current result maps to a single base table with a
    /// primary key, letting the grid's staged edits be reconciled to
    /// `UPDATE`/`DELETE`/`INSERT` by [`submit_edits`](Self::submit_edits).
    /// Recomputed by [`finish_query`](Self::finish_query) via
    /// [`detect_editability`] on each `Run`; kept as-is across `Sort`/`LoadMore`
    /// since those re-query the same table.
    editability: Option<Editability>,
    /// Auto (default) or Manual commit mode for this tab; persisted.
    commit_mode: CommitMode,
    /// Lazily allocated in Manual mode on the first statement; holds the open
    /// transaction. `None` until then, or after commit/rollback in Auto.
    #[allow(dead_code)] // Routed through by Task 5's statement dispatch.
    session: Option<TransactionSession>,
    /// Set when the user asked to leave Manual mode while a transaction is open;
    /// the render layer shows a Commit / Rollback / Cancel prompt and the switch
    /// completes only once the transaction is resolved.
    pending_commit_mode_switch: Option<CommitMode>,
    results_grid: Entity<ResultsGrid>,
    /// Read-only full-cell viewer, updated on `ResultsGridEvent::CellSelected`.
    value_viewer: Entity<ValueViewer>,
    /// Whether the value-viewer pane is shown alongside the results grid.
    value_viewer_visible: bool,
    /// Schema/table/column metadata backing `SqlCompletionProvider`, and read
    /// by `finish_query` to compute `editability` for the just-run SQL. The
    /// `Rc` is also cloned into the provider and the background sweep, so it
    /// stays populated (and alive) for the `QueryView`'s lifetime.
    metadata: Rc<RefCell<MetadataCache>>,
    // Retained for future actions that need workspace access (e.g. opening linked tabs).
    #[allow(dead_code)]
    workspace: WeakEntity<Workspace>,
    workspace_id: Option<WorkspaceId>,
    _editor_subscription: Subscription,
    _grid_subscription: Subscription,
}

impl QueryView {
    pub fn new(
        profile: Option<ConnectionProfile>,
        connection: Option<Connection>,
        initial_sql: String,
        workspace: WeakEntity<Workspace>,
        workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| Buffer::local(initial_sql, cx));
        let editor = cx.new(|cx| Editor::for_buffer(buffer.clone(), None, window, cx));
        let results_grid = cx.new(ResultsGrid::new);

        cx.spawn({
            let workspace = workspace.clone();
            let buffer = buffer.clone();
            async move |_this, cx| {
                let Ok(language_future) = workspace.read_with(cx, |workspace, cx| {
                    workspace
                        .project()
                        .read(cx)
                        .languages()
                        .language_for_name("SQL")
                }) else {
                    return;
                };
                let language = language_future.await.ok();
                buffer.update(cx, |buffer, cx| {
                    buffer.set_language(language, cx);
                });
            }
        })
        .detach();

        let editor_subscription =
            cx.subscribe(&editor, |this, _editor, event: &EditorEvent, cx| {
                if matches!(
                    event,
                    EditorEvent::BufferEdited | EditorEvent::Edited { .. }
                ) {
                    this.needs_serialize = true;
                    cx.emit(QueryViewEvent::Edit);
                }
            });

        let grid_subscription = cx.subscribe_in(
            &results_grid,
            window,
            |this, _grid, event: &ResultsGridEvent, window, cx| match event {
                ResultsGridEvent::SortRequested(sort) => {
                    // While a query is in flight the grid still shows the
                    // previous result; a header click there must not wrap the
                    // in-flight SQL with an ordinal from the old column list.
                    if this.running {
                        return;
                    }
                    if this.last_sql.is_none() {
                        return;
                    }
                    // Filter-aware: if a filter is currently applied, keep
                    // sorting/paging the filtered SQL, not the raw base.
                    let sql = this.effective_base_sql();
                    // Belt-and-braces: the grid only makes headers clickable when
                    // sortable, but a stale event must never wrap unwrappable SQL.
                    if !crate::sql_paging::wrappable(&sql) {
                        return;
                    }
                    // Stored verbatim — the none→asc→desc→none decision was already
                    // made by results_grid::next_sort. A sort change restarts the
                    // result session at page 0.
                    this.sort = sort.clone();
                    this.dispatch_query(sql, 0, QueryIntent::Sort, cx);
                }
                ResultsGridEvent::FilterRequested(expr) => {
                    // Mirrors the SortRequested guard: a stale event from a
                    // no-longer-current result must not re-query.
                    if this.running {
                        return;
                    }
                    this.apply_filter_request(expr.clone(), cx);
                }
                ResultsGridEvent::CellSelected { value, data_type } => {
                    this.on_cell_selected(value.clone(), data_type.clone(), cx);
                }
                ResultsGridEvent::SubmitRequested => this.submit_edits(window, cx),
                ResultsGridEvent::RevertRequested => this.revert_edits(cx),
            },
        );

        let metadata: Rc<RefCell<MetadataCache>> = Rc::new(RefCell::new(MetadataCache::default()));
        editor.update(cx, |editor, _cx| {
            editor.set_completion_provider(Some(Rc::new(SqlCompletionProvider {
                metadata: metadata.clone(),
            })));
        });

        // Metadata sweep for autocomplete, gated on `database.metadata_cache`.
        if DatabaseClientSettings::get_global(cx).metadata_cache
            && let Some(connection) = connection.clone()
        {
            cx.spawn({
                let metadata = metadata.clone();
                async move |_this, cx| {
                    let task = cx.update(|cx| connection.load_metadata(cx));
                    if let Some(loaded) = task.await.log_err() {
                        *metadata.borrow_mut() = loaded;
                    }
                }
            })
            .detach();
        }

        results_grid.update(cx, |grid, cx| grid.ensure_filter_editor(window, cx));

        let settings = DatabaseClientSettings::get_global(cx);
        let read_only =
            settings.read_only || profile.as_ref().is_some_and(|profile| profile.read_only);
        let commit_mode = CommitMode::from_stored_str(&settings.commit_mode);

        QueryView {
            editor,
            connection,
            profile,
            result: None,
            error: None,
            elapsed_ms: None,
            run_generation: 0,
            running: false,
            page: 0,
            sort: None,
            applied_sort: None,
            filter_expr: None,
            applied_filter: None,
            session_page_size: crate::settings::DatabaseClientSettings::get_global(cx).page_size,
            has_more: false,
            last_sql: None,
            needs_serialize: false,
            editor_ratio: DEFAULT_EDITOR_RATIO,
            visible_editor_ratio: DEFAULT_EDITOR_RATIO,
            label: None,
            last_run_label: None,
            read_only,
            editability: None,
            commit_mode,
            session: None,
            pending_commit_mode_switch: None,
            results_grid,
            value_viewer: cx.new(|_| ValueViewer::new()),
            value_viewer_visible: false,
            metadata,
            workspace,
            workspace_id,
            _editor_subscription: editor_subscription,
            _grid_subscription: grid_subscription,
        }
    }

    pub fn open(
        workspace: &mut Workspace,
        profile: Option<ConnectionProfile>,
        connection: Option<Connection>,
        initial_sql: String,
        label: Option<String>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.entity().downgrade();
        let workspace_id = workspace.database_id();
        let label = label.map(SharedString::from);
        let view = cx.new(|cx| {
            let mut view = QueryView::new(
                profile,
                connection,
                initial_sql,
                weak,
                workspace_id,
                window,
                cx,
            );
            view.label = label;
            view
        });
        workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
    }

    /// Shared run entry: guards blank SQL and the missing-connection case, resets a
    /// sort tied to a now-stale result, records `last_sql`, and dispatches page 0.
    fn run_sql(&mut self, sql: String, cx: &mut Context<Self>) {
        if sql.trim().is_empty() {
            return;
        }

        if self.connection.is_none() && self.profile.is_none() {
            self.error =
                Some("No connection — open this query from a table in the Database panel".into());
            cx.notify();
            return;
        }

        // An edited statement invalidates a sort tied to the old result's columns.
        if self.last_sql.as_deref() != Some(sql.as_str()) {
            self.sort = None;
        }
        // A Run/RunScript always dispatches the raw statement — never through
        // `effective_base_sql()` — so an active filter can never be re-applied
        // here. Clear it unconditionally (even on a same-SQL re-run): leaving
        // it set would let `finish_query` claim `applied_filter` for rows that
        // were never actually filtered.
        self.filter_expr = None;
        self.applied_filter = None;
        self.results_grid.update(cx, |grid, cx| {
            grid.set_filter_text("", cx);
            grid.set_filter_error(None, cx);
        });
        self.last_sql = Some(sql.clone());
        self.dispatch_query(sql, 0, QueryIntent::Run, cx);
    }

    /// Runs the entire editor buffer as one batch (today's whole-buffer behavior).
    fn run_script(&mut self, _: &RunScript, _window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.editor.read(cx).text(cx);
        // A whole-buffer run has no single statement ordinal to show.
        self.last_run_label = None;
        self.run_sql(sql, cx);
    }

    /// Loads `sql` into the editor, replacing its contents. When `run` is true,
    /// dispatches the query immediately (Cmd/Ctrl-Enter behavior from the
    /// history picker); otherwise it just seeds the editor for the user to run.
    pub fn load_sql(
        &mut self,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor
            .update(cx, |editor, cx| editor.set_text(sql.as_str(), window, cx));
        if run {
            self.run_script(&RunScript, window, cx);
        }
    }

    /// Runs the SQL statement under the primary caret (⌘↵). Selection handling is
    /// added in Task 5; here a collapsed caret resolves via `statement_at`.
    fn run_statement(&mut self, _: &RunStatement, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text(cx);
        let selection = self.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            editor
                .selections
                .newest::<editor::MultiBufferOffset>(&snapshot)
        });
        let range = selection.range();
        let statement_range = if range.start.0 != range.end.0 {
            // A non-empty selection runs verbatim (DataGrip "run selection").
            Some(crate::sql_statements::StatementRange {
                start: range.start.0,
                end: range.end.0,
            })
        } else {
            // A collapsed caret runs the enclosing statement.
            crate::sql_statements::statement_at(&text, selection.head().0)
        };
        let Some(statement_range) = statement_range else {
            return;
        };
        let Some(sql) = text
            .get(statement_range.start..statement_range.end)
            .map(str::to_string)
        else {
            return;
        };
        self.highlight_active_statement(statement_range, cx);
        self.run_sql(sql, cx);
    }

    /// Paints a subtle gutter+line highlight over the statement currently running,
    /// and records a "stmt N/M" toolbar label. A raw (non-statement-aligned)
    /// selection doesn't correspond to a single split statement, so it gets no
    /// ordinal label.
    fn highlight_active_statement(
        &mut self,
        range: crate::sql_statements::StatementRange,
        cx: &mut Context<Self>,
    ) {
        let text = self.editor.read(cx).text(cx);
        let statements = crate::sql_statements::split(&text);
        let position = statements
            .iter()
            .position(|statement| statement.start == range.start && statement.end == range.end);
        self.last_run_label = position
            .map(|index| SharedString::from(format!("stmt {}/{}", index + 1, statements.len())));
        self.editor.update(cx, |editor, cx| {
            editor.clear_row_highlights::<ActiveStatement>();
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let anchors = snapshot.anchor_after(editor::MultiBufferOffset(range.start))
                ..snapshot.anchor_before(editor::MultiBufferOffset(range.end));
            editor.highlight_rows::<ActiveStatement>(
                anchors,
                |cx| cx.theme().colors().editor_document_highlight_read_background,
                RowHighlightOptions {
                    include_gutter: true,
                    autoscroll: false,
                },
                cx,
            );
        });
    }

    /// Clears the running-statement row highlight once its query settles
    /// (success or error).
    fn clear_active_statement(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, _cx| {
            editor.clear_row_highlights::<ActiveStatement>()
        });
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.running || !self.has_more {
            return;
        }
        if self.last_sql.is_none() {
            return;
        }
        // Filter-aware: keep paging the filtered SQL once a filter is applied.
        let sql = self.effective_base_sql();
        self.dispatch_query(sql, self.page + 1, QueryIntent::LoadMore, cx);
    }

    /// The base statement with the active WHERE filter applied (still
    /// unwrapped by paging). `build_page_request` wraps this for the actual
    /// fetch. Falls back to the plain base SQL when there is no filter, the
    /// filter is blank, or the base isn't wrappable.
    fn effective_base_sql(&self) -> String {
        let base = self.last_sql.clone().unwrap_or_default();
        match &self.filter_expr {
            Some(expr) if !expr.trim().is_empty() && crate::sql_paging::wrappable(&base) => {
                crate::sql_paging::filter_subquery(&base, expr)
            }
            _ => base,
        }
    }

    /// Handles `ResultsGridEvent::FilterRequested`: validates the base SQL is
    /// wrappable, stores the (possibly cleared) filter, syncs the filter bar,
    /// and re-queries page 0 (preserving `sort`).
    fn apply_filter_request(&mut self, expr: String, cx: &mut Context<Self>) {
        let Some(base) = self.last_sql.clone() else {
            return;
        };
        if !crate::sql_paging::wrappable(&base) {
            self.results_grid.update(cx, |grid, cx| {
                grid.set_filter_error(
                    Some("Filtering needs a single wrappable SELECT.".into()),
                    cx,
                );
            });
            return;
        }
        let trimmed = expr.trim();
        self.filter_expr = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self.results_grid.update(cx, |grid, cx| {
            grid.set_filter_text(&expr, cx);
            grid.set_filter_error(None, cx);
        });
        let effective = self.effective_base_sql();
        self.dispatch_query(effective, 0, QueryIntent::Filter, cx);
    }

    /// Handles `ResultsGridEvent::CellSelected`: pushes the selected cell's
    /// value into the value viewer.
    fn on_cell_selected(
        &mut self,
        value: Option<String>,
        data_type: String,
        cx: &mut Context<Self>,
    ) {
        self.value_viewer
            .update(cx, |viewer, cx| viewer.set_value(value, data_type, cx));
    }

    fn set_commit_mode(&mut self, mode: CommitMode, cx: &mut Context<Self>) {
        if self.commit_mode == mode {
            return;
        }
        self.commit_mode = mode;
        self.needs_serialize = true;
        cx.emit(QueryViewEvent::Edit);
        cx.notify();
    }

    /// Leaving Manual mode with work in an open transaction must not silently
    /// drop it: defer the switch and prompt to Commit or Rollback first.
    fn request_commit_mode_change(&mut self, mode: CommitMode, cx: &mut Context<Self>) {
        if self.commit_mode == CommitMode::Manual
            && mode == CommitMode::Auto
            && self.transaction_is_open()
        {
            self.pending_commit_mode_switch = Some(mode);
            cx.notify();
            return;
        }
        self.set_commit_mode(mode, cx);
    }

    /// The mode flip must only happen once the async COMMIT actually
    /// succeeds: `TransactionSession.conn` is a clone of the tab's shared
    /// connection, so dropping the session on a failed COMMIT would silently
    /// leave the physical connection sitting in an open/aborted transaction
    /// that a later Auto-mode query would then reuse.
    fn resolve_commit_mode_switch_with_commit(&mut self, cx: &mut Context<Self>) {
        self.resolve_pending_commit_mode_switch(true, cx);
    }

    /// Same success-gating as commit: a failed ROLLBACK usually means a dead
    /// connection, so it must surface the error and keep Manual rather than
    /// silently flipping to Auto over a connection that may still be mid-transaction.
    fn resolve_commit_mode_switch_with_rollback(&mut self, cx: &mut Context<Self>) {
        self.resolve_pending_commit_mode_switch(false, cx);
    }

    fn resolve_pending_commit_mode_switch(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            self.finish_pending_commit_mode_switch(cx);
            return;
        };
        if !session.is_open() {
            self.finish_pending_commit_mode_switch(cx);
            return;
        }
        let task = if commit {
            session.commit(cx)
        } else {
            session.rollback(cx)
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.finish_pending_commit_mode_switch(cx);
                }
                Err(error) => {
                    // Keep Manual and the prompt visible so the user can
                    // retry Commit/Rollback or Cancel instead of losing the
                    // open transaction silently.
                    this.error = Some(error.to_string());
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
        cx.notify();
    }

    fn cancel_commit_mode_switch(&mut self, cx: &mut Context<Self>) {
        self.pending_commit_mode_switch = None;
        cx.notify();
    }

    fn finish_pending_commit_mode_switch(&mut self, cx: &mut Context<Self>) {
        if let Some(mode) = self.pending_commit_mode_switch.take() {
            self.session = None;
            self.set_commit_mode(mode, cx);
        }
    }

    fn commit_transaction(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !session.is_open() {
            return;
        }
        let task = session.commit(cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.error = Some(error.to_string());
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
        cx.notify();
    }

    fn rollback_transaction(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if !session.is_open() {
            return;
        }
        let task = session.rollback(cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.error = Some(error.to_string());
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
        cx.notify();
    }

    fn transaction_is_open(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.is_open())
    }

    /// In Manual mode, lazily allocate a `TransactionSession` over a dedicated
    /// clone of the tab's connection and begin its transaction. No-op if the
    /// tab has no live connection yet. Returns the in-flight `BEGIN` task so
    /// callers that must not run statements before the transaction is
    /// actually open (like `dispatch_query`) can await it; when a
    /// transaction is already open this resolves immediately with `Ok(())`
    /// since there is nothing left to wait for.
    fn ensure_session_open(&mut self, cx: &mut Context<Self>) -> Option<Task<anyhow::Result<()>>> {
        if self.commit_mode != CommitMode::Manual {
            return None;
        }
        if self.transaction_is_open() {
            return Some(Task::ready(Ok(())));
        }
        let Some(connection) = self.connection.clone() else {
            return None;
        };
        let mut session = TransactionSession::new(connection);
        let begin_task = session.begin(cx);
        self.session = Some(session);
        cx.notify();
        Some(begin_task)
    }

    /// The connection statements should run on for the current mode: the
    /// transaction-bound connection in Manual mode with an open session,
    /// otherwise the tab's plain connection.
    fn manual_execute_connection(&self) -> Option<Connection> {
        match self.commit_mode {
            CommitMode::Manual => self
                .session
                .as_ref()
                .filter(|session| session.is_open())
                .map(|session| session.connection()),
            CommitMode::Auto => None,
        }
    }

    /// Discards all pending grid edits without touching the database.
    fn revert_edits(&mut self, cx: &mut Context<Self>) {
        self.results_grid
            .update(cx, |grid, cx| grid.clear_pending(cx));
    }

    /// The effective read-only state for this tab: the global
    /// `database.read_only` setting OR'd with the active connection
    /// profile's own `read_only` flag. Either source disables editing.
    fn effective_read_only(&self, cx: &App) -> bool {
        DatabaseClientSettings::get_global(cx).read_only
            || self
                .profile
                .as_ref()
                .is_some_and(|profile| profile.read_only)
    }

    /// Submits the grid's pending edits as DML, gated on
    /// `database.confirm_edits`: when set (the default) the user is prompted
    /// before anything runs; otherwise [`execute_submit_edits`](Self::execute_submit_edits)
    /// runs immediately, mirroring the pre-setting behavior.
    fn submit_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.running {
            return;
        }
        if self.editability.is_none() || self.result.is_none() {
            return;
        }
        if !DatabaseClientSettings::get_global(cx).confirm_edits {
            self.execute_submit_edits(cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Submit changes to the database?",
            None,
            &["Submit", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.execute_submit_edits(cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Runs the actual DML for [`submit_edits`](Self::submit_edits) once any
    /// confirmation has been resolved. In **Auto** commit mode this
    /// builds the ordered `BEGIN;…;COMMIT;` batch via
    /// [`build_edit_transaction`] and runs it atomically on the tab's plain
    /// connection — unchanged from before Manual-mode routing existed. In
    /// **Manual** mode the statements from [`build_edit_statements`] run
    /// unwrapped (no `BEGIN`/`COMMIT` of their own) on the open session's
    /// connection, so they land inside the user's in-progress transaction
    /// instead of autocommitting; the user commits or rolls back later via
    /// the toolbar. A failure leaves `PendingChanges` staged and surfaces the
    /// Postgres error; success clears the staged edits and refreshes the
    /// current page by re-running `last_sql`.
    fn execute_submit_edits(&mut self, cx: &mut Context<Self>) {
        if self.read_only || self.running {
            return;
        }
        let Some(editability) = self.editability.clone() else {
            return;
        };
        let Some(result) = self.result.clone() else {
            return;
        };
        let pending = self.results_grid.read(cx).pending_changes().clone();

        match self.commit_mode {
            CommitMode::Auto => {
                let Some(connection) = self.connection.clone() else {
                    self.error = Some("No live connection for edit submit".into());
                    cx.notify();
                    return;
                };
                let Some(sql) =
                    build_edit_transaction(&editability, &pending, &result.columns, &result.rows)
                else {
                    return;
                };

                self.running = true;
                cx.notify();
                // Filter-aware: an active filter must survive the post-commit
                // refresh, so this goes through the same `effective_base_sql()`
                // path `load_more`/`SortRequested` already use rather than
                // `last_sql` directly — one application of the filter, no
                // double-wrapping.
                let refresh_sql = self.effective_base_sql();
                let page = self.page;
                cx.spawn(async move |this, cx| {
                    // `execute` runs the whole `BEGIN;…;COMMIT;` batch as one
                    // simple_query; any error aborts the transaction
                    // server-side (implicit ROLLBACK), so nothing in it takes
                    // effect.
                    let execute_task =
                        cx.update(|cx| connection.execute(sql, DEFAULT_RESULT_LIMIT, cx));
                    let result = execute_task.await;
                    this.update(cx, |this, cx| {
                        this.running = false;
                        match result {
                            Ok(_) => {
                                // Success: the staged changes are now
                                // authoritative on the server — clear them
                                // and refresh the current page to show the
                                // committed effects.
                                this.results_grid
                                    .update(cx, |grid, cx| grid.clear_pending(cx));
                                this.error = None;
                                if this.last_sql.is_some() {
                                    this.dispatch_query(refresh_sql, page, QueryIntent::Sort, cx);
                                }
                            }
                            Err(err) => {
                                // Rollback already happened; keep
                                // PendingChanges so the user can fix and
                                // retry, and surface the Postgres message.
                                this.error = Some(err.to_string());
                                cx.emit(QueryViewEvent::ResultsChanged);
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }
            CommitMode::Manual => {
                let Some(statements) =
                    build_edit_statements(&editability, &pending, &result.columns, &result.rows)
                else {
                    return;
                };
                // Bare DML only — no BEGIN/COMMIT of its own — so the
                // statements join the already-open Manual transaction rather
                // than opening/closing one of their own.
                let sql = statements.join(";\n");

                self.running = true;
                cx.notify();
                // Filter-aware, same as the Auto branch above: refresh through
                // `effective_base_sql()` so an active filter is preserved
                // (single application — mirrors `load_more`/`SortRequested`).
                let refresh_sql = self.effective_base_sql();
                let page = self.page;
                // `BEGIN` is chained first, mirroring `dispatch_query`, so the
                // statements never race ahead of the session's transaction
                // actually opening.
                let begin_task = self.ensure_session_open(cx);
                cx.spawn(async move |this, cx| {
                    if let Some(begin_task) = begin_task {
                        if let Err(error) = begin_task.await {
                            this.update(cx, |this, cx| {
                                this.running = false;
                                this.error = Some(error.to_string());
                                cx.emit(QueryViewEvent::ResultsChanged);
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    }
                    let connection = this
                        .update(cx, |this, _cx| this.manual_execute_connection())
                        .ok()
                        .flatten();
                    let Some(connection) = connection else {
                        this.update(cx, |this, cx| {
                            this.running = false;
                            this.error = Some("No open transaction for edit submit".into());
                            cx.emit(QueryViewEvent::ResultsChanged);
                            cx.notify();
                        })
                        .ok();
                        return;
                    };
                    let execute_task =
                        cx.update(|cx| connection.execute(sql, DEFAULT_RESULT_LIMIT, cx));
                    let result = execute_task.await;
                    this.update(cx, |this, cx| {
                        this.running = false;
                        match result {
                            Ok(_) => {
                                // Success: the edits are now staged inside the
                                // open Manual transaction — clear the pending
                                // set and refresh the current page, which
                                // routes through the same session so it shows
                                // the in-transaction (uncommitted) state.
                                this.results_grid
                                    .update(cx, |grid, cx| grid.clear_pending(cx));
                                this.error = None;
                                if this.last_sql.is_some() {
                                    this.dispatch_query(refresh_sql, page, QueryIntent::Sort, cx);
                                }
                            }
                            Err(err) => {
                                // The transaction is now in an aborted state
                                // server-side; keep PendingChanges so the
                                // user can fix and retry after a Rollback,
                                // and surface the Postgres message.
                                this.error = Some(err.to_string());
                                cx.emit(QueryViewEvent::ResultsChanged);
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Exports `self.result` (whatever is currently displayed) to `format`,
    /// either onto the clipboard or via a save-file dialog. Purely operates
    /// on already-fetched data — never re-runs the query.
    fn export_current(
        &mut self,
        format: ExportFormat,
        to_clipboard: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self.result.clone() else {
            return;
        };
        let null_string = DatabaseClientSettings::get_global(cx)
            .export_null_string
            .clone();
        let payload = export_payload(&result, format, &null_string);
        if to_clipboard {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(payload));
            return;
        }
        let suggested = match format {
            ExportFormat::Csv => "export.csv",
            ExportFormat::Json => "export.json",
            ExportFormat::Sql => "export.sql",
        };
        let directory = std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from(""));
        let save = cx.prompt_for_new_path(&directory, Some(suggested));
        cx.background_spawn(async move {
            let path = save
                .await
                .log_err()
                .and_then(|inner| inner.log_err())
                .flatten();
            let Some(path) = path else {
                return;
            };
            if let Err(error) = std::fs::write(&path, payload) {
                log::error!("export: writing {path:?} failed: {error:#}");
            }
        })
        .detach();
    }

    /// Runs `sql` for the given page. [`QueryIntent::LoadMore`] extends the
    /// accumulated rows; the other intents replace the result set (page 0 of
    /// a session).
    fn dispatch_query(
        &mut self,
        sql: String,
        page: usize,
        intent: QueryIntent,
        cx: &mut Context<Self>,
    ) {
        self.run_generation += 1;
        let generation_id = self.run_generation;
        self.running = true;
        cx.notify();

        let page_size = if intent == QueryIntent::LoadMore {
            // Offsets are `page * page_size`: later pages must use the size the
            // session started with even if the setting changed since.
            self.session_page_size
        } else {
            let page_size = crate::settings::DatabaseClientSettings::get_global(cx).page_size;
            self.session_page_size = page_size;
            page_size
        };
        let (effective_sql, fetch_limit, paged) =
            build_page_request(sql, self.sort.as_ref(), page, page_size);

        let start = Instant::now();
        // Manual mode: lazily open the transaction session so this statement lands
        // inside the open transaction rather than autocommitting. `BEGIN` is async
        // and only flips `is_open()` once it actually succeeds, so the task below
        // is awaited before anything is executed — `manual_execute_connection`
        // would otherwise still see the session as closed and fall back to the
        // plain connection, racing the `BEGIN` on the wire.
        let begin_task = if self.commit_mode == CommitMode::Manual && self.connection.is_some() {
            self.ensure_session_open(cx)
        } else {
            None
        };
        let commit_mode = self.commit_mode;
        let existing_connection = self.connection.clone();
        let profile = self.profile.clone();

        cx.spawn(async move |this, cx| {
            if let Some(begin_task) = begin_task {
                if let Err(error) = begin_task.await {
                    this.update(cx, |this, cx| {
                        if this.run_generation != generation_id {
                            return;
                        }
                        this.running = false;
                        this.error = Some(error.to_string());
                        cx.emit(QueryViewEvent::ResultsChanged);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            // Prefer the transaction-bound connection once `BEGIN` has landed;
            // otherwise use the live connection if present, or lazily connect using
            // the stored profile. Lazy-connect covers query tabs restored after
            // restart: they have a profile but no live connection, so the first Run
            // establishes one transparently.
            let manual_connection = this
                .update(cx, |this, _cx| this.manual_execute_connection())
                .ok()
                .flatten();
            let reused_connection = manual_connection.or(existing_connection);
            // A restored tab has a profile but no live connection, so `begin_task`
            // above was never created (there was nothing to `BEGIN` on yet). If
            // that's the case here, the session still needs to be opened once the
            // lazy connect below succeeds — otherwise this first statement would
            // run in autocommit despite Manual mode.
            let needs_lazy_session_open =
                commit_mode == CommitMode::Manual && reused_connection.is_none();
            let connection = if let Some(conn) = reused_connection {
                Ok(conn)
            } else {
                let Some(profile) = profile else {
                    return;
                };
                // The keychain key embeds the database name (see
                // `ConnectionProfile::keychain_url`), so a tab profile pointing at a
                // non-default database would miss the stored credential. Look up the
                // base profile (as saved in connections.json) for the password read,
                // but connect with the tab's profile so the right database is used.
                let base_profile = store::load_profiles()
                    .into_iter()
                    .find(|p| p.id == profile.id)
                    .unwrap_or_else(|| profile.clone());
                let password = cx
                    .update(|cx| store::read_password(cx, &base_profile))
                    .await
                    .ok()
                    .flatten();
                cx.update(|cx| Connection::connect(profile, password, cx))
                    .await
            };

            let connection = match connection {
                Ok(conn) => conn,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        if this.run_generation != generation_id {
                            return;
                        }
                        this.running = false;
                        this.error = Some(err.to_string());
                        cx.emit(QueryViewEvent::ResultsChanged);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            // The connection just lazily established above has no transaction on
            // it yet: open the Manual session now, before the user's statement
            // runs, and await `BEGIN` the same way the top-of-function path does
            // for tabs that were already connected.
            let execute_connection = if needs_lazy_session_open {
                let lazy_begin_task = this
                    .update(cx, |this, cx| {
                        if this.connection.is_none() {
                            this.connection = Some(connection.clone());
                        }
                        this.ensure_session_open(cx)
                    })
                    .ok()
                    .flatten();
                if let Some(begin_task) = lazy_begin_task {
                    if let Err(error) = begin_task.await {
                        this.update(cx, |this, cx| {
                            if this.run_generation != generation_id {
                                return;
                            }
                            this.running = false;
                            this.error = Some(error.to_string());
                            cx.emit(QueryViewEvent::ResultsChanged);
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                }
                this.update(cx, |this, _cx| this.manual_execute_connection())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| connection.clone())
            } else {
                connection.clone()
            };

            let execute_task =
                cx.update(|cx| execute_connection.execute(effective_sql, fetch_limit, cx));
            let outcome = execute_task.await;
            let elapsed = start.elapsed().as_millis();

            this.update(cx, |this, cx| {
                // Discard result if a newer query has been dispatched in the meantime.
                if this.run_generation != generation_id {
                    return;
                }
                this.running = false;
                // Cache a lazily-established connection so subsequent runs reuse it.
                if this.connection.is_none() {
                    this.connection = Some(connection);
                }
                this.finish_query(intent, paged, page, page_size, outcome, elapsed, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Applies a completed query's outcome. The caller has already verified
    /// the generation and cleared `running`.
    fn finish_query(
        &mut self,
        intent: QueryIntent,
        paged: bool,
        page: usize,
        page_size: usize,
        outcome: anyhow::Result<QueryResult>,
        elapsed: u128,
        cx: &mut Context<Self>,
    ) {
        self.elapsed_ms = Some(elapsed);
        if intent == QueryIntent::Run {
            if let Some(sql) = self.last_sql.clone() {
                self.record_history(sql, &outcome, elapsed, cx);
            }
        }
        match outcome {
            Ok(mut result) => {
                // The extra fetched row only signals another page.
                if paged {
                    let rows = std::mem::take(&mut result.rows);
                    let (rows, has_more) = truncate_rows(rows, page_size);
                    result.rows = rows;
                    self.has_more = has_more;
                } else {
                    self.has_more = false;
                }
                self.page = page;
                match (intent == QueryIntent::LoadMore, self.result.as_mut()) {
                    (true, Some(existing)) => existing.rows.extend(result.rows),
                    _ => self.result = Some(result),
                }
                self.error = None;
                self.applied_sort = self.sort.clone();
                self.applied_filter = self.filter_expr.clone();
                // The grid holds its own clone of the displayed result —
                // push the MERGED rows and the confirmed sort into it, or
                // the grid never shows new/sorted/appended rows and the
                // header chevron never updates.
                let merged = self.result.clone();
                let sort = self.sort.clone();
                self.results_grid.update(cx, |grid, cx| {
                    grid.set_result(merged, paged, cx);
                    grid.set_sort(sort, cx);
                });
                // A fresh Run may point at a different table (or none); Sort
                // and LoadMore re-query the same table, so keep whatever
                // editability was already established for it.
                if intent == QueryIntent::Run {
                    self.editability = self.result.as_ref().and_then(|result| {
                        detect_editability(
                            self.last_sql.as_deref().unwrap_or_default(),
                            &self.metadata.borrow(),
                            &result.columns,
                        )
                    });
                }
                let editability = self.editability.clone();
                let read_only = self.effective_read_only(cx);
                self.read_only = read_only;
                self.results_grid.update(cx, |grid, cx| {
                    grid.set_editability(editability, read_only, cx)
                });
            }
            Err(err) => {
                self.error = Some(err.to_string());
                match intent {
                    QueryIntent::Run => {
                        self.result = None;
                        self.has_more = false;
                        self.sort = None;
                        self.applied_sort = None;
                        self.filter_expr = None;
                        self.applied_filter = None;
                        self.editability = None;
                        let read_only = self.effective_read_only(cx);
                        self.read_only = read_only;
                        self.results_grid.update(cx, |grid, cx| {
                            grid.set_result(None, false, cx);
                            grid.set_editability(None, read_only, cx);
                        });
                    }
                    QueryIntent::LoadMore | QueryIntent::Sort => {
                        // Keep the rows the user is looking at; revert to the
                        // sort they actually reflect so a failed request never
                        // sticks and re-fails every subsequent Run.
                        self.sort = self.applied_sort.clone();
                    }
                    QueryIntent::Filter => {
                        // Keep the rows the user sees; revert the requested
                        // filter to the one those rows reflect so a bad
                        // expression never sticks.
                        self.filter_expr = self.applied_filter.clone();
                        let error = self.error.clone();
                        self.results_grid.update(cx, |grid, cx| {
                            grid.set_filter_error(error, cx);
                        });
                    }
                }
            }
        }
        // The query settled (either way) — the active-statement highlight no
        // longer reflects anything running.
        self.clear_active_statement(cx);
        cx.emit(QueryViewEvent::ResultsChanged);
        cx.notify();
    }

    fn on_split_drag_move(
        &mut self,
        drag_event: &DragMoveEvent<DraggedQuerySplitHandle>,
        cx: &mut Context<Self>,
    ) {
        if drag_event.drag(cx).view != cx.entity_id() {
            return;
        }
        let bounds = drag_event.bounds;
        // The editor/results flex fractions distribute the container height
        // minus its only fixed-height row, the 1px divider. Computing the
        // ratio over that shared space keeps the divider under the cursor.
        let shared_height = bounds.bottom() - bounds.top() - px(1.);
        if shared_height <= px(0.) {
            return;
        }
        let new_ratio = (drag_event.event.position.y - bounds.top()) / shared_height;
        self.visible_editor_ratio = clamp_editor_ratio(new_ratio);
        cx.notify();
    }

    fn commit_split_ratio(&mut self, cx: &mut Context<Self>) {
        if self.editor_ratio != self.visible_editor_ratio {
            self.editor_ratio = self.visible_editor_ratio;
            self.needs_serialize = true;
            cx.emit(QueryViewEvent::Edit);
        }
        cx.notify();
    }

    /// Persists a completed user-initiated run to the query history, then prunes
    /// old entries. Fire-and-forget: history failures never block the UI.
    fn record_history(
        &self,
        sql: String,
        outcome: &anyhow::Result<QueryResult>,
        elapsed: u128,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace_id) = self.workspace_id else {
            return;
        };
        let Some(profile) = self.profile.as_ref() else {
            return;
        };
        let executed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed_since_epoch| elapsed_since_epoch.as_millis() as i64)
            .unwrap_or_default();
        let (succeeded, error, row_count) = match outcome {
            Ok(result) => (true, None, result.rows.len() as i64),
            Err(error) => (false, Some(error.to_string()), 0),
        };
        let entry = HistoryEntry {
            id: 0,
            profile_id: profile.id.clone(),
            database: Some(profile.database.clone()),
            sql,
            executed_at,
            duration_ms: elapsed as i64,
            succeeded,
            error,
            row_count,
        };
        let history_limit = DatabaseClientSettings::get_global(cx).history_limit;
        let history_db = HistoryDb::global(cx);
        cx.background_spawn(async move {
            history_db.record(workspace_id, entry).await?;
            history_db.prune(workspace_id, history_limit).await?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn reset_split_ratio(&mut self, cx: &mut Context<Self>) {
        self.editor_ratio = DEFAULT_EDITOR_RATIO;
        self.visible_editor_ratio = DEFAULT_EDITOR_RATIO;
        self.needs_serialize = true;
        cx.emit(QueryViewEvent::Edit);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn profile(&self) -> Option<&ConnectionProfile> {
        self.profile.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    fn editor_style(cx: &App) -> EditorStyle {
        let settings = ThemeSettings::get_global(cx);
        let theme = cx.theme();
        let text_style = TextStyle {
            color: theme.colors().text,
            font_family: settings.buffer_font.family.clone(),
            font_features: settings.buffer_font.features.clone(),
            font_fallbacks: settings.buffer_font.fallbacks.clone(),
            font_size: settings.buffer_font_size(cx).into(),
            font_weight: settings.buffer_font.weight,
            line_height: relative(settings.buffer_line_height.value()),
            ..Default::default()
        };
        EditorStyle {
            background: theme.colors().editor_background,
            local_player: theme.players().local(),
            text: text_style,
            syntax: theme.syntax().clone(),
            ..Default::default()
        }
    }
}

/// Non-LSP completion provider backed by the connection's schema metadata.
/// Thin adapter: all logic lives in `classify_context` + `candidates`.
struct SqlCompletionProvider {
    metadata: Rc<RefCell<MetadataCache>>,
}

fn candidate_label(candidate: &Candidate) -> CodeLabel {
    CodeLabel::plain(candidate.text.clone(), None)
}

fn candidate_detail(candidate: &Candidate) -> Option<String> {
    candidate.detail.clone()
}

impl SqlCompletionProvider {
    fn replace_range_for_completion(
        buffer_text: &str,
        buffer_position: LanguageAnchor,
        new_bytes: &[u8],
        snapshot: &TextBufferSnapshot,
    ) -> std::ops::Range<LanguageAnchor> {
        let buffer_offset = buffer_position.to_offset(snapshot);
        let buffer_bytes = &buffer_text.as_bytes()[0..buffer_offset];
        let mut prefix_len = 0;
        for i in (0..new_bytes.len()).rev() {
            if buffer_bytes.ends_with(&new_bytes[0..i]) {
                prefix_len = i;
                break;
            }
        }
        let start = snapshot.clip_offset(buffer_offset - prefix_len, Bias::Left);
        snapshot.anchor_before(start)..buffer_position
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: LanguageAnchor,
        _trigger: editor::CompletionContext,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<Vec<CompletionResponse>>> {
        let snapshot = buffer.read(cx).text_snapshot();
        let buffer_text = snapshot.text();
        let offset = buffer_position.to_offset(&snapshot);
        let before = buffer_text.get(..offset).unwrap_or("").to_string();
        let context = classify_context(&before);
        // The partial word under the caret.
        let prefix: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let candidate_list = candidates(&self.metadata.borrow(), &context, &prefix, &buffer_text);

        let completions = candidate_list
            .into_iter()
            .map(|candidate| {
                let new_text = candidate.text.clone();
                Completion {
                    replace_range: Self::replace_range_for_completion(
                        &buffer_text,
                        buffer_position,
                        new_text.as_bytes(),
                        &snapshot,
                    ),
                    new_text,
                    label: candidate_label(&candidate),
                    icon_path: None,
                    icon_color: None,
                    documentation: candidate_detail(&candidate)
                        .map(|detail| CompletionDocumentation::SingleLine(detail.into())),
                    match_start: None,
                    snippet_deduplication_key: None,
                    confirm: None,
                    source: CompletionSource::Custom,
                    insert_text_mode: None,
                    group: None,
                }
            })
            .collect::<Vec<_>>();

        Task::ready(Ok(vec![CompletionResponse {
            completions,
            display_options: CompletionDisplayOptions::default(),
            is_incomplete: false,
        }]))
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: LanguageAnchor,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        let Some(char) = text.chars().next() else {
            return false;
        };
        if char == '.' {
            return true;
        }
        let snapshot = buffer.read(cx).snapshot();
        let classifier = snapshot
            .char_classifier_at(position)
            .scope_context(Some(CharScopeContext::Completion));
        trigger_in_words && classifier.is_word(char)
    }
}

impl Focusable for QueryView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<QueryViewEvent> for QueryView {}

impl Item for QueryView {
    type Event = QueryViewEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.label
            .clone()
            .or_else(|| {
                self.profile
                    .as_ref()
                    .map(|p| SharedString::from(p.name.clone()))
            })
            .unwrap_or_else(|| "Query".into())
    }

    fn to_item_events(event: &QueryViewEvent, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            QueryViewEvent::Edit => f(ItemEvent::Edit),
            QueryViewEvent::ResultsChanged => f(ItemEvent::UpdateTab),
        }
    }
}

impl SerializableItem for QueryView {
    fn serialized_item_kind() -> &'static str {
        "DatabaseQueryView"
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let db = QueryDb::global(cx);
        delete_unloaded_items(alive_items, workspace_id, "database_query_views", &db, cx)
    }

    fn serialize(
        &mut self,
        _workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<anyhow::Result<()>>> {
        if !self.needs_serialize {
            return None;
        }

        let workspace_id = self.workspace_id?;
        let profile = self.profile.as_ref()?;
        let profile_id = profile.id.clone();
        let database = Some(profile.database.clone());
        let sql = self.editor.read(cx).text(cx);
        let editor_ratio = Some(f64::from(self.editor_ratio));
        let commit_mode = Some(self.commit_mode.as_stored_str().to_string());
        self.needs_serialize = false;

        let db = QueryDb::global(cx);
        Some(cx.background_spawn(async move {
            db.save_query(
                item_id,
                workspace_id,
                profile_id,
                sql,
                database,
                editor_ratio,
                commit_mode,
            )
            .await
        }))
    }

    fn should_serialize(&self, _event: &Self::Event) -> bool {
        self.needs_serialize
    }

    fn deserialize(
        _project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Entity<Self>>> {
        let db = QueryDb::global(cx);
        let loaded = db.get_query(item_id, workspace_id).log_err().flatten();
        window.spawn(cx, async move |cx| {
            let (profile_id, sql, persisted_database, editor_ratio, commit_mode) =
                loaded.unwrap_or_default();
            let editor_ratio = editor_ratio
                .map(|ratio| clamp_editor_ratio(ratio as f32))
                .unwrap_or(DEFAULT_EDITOR_RATIO);
            // `None` here (no persisted tab, or an old row saved before the
            // column existed) must fall through to `QueryView::new`'s own
            // `commit_mode` setting default below, rather than hardcoding
            // Auto and shadowing it.
            let commit_mode = commit_mode.map(|stored| CommitMode::from_stored_str(&stored));
            let profile = store::load_profiles()
                .into_iter()
                .find(|p| p.id == profile_id)
                .map(|mut p| {
                    if let Some(database) = persisted_database {
                        if database != p.database {
                            p.database = database;
                        }
                    }
                    p
                });
            cx.update(|window, cx| {
                Ok(cx.new(|cx| {
                    let mut view = QueryView::new(
                        profile,
                        None,
                        sql,
                        workspace.clone(),
                        Some(workspace_id),
                        window,
                        cx,
                    );
                    view.editor_ratio = editor_ratio;
                    view.visible_editor_ratio = editor_ratio;
                    if let Some(commit_mode) = commit_mode {
                        view.commit_mode = commit_mode;
                    }
                    view
                }))
            })?
        })
    }
}

impl Render for QueryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor_style = Self::editor_style(cx);

        let profile_name: SharedString = self
            .profile
            .as_ref()
            .map(|p| SharedString::from(p.name.clone()))
            .unwrap_or_else(|| "No Connection".into());
        let database_name: Option<SharedString> = self
            .profile
            .as_ref()
            .map(|p| SharedString::from(p.database.clone()));

        let status_color = if self.error.is_some() {
            Color::Error
        } else if self.connection.is_some() {
            Color::Success
        } else {
            Color::Muted
        };

        // The session's snapshot, not the live setting: it is what the fetch
        // actually used, so the label stays truthful if the setting changed.
        let page_size = self.session_page_size;
        let non_wrappable_truncated = self.result.as_ref().is_some_and(|r| r.truncated);

        let toolbar_settings = DatabaseClientSettings::get_global(cx).toolbar;
        let ui_font_size = f32::from(ThemeSettings::get_global(cx).ui_font_size(cx));
        let toolbar_rem_size = toolbar_settings.font_size.unwrap_or(ui_font_size);
        let toolbar_icon_size = toolbar_settings
            .icon_size
            .map(|size| ui::bar_icon_size(size, toolbar_rem_size))
            .unwrap_or(IconSize::Small);
        let toolbar_padding = toolbar_settings
            .padding
            .map(|padding| px(ui::clamp_bar_padding(padding)));

        let grid_state = self.results_grid.read(cx);
        let show_edit_controls = grid_state.is_editable() && !self.read_only;
        let pending_count = grid_state.pending_changes().count();

        let can_export = self
            .result
            .as_ref()
            .is_some_and(|result| !result.columns.is_empty());
        let export_entity = cx.weak_entity();
        let export_menu = PopoverMenu::new("export-menu")
            .trigger_with_tooltip(
                Button::new("export-trigger", "Export")
                    .label_size(LabelSize::Small)
                    .disabled(!can_export)
                    .end_icon(
                        Icon::new(IconName::ChevronDown)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
                Tooltip::text("Export results"),
            )
            .menu(move |window, cx| {
                let export_entity = export_entity.clone();
                Some(ContextMenu::build(window, cx, move |menu, _window, _cx| {
                    let mut menu = menu;
                    for (label, format, to_clipboard) in [
                        ("Export as CSV", ExportFormat::Csv, false),
                        ("Export as JSON", ExportFormat::Json, false),
                        ("Export as SQL", ExportFormat::Sql, false),
                        ("Copy as CSV", ExportFormat::Csv, true),
                        ("Copy as JSON", ExportFormat::Json, true),
                        ("Copy as SQL", ExportFormat::Sql, true),
                    ] {
                        let export_entity = export_entity.clone();
                        menu =
                            menu.item(ContextMenuEntry::new(label).handler(move |window, cx| {
                                export_entity
                                    .update(cx, |this, cx| {
                                        this.export_current(format, to_clipboard, window, cx)
                                    })
                                    .log_err();
                            }));
                    }
                    menu
                }))
            });

        let run_button = IconButton::new("run-query", IconName::PlayFilled)
            .shape(IconButtonShape::Square)
            .icon_size(toolbar_icon_size)
            .icon_color(Color::Success)
            .disabled(self.running)
            .tooltip(Tooltip::text("Run Script (⌘⇧↵)"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.run_script(&RunScript, window, cx);
            }));

        let value_viewer_toggle = IconButton::new("toggle-value-viewer", IconName::Eye)
            .shape(IconButtonShape::Square)
            .icon_size(toolbar_icon_size)
            .toggle_state(self.value_viewer_visible)
            .tooltip(Tooltip::text("Toggle value viewer"))
            .on_click(cx.listener(|this, _, _window, cx| {
                this.value_viewer_visible = !this.value_viewer_visible;
                cx.notify();
            }));

        let toolbar_content = h_flex()
            .px_2()
            .map(|this| match toolbar_padding {
                Some(padding) => this.py(padding),
                None => this.py_1(),
            })
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(run_button)
            .when(self.running, |toolbar| {
                toolbar.child(
                    Label::new("Running…")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(
                Label::new(profile_name)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when_some(database_name, |toolbar, database| {
                toolbar.child(
                    Label::new(database)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(Indicator::dot().color(status_color))
            .child(export_menu)
            .child(value_viewer_toggle)
            .when_some(self.result.as_ref(), |toolbar, result| {
                toolbar.child(
                    Label::new(row_info_text(result.rows.len(), self.has_more))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .when(self.has_more && !self.running, |toolbar| {
                toolbar.child(
                    Button::new("load-more", "Load more")
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _window, cx| this.load_more(cx))),
                )
            })
            .when(show_edit_controls, |toolbar| {
                toolbar
                    .child(
                        Label::new(format!("{pending_count} pending"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Button::new("submit-edits", "Submit")
                            .label_size(LabelSize::Small)
                            .disabled(pending_count == 0)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_edits(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("revert-edits", "Revert")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _window, cx| this.revert_edits(cx))),
                    )
            })
            .child(
                Button::new(
                    "commit-mode-toggle",
                    match self.commit_mode {
                        CommitMode::Auto => "Auto",
                        CommitMode::Manual => "Manual",
                    },
                )
                .label_size(LabelSize::Small)
                .tooltip(Tooltip::text("Toggle commit mode (Auto/Manual)"))
                .on_click(cx.listener(|this, _, _window, cx| {
                    let next = match this.commit_mode {
                        CommitMode::Auto => CommitMode::Manual,
                        CommitMode::Manual => CommitMode::Auto,
                    };
                    this.request_commit_mode_change(next, cx);
                })),
            )
            .when(self.commit_mode == CommitMode::Manual, |toolbar| {
                let tx_open = self.transaction_is_open();
                toolbar
                    .child(
                        Button::new("commit-tx", "Commit")
                            .label_size(LabelSize::Small)
                            .disabled(!tx_open)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.commit_transaction(cx);
                            })),
                    )
                    .child(
                        Button::new("rollback-tx", "Rollback")
                            .label_size(LabelSize::Small)
                            .disabled(!tx_open)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.rollback_transaction(cx);
                            })),
                    )
                    .when(tx_open, |toolbar| {
                        toolbar.child(
                            Label::new("● uncommitted")
                                .size(LabelSize::Small)
                                .color(Color::Warning),
                        )
                    })
            })
            .when_some(self.pending_commit_mode_switch, |toolbar, _mode| {
                toolbar
                    .child(
                        Label::new("Open transaction — commit or roll back to switch to Auto")
                            .size(LabelSize::Small)
                            .color(Color::Warning),
                    )
                    .child(
                        Button::new("resolve-commit", "Commit")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.resolve_commit_mode_switch_with_commit(cx);
                            })),
                    )
                    .child(
                        Button::new("resolve-rollback", "Rollback")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.resolve_commit_mode_switch_with_rollback(cx);
                            })),
                    )
                    .child(
                        Button::new("resolve-cancel", "Cancel")
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.cancel_commit_mode_switch(cx);
                            })),
                    )
            })
            .when_some(self.elapsed_ms, |toolbar, elapsed| {
                toolbar.child(
                    Label::new(format_duration(elapsed))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .when_some(self.last_run_label.clone(), |toolbar, label| {
                toolbar.child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            })
            .when(non_wrappable_truncated, |toolbar| {
                toolbar.child(
                    Label::new(format!("(showing first {page_size})"))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            });

        let toolbar = match toolbar_settings.font_size {
            Some(font_size) => ui::utils::WithRemSize::new(px(font_size))
                .w_full()
                .child(toolbar_content)
                .into_any_element(),
            None => toolbar_content.into_any_element(),
        };

        let results_area: AnyElement = if let Some(error) = &self.error {
            let error_msg = format!("Error: {}", error);
            let banner = div()
                .w_full()
                .px_2()
                .py_1()
                .child(Label::new(error_msg).color(Color::Error));
            // A failed sort/Load-more keeps the loaded rows (see
            // `finish_query`): surface the error above the still-populated
            // grid instead of replacing it.
            if self.result.as_ref().is_some_and(|r| !r.columns.is_empty()) {
                v_flex()
                    .flex_1()
                    .w_full()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .child(banner)
                    .child(
                        div()
                            .flex_1()
                            .w_full()
                            .min_h_0()
                            .overflow_hidden()
                            .child(self.results_grid.clone()),
                    )
                    .into_any_element()
            } else {
                banner.flex_1().into_any_element()
            }
        } else if let Some(result) = &self.result {
            if result.columns.is_empty() {
                // DML/DDL result with no column data — toolbar already shows row count and timing.
                let success_msg = match &result.command_tag {
                    Some(tag) => format!("Query executed successfully. {tag}"),
                    None => "Query executed successfully.".to_string(),
                };
                div()
                    .flex_1()
                    .w_full()
                    .px_2()
                    .py_1()
                    .child(
                        Label::new(success_msg)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .w_full()
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.results_grid.clone())
                    .into_any_element()
            }
        } else {
            div()
                .flex_1()
                .w_full()
                .px_2()
                .py_1()
                .child(
                    Label::new("Run statement (⌘↵)")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element()
        };

        let results_row = h_flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .child(results_area)
            .when(self.value_viewer_visible, |row| {
                row.child(
                    div()
                        .w(px(320.))
                        .flex_shrink_0()
                        .h_full()
                        .border_l_1()
                        .border_color(cx.theme().colors().border)
                        .child(self.value_viewer.clone()),
                )
            });

        let separator_color = cx.theme().colors().border_variant;
        let view_id = cx.entity_id();
        let resize_handle = div()
            .id("query-split-container")
            .relative()
            .w_full()
            .flex_shrink_0()
            .h(px(1.))
            .bg(separator_color)
            .child(
                div()
                    .id("query-split-handle")
                    .absolute()
                    .top(px(-RESIZE_HANDLE_HEIGHT / 2.0))
                    .h(px(RESIZE_HANDLE_HEIGHT))
                    .w_full()
                    .cursor_row_resize()
                    .block_mouse_except_scroll()
                    .on_click(cx.listener(|this, event: &gpui::ClickEvent, _window, cx| {
                        if event.click_count() >= 2 {
                            this.reset_split_ratio(cx);
                        }
                        cx.stop_propagation();
                    }))
                    .on_drag(DraggedQuerySplitHandle { view: view_id }, |_, _, _, cx| {
                        cx.new(|_| gpui::Empty)
                    }),
            );

        let editor_ratio = self.visible_editor_ratio;

        div()
            .key_context("QueryView")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::run_script))
            .on_action(cx.listener(Self::run_statement))
            .size_full()
            .flex()
            .flex_col()
            .on_drag_move::<DraggedQuerySplitHandle>(cx.listener(|this, event, _window, cx| {
                this.on_split_drag_move(event, cx);
            }))
            .on_drop::<DraggedQuerySplitHandle>(cx.listener(|this, _, _window, cx| {
                this.commit_split_ratio(cx);
            }))
            .child(
                div()
                    .flex_shrink_1()
                    .min_h_0()
                    .w_full()
                    .flex_basis(DefiniteLength::Fraction(editor_ratio))
                    .overflow_hidden()
                    .child(EditorElement::new(&self.editor, editor_style)),
            )
            .child(resize_handle)
            .child(
                // The toolbar lives inside the results fraction so the split
                // fractions share (container − 1px divider) — the same space
                // `on_split_drag_move` computes the drag ratio against.
                div()
                    .flex_shrink_1()
                    .min_h_0()
                    .w_full()
                    .flex_basis(DefiniteLength::Fraction(1.0 - editor_ratio))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .child(results_row),
            )
    }
}

/// Pure page-request builder: wraps `sql` in the paging subquery when it is
/// wrappable (returning the wrapped SQL, a fetch limit of one extra
/// page-boundary row, and `paged: true`), or passes it through with a plain
/// truncation limit otherwise. Sorts translate to 1-based positional
/// ordinals, immune to duplicate column names.
fn build_page_request(
    sql: String,
    sort: Option<&ColumnSort>,
    page: usize,
    page_size: usize,
) -> (String, usize, bool) {
    if crate::sql_paging::wrappable(&sql) {
        let sort = sort.map(|sort| (sort.column + 1, sort.direction));
        (
            crate::sql_paging::wrap(&sql, sort, page_size + 1, page * page_size),
            page_size + 1,
            true,
        )
    } else {
        (sql, page_size, false)
    }
}

pub(crate) fn row_info_text(loaded: usize, has_more: bool) -> String {
    if loaded == 0 && !has_more {
        return "0 rows".to_string();
    }
    if has_more {
        format!("1–{loaded} of {loaded}+")
    } else {
        format!("1–{loaded} of {loaded}")
    }
}

/// Serializes the currently-displayed result for the Export menu. Pure and
/// total (see [`serialize_result`]): no re-query, no table name (the grid has
/// no notion of a target relation to export into).
fn export_payload(result: &QueryResult, format: ExportFormat, null_string: &str) -> String {
    serialize_result(format, &result.columns, &result.rows, None, null_string)
}

pub(crate) fn format_duration(ms: u128) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// Names the base table (with primary key) a query result maps to, so the
/// results grid can offer inline edits. `None` means read-only.
///
/// Populated by [`detect_editability`], which `QueryView::finish_query` calls
/// on each `Run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editability {
    pub schema: String,
    pub table: String,
    pub pk_cols: Vec<String>,
    pub col_types: HashMap<String, String>,
}

/// Rejects anything but a plain `SELECT … FROM <one relation> [WHERE …]`,
/// returning the (optional schema, table) named in the single FROM item.
fn single_relation(sql: &str) -> Option<(Option<String>, String)> {
    let lowered = sql.trim().to_ascii_lowercase();
    let body = lowered.split(';').next().unwrap_or(&lowered).trim();
    if !body.starts_with("select") {
        return None;
    }
    for disqualifier in [
        " join ",
        " group by ",
        " having ",
        " union ",
        " intersect ",
        " except ",
        " distinct ",
    ] {
        if body.contains(disqualifier) {
            return None;
        }
    }
    let from_at = body.find(" from ")?;
    // The projection (SELECT list) must be `*` or a plain comma-separated
    // list of (optionally-qualified, optionally-aliased) bare identifiers.
    // Any parenthesized expression (aggregate, function call, `DISTINCT(…)`,
    // …) means the projected values are no longer guaranteed to be the raw
    // column values, so an edit built from them could silently target the
    // wrong row. Reject rather than risk that.
    let select_list = body.get("select".len()..from_at)?.trim();
    let select_list = strip_leading_keyword(select_list, "distinct")
        .or_else(|| strip_leading_keyword(select_list, "all"))
        .unwrap_or(select_list);
    if select_list.is_empty() || select_list.contains('(') {
        return None;
    }
    let after = body.get(from_at + " from ".len()..)?;
    // The FROM item ends at the first following clause keyword.
    let end = [
        " where ",
        " order by ",
        " limit ",
        " offset ",
        " group by ",
        " fetch ",
        " for ",
    ]
    .iter()
    .filter_map(|kw| after.find(kw))
    .min()
    .unwrap_or(after.len());
    let from_item = after.get(..end)?.trim();
    // Reject subqueries and multi-relation FROM lists.
    if from_item.is_empty()
        || from_item.starts_with('(')
        || from_item.contains(',')
        || from_item.contains(' ')
    {
        return None;
    }
    // Map the FROM token back to the original-case slice so identifiers keep
    // their case for the metadata lookup (which is case-insensitive anyway).
    let mut parts = from_item.split('.');
    let first = strip_ident(parts.next()?);
    let second = parts.next();
    // Three-plus-part names (`catalog.schema.table`) would silently drop a
    // segment under a naive 2-`next()` parse and could coincidentally match
    // an unrelated relation; reject them instead.
    if parts.next().is_some() {
        return None;
    }
    match second {
        Some(second) => Some((Some(first), strip_ident(second))),
        None => Some((None, first)),
    }
}

/// Strips a leading `keyword` from `text` only when it stands alone as a
/// whole word (followed by whitespace, `(`, or end-of-string), so e.g.
/// `"all"` doesn't match inside a column named `allowance`.
fn strip_leading_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(keyword)?;
    match rest.chars().next() {
        None | Some('(') => Some(rest.trim()),
        Some(next) if next.is_whitespace() => Some(rest.trim()),
        _ => None,
    }
}

fn strip_ident(token: &str) -> String {
    token.trim().trim_matches('"').to_string()
}

/// Decides whether `sql`'s result maps to exactly one base table with a
/// primary key present in `result_columns`. Returns `None` (read-only) for
/// joins, aggregates, set operations, views/matviews, unknown relations, or
/// tables whose PK columns weren't projected (no safe `WHERE` to build).
pub fn detect_editability(
    sql: &str,
    meta: &MetadataCache,
    result_columns: &[String],
) -> Option<Editability> {
    let (schema, table) = single_relation(sql)?;
    let relation = meta.relations.iter().find(|relation| {
        matches!(relation.kind, RelationKind::Table)
            && relation.name.eq_ignore_ascii_case(&table)
            && schema
                .as_ref()
                .is_none_or(|schema| relation.schema.eq_ignore_ascii_case(schema))
    })?;

    let pk_cols: Vec<String> = relation
        .columns
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| column.name.clone())
        .collect();
    if pk_cols.is_empty() {
        return None;
    }
    // Every PK column must be projected so a WHERE can be built.
    if !pk_cols
        .iter()
        .all(|pk| result_columns.iter().any(|column| column == pk))
    {
        return None;
    }

    let col_types = relation
        .columns
        .iter()
        .map(|column| (column.name.clone(), column.data_type.clone()))
        .collect();
    Some(Editability {
        schema: relation.schema.clone(),
        table: relation.name.clone(),
        pk_cols,
        col_types,
    })
}

/// Assembles the ordered delete/update/insert statements (each without a
/// trailing `;`) from staged changes, or `None` when nothing is staged. A
/// deleted original row's staged updates are dropped. PK values come from the
/// *displayed* row cells (PKs are never NULL, so a missing/None PK cell drops
/// that row's statement rather than emitting `= NULL`).
pub(crate) fn build_edit_statements(
    editability: &Editability,
    pending: &PendingChanges,
    columns: &[String],
    rows: &[Vec<Option<String>>],
) -> Option<Vec<String>> {
    let column_index = |name: &str| columns.iter().position(|c| c == name);
    let pk_for_row = |row_index: usize| -> Option<Vec<(String, String)>> {
        let row = rows.get(row_index)?;
        editability
            .pk_cols
            .iter()
            .map(|pk| {
                let value = column_index(pk)
                    .and_then(|i| row.get(i))
                    .cloned()
                    .flatten()?;
                Some((pk.clone(), value))
            })
            .collect()
    };

    let mut statements: Vec<String> = Vec::new();

    // Deletes first.
    let mut deletes: Vec<usize> = pending.deletes.iter().copied().collect();
    deletes.sort_unstable();
    for row_index in &deletes {
        if let Some(pk) = pk_for_row(*row_index) {
            statements.push(generate_delete(
                &editability.schema,
                &editability.table,
                &pk,
                &editability.col_types,
            ));
        }
    }

    // Updates, grouped by row, skipping deleted rows.
    let mut update_rows: Vec<usize> = pending
        .updates
        .keys()
        .map(|(row, _)| *row)
        .filter(|row| !pending.deletes.contains(row))
        .collect();
    update_rows.sort_unstable();
    update_rows.dedup();
    for row_index in update_rows {
        let Some(pk) = pk_for_row(row_index) else {
            continue;
        };
        let mut set: Vec<(String, Option<String>)> = pending
            .updates
            .iter()
            .filter(|((row, _), _)| *row == row_index)
            .filter_map(|((_, col), value)| {
                columns.get(*col).map(|name| (name.clone(), value.clone()))
            })
            .collect();
        set.sort_by(|a, b| a.0.cmp(&b.0));
        if set.is_empty() {
            continue;
        }
        statements.push(generate_update(
            &editability.schema,
            &editability.table,
            &RowEdit { pk, set },
            &editability.col_types,
        ));
    }

    // Inserts last.
    for insert in &pending.inserts {
        let values: Vec<(String, Option<String>)> = columns
            .iter()
            .cloned()
            .zip(insert.iter().cloned())
            .collect();
        statements.push(generate_insert(
            &editability.schema,
            &editability.table,
            &InsertRow { values },
            &editability.col_types,
        ));
    }

    if statements.is_empty() {
        return None;
    }
    Some(statements)
}

/// Wraps [`build_edit_statements`]'s output in `BEGIN;…;COMMIT;`, or `None`
/// when nothing is staged. Used by the Auto commit-mode path, which runs the
/// whole batch atomically on the tab's plain connection.
pub(crate) fn build_edit_transaction(
    editability: &Editability,
    pending: &PendingChanges,
    columns: &[String],
    rows: &[Vec<Option<String>>],
) -> Option<String> {
    let statements = build_edit_statements(editability, pending, columns, rows)?;
    let mut sql = String::from("BEGIN;\n");
    for statement in &statements {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    sql.push_str("COMMIT;");
    Some(sql)
}

#[cfg(test)]
mod editability_tests {
    use super::*;
    use crate::connection::introspect::RelationKind;
    use crate::connection::metadata_cache::{ColumnMeta, MetadataCache, RelationMeta};

    fn col(name: &str, ty: &str, pk: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            data_type: ty.into(),
            is_primary_key: pk,
        }
    }

    fn meta() -> MetadataCache {
        MetadataCache {
            relations: vec![
                RelationMeta {
                    schema: "public".into(),
                    name: "users".into(),
                    kind: RelationKind::Table,
                    columns: vec![col("id", "integer", true), col("name", "text", false)],
                },
                RelationMeta {
                    schema: "public".into(),
                    name: "user_emails".into(),
                    kind: RelationKind::View,
                    columns: vec![col("id", "integer", true)],
                },
                RelationMeta {
                    schema: "public".into(),
                    name: "logs".into(),
                    kind: RelationKind::Table,
                    columns: vec![col("msg", "text", false)],
                },
            ],
        }
    }

    #[test]
    fn plain_select_over_pk_table_is_editable() {
        let e = detect_editability(
            "select id, name from public.users",
            &meta(),
            &["id".into(), "name".into()],
        )
        .expect("single PK'd base table is editable");
        assert_eq!(e.schema, "public");
        assert_eq!(e.table, "users");
        assert_eq!(e.pk_cols, vec!["id".to_string()]);
        assert_eq!(e.col_types.get("name").map(String::as_str), Some("text"));
    }

    #[test]
    fn bare_and_starred_and_where_still_editable() {
        assert!(
            detect_editability(
                "select * from users",
                &meta(),
                &["id".into(), "name".into()]
            )
            .is_some()
        );
        assert!(
            detect_editability(
                "SELECT * FROM users WHERE id > 1",
                &meta(),
                &["id".into(), "name".into()]
            )
            .is_some()
        );
    }

    #[test]
    fn missing_pk_column_in_result_is_read_only() {
        // PK not projected → no safe WHERE.
        assert!(detect_editability("select name from users", &meta(), &["name".into()]).is_none());
    }

    #[test]
    fn table_without_pk_is_read_only() {
        assert!(detect_editability("select msg from logs", &meta(), &["msg".into()]).is_none());
    }

    #[test]
    fn view_join_aggregate_and_multi_relation_are_read_only() {
        assert!(
            detect_editability("select id from user_emails", &meta(), &["id".into()]).is_none()
        );
        assert!(
            detect_editability(
                "select u.id from users u join logs l on true",
                &meta(),
                &["id".into()]
            )
            .is_none()
        );
        assert!(
            detect_editability("select count(*) from users", &meta(), &["count".into()]).is_none()
        );
        assert!(
            detect_editability("select id from users group by id", &meta(), &["id".into()])
                .is_none()
        );
        assert!(
            detect_editability(
                "select id from users union select id from users",
                &meta(),
                &["id".into()]
            )
            .is_none()
        );
        assert!(
            detect_editability("select id from users, logs", &meta(), &["id".into()]).is_none()
        );
        assert!(
            detect_editability("select id from (select 1 id) s", &meta(), &["id".into()]).is_none()
        );
    }

    #[test]
    fn unknown_relation_is_read_only() {
        assert!(detect_editability("select id from nope", &meta(), &["id".into()]).is_none());
    }

    #[test]
    fn ungrouped_aggregate_aliased_to_pk_name_is_read_only() {
        // Regression test: an aggregate/function aliased to the PK's name
        // must never be mistaken for the raw PK column, or an edit would
        // build an `UPDATE … WHERE id = <aggregate value>` against an
        // unrelated row.
        assert!(
            detect_editability("SELECT COUNT(*) AS id FROM users", &meta(), &["id".into()])
                .is_none()
        );
        assert!(
            detect_editability("SELECT MAX(id) AS id FROM users", &meta(), &["id".into()])
                .is_none()
        );
        assert!(
            detect_editability(
                "SELECT DISTINCT(id), name FROM users",
                &meta(),
                &["id".into(), "name".into()]
            )
            .is_none()
        );
    }

    #[test]
    fn plain_projection_regression_guard() {
        assert!(
            detect_editability(
                "SELECT id, name FROM users",
                &meta(),
                &["id".into(), "name".into()]
            )
            .is_some()
        );
        assert!(
            detect_editability(
                "SELECT * FROM users",
                &meta(),
                &["id".into(), "name".into()]
            )
            .is_some()
        );
    }

    #[test]
    fn three_part_relation_name_is_read_only() {
        assert!(
            detect_editability("select id from mydb.public.users", &meta(), &["id".into()])
                .is_none()
        );
    }
}

#[cfg(test)]
mod unit {
    use super::*;
    use crate::sql_paging::SortDirection;

    #[test]
    fn page_request_sorts_by_positional_ordinal() {
        // Ordinals stay valid when the result has duplicate column names
        // (`select u.id, o.id from …`), where `order by "id"` is ambiguous.
        let sort = ColumnSort {
            column: 1,
            name: "id".into(),
            direction: SortDirection::Ascending,
        };
        let (sql, fetch_limit, paged) =
            build_page_request("select 1 as id, 2 as id".to_string(), Some(&sort), 2, 500);
        assert!(paged);
        assert_eq!(fetch_limit, 501);
        assert_eq!(
            sql,
            "select * from (\nselect 1 as id, 2 as id\n) zedium_page order by 2 asc limit 501 offset 1000"
        );
    }

    #[test]
    fn page_request_passes_non_wrappable_sql_through() {
        let (sql, fetch_limit, paged) =
            build_page_request("select 1 as x into my_backup".to_string(), None, 0, 500);
        assert!(
            !paged,
            "SELECT … INTO must fall back to the truncation path"
        );
        assert_eq!(sql, "select 1 as x into my_backup");
        assert_eq!(fetch_limit, 500);
    }

    #[test]
    fn clamps_editor_ratio() {
        assert_eq!(clamp_editor_ratio(0.05), 0.1);
        assert_eq!(clamp_editor_ratio(0.95), 0.9);
        assert_eq!(clamp_editor_ratio(0.3), 0.3);
        assert_eq!(
            clamp_editor_ratio(DEFAULT_EDITOR_RATIO),
            DEFAULT_EDITOR_RATIO
        );
    }

    #[test]
    fn row_info_shows_open_and_final_totals() {
        assert_eq!(row_info_text(0, false), "0 rows");
        assert_eq!(row_info_text(500, true), "1–500 of 500+");
        assert_eq!(row_info_text(342, false), "1–342 of 342");
    }

    #[test]
    fn duration_formats_ms_under_a_second_and_seconds_above() {
        assert_eq!(format_duration(54), "54 ms");
        assert_eq!(format_duration(999), "999 ms");
        assert_eq!(format_duration(1000), "1.0 s");
        assert_eq!(format_duration(1234), "1.2 s");
    }

    // Paged fetches request page_size + 1 rows; the extra row only signals more pages.
    #[test]
    fn page_split_detects_more_pages() {
        let fetched: Vec<Vec<Option<String>>> =
            (0..11).map(|i| vec![Some(i.to_string())]).collect();
        let (rows, has_more) = crate::connection::client::truncate_rows(fetched, 10);
        assert_eq!((rows.len(), has_more), (10, true));

        let exact: Vec<Vec<Option<String>>> = (0..10).map(|i| vec![Some(i.to_string())]).collect();
        let (rows, has_more) = crate::connection::client::truncate_rows(exact, 10);
        assert_eq!((rows.len(), has_more), (10, false));
    }
}

#[cfg(test)]
mod live_tests {
    use crate::connection::client::{Connection, truncate_rows};
    use crate::connection::profile::ConnectionProfile;
    use crate::sql_paging::{self, SortDirection};

    fn password_from_url(url: &str) -> Option<String> {
        use std::str::FromStr as _;
        tokio_postgres::Config::from_str(url)
            .ok()
            .and_then(|config| {
                config
                    .get_password()
                    .and_then(|p| String::from_utf8(p.to_vec()).ok())
            })
    }

    async fn connect(cx: &mut gpui::TestAppContext, url: &str) -> Connection {
        cx.executor().allow_parking();
        cx.update(|cx| gpui_tokio::init(cx));
        let profile = ConnectionProfile::from_url(url).unwrap();
        let password = password_from_url(url);
        cx.update(|cx| Connection::connect(profile, password, cx))
            .await
            .unwrap()
    }

    const SERIES_SQL: &str = "select i from generate_series(1, 25) as t(i)";

    #[gpui::test]
    #[ignore]
    async fn live_pages_accumulate_until_exhausted(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;
        let page_size = 10;
        assert!(sql_paging::wrappable(SERIES_SQL));

        let mut loaded: Vec<Vec<Option<String>>> = Vec::new();
        let mut pages = 0;
        loop {
            let wrapped = sql_paging::wrap(
                SERIES_SQL,
                Some((1, SortDirection::Ascending)),
                page_size + 1,
                pages * page_size,
            );
            let result = cx
                .update(|cx| conn.execute(wrapped, page_size + 1, cx))
                .await
                .unwrap();
            let (rows, has_more) = truncate_rows(result.rows, page_size);
            loaded.extend(rows);
            pages += 1;
            if pages == 1 {
                assert_eq!(loaded.len(), 10);
                assert!(has_more, "25 rows must not fit one 10-row page");
            }
            if !has_more {
                break;
            }
            assert!(pages < 5, "paging did not terminate");
        }
        assert_eq!(pages, 3); // 10 + 10 + 5
        assert_eq!(loaded.len(), 25);
        assert_eq!(loaded.first(), Some(&vec![Some("1".to_string())]));
        assert_eq!(loaded.last(), Some(&vec![Some("25".to_string())]));
    }

    #[gpui::test]
    #[ignore]
    async fn live_sort_direction_changes_first_row(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;
        for (direction, expected_first) in [
            (SortDirection::Ascending, "1"),
            (SortDirection::Descending, "25"),
        ] {
            let wrapped = sql_paging::wrap(SERIES_SQL, Some((1, direction)), 11, 0);
            let result = cx.update(|cx| conn.execute(wrapped, 11, cx)).await.unwrap();
            assert_eq!(
                result.rows.first(),
                Some(&vec![Some(expected_first.to_string())])
            );
        }
    }

    #[gpui::test]
    #[ignore]
    async fn live_duplicate_column_names_sort_by_ordinal(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;
        // Both output columns are named "id": `order by "id"` would fail with
        // `ORDER BY "id" is ambiguous`; the positional ordinal cannot.
        let sql = r#"select i as "id", 26 - i as "id" from generate_series(1, 25) as t(i)"#;
        assert!(sql_paging::wrappable(sql));
        let wrapped = sql_paging::wrap(sql, Some((2, SortDirection::Ascending)), 11, 0);
        let result = cx.update(|cx| conn.execute(wrapped, 11, cx)).await.unwrap();
        // Sorted by the SECOND id column, whose smallest value 1 pairs with i = 25.
        assert_eq!(
            result.rows.first(),
            Some(&vec![Some("25".to_string()), Some("1".to_string())])
        );
    }

    #[gpui::test]
    #[ignore]
    async fn live_non_wrappable_statements_run_unwrapped(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;

        // Postgres forbids `SELECT … INTO` inside a subquery; wrappable() must
        // route it to the unwrapped truncation path, where it executes fine.
        let select_into = "select 1 as x into temp zedium_select_into";
        assert!(!sql_paging::wrappable(select_into));
        cx.update(|cx| conn.execute(select_into.to_string(), 10, cx))
            .await
            .unwrap();

        // Same for a WITH clause containing a data-modifying statement.
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_cte_target (x int)".to_string(),
                10,
                cx,
            )
        })
        .await
        .unwrap();
        let dml_cte =
            "with ins as (insert into zedium_cte_target values (7) returning x) select * from ins";
        assert!(!sql_paging::wrappable(dml_cte));
        let result = cx
            .update(|cx| conn.execute(dml_cte.to_string(), 10, cx))
            .await
            .unwrap();
        assert_eq!(result.rows, vec![vec![Some("7".to_string())]]);
    }

    #[gpui::test]
    #[ignore]
    async fn live_edit_round_trip_update_insert_delete(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;
        // Isolated temp table so the seeded DB is untouched.
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_edit (id int primary key, name text)".into(),
                10,
                cx,
            )
        })
        .await
        .unwrap();
        cx.update(|cx| {
            conn.execute(
                "insert into zedium_edit values (1,'a'),(2,'b'),(3,'c')".into(),
                10,
                cx,
            )
        })
        .await
        .unwrap();

        let editability = crate::query_view::Editability {
            schema: "pg_temp".into(), // temp tables live in the session's pg_temp schema
            table: "zedium_edit".into(),
            pk_cols: vec!["id".into()],
            col_types: [
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![Some("1".into()), Some("a".into())],
            vec![Some("2".into()), Some("b".into())],
            vec![Some("3".into()), Some("c".into())],
        ];
        let mut pending = crate::results_grid::PendingChanges::default();
        pending.stage_update(0, 1, Some("A".into())); // id=1 name→A
        pending.deletes.insert(1); // delete id=2
        pending.inserts.push(vec![Some("4".into()), None]); // insert id=4 name NULL

        let sql =
            crate::query_view::build_edit_transaction(&editability, &pending, &columns, &rows)
                .unwrap();
        cx.update(|cx| conn.execute(sql, 10, cx)).await.unwrap();

        let after = cx
            .update(|cx| {
                conn.execute(
                    "select id, name from zedium_edit order by id".into(),
                    100,
                    cx,
                )
            })
            .await
            .unwrap();
        assert_eq!(
            after.rows,
            vec![
                vec![Some("1".into()), Some("A".into())],
                vec![Some("3".into()), Some("c".into())],
                vec![Some("4".into()), None],
            ]
        );
    }

    #[gpui::test]
    #[ignore]
    async fn live_failed_submit_rolls_back_and_changes_nothing(cx: &mut gpui::TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        let conn = connect(cx, &url).await;
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_rb (id int primary key)".into(),
                10,
                cx,
            )
        })
        .await
        .unwrap();
        cx.update(|cx| conn.execute("insert into zedium_rb values (1)".into(), 10, cx))
            .await
            .unwrap();
        // Second insert of id=1 (duplicate PK) makes the batch fail; the prior
        // insert of id=2 must roll back.
        let batch =
            "BEGIN;\ninsert into zedium_rb values (2);\ninsert into zedium_rb values (1);\nCOMMIT;";
        assert!(
            cx.update(|cx| conn.execute(batch.into(), 10, cx))
                .await
                .is_err()
        );
        let after = cx
            .update(|cx| conn.execute("select id from zedium_rb order by id".into(), 10, cx))
            .await
            .unwrap();
        assert_eq!(
            after.rows,
            vec![vec![Some("1".into())]],
            "failed batch left only the original row"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::introspect::RelationKind;
    use crate::connection::metadata_cache::{ColumnMeta, RelationMeta};
    use crate::connection::profile::{ConnectionProfile, SslMode};
    use crate::sql_paging::SortDirection;
    use gpui::{BorrowAppContext, TestAppContext, VisualTestContext};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            gpui_tokio::init(cx);
        });
    }

    fn set_database_settings(
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut settings::DatabaseSettingsContent) + 'static,
    ) {
        cx.update(|cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |content| {
                    update(content.database.get_or_insert_default());
                });
            })
        });
    }

    fn test_profile() -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "Test".into(),
            host: "127.0.0.1".into(),
            // Port 1: guaranteed connection refusal if anything ever tries to connect.
            port: 1,
            database: "db".into(),
            user: "u".into(),
            ssl_mode: SslMode::Disable,
            read_only: false,
        }
    }

    #[gpui::test]
    async fn run_script_with_empty_sql_is_a_no_op(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "   \n".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });

        view.update_in(cx, |view, window, cx| {
            view.run_script(&RunScript, window, cx)
        });

        // The guard must bail synchronously: no generation bump, no spinner,
        // no lazy-connect attempt, no error.
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.run_generation, 0,
                "blank SQL must not dispatch a query"
            );
            assert!(!view.running);
            assert!(view.error.is_none());
            assert!(view.result.is_none());
        });
    }

    #[gpui::test]
    async fn load_sql_replaces_editor_text(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1 as id".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });
        view.update_in(cx, |view, window, cx| {
            view.load_sql("select 42".to_string(), false, window, cx)
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.editor.read(cx).text(cx), "select 42");
            assert_eq!(view.run_generation, 0, "run=false must not dispatch");
        });
    }

    #[gpui::test]
    async fn load_sql_with_run_dispatches(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1 as id".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });
        view.update_in(cx, |view, window, cx| {
            view.load_sql("select 7".to_string(), true, window, cx)
        });
        view.read_with(cx, |view, cx| {
            assert_eq!(view.editor.read(cx).text(cx), "select 7");
            assert_eq!(view.run_generation, 1, "run=true must dispatch one query");
        });
    }

    // The handler's core decision is `statement_at` over the caret offset; test
    // it directly so the run wiring stays thin.
    #[test]
    fn caret_statement_is_selected_from_a_multi_statement_buffer() {
        let text = "select 1;\nselect 2;\nselect 3";
        // Caret on line 2 ("select 2" begins at offset 10).
        let range =
            crate::sql_statements::statement_at(text, 13).expect("caret is inside select 2");
        assert_eq!(&text[range.start..range.end], "select 2");
    }

    #[gpui::test]
    async fn run_statement_runs_only_the_caret_statement(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1;\nselect 2;\nselect 3".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });

        // Place the primary caret inside "select 2".
        view.update_in(cx, |view, window, cx| {
            view.editor.update(cx, |editor, cx| {
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(13)..editor::MultiBufferOffset(13)
                    ]);
                });
            });
            view.run_statement(&RunStatement, window, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.last_sql.as_deref(),
                Some("select 2"),
                "RunStatement must run only the caret statement"
            );
        });
    }

    #[gpui::test]
    async fn run_statement_runs_the_selection_when_present(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1;\nselect 2;\nselect 3".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });

        // Select the text "elect 1;\nsel" — a fragment spanning two statements;
        // RunStatement must run the RAW selection, not a re-split statement.
        view.update_in(cx, |view, window, cx| {
            view.editor.update(cx, |editor, cx| {
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(1)..editor::MultiBufferOffset(13)
                    ]);
                });
            });
            view.run_statement(&RunStatement, window, cx);
        });

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.last_sql.as_deref(),
                Some("elect 1;\nsel"),
                "a selection must run verbatim"
            );
        });
    }

    #[gpui::test]
    async fn run_statement_records_which_statement_ran(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1;\nselect 2;\nselect 3".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });

        // Caret in the 2nd of 3 statements.
        view.update_in(cx, |view, window, cx| {
            view.editor.update(cx, |editor, cx| {
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(13)..editor::MultiBufferOffset(13)
                    ]);
                });
            });
            view.run_statement(&RunStatement, window, cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.last_run_label.as_deref(), Some("stmt 2/3"));
        });

        // Running the whole script clears the per-statement label.
        view.update_in(cx, |view, window, cx| {
            view.run_script(&RunScript, window, cx)
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.last_run_label, None);
        });
    }

    // Regression test for the QueryView run-keybinding shadowing bug: the SQL
    // editor is a full-mode `Editor`, whose default keymap binds
    // `cmd-enter -> editor::NewlineBelow` at the `Editor && mode == full`
    // context. GPUI resolves keystrokes at the deepest focused node first, so
    // scoping the QueryView run bindings to the plain `"QueryView"` context
    // (an ancestor of the editor) let the editor's own binding win. This
    // dispatches a real keystroke through the window (not a direct action
    // call) to exercise that resolution end to end.
    #[gpui::test]
    async fn cmd_enter_dispatches_run_statement_not_editor_newline(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            let default_key_bindings = settings::KeymapFile::load_asset_allow_partial_failure(
                "keymaps/default-macos.json",
                None,
                cx,
            )
            .expect("default keymap asset should parse");
            cx.bind_keys(default_key_bindings);
            editor::init(cx);
            register_keybindings(cx);
        });

        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1;\nselect 2;\nselect 3".to_string(),
                None,
                window,
                cx,
            );
        });
        let view = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        });

        // Place the caret inside "select 2" and focus the editor, the normal
        // state when a user is about to run a statement.
        view.update_in(cx, |view, window, cx| {
            view.editor.update(cx, |editor, cx| {
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([
                        editor::MultiBufferOffset(13)..editor::MultiBufferOffset(13)
                    ]);
                });
            });
            view.editor.read(cx).focus_handle(cx).focus(window, cx);
        });
        cx.run_until_parked();

        let text_before = view.read_with(cx, |view, cx| view.editor.read(cx).text(cx));
        assert_eq!(
            view.read_with(cx, |view, _| view.run_generation),
            0,
            "sanity check: no run has happened yet"
        );

        cx.simulate_keystrokes("cmd-enter");

        let text_after = view.read_with(cx, |view, cx| view.editor.read(cx).text(cx));
        assert_eq!(
            text_before, text_after,
            "cmd-enter must dispatch RunStatement, not insert an editor::NewlineBelow"
        );
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.last_sql.as_deref(),
                Some("select 2"),
                "cmd-enter must have actually dispatched RunStatement for the caret statement"
            );
        });
    }

    async fn open_test_view(cx: &mut TestAppContext) -> Entity<QueryView> {
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, window_cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        workspace.update_in(window_cx, |workspace, window, cx| {
            QueryView::open(
                workspace,
                Some(test_profile()),
                None,
                "select 1 as id".to_string(),
                None,
                window,
                cx,
            );
        });
        workspace.update(window_cx, |workspace, cx| {
            workspace
                .items_of_type::<QueryView>(cx)
                .next()
                .expect("QueryView tab should exist")
        })
    }

    #[gpui::test]
    async fn commit_mode_toggle_updates_field_and_marks_dirty(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        view.update(cx, |view, cx| {
            assert_eq!(view.commit_mode, CommitMode::Auto);
            assert!(view.session.is_none());
            view.set_commit_mode(CommitMode::Manual, cx);
            assert_eq!(view.commit_mode, CommitMode::Manual);
            assert!(
                view.needs_serialize,
                "changing commit mode must schedule a persist"
            );
            // No statement has run yet, so the session is still lazily unallocated
            // and there is nothing to commit.
            assert!(view.session.as_ref().map(|s| s.is_open()) != Some(true));
        });
    }

    #[gpui::test]
    async fn commit_mode_setting_defaults_a_fresh_tab(cx: &mut TestAppContext) {
        init_test(cx);
        set_database_settings(cx, |database| database.commit_mode = Some("manual".into()));
        let view = open_test_view(cx).await;

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.commit_mode,
                CommitMode::Manual,
                "a fresh tab must default its commit mode from the database.commit_mode setting"
            );
        });
    }

    #[gpui::test]
    async fn read_only_setting_disables_grid_editing_and_blocks_submit(cx: &mut TestAppContext) {
        init_test(cx);
        set_database_settings(cx, |database| database.read_only = Some(true));
        let view = open_test_view(cx).await;

        let grid = view.update(cx, |view, cx| {
            *view.metadata.borrow_mut() = users_table_metadata();
            view.last_sql = Some("select id, name from users".into());
            view.finish_query(
                QueryIntent::Run,
                false,
                0,
                500,
                Ok(QueryResult {
                    columns: vec!["id".into(), "name".into()],
                    rows: vec![vec![Some("1".into()), Some("alice".into())]],
                    command_tag: None,
                    truncated: false,
                }),
                1,
                cx,
            );
            view.results_grid.clone()
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.editability.is_some(),
                "a single-table select over a PK'd table is still detected as editable"
            );
            assert!(
                view.read_only,
                "the global read_only setting must flow into the tab's own read_only field"
            );
        });
        grid.read_with(cx, |grid, _| {
            assert!(
                !grid.is_editable(),
                "read_only must override the grid's own editability detection"
            );
        });

        let mut window_cx = VisualTestContext::from_window(cx.windows()[0], cx);
        view.update_in(&mut window_cx, |view, window, cx| {
            view.results_grid.update(cx, |grid, _cx| {
                grid.pending_mut_for_test()
                    .stage_update(0, 1, Some("bob".into()));
            });
            view.submit_edits(window, cx);
        });
        window_cx.run_until_parked();

        view.read_with(cx, |view, cx| {
            assert!(
                !view.results_grid.read(cx).pending_changes().is_empty(),
                "read_only must block submit — the staged edit must remain pending"
            );
            assert_eq!(
                view.error, None,
                "a blocked submit must not attempt any DML or surface a DB error"
            );
        });
    }

    #[gpui::test]
    async fn confirm_edits_defaults_true_and_gates_submit_behind_a_prompt(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;
        let mut window_cx = VisualTestContext::from_window(cx.windows()[0], cx);

        let editability = Editability {
            schema: "public".into(),
            table: "t".into(),
            pk_cols: vec!["id".into()],
            col_types: [("id".to_string(), "integer".to_string())]
                .into_iter()
                .collect(),
        };
        let result = QueryResult {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            command_tag: None,
            truncated: false,
        };
        view.update_in(&mut window_cx, |view, window, cx| {
            view.commit_mode = CommitMode::Auto;
            view.connection = None;
            view.editability = Some(editability);
            view.result = Some(result);
            view.results_grid.update(cx, |grid, _cx| {
                grid.pending_mut_for_test()
                    .stage_update(0, 0, Some("2".into()));
            });
            view.submit_edits(window, cx);
        });
        window_cx.run_until_parked();

        assert!(
            cx.has_pending_prompt(),
            "confirm_edits defaults to true — submit must show a confirm prompt"
        );
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.error, None,
                "nothing must run until the prompt is answered"
            );
        });

        cx.simulate_prompt_answer("Submit");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.error.as_deref(),
                Some("No live connection for edit submit"),
                "confirming the prompt must run the DML path, which then fails fast \
                 without a connection — proving execute_submit_edits actually ran"
            );
        });
    }

    #[gpui::test]
    async fn confirm_edits_false_submits_immediately_without_a_prompt(cx: &mut TestAppContext) {
        init_test(cx);
        set_database_settings(cx, |database| database.confirm_edits = Some(false));
        let view = open_test_view(cx).await;
        let mut window_cx = VisualTestContext::from_window(cx.windows()[0], cx);

        let editability = Editability {
            schema: "public".into(),
            table: "t".into(),
            pk_cols: vec!["id".into()],
            col_types: [("id".to_string(), "integer".to_string())]
                .into_iter()
                .collect(),
        };
        let result = QueryResult {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            command_tag: None,
            truncated: false,
        };
        view.update_in(&mut window_cx, |view, window, cx| {
            view.commit_mode = CommitMode::Auto;
            view.connection = None;
            view.editability = Some(editability);
            view.result = Some(result);
            view.results_grid.update(cx, |grid, _cx| {
                grid.pending_mut_for_test()
                    .stage_update(0, 0, Some("2".into()));
            });
            view.submit_edits(window, cx);
        });
        window_cx.run_until_parked();

        assert!(
            !cx.has_pending_prompt(),
            "confirm_edits=false must skip the prompt entirely"
        );
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.error.as_deref(),
                Some("No live connection for edit submit"),
                "submit must run immediately — unchanged from before the setting existed"
            );
        });
    }

    // `TransactionSession::new` requires a real `Connection` (no test double
    // exists for it — see `Connection::connect`), so a live DB is needed to
    // exercise `is_open()`'s transition from `BEGIN` actually landing. Without
    // `DATABASE_CLIENT_TEST_PG_URL` this still asserts the no-connection no-op
    // path, so `cargo test -p database_client --lib` needs no DB to pass.
    #[gpui::test]
    async fn manual_mode_allocates_session_on_first_dispatch(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            view.update(cx, |view, cx| {
                view.commit_mode = CommitMode::Manual;
                view.ensure_session_open(cx);
                assert!(
                    view.session.is_none(),
                    "ensure_session_open must stay a no-op without a live connection"
                );
            });
            return;
        };

        cx.executor().allow_parking();
        let profile = ConnectionProfile::from_url(&url).expect("valid test DB URL");
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .expect("connect to test DB");

        view.update(cx, |view, cx| {
            view.commit_mode = CommitMode::Manual;
            view.connection = Some(conn);
            view.ensure_session_open(cx);
            assert!(
                view.session.is_some(),
                "Manual mode must allocate a TransactionSession"
            );
            // Task 3's fix only flips `is_open()` to true once the async `BEGIN`
            // completes, so it cannot be asserted synchronously here.
            assert!(!view.transaction_is_open());
        });

        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            assert!(
                view.transaction_is_open(),
                "BEGIN must have landed once the executor is parked"
            );
        });
    }

    // Regression test for the restart-autocommit bug: a query tab restored
    // after restart has a `profile` but no live `connection` (it lazily
    // connects on first Run — see the "Lazy-connect covers query tabs
    // restored after restart" comment in `dispatch_query`). Before the fix,
    // the Manual-session guard required `self.connection.is_some()`, so that
    // lazy-connect path never opened a `BEGIN`: the tab's first statement
    // after a restart silently ran in autocommit despite Manual mode. This
    // dispatches through the real `dispatch_query` (not `ensure_session_open`
    // directly) with `connection = None` and a `profile` set, exactly
    // mirroring a restored tab, and asserts the first statement lands inside
    // an open transaction rather than autocommitting.
    #[gpui::test]
    #[ignore]
    async fn live_manual_restart_opens_transaction_before_first_statement(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        init_test(cx);
        cx.executor().allow_parking();
        let profile = ConnectionProfile::from_url(&url).expect("valid test DB URL");
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        // A separate bootstrap connection stands in for "another client",
        // proving the INSERT below is not visible outside the dispatched
        // tab's own (uncommitted) transaction.
        let bootstrap_conn = cx
            .update(|cx| Connection::connect(profile.clone(), password.clone(), cx))
            .await
            .expect("connect to test DB");
        cx.update(|cx| {
            bootstrap_conn.execute(
                "create table if not exists zedium_manual_restart (id int primary key)".into(),
                10,
                cx,
            )
        })
        .await
        .expect("create table");
        cx.update(|cx| bootstrap_conn.execute("delete from zedium_manual_restart".into(), 10, cx))
            .await
            .expect("clear table");

        let view = open_test_view(cx).await;
        view.update(cx, |view, _cx| {
            // Mirrors a tab restored after restart: a profile to lazily
            // connect with, but no live connection yet.
            view.commit_mode = CommitMode::Manual;
            view.connection = None;
            view.profile = Some(profile);
        });

        view.update(cx, |view, cx| {
            view.dispatch_query(
                "insert into zedium_manual_restart values (1)".to_string(),
                0,
                QueryIntent::Run,
                cx,
            );
        });
        // The lazy connect + BEGIN + INSERT round-trip through real network
        // I/O (`gpui_tokio`), which a single `run_until_parked()` doesn't
        // reliably drain (mirrors the polling loop other live-DB tests in
        // this file use, e.g. `live_edit_submit_refresh_preserves_active_filter`).
        for _ in 0..50 {
            cx.run_until_parked();
            if !view.read_with(cx, |view, _| view.running) {
                break;
            }
            cx.executor()
                .timer(std::time::Duration::from_millis(20))
                .await;
        }

        view.read_with(cx, |view, _cx| {
            assert_eq!(view.error, None, "the lazily-connected insert must succeed");
            assert!(
                view.transaction_is_open(),
                "Manual mode must have opened a transaction before the first statement, \
                 even on the lazy-connect (restored-tab) path"
            );
        });

        // The insert must not be visible to another connection: if it had
        // autocommitted (the bug), this select would see the row.
        let seen_elsewhere = cx
            .update(|cx| {
                bootstrap_conn.execute("select id from zedium_manual_restart".into(), 10, cx)
            })
            .await
            .expect("select from bootstrap connection");
        assert!(
            seen_elsewhere.rows.is_empty(),
            "the insert must still be uncommitted, not autocommitted: {:?}",
            seen_elsewhere.rows
        );

        // Clean up: roll back the tab's transaction and drop the test table.
        let rollback_task = view.update(cx, |view, cx| {
            view.session
                .as_mut()
                .expect("Manual session must exist")
                .rollback(cx)
        });
        rollback_task.await.expect("rollback");
        cx.update(|cx| bootstrap_conn.execute("drop table zedium_manual_restart".into(), 10, cx))
            .await
            .expect("drop test table");
    }

    // Exercises the Manual-mode routing fix: `submit_edits` must run the bare
    // DML on the open session's connection (no nested `BEGIN`/`COMMIT`)
    // rather than the Auto-mode wrapped batch, so the edit stays uncommitted
    // until the user commits/rolls back. `TransactionSession` needs a real
    // `Connection` (no test double exists for it), so this is `#[ignore]`d
    // like the other live-DB DML tests in this file and needs
    // `DATABASE_CLIENT_TEST_PG_URL` to run.
    #[gpui::test]
    #[ignore]
    async fn live_manual_submit_lands_in_open_transaction_uncommitted(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        init_test(cx);
        cx.executor().allow_parking();
        let profile = ConnectionProfile::from_url(&url).expect("valid test DB URL");
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password.clone(), cx))
            .await
            .expect("connect to test DB");

        // Isolated temp table (pg_temp is scoped to this connection/session).
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_manual_submit (id int primary key, name text)".into(),
                10,
                cx,
            )
        })
        .await
        .expect("create temp table");
        cx.update(|cx| {
            conn.execute(
                "insert into zedium_manual_submit values (1, 'a')".into(),
                10,
                cx,
            )
        })
        .await
        .expect("seed row");

        let view = open_test_view(cx).await;
        let mut window_cx = VisualTestContext::from_window(cx.windows()[0], cx);
        let editability = Editability {
            schema: "pg_temp".into(),
            table: "zedium_manual_submit".into(),
            pk_cols: vec!["id".into()],
            col_types: [
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let result = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), Some("a".into())]],
            command_tag: None,
            truncated: false,
        };
        view.update_in(&mut window_cx, |view, window, cx| {
            view.commit_mode = CommitMode::Manual;
            view.connection = Some(conn.clone());
            view.editability = Some(editability);
            view.last_sql = Some("select id, name from zedium_manual_submit order by id".into());
            view.results_grid.update(cx, |grid, cx| {
                grid.set_result(Some(result.clone()), false, cx);
                grid.pending_mut_for_test()
                    .stage_update(0, 1, Some("Z".into()));
            });
            view.result = Some(result);
            view.submit_edits(window, cx);
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Submit");
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            assert_eq!(view.error, None, "the Manual submit must succeed");
            assert!(
                view.results_grid.read(cx).pending_changes().is_empty(),
                "success clears the staged edit"
            );
            assert!(
                view.transaction_is_open(),
                "the Manual session's transaction must still be open — submit_edits must not \
                 have run a nested COMMIT"
            );
        });

        // Same connection/session: the uncommitted UPDATE is visible here.
        let visible_in_session = cx
            .update(|cx| {
                conn.execute(
                    "select name from zedium_manual_submit where id = 1".into(),
                    10,
                    cx,
                )
            })
            .await
            .expect("select inside the open transaction");
        assert_eq!(visible_in_session.rows, vec![vec![Some("Z".into())]]);

        // Rolling back must discard the uncommitted edit — proof it never
        // auto-committed.
        let rollback_task = view.update(cx, |view, cx| {
            view.session
                .as_mut()
                .expect("Manual session must exist")
                .rollback(cx)
        });
        rollback_task.await.expect("rollback");
        let after_rollback = cx
            .update(|cx| {
                conn.execute(
                    "select name from zedium_manual_submit where id = 1".into(),
                    10,
                    cx,
                )
            })
            .await
            .expect("select after rollback");
        assert_eq!(
            after_rollback.rows,
            vec![vec![Some("a".into())]],
            "rollback must discard the never-committed edit"
        );
    }

    // `submit_edits`'s post-commit refresh must go through the same
    // `effective_base_sql()` path as `load_more`/`SortRequested`, not the raw
    // `last_sql`, or an active filter silently drops from the refreshed page
    // while the filter bar still claims it's applied. `Connection` has no
    // test double (see the comment on `live_manual_submit_lands_in_open_transaction_uncommitted`
    // above), so this needs `DATABASE_CLIENT_TEST_PG_URL` to actually run.
    #[gpui::test]
    #[ignore]
    async fn live_edit_submit_refresh_preserves_active_filter(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        init_test(cx);
        cx.executor().allow_parking();
        let profile = ConnectionProfile::from_url(&url).expect("valid test DB URL");
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password.clone(), cx))
            .await
            .expect("connect to test DB");

        // Isolated temp table with a row the filter excludes and a row it keeps.
        cx.update(|cx| {
            conn.execute(
                "create temp table zedium_filter_submit (id int primary key, name text)".into(),
                10,
                cx,
            )
        })
        .await
        .expect("create temp table");
        cx.update(|cx| {
            conn.execute(
                "insert into zedium_filter_submit values (1, 'drop'), (2, 'keep')".into(),
                10,
                cx,
            )
        })
        .await
        .expect("seed rows");

        let view = open_test_view(cx).await;
        let mut window_cx = VisualTestContext::from_window(cx.windows()[0], cx);
        let editability = Editability {
            schema: "pg_temp".into(),
            table: "zedium_filter_submit".into(),
            pk_cols: vec!["id".into()],
            col_types: [
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        // The grid is already showing only the filtered ("keep") row, as if
        // `FilterRequested` had already run — `submit_edits` must refresh
        // through that same filter, not the unfiltered base table.
        let result = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("2".into()), Some("keep".into())]],
            command_tag: None,
            truncated: false,
        };
        view.update_in(&mut window_cx, |view, window, cx| {
            view.commit_mode = CommitMode::Auto;
            view.connection = Some(conn.clone());
            view.editability = Some(editability);
            view.last_sql = Some("select id, name from zedium_filter_submit order by id".into());
            // Filter on `id`, not the column being edited: the edit changes
            // `name`, so a `name`-based filter would (correctly) exclude the
            // just-edited row post-commit and the test couldn't tell "filter
            // dropped" apart from "filter still applied, row legitimately no
            // longer matches".
            view.filter_expr = Some("\"id\" = 2".into());
            view.applied_filter = view.filter_expr.clone();
            view.results_grid.update(cx, |grid, cx| {
                grid.set_result(Some(result.clone()), false, cx);
                grid.pending_mut_for_test()
                    .stage_update(0, 1, Some("kept".into()));
            });
            view.result = Some(result);
            view.submit_edits(window, cx);
        });
        cx.run_until_parked();
        cx.simulate_prompt_answer("Submit");
        cx.run_until_parked();
        // The submit's success handler dispatches a nested refresh query
        // (`dispatch_query`), which sets `running` again and settles on its
        // own subsequent poll — wait for both hops to fully quiesce.
        for _ in 0..50 {
            if !view.read_with(cx, |view, _| view.running) {
                break;
            }
            cx.executor()
                .timer(std::time::Duration::from_millis(20))
                .await;
            cx.run_until_parked();
        }

        view.update(cx, |view, cx| {
            assert_eq!(view.error, None, "the Auto submit + refresh must succeed");
            assert!(
                view.results_grid.read(cx).pending_changes().is_empty(),
                "success clears the staged edit"
            );
            assert_eq!(
                view.filter_expr.as_deref(),
                Some("\"id\" = 2"),
                "the refresh must not clear the active filter"
            );
            assert_eq!(
                view.applied_filter, view.filter_expr,
                "filter stays applied"
            );
            let rows = &view
                .result
                .as_ref()
                .expect("refresh must produce a result")
                .rows;
            assert_eq!(
                rows,
                &vec![vec![Some("2".into()), Some("kept".into())]],
                "the refresh must dispatch through the filter (effective_base_sql), so the \
                 excluded 'drop' row must not reappear and the committed edit must be visible"
            );
        });
    }

    // Same DB-availability caveat as `manual_mode_allocates_session_on_first_dispatch`:
    // `TransactionSession` needs a real `Connection`, so without
    // `DATABASE_CLIENT_TEST_PG_URL` this only exercises the no-open-transaction
    // path (immediate switch, no prompt), which needs no DB.
    #[gpui::test]
    async fn switching_away_from_open_tx_defers_until_resolved(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            view.update(cx, |view, cx| {
                view.commit_mode = CommitMode::Manual;
                view.request_commit_mode_change(CommitMode::Auto, cx);
                assert_eq!(
                    view.commit_mode,
                    CommitMode::Auto,
                    "no open transaction means the switch happens immediately"
                );
                assert!(view.pending_commit_mode_switch.is_none());
            });
            return;
        };

        cx.executor().allow_parking();
        let profile = ConnectionProfile::from_url(&url).expect("valid test DB URL");
        let password = {
            use std::str::FromStr as _;
            tokio_postgres::Config::from_str(&url)
                .ok()
                .and_then(|config| {
                    config
                        .get_password()
                        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                })
        };
        let conn = cx
            .update(|cx| Connection::connect(profile, password, cx))
            .await
            .expect("connect to test DB");

        view.update(cx, |view, cx| {
            view.commit_mode = CommitMode::Manual;
            view.connection = Some(conn.clone());
            // Force `is_open()` true without a real `BEGIN` (there is no test
            // double for `Connection`, so a live one is used, but the round
            // trip through `begin` itself is not what this test exercises).
            let mut session = TransactionSession::new(conn);
            session.mark_open_for_test();
            view.session = Some(session);

            // With an open tx, requesting Auto must NOT switch yet.
            view.request_commit_mode_change(CommitMode::Auto, cx);
            assert_eq!(
                view.commit_mode,
                CommitMode::Manual,
                "an open transaction must block the immediate switch"
            );
            assert!(
                view.pending_commit_mode_switch.is_some(),
                "the pending switch must be recorded so the UI can prompt"
            );

            // Resolving via rollback must not flip the mode synchronously:
            // the ROLLBACK is async, and only its successful completion may
            // flip Manual -> Auto (see `resolve_pending_commit_mode_switch`).
            view.resolve_commit_mode_switch_with_rollback(cx);
            assert_eq!(
                view.commit_mode,
                CommitMode::Manual,
                "the mode must not flip until the async ROLLBACK actually succeeds"
            );
            assert!(
                view.pending_commit_mode_switch.is_some(),
                "the prompt must stay visible while the ROLLBACK is in flight"
            );
        });

        // Let the background ROLLBACK actually land against the test DB.
        cx.run_until_parked();

        view.update(cx, |view, _cx| {
            assert_eq!(
                view.commit_mode,
                CommitMode::Auto,
                "a successful ROLLBACK must complete the deferred switch"
            );
            assert!(view.pending_commit_mode_switch.is_none());
        });
    }

    fn one_column_result() -> QueryResult {
        QueryResult {
            columns: vec!["id".into()],
            rows: vec![vec![Some("1".into())]],
            command_tag: None,
            truncated: false,
        }
    }

    fn ascending_sort_on_id() -> ColumnSort {
        ColumnSort {
            column: 0,
            name: "id".into(),
            direction: SortDirection::Ascending,
        }
    }

    #[gpui::test]
    async fn failed_sort_requery_keeps_rows_and_reverts_sort(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        view.update(cx, |view, cx| {
            // A successful page-0 run…
            view.last_sql = Some("select 1 as id".into());
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Ok(one_column_result()),
                1,
                cx,
            );
            assert!(view.result.is_some());
            assert_eq!(view.applied_sort, None);

            // …then a sort re-query that the server rejects.
            view.sort = Some(ascending_sort_on_id());
            view.finish_query(
                QueryIntent::Sort,
                true,
                0,
                500,
                Err(anyhow::anyhow!("boom")),
                1,
                cx,
            );
            assert_eq!(view.error.as_deref(), Some("boom"));
            assert!(
                view.result.is_some(),
                "a failed sort must keep the loaded rows"
            );
            assert_eq!(
                view.sort, None,
                "the requested sort must revert to the applied one"
            );

            // A fresh-run failure still replaces the result and clears the sort.
            view.sort = Some(ascending_sort_on_id());
            view.applied_sort = view.sort.clone();
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Err(anyhow::anyhow!("bad sql")),
                1,
                cx,
            );
            assert!(view.result.is_none());
            assert_eq!(view.sort, None);
            assert_eq!(view.applied_sort, None);
        });
    }

    #[gpui::test]
    async fn run_records_history(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;
        let workspace_db = cx.update(|cx| workspace::WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| crate::query_history::HistoryDb::global(cx));

        view.update(cx, |view, cx| {
            view.workspace_id = Some(workspace_id);
            view.last_sql = Some("select 1 as id".to_string());
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Ok(one_column_result()),
                12,
                cx,
            );
        });
        cx.run_until_parked();

        let recent = history_db.recent(workspace_id, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].sql, "select 1 as id");
        assert!(recent[0].succeeded);
        assert_eq!(recent[0].row_count, 1);
        assert_eq!(recent[0].duration_ms, 12);
        assert_eq!(recent[0].error, None);
    }

    #[gpui::test]
    async fn failed_run_records_error(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;
        let workspace_db = cx.update(|cx| workspace::WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| crate::query_history::HistoryDb::global(cx));

        view.update(cx, |view, cx| {
            view.workspace_id = Some(workspace_id);
            view.last_sql = Some("boom".to_string());
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Err(anyhow::anyhow!("syntax error")),
                3,
                cx,
            );
        });
        cx.run_until_parked();

        let recent = history_db.recent(workspace_id, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert!(!recent[0].succeeded);
        assert_eq!(recent[0].error.as_deref(), Some("syntax error"));
    }

    #[gpui::test]
    async fn failed_load_more_keeps_rows(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        view.update(cx, |view, cx| {
            view.last_sql = Some("select 1 as id".into());
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Ok(one_column_result()),
                1,
                cx,
            );
            view.finish_query(
                QueryIntent::LoadMore,
                true,
                1,
                500,
                Err(anyhow::anyhow!("connection reset")),
                1,
                cx,
            );
            assert!(
                view.result.is_some(),
                "a failed Load more must keep the already-loaded rows"
            );
            assert_eq!(view.error.as_deref(), Some("connection reset"));
        });
    }

    #[gpui::test]
    async fn sort_click_is_ignored_while_a_query_is_in_flight(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let grid = view.update(cx, |view, cx| {
            view.last_sql = Some("select 1 as id".into());
            view.finish_query(
                QueryIntent::Run,
                true,
                0,
                500,
                Ok(one_column_result()),
                1,
                cx,
            );
            // A new run of edited SQL is now in flight; the grid still shows
            // the old result.
            view.running = true;
            view.results_grid.clone()
        });
        grid.update(cx, |_, cx| {
            cx.emit(ResultsGridEvent::SortRequested(
                Some(ascending_sort_on_id()),
            ));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.sort, None,
                "a header click during an in-flight run must be ignored"
            );
            assert_eq!(view.run_generation, 0, "no re-query must be dispatched");
        });
    }

    fn users_table_metadata() -> MetadataCache {
        MetadataCache {
            relations: vec![RelationMeta {
                schema: "public".into(),
                name: "users".into(),
                kind: RelationKind::Table,
                columns: vec![
                    ColumnMeta {
                        name: "id".into(),
                        data_type: "integer".into(),
                        is_primary_key: true,
                    },
                    ColumnMeta {
                        name: "name".into(),
                        data_type: "text".into(),
                        is_primary_key: false,
                    },
                ],
            }],
        }
    }

    #[gpui::test]
    async fn finish_query_run_makes_editable_grid_when_single_table_select(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let grid = view.update(cx, |view, cx| {
            *view.metadata.borrow_mut() = users_table_metadata();
            view.last_sql = Some("select id, name from users".into());
            view.finish_query(
                QueryIntent::Run,
                false,
                0,
                500,
                Ok(QueryResult {
                    columns: vec!["id".into(), "name".into()],
                    rows: vec![vec![Some("1".into()), Some("alice".into())]],
                    command_tag: None,
                    truncated: false,
                }),
                1,
                cx,
            );
            view.results_grid.clone()
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.editability.is_some(),
                "a single-table select over a PK'd table must be editable"
            );
        });
        grid.read_with(cx, |grid, _| {
            assert!(
                grid.is_editable(),
                "the grid must reflect the computed editability"
            );
        });
    }

    #[gpui::test]
    async fn finish_query_run_leaves_editability_none_for_aggregate(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let grid = view.update(cx, |view, cx| {
            *view.metadata.borrow_mut() = users_table_metadata();
            view.last_sql = Some("select count(*) from users".into());
            view.finish_query(
                QueryIntent::Run,
                false,
                0,
                500,
                Ok(QueryResult {
                    columns: vec!["count".into()],
                    rows: vec![vec![Some("1".into())]],
                    command_tag: None,
                    truncated: false,
                }),
                1,
                cx,
            );
            view.results_grid.clone()
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.editability.is_none(),
                "an aggregate query must not be editable"
            );
        });
        grid.read_with(cx, |grid, _| {
            assert!(
                !grid.is_editable(),
                "the grid must not become editable for a non-editable result"
            );
        });
    }

    #[gpui::test]
    async fn finish_query_run_failure_clears_editability(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        let grid = view.update(cx, |view, cx| {
            *view.metadata.borrow_mut() = users_table_metadata();
            view.last_sql = Some("select id, name from users".into());
            view.finish_query(
                QueryIntent::Run,
                false,
                0,
                500,
                Ok(QueryResult {
                    columns: vec!["id".into(), "name".into()],
                    rows: vec![vec![Some("1".into()), Some("alice".into())]],
                    command_tag: None,
                    truncated: false,
                }),
                1,
                cx,
            );
            assert!(view.editability.is_some());

            // A subsequent Run that fails must clear editability so a stale
            // grid doesn't stay editable over no result.
            view.finish_query(
                QueryIntent::Run,
                false,
                0,
                500,
                Err(anyhow::anyhow!("bad sql")),
                1,
                cx,
            );
            view.results_grid.clone()
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.editability.is_none(),
                "a failed run must clear editability"
            );
        });
        grid.read_with(cx, |grid, _| {
            assert!(
                !grid.is_editable(),
                "a failed run must leave the grid non-editable"
            );
        });
    }

    #[gpui::test]
    async fn editor_has_sql_completion_provider(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;
        view.read_with(cx, |view, cx| {
            assert!(
                view.editor.read(cx).completion_provider().is_some(),
                "the query editor must have the SQL completion provider attached"
            );
        });
    }

    #[gpui::test]
    fn candidate_to_completion_maps_kind_and_detail() {
        // The pure conversion helper: a Column candidate yields a completion
        // whose new_text is the column and whose documentation carries the type.
        let candidate = crate::connection::metadata_cache::Candidate {
            text: "email".into(),
            kind: crate::connection::metadata_cache::CandidateKind::Column,
            detail: Some("text".into()),
        };
        let label = candidate_label(&candidate);
        assert_eq!(label.text, "email");
        assert_eq!(candidate_detail(&candidate).as_deref(), Some("text"));
    }

    #[gpui::test]
    async fn load_more_keeps_the_session_page_size(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;

        view.update(cx, |view, cx| {
            // Pretend the session started when the setting was 123 rows/page.
            view.session_page_size = 123;
            view.has_more = true;
            view.last_sql = Some("select 1 as id".into());
            view.dispatch_query("select 1 as id".to_string(), 1, QueryIntent::LoadMore, cx);
            assert_eq!(
                view.session_page_size, 123,
                "Load more must keep computing offsets with the session's page size"
            );
        });
        view.update(cx, |view, cx| {
            // A fresh run starts a new session and re-reads the setting.
            view.running = false;
            view.dispatch_query("select 1 as id".to_string(), 0, QueryIntent::Run, cx);
            assert_eq!(
                view.session_page_size,
                crate::settings::DEFAULT_PAGE_SIZE,
                "a fresh run must snapshot the current setting"
            );
        });
    }

    #[gpui::test]
    async fn revert_clears_pending_changes(cx: &mut TestAppContext) {
        init_test(cx);
        let view = open_test_view(cx).await;
        view.update(cx, |view, cx| {
            view.results_grid.update(cx, |grid, cx| {
                let result = QueryResult {
                    columns: vec!["id".into()],
                    rows: vec![vec![Some("1".into())]],
                    command_tag: None,
                    truncated: false,
                };
                grid.set_result(Some(result), true, cx);
                grid.pending_mut_for_test()
                    .stage_update(0, 0, Some("9".into()));
            });
            view.revert_edits(cx);
            assert!(view.results_grid.read(cx).pending_changes().is_empty());
        });
    }

    #[test]
    fn build_transaction_orders_deletes_updates_inserts_and_wraps() {
        use crate::results_grid::PendingChanges;
        let editability = Editability {
            schema: "public".into(),
            table: "users".into(),
            pk_cols: vec!["id".into()],
            col_types: [
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![Some("1".into()), Some("al".into())],
            vec![Some("2".into()), Some("bo".into())],
        ];
        let mut pending = PendingChanges::default();
        pending.stage_update(0, 1, Some("ann".into())); // update row 0
        pending.deletes.insert(1); // delete row 1
        pending.inserts.push(vec![Some("3".into()), None]); // insert

        let sql = build_edit_transaction(&editability, &pending, &columns, &rows)
            .expect("non-empty pending yields a transaction");
        assert_eq!(
            sql,
            "BEGIN;\n\
             DELETE FROM \"public\".\"users\" WHERE \"id\" = '2'::integer;\n\
             UPDATE \"public\".\"users\" SET \"name\" = 'ann'::text WHERE \"id\" = '1'::integer;\n\
             INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES ('3'::integer, NULL);\n\
             COMMIT;"
        );
    }

    // Regression guard for the `build_edit_statements`/`build_edit_transaction`
    // refactor: the unwrapped statements must be exactly what
    // `build_edit_transaction` wraps in `BEGIN;…;COMMIT;`, and the wrapped
    // output must stay byte-identical to before the refactor.
    #[test]
    fn build_edit_statements_matches_transaction_wrapping() {
        use crate::results_grid::PendingChanges;
        let editability = Editability {
            schema: "public".into(),
            table: "users".into(),
            pk_cols: vec!["id".into()],
            col_types: [
                ("id".to_string(), "integer".to_string()),
                ("name".to_string(), "text".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![Some("1".into()), Some("al".into())],
            vec![Some("2".into()), Some("bo".into())],
        ];
        let mut pending = PendingChanges::default();
        pending.stage_update(0, 1, Some("ann".into())); // update row 0
        pending.deletes.insert(1); // delete row 1
        pending.inserts.push(vec![Some("3".into()), None]); // insert

        let statements = build_edit_statements(&editability, &pending, &columns, &rows)
            .expect("non-empty pending yields statements");
        assert_eq!(
            statements,
            vec![
                "DELETE FROM \"public\".\"users\" WHERE \"id\" = '2'::integer".to_string(),
                "UPDATE \"public\".\"users\" SET \"name\" = 'ann'::text WHERE \"id\" = '1'::integer"
                    .to_string(),
                "INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES ('3'::integer, NULL)"
                    .to_string(),
            ]
        );

        let mut expected_wrapped = String::from("BEGIN;\n");
        for statement in &statements {
            expected_wrapped.push_str(statement);
            expected_wrapped.push_str(";\n");
        }
        expected_wrapped.push_str("COMMIT;");
        let sql = build_edit_transaction(&editability, &pending, &columns, &rows)
            .expect("non-empty pending yields a transaction");
        assert_eq!(sql, expected_wrapped);
        assert_eq!(
            sql,
            "BEGIN;\n\
             DELETE FROM \"public\".\"users\" WHERE \"id\" = '2'::integer;\n\
             UPDATE \"public\".\"users\" SET \"name\" = 'ann'::text WHERE \"id\" = '1'::integer;\n\
             INSERT INTO \"public\".\"users\" (\"id\", \"name\") VALUES ('3'::integer, NULL);\n\
             COMMIT;"
        );

        assert!(
            build_edit_statements(&editability, &PendingChanges::default(), &columns, &rows)
                .is_none()
        );
    }

    #[test]
    fn build_transaction_skips_updates_on_deleted_rows_and_returns_none_when_empty() {
        use crate::results_grid::PendingChanges;
        let editability = Editability {
            schema: "public".into(),
            table: "t".into(),
            pk_cols: vec!["id".into()],
            col_types: [("id".to_string(), "integer".to_string())]
                .into_iter()
                .collect(),
        };
        let columns = vec!["id".to_string(), "v".to_string()];
        let rows = vec![vec![Some("1".into()), Some("x".into())]];
        let mut pending = PendingChanges::default();
        pending.stage_update(0, 1, Some("y".into()));
        pending.deletes.insert(0); // deleting the same row drops its update
        let sql = build_edit_transaction(&editability, &pending, &columns, &rows).unwrap();
        assert!(sql.contains("DELETE FROM \"public\".\"t\""));
        assert!(!sql.contains("UPDATE"), "a deleted row's update is skipped");

        assert!(
            build_edit_transaction(&editability, &PendingChanges::default(), &columns, &rows)
                .is_none()
        );
    }

    #[test]
    fn export_payload_serializes_current_result_by_format() {
        let result = QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), None]],
            command_tag: None,
            truncated: false,
        };
        let csv = export_payload(&result, ExportFormat::Csv, "\\N");
        assert_eq!(csv, "id,name\r\n1,\\N\r\n");
        let sql = export_payload(&result, ExportFormat::Sql, "");
        assert_eq!(
            sql,
            "INSERT INTO \"exported_table\" (\"id\", \"name\") VALUES ('1', NULL);\n"
        );
    }

    /// Builds a `QueryView` (no live connection) with `sql` recorded as
    /// `last_sql` and a `QueryResult` for `columns` already installed via
    /// `finish_query(QueryIntent::Run, ...)`, as if a fresh run had just
    /// completed. `workspace` is `WeakEntity::new_invalid()` — nothing in the
    /// filter/value-viewer paths needs a live workspace.
    fn build_view_with_result(
        cx: &mut TestAppContext,
        sql: &str,
        columns: &[&str],
    ) -> Entity<QueryView> {
        let (view, cx) = cx.add_window_view(|window, cx| {
            QueryView::new(
                Some(test_profile()),
                None,
                sql.to_string(),
                WeakEntity::new_invalid(),
                None,
                window,
                cx,
            )
        });
        let result = QueryResult {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows: vec![
                columns
                    .iter()
                    .enumerate()
                    .map(|(i, _)| Some(format!("v{i}")))
                    .collect(),
            ],
            command_tag: None,
            truncated: false,
        };
        view.update(cx, |view, cx| {
            view.last_sql = Some(sql.to_string());
            view.finish_query(QueryIntent::Run, true, 0, 500, Ok(result), 1, cx);
        });
        view
    }

    #[gpui::test]
    fn filter_request_applies_where_and_preserves_sort_on_dispatch(cx: &mut TestAppContext) {
        init_test(cx);
        let view = build_view_with_result(cx, "select * from users", &["id", "name"]);
        view.update(cx, |view, cx| {
            view.applied_sort = Some(ColumnSort {
                column: 0,
                name: "id".into(),
                direction: crate::sql_paging::SortDirection::Ascending,
            });
            view.sort = view.applied_sort.clone();
            view.apply_filter_request("\"name\" IS NULL".to_string(), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.filter_expr.as_deref(), Some("\"name\" IS NULL"));
            // The base sql stays unfiltered; the effective sql the dispatch
            // builds wraps it in the filter subquery, still wrappable + sorted.
            let effective = view.effective_base_sql();
            assert_eq!(
                effective,
                "select * from (\nselect * from users\n) zedium_filter where \"name\" IS NULL"
            );
            assert!(crate::sql_paging::wrappable(&effective));
            assert_eq!(
                view.sort, view.applied_sort,
                "the sort must survive the filter re-query dispatch"
            );
        });
    }

    #[gpui::test]
    fn failed_filter_reverts_to_applied_filter_and_keeps_rows(cx: &mut TestAppContext) {
        init_test(cx);
        let view = build_view_with_result(cx, "select * from users", &["id", "name"]);
        view.update(cx, |view, cx| {
            view.filter_expr = Some("bogus(".into());
            view.applied_filter = None;
            view.finish_query(
                QueryIntent::Filter,
                true,
                0,
                10,
                Err(anyhow::anyhow!("syntax error at or near \"(\"")),
                5,
                cx,
            );
        });
        view.read_with(cx, |view, _| {
            assert!(
                view.error
                    .as_deref()
                    .is_some_and(|e| e.contains("syntax error"))
            );
            assert_eq!(view.filter_expr, None, "failed filter reverts to applied");
            assert!(view.result.is_some(), "prior rows are kept");
        });
    }

    /// `run_sql` (Run/RunScript) dispatches the raw editor SQL, never through
    /// `effective_base_sql()`, so an active filter can never actually be
    /// re-applied on a Run. Regression test for the desync where a stale
    /// `filter_expr`/`applied_filter` survived a same-SQL re-run: the filter
    /// bar kept claiming a filter that the (unfiltered) dispatched rows never
    /// reflected. A fresh Run must reset the filter, exactly like `sort`.
    #[gpui::test]
    fn run_resets_filter_state_so_it_cannot_desync(cx: &mut TestAppContext) {
        init_test(cx);
        let view = build_view_with_result(cx, "select * from users", &["id", "name"]);
        view.update(cx, |view, cx| {
            view.apply_filter_request("\"name\" IS NULL".to_string(), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.filter_expr.as_deref(), Some("\"name\" IS NULL"));
        });
        view.update(cx, |view, cx| {
            // Simulate the filter request's own re-query having settled, as
            // `finish_query`'s `QueryIntent::Filter` success branch would.
            view.applied_filter = view.filter_expr.clone();
            // Re-running the SAME sql is the exact scenario the old code
            // missed: the `last_sql != sql` guard skipped clearing the filter
            // entirely when the statement was unchanged.
            view.run_sql("select * from users".to_string(), cx);
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.filter_expr, None, "a fresh Run must reset the filter");
            assert_eq!(
                view.applied_filter, None,
                "applied_filter must not keep claiming a filter Run never re-applied"
            );
        });
    }

    #[gpui::test]
    fn cell_selected_event_populates_value_viewer(cx: &mut TestAppContext) {
        init_test(cx);
        let view = build_view_with_result(cx, "select * from users", &["id", "name"]);
        view.update(cx, |view, cx| {
            view.on_cell_selected(Some("hello".into()), "text".into(), cx);
        });
        view.read_with(cx, |view, cx| {
            view.value_viewer.read_with(cx, |viewer, _| {
                assert_eq!(viewer.value_for_test(), Some("hello".to_string()));
            });
        });
    }
}
