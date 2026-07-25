use anyhow::Result;
use db::{
    query,
    sqlez::{
        bindable::Column, domain::Domain, statement::Statement,
        thread_safe_connection::ThreadSafeConnection,
    },
    sqlez_macros::sql,
};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, ParentElement,
    Render, Styled, Task, WeakEntity, Window, rems,
};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{Color, Label, LabelSize, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, Workspace, WorkspaceDb, WorkspaceId};

use crate::query_view::QueryView;

pub struct HistoryDb(ThreadSafeConnection);

impl Domain for HistoryDb {
    const NAME: &str = stringify!(HistoryDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE database_query_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id INTEGER NOT NULL,
            profile_id TEXT NOT NULL,
            database TEXT,
            sql TEXT NOT NULL,
            executed_at INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL,
            succeeded INTEGER NOT NULL,
            error TEXT,
            row_count INTEGER NOT NULL,
            FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
            ON DELETE CASCADE
        ) STRICT;
    )];
}

db::static_connection!(HistoryDb, [WorkspaceDb]);

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub profile_id: String,
    pub database: Option<String>,
    pub sql: String,
    pub executed_at: i64,
    pub duration_ms: i64,
    pub succeeded: bool,
    pub error: Option<String>,
    pub row_count: i64,
}

impl Column for HistoryEntry {
    fn column(statement: &mut Statement, start_index: i32) -> Result<(Self, i32)> {
        let (id, next_index): (i64, i32) = Column::column(statement, start_index)?;
        let (profile_id, next_index): (String, i32) = Column::column(statement, next_index)?;
        let (database, next_index): (Option<String>, i32) = Column::column(statement, next_index)?;
        let (sql, next_index): (String, i32) = Column::column(statement, next_index)?;
        let (executed_at, next_index): (i64, i32) = Column::column(statement, next_index)?;
        let (duration_ms, next_index): (i64, i32) = Column::column(statement, next_index)?;
        let (succeeded, next_index): (i64, i32) = Column::column(statement, next_index)?;
        let (error, next_index): (Option<String>, i32) = Column::column(statement, next_index)?;
        let (row_count, next_index): (i64, i32) = Column::column(statement, next_index)?;
        Ok((
            HistoryEntry {
                id,
                profile_id,
                database,
                sql,
                executed_at,
                duration_ms,
                succeeded: succeeded != 0,
                error,
                row_count,
            },
            next_index,
        ))
    }
}

impl HistoryDb {
    // The shared contract's `record(&self, entry)` omits `workspace_id`, which the
    // table (and `recent`/`prune`) require; it is passed explicitly here to match.
    pub async fn record(&self, workspace_id: WorkspaceId, entry: HistoryEntry) -> Result<()> {
        self.write(move |conn| {
            let query_str = "INSERT INTO database_query_history
                (workspace_id, profile_id, database, sql, executed_at, duration_ms, succeeded, error, row_count)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
            let mut statement = Statement::prepare(conn, query_str)?;
            let mut next_index = statement.bind(&workspace_id, 1)?;
            next_index = statement.bind(&entry.profile_id, next_index)?;
            next_index = statement.bind(&entry.database, next_index)?;
            next_index = statement.bind(&entry.sql, next_index)?;
            next_index = statement.bind(&entry.executed_at, next_index)?;
            next_index = statement.bind(&entry.duration_ms, next_index)?;
            next_index = statement.bind(&(entry.succeeded as i64), next_index)?;
            next_index = statement.bind(&entry.error, next_index)?;
            statement.bind(&entry.row_count, next_index)?;
            statement.exec()
        })
        .await
    }

    pub async fn prune(&self, workspace_id: WorkspaceId, keep: usize) -> Result<()> {
        self.write(move |conn| {
            let query_str = "DELETE FROM database_query_history
                WHERE workspace_id = ?1
                AND id NOT IN (
                    SELECT id FROM database_query_history
                    WHERE workspace_id = ?2
                    ORDER BY executed_at DESC, id DESC
                    LIMIT ?3
                )";
            let mut statement = Statement::prepare(conn, query_str)?;
            let mut next_index = statement.bind(&workspace_id, 1)?;
            next_index = statement.bind(&workspace_id, next_index)?;
            statement.bind(&keep, next_index)?;
            statement.exec()
        })
        .await
    }

    query! {
        pub fn recent(workspace_id: WorkspaceId, limit: usize) -> Result<Vec<HistoryEntry>> {
            SELECT id, profile_id, database, sql, executed_at, duration_ms, succeeded, error, row_count
            FROM database_query_history
            WHERE workspace_id = ?
            ORDER BY executed_at DESC, id DESC
            LIMIT ?
        }
    }
}

/// Indices of `entries` whose SQL contains `query` (case-insensitive). Empty
/// `query` returns all indices. Order is preserved, so a recent-first input
/// stays recent-first.
pub(crate) fn filter_entries(entries: &[HistoryEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let needle = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.sql.to_lowercase().contains(&needle))
        .map(|(index, _)| index)
        .collect()
}

pub(crate) struct QueryHistoryModal {
    picker: Entity<Picker<HistoryDelegate>>,
}

impl QueryHistoryModal {
    fn new(
        entries: Vec<HistoryEntry>,
        query_view: WeakEntity<QueryView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let matches = filter_entries(&entries, "");
        let delegate = HistoryDelegate {
            modal: cx.entity().downgrade(),
            query_view,
            entries,
            matches,
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for QueryHistoryModal {}
impl EventEmitter<DismissEvent> for QueryHistoryModal {}

impl Focusable for QueryHistoryModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for QueryHistoryModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("QueryHistoryModal")
            .w(rems(44.))
            .child(self.picker.clone())
    }
}

pub(crate) struct HistoryDelegate {
    modal: WeakEntity<QueryHistoryModal>,
    query_view: WeakEntity<QueryView>,
    entries: Vec<HistoryEntry>,
    matches: Vec<usize>,
    selected_index: usize,
}

impl PickerDelegate for HistoryDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "database query history"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search query history…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        self.matches = filter_entries(&self.entries, &query);
        self.selected_index = 0;
        Task::ready(())
    }

    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(&entry_index) = self.matches.get(self.selected_index) else {
            return;
        };
        let Some(entry) = self.entries.get(entry_index) else {
            return;
        };
        let sql = entry.sql.clone();
        self.query_view
            .update(cx, |view, cx| view.load_sql(sql, secondary, window, cx))
            .log_err();
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry_index = *self.matches.get(index)?;
        let entry = self.entries.get(entry_index)?;
        let first_line = entry.sql.lines().next().unwrap_or("").to_string();
        let (status_glyph, status_color) = if entry.succeeded {
            ("✓", Color::Success)
        } else {
            ("✗", Color::Error)
        };
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .gap_2()
                        .child(Label::new(first_line).single_line())
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Label::new(status_glyph)
                                        .size(LabelSize::Small)
                                        .color(status_color),
                                )
                                .child(
                                    Label::new(format!("{} ms", entry.duration_ms))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        ),
                ),
        )
    }
}

pub(crate) fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(query_view) = workspace.active_item_as::<QueryView>(cx) else {
        return;
    };
    let Some(workspace_id) = workspace.database_id() else {
        return;
    };
    let entries = HistoryDb::global(cx)
        .recent(workspace_id, 500)
        .log_err()
        .unwrap_or_default();
    let query_view = query_view.downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        QueryHistoryModal::new(entries, query_view, window, cx)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use workspace::WorkspaceDb;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(db::AppDatabase::test_new()));
    }

    fn sample_entry(sql: &str) -> HistoryEntry {
        HistoryEntry {
            id: 0,
            profile_id: "p1".to_string(),
            database: None,
            sql: sql.to_string(),
            executed_at: 0,
            duration_ms: 0,
            succeeded: true,
            error: None,
            row_count: 0,
        }
    }

    #[test]
    fn filter_entries_matches_sql_case_insensitively() {
        let entries = vec![
            sample_entry("SELECT * FROM users"),
            sample_entry("INSERT INTO orders VALUES (1)"),
            sample_entry("select id from USERS where id = 1"),
        ];
        assert_eq!(filter_entries(&entries, "users"), vec![0, 2]);
        assert_eq!(filter_entries(&entries, ""), vec![0, 1, 2]);
        assert_eq!(filter_entries(&entries, "orders"), vec![1]);
        assert!(filter_entries(&entries, "no-such-token").is_empty());
    }

    fn entry(sql: &str, executed_at: i64) -> HistoryEntry {
        HistoryEntry {
            id: 0,
            profile_id: "p1".to_string(),
            database: Some("db".to_string()),
            sql: sql.to_string(),
            executed_at,
            duration_ms: 5,
            succeeded: true,
            error: None,
            row_count: 1,
        }
    }

    #[gpui::test]
    async fn recent_returns_newest_first(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| HistoryDb::global(cx));

        history_db
            .record(workspace_id, entry("select 1", 100))
            .await
            .unwrap();
        history_db
            .record(workspace_id, entry("select 2", 300))
            .await
            .unwrap();
        history_db
            .record(workspace_id, entry("select 3", 200))
            .await
            .unwrap();

        let recent = history_db.recent(workspace_id, 10).unwrap();
        let sqls: Vec<_> = recent.iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sqls, vec!["select 2", "select 3", "select 1"]);
    }

    #[gpui::test]
    async fn round_trips_failure_and_row_count(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| HistoryDb::global(cx));

        history_db
            .record(
                workspace_id,
                HistoryEntry {
                    id: 0,
                    profile_id: "p1".to_string(),
                    database: None,
                    sql: "boom".to_string(),
                    executed_at: 500,
                    duration_ms: 7,
                    succeeded: false,
                    error: Some("syntax error".to_string()),
                    row_count: 0,
                },
            )
            .await
            .unwrap();

        let recent = history_db.recent(workspace_id, 10).unwrap();
        let latest = recent.first().unwrap();
        assert!(!latest.succeeded);
        assert_eq!(latest.error.as_deref(), Some("syntax error"));
        assert_eq!(latest.database, None);
        assert_eq!(latest.duration_ms, 7);
        assert_eq!(latest.row_count, 0);
    }

    #[gpui::test]
    async fn recent_honors_limit_and_workspace_scope(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_a = workspace_db.next_id().await.unwrap();
        let workspace_b = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| HistoryDb::global(cx));

        history_db
            .record(workspace_a, entry("a1", 100))
            .await
            .unwrap();
        history_db
            .record(workspace_a, entry("a2", 200))
            .await
            .unwrap();
        history_db
            .record(workspace_b, entry("b1", 300))
            .await
            .unwrap();

        let recent_a = history_db.recent(workspace_a, 1).unwrap();
        assert_eq!(recent_a.len(), 1);
        assert_eq!(recent_a[0].sql, "a2");

        let recent_b = history_db.recent(workspace_b, 10).unwrap();
        assert_eq!(recent_b.len(), 1);
        assert_eq!(recent_b[0].sql, "b1");
    }

    #[gpui::test]
    async fn prune_keeps_newest(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| HistoryDb::global(cx));

        for (sql, at) in [("a", 100_i64), ("b", 200), ("c", 300), ("d", 400)] {
            history_db
                .record(workspace_id, entry(sql, at))
                .await
                .unwrap();
        }

        history_db.prune(workspace_id, 2).await.unwrap();

        let recent = history_db.recent(workspace_id, 10).unwrap();
        let sqls: Vec<_> = recent.iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sqls, vec!["d", "c"]);
    }

    #[gpui::test]
    async fn prune_only_touches_its_workspace(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_a = workspace_db.next_id().await.unwrap();
        let workspace_b = workspace_db.next_id().await.unwrap();
        let history_db = cx.update(|cx| HistoryDb::global(cx));

        history_db
            .record(workspace_a, entry("a1", 100))
            .await
            .unwrap();
        history_db
            .record(workspace_a, entry("a2", 200))
            .await
            .unwrap();
        history_db
            .record(workspace_b, entry("b1", 300))
            .await
            .unwrap();

        history_db.prune(workspace_a, 1).await.unwrap();

        assert_eq!(history_db.recent(workspace_a, 10).unwrap().len(), 1);
        assert_eq!(history_db.recent(workspace_b, 10).unwrap().len(), 1);
    }
}
