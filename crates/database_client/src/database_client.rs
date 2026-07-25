//! Postgres database client: schema explorer panel + query tabs.

use gpui::{App, actions};
use workspace::{Workspace, register_serializable_item};

mod column_meta;
mod connection;
mod connection_modal;
pub mod ddl_view;
mod export;
mod object_search;
mod panel;
mod persistence;
mod query_history;
pub mod query_view;
pub mod results_grid;
pub mod settings;
pub mod sql_paging;
mod sql_statements;
mod tree;
pub mod value_viewer;

pub use connection::client::DEFAULT_RESULT_LIMIT;
pub use ddl_view::DdlView;
pub use panel::DatabasePanel;
pub use query_view::QueryView;

actions!(
    database_client,
    [
        /// Toggles focus on the database panel.
        ToggleFocus,
        /// Opens the new-connection dialog.
        NewConnection,
        /// Expands the selected database-panel node, or moves to its first child.
        ExpandSelectedEntry,
        /// Collapses the selected database-panel node, or moves to its parent.
        CollapseSelectedEntry,
        /// Opens the query history picker for the active query tab.
        ShowQueryHistory,
        /// Searches connected objects by name and reveals the match in the tree.
        SearchObjects,
    ]
);

/// Initialize the database client: register actions, panel, and items.
pub fn init(cx: &mut App) {
    query_view::register_keybindings(cx);
    results_grid::register_keybindings(cx);
    panel::register_keybindings(cx);
    register_serializable_item::<query_view::QueryView>(cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<panel::DatabasePanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &ShowQueryHistory, window, cx| {
            query_history::toggle(workspace, window, cx);
        });
        workspace.register_action(|workspace, _: &SearchObjects, window, cx| {
            object_search::toggle(workspace, window, cx);
        });
    })
    .detach();
}
