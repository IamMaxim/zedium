use std::collections::{HashMap, HashSet};

use crate::{
    CollapseSelectedEntry, ExpandSelectedEntry, ToggleFocus,
    connection::{
        client::Connection,
        ddl::{DdlTarget, ddl_target_for_relation},
        introspect::RelationKind,
        profile::ConnectionProfile,
        store,
    },
    ddl_view::DdlView,
    object_search,
    settings::DatabaseClientSettings,
    tree::{NodeKind, TreeNode, TreeState},
};
use anyhow::Result;
use editor::{Editor, EditorEvent, EditorSettingsScrollbarProxy};
use gpui::{
    AnyElement, App, AsyncWindowContext, Bounds, ClickEvent, ClipboardItem, Context, DismissEvent,
    Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding, MouseDownEvent,
    ParentElement, Pixels, Point, Render, ScrollStrategy, Styled, Subscription,
    UniformListScrollHandle, WeakEntity, Window, anchored, deferred, div, point, px, size,
    uniform_list,
};
use menu::{Confirm, SelectNext, SelectPrevious};
use settings::Settings as _;
use smallvec::SmallVec;
use std::ops::Range;
use theme_settings::ThemeSettings;
use ui::{
    ContextMenu, ContextMenuEntry, Disclosure, IconName, IndentGuideColors, ListItem,
    ListItemSpacing, Scrollbars, Tooltip, WithScrollbar, prelude::*,
};
use util::ResultExt;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

pub struct DatabasePanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    profiles: Vec<ConnectionProfile>,
    tree: TreeState,
    /// Live connections, keyed `"{profile_id}/{database}"`.
    connections: HashMap<String, Connection>,
    /// Password read from the keychain on first connect, cached per profile so
    /// the same server's other databases reuse it (the keychain key embeds the
    /// database name, so a fresh read for another db would miss).
    passwords: HashMap<String, Option<String>>,
    errors: HashMap<String, String>,
    loading: HashSet<String>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    selected_node_id: Option<String>,
    scroll_handle: UniformListScrollHandle,
    /// Live subsequence filter over the tree, edited via `filter_editor`.
    tree_filter: String,
    filter_editor: Entity<Editor>,
    _filter_subscription: Subscription,
}

pub(crate) fn register_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some("DatabasePanel")),
        KeyBinding::new("down", SelectNext, Some("DatabasePanel")),
        KeyBinding::new("right", ExpandSelectedEntry, Some("DatabasePanel")),
        KeyBinding::new("left", CollapseSelectedEntry, Some("DatabasePanel")),
        KeyBinding::new("enter", Confirm, Some("DatabasePanel")),
    ]);
}

impl DatabasePanel {
    pub fn new(
        _workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let weak = cx.entity().downgrade();
        cx.new(|cx| {
            let profiles = store::load_profiles();
            let tree = Self::build_tree(&profiles);
            let filter_editor = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("Filter…", window, cx);
                editor
            });
            let filter_subscription =
                cx.subscribe(&filter_editor, |this: &mut Self, editor, event, cx| {
                    if matches!(event, EditorEvent::BufferEdited) {
                        this.tree_filter = editor.read(cx).text(cx);
                        cx.notify();
                    }
                });
            DatabasePanel {
                focus_handle: cx.focus_handle(),
                workspace: weak,
                profiles,
                tree,
                connections: HashMap::new(),
                passwords: HashMap::new(),
                errors: HashMap::new(),
                loading: HashSet::new(),
                context_menu: None,
                selected_node_id: None,
                scroll_handle: UniformListScrollHandle::new(),
                tree_filter: String::new(),
                filter_editor,
                _filter_subscription: filter_subscription,
            }
        })
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            DatabasePanel::new(workspace, window, cx)
        })
    }

    /// Visible tree nodes narrowed to the live header filter (fuzzy
    /// subsequence match, keeping ancestors of any match).
    fn filtered_nodes(&self) -> Vec<TreeNode> {
        object_search::filter_visible_nodes(self.tree.visible_nodes(), &self.tree_filter)
    }

    fn build_tree(profiles: &[ConnectionProfile]) -> TreeState {
        TreeState::new(
            profiles
                .iter()
                .map(|p| (p.id.clone(), p.name.clone()))
                .collect(),
        )
    }

    pub(crate) fn reload_profiles(&mut self, cx: &mut Context<Self>) {
        self.profiles = store::load_profiles();
        self.tree = Self::build_tree(&self.profiles);
        self.connections.clear();
        self.passwords.clear();
        self.errors.clear();
        self.loading.clear();
        self.selected_node_id = None;
        cx.notify();
    }

    fn toggle_node(&mut self, node_id: String, kind: NodeKind, cx: &mut Context<Self>) {
        if matches!(
            kind,
            NodeKind::Column
                | NodeKind::Empty
                | NodeKind::Function
                | NodeKind::Sequence
                | NodeKind::Index
                | NodeKind::ForeignKey
                | NodeKind::Constraint
                | NodeKind::Trigger
        ) {
            return;
        }
        self.tree.toggle(&node_id);

        if self.tree.is_expanded(&node_id) {
            match kind {
                NodeKind::Connection => self.expand_connection(node_id, cx),
                NodeKind::Database => self.expand_database(node_id, cx),
                NodeKind::Schema => self.expand_schema(node_id, cx),
                NodeKind::ObjectGroup(kind) => {
                    if node_id.split('/').count() >= 6 {
                        self.expand_table_group(node_id, kind, cx);
                    } else {
                        self.expand_schema_group(node_id, kind, cx);
                    }
                }
                NodeKind::Relation => self.expand_relation(node_id, cx),
                NodeKind::Column
                | NodeKind::Index
                | NodeKind::ForeignKey
                | NodeKind::Constraint
                | NodeKind::Function
                | NodeKind::Sequence
                | NodeKind::Trigger
                | NodeKind::Empty => {}
            }
        }

        cx.notify();
    }

    fn refresh_node(&mut self, node_id: String, kind: NodeKind, cx: &mut Context<Self>) {
        self.tree.clear_subtree(&node_id);
        self.loading
            .retain(|k| k != &node_id && !k.starts_with(&format!("{node_id}/")));
        if self.tree.is_expanded(&node_id) {
            match kind {
                NodeKind::Schema => {}
                NodeKind::ObjectGroup(group) => {
                    if node_id.split('/').count() >= 6 {
                        self.expand_table_group(node_id.clone(), group, cx);
                    } else {
                        self.expand_schema_group(node_id.clone(), group, cx);
                    }
                }
                NodeKind::Relation => self.expand_relation(node_id.clone(), cx),
                _ => {}
            }
        }
        cx.notify();
    }

    fn expand_connection(&mut self, profile_id: String, cx: &mut Context<Self>) {
        if self.tree.databases_loaded(&profile_id) || self.loading.contains(&profile_id) {
            return;
        }

        let Some(profile) = self.profiles.iter().find(|p| p.id == profile_id).cloned() else {
            return;
        };

        self.loading.insert(profile_id.clone());

        cx.spawn(async move |this, cx| {
            let password: Option<String> = cx
                .update(|cx| store::read_password(cx, &profile))
                .await
                .ok()
                .flatten();

            let conn_result: Result<Connection> = cx
                .update(|cx| Connection::connect(profile.clone(), password.clone(), cx))
                .await;

            match conn_result {
                Ok(conn) => {
                    let databases_result = cx.update(|cx| conn.list_databases(cx)).await;
                    this.update(cx, |this, cx| {
                        this.loading.remove(&profile_id);
                        match databases_result {
                            Ok(databases) => {
                                this.passwords.insert(profile_id.clone(), password);
                                this.connections
                                    .insert(format!("{}/{}", profile_id, profile.database), conn);
                                this.tree.set_databases(
                                    &profile_id,
                                    databases.into_iter().map(|d| d.name).collect(),
                                );
                                this.errors.remove(&profile_id);
                            }
                            Err(err) => {
                                this.errors.insert(profile_id.clone(), err.to_string());
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.loading.remove(&profile_id);
                        this.errors.insert(profile_id.clone(), err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn expand_database(&mut self, db_id: String, cx: &mut Context<Self>) {
        if self.tree.schemas_loaded(&db_id) || self.loading.contains(&db_id) {
            return;
        }

        let mut parts = db_id.splitn(2, '/');
        let (Some(conn_id), Some(db_name)) = (parts.next(), parts.next()) else {
            return;
        };
        let conn_id = conn_id.to_string();
        let db_name = db_name.to_string();

        let existing = self.connections.get(&db_id).cloned();
        let profile = self.profiles.iter().find(|p| p.id == conn_id).cloned();

        enum ConnectionSource {
            Existing(Connection),
            Connect(ConnectionProfile),
        }
        let source = match (existing, profile) {
            (Some(connection), _) => ConnectionSource::Existing(connection),
            (None, Some(mut profile)) => {
                // Same server, different database: clone the profile onto it.
                profile.database = db_name;
                ConnectionSource::Connect(profile)
            }
            (None, None) => return,
        };
        let password = self.passwords.get(&conn_id).cloned().flatten();

        self.loading.insert(db_id.clone());

        cx.spawn(async move |this, cx| {
            let conn_result: Result<Connection> = match source {
                ConnectionSource::Existing(conn) => Ok(conn),
                ConnectionSource::Connect(profile) => {
                    cx.update(|cx| Connection::connect(profile, password, cx))
                        .await
                }
            };

            match conn_result {
                Ok(conn) => {
                    let schemas_result = cx.update(|cx| conn.list_schemas(cx)).await;
                    this.update(cx, |this, cx| {
                        this.loading.remove(&db_id);
                        match schemas_result {
                            Ok(schemas) => {
                                this.connections.insert(db_id.clone(), conn);
                                this.tree.set_schemas(
                                    &db_id,
                                    schemas.into_iter().map(|s| s.name).collect(),
                                );
                                this.errors.remove(&db_id);
                            }
                            Err(err) => {
                                log::error!("list_schemas for {db_id}: {err}");
                                this.connections.remove(&db_id);
                                this.errors.insert(db_id.clone(), err.to_string());
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.loading.remove(&db_id);
                        this.errors.insert(db_id.clone(), err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn expand_schema(&mut self, schema_id: String, cx: &mut Context<Self>) {
        if self.tree.relations_loaded(&schema_id) || self.loading.contains(&schema_id) {
            return;
        }

        let mut parts = schema_id.splitn(3, '/');
        let (Some(conn_id), Some(db_name), Some(schema_name)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let conn_id = conn_id.to_string();
        let db_key = format!("{conn_id}/{db_name}");
        let schema_name = schema_name.to_string();

        let Some(conn) = self.connections.get(&db_key).cloned() else {
            return;
        };

        self.loading.insert(schema_id.clone());

        cx.spawn(async move |this, cx| {
            let relations_result = cx
                .update(|cx| conn.list_relations(schema_name.clone(), cx))
                .await;
            this.update(cx, |this, cx| {
                this.loading.remove(&schema_id);
                match relations_result {
                    Ok(relations) => {
                        this.tree.set_relations(&schema_id, relations);
                        this.errors.remove(&schema_id);
                    }
                    Err(err) => {
                        log::error!("list_relations for {schema_id}: {err}");
                        // Query failure implies a dead connection; reconnect on next expand.
                        this.mark_connection_stale(&conn_id);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn expand_schema_group(
        &mut self,
        group_id: String,
        kind: crate::tree::ObjectGroupKind,
        cx: &mut Context<Self>,
    ) {
        use crate::tree::ObjectGroupKind;
        let segments: Vec<&str> = group_id.split('/').collect();
        let [conn_id, db_name, schema_name, _group_key] = segments.as_slice() else {
            return;
        };
        let conn_id = conn_id.to_string();
        let db_key = format!("{conn_id}/{db_name}");
        let schema_id = format!("{conn_id}/{db_name}/{schema_name}");
        let schema_name = schema_name.to_string();

        let already_loaded = match kind {
            ObjectGroupKind::Tables | ObjectGroupKind::Views => {
                self.tree.relations_loaded(&schema_id)
            }
            ObjectGroupKind::MaterializedViews => self.tree.materialized_views_loaded(&schema_id),
            ObjectGroupKind::Functions => self.tree.functions_loaded(&schema_id),
            ObjectGroupKind::Sequences => self.tree.sequences_loaded(&schema_id),
            _ => true,
        };
        if already_loaded || self.loading.contains(&group_id) {
            return;
        }
        let Some(conn) = self.connections.get(&db_key).cloned() else {
            return;
        };
        self.loading.insert(group_id.clone());

        cx.spawn(async move |this, cx| {
            enum GroupData {
                Relations(Vec<crate::connection::introspect::RelationInfo>),
                MaterializedViews(Vec<crate::connection::introspect::RelationInfo>),
                Functions(Vec<crate::connection::introspect::FunctionInfo>),
                Sequences(Vec<crate::connection::introspect::SequenceInfo>),
            }
            let result: Result<GroupData> = match kind {
                ObjectGroupKind::Tables | ObjectGroupKind::Views => cx
                    .update(|cx| conn.list_relations(schema_name.clone(), cx))
                    .await
                    .map(GroupData::Relations),
                ObjectGroupKind::MaterializedViews => cx
                    .update(|cx| conn.list_materialized_views(schema_name.clone(), cx))
                    .await
                    .map(GroupData::MaterializedViews),
                ObjectGroupKind::Functions => cx
                    .update(|cx| conn.list_functions(schema_name.clone(), cx))
                    .await
                    .map(GroupData::Functions),
                ObjectGroupKind::Sequences => cx
                    .update(|cx| conn.list_sequences(schema_name.clone(), cx))
                    .await
                    .map(GroupData::Sequences),
                _ => return,
            };
            this.update(cx, |this, cx| {
                this.loading.remove(&group_id);
                match result {
                    Ok(GroupData::Relations(relations)) => {
                        this.tree.set_relations(&schema_id, relations);
                        this.errors.remove(&group_id);
                    }
                    Ok(GroupData::MaterializedViews(views)) => {
                        this.tree.set_materialized_views(&schema_id, views);
                        this.errors.remove(&group_id);
                    }
                    Ok(GroupData::Functions(functions)) => {
                        this.tree.set_functions(&schema_id, functions);
                        this.errors.remove(&group_id);
                    }
                    Ok(GroupData::Sequences(sequences)) => {
                        this.tree.set_sequences(&schema_id, sequences);
                        this.errors.remove(&group_id);
                    }
                    Err(err) => {
                        log::error!("load object group {group_id}: {err}");
                        this.mark_connection_stale(&conn_id);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn expand_table_group(
        &mut self,
        group_id: String,
        kind: crate::tree::ObjectGroupKind,
        cx: &mut Context<Self>,
    ) {
        use crate::tree::ObjectGroupKind;
        let segments: Vec<&str> = group_id.split('/').collect();
        let [conn_id, db_name, schema_name, group, table_name, _subkey] = segments.as_slice()
        else {
            return;
        };
        let conn_id = conn_id.to_string();
        let db_key = format!("{conn_id}/{db_name}");
        let schema_name = schema_name.to_string();
        let table_name = table_name.to_string();
        let rel_id = format!("{conn_id}/{db_name}/{schema_name}/{group}/{table_name}");

        if self.tree.table_group_loaded(&rel_id, kind) || self.loading.contains(&group_id) {
            return;
        }
        let Some(conn) = self.connections.get(&db_key).cloned() else {
            return;
        };
        self.loading.insert(group_id.clone());

        cx.spawn(async move |this, cx| {
            enum TableData {
                Columns(Vec<crate::connection::introspect::ColumnInfo>),
                Indexes(Vec<crate::connection::introspect::IndexInfo>),
                ForeignKeys(Vec<crate::connection::introspect::ForeignKeyInfo>),
                Constraints(Vec<crate::connection::introspect::ConstraintInfo>),
                Triggers(Vec<crate::connection::introspect::TriggerInfo>),
            }
            let result: Result<TableData> = match kind {
                ObjectGroupKind::Columns => cx
                    .update(|cx| conn.list_columns(schema_name.clone(), table_name.clone(), cx))
                    .await
                    .map(TableData::Columns),
                ObjectGroupKind::Indexes => cx
                    .update(|cx| conn.list_indexes(schema_name.clone(), table_name.clone(), cx))
                    .await
                    .map(TableData::Indexes),
                ObjectGroupKind::ForeignKeys => cx
                    .update(|cx| {
                        conn.list_foreign_keys(schema_name.clone(), table_name.clone(), cx)
                    })
                    .await
                    .map(TableData::ForeignKeys),
                ObjectGroupKind::Constraints => cx
                    .update(|cx| conn.list_constraints(schema_name.clone(), table_name.clone(), cx))
                    .await
                    .map(TableData::Constraints),
                ObjectGroupKind::Triggers => cx
                    .update(|cx| conn.list_triggers(schema_name.clone(), table_name.clone(), cx))
                    .await
                    .map(TableData::Triggers),
                _ => return,
            };
            this.update(cx, |this, cx| {
                this.loading.remove(&group_id);
                match result {
                    Ok(TableData::Columns(columns)) => {
                        this.tree.set_columns(&rel_id, columns);
                        this.errors.remove(&group_id);
                    }
                    Ok(TableData::Indexes(indexes)) => {
                        this.tree.set_indexes(&rel_id, indexes);
                        this.errors.remove(&group_id);
                    }
                    Ok(TableData::ForeignKeys(fks)) => {
                        this.tree.set_foreign_keys(&rel_id, fks);
                        this.errors.remove(&group_id);
                    }
                    Ok(TableData::Constraints(cons)) => {
                        this.tree.set_constraints(&rel_id, cons);
                        this.errors.remove(&group_id);
                    }
                    Ok(TableData::Triggers(triggers)) => {
                        this.tree.set_triggers(&rel_id, triggers);
                        this.errors.remove(&group_id);
                    }
                    Err(err) => {
                        log::error!("load table group {group_id}: {err}");
                        this.mark_connection_stale(&conn_id);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Live connections keyed `"{profile_id}/{database}"`, for building the
    /// `SearchObjects` picker's candidate list.
    pub(crate) fn live_connections(&self) -> Vec<(String, Connection)> {
        self.connections
            .iter()
            .map(|(key, conn)| (key.clone(), conn.clone()))
            .collect()
    }

    /// Whether `node_id`'s own children are loaded, i.e. whether expanding it
    /// requires no further async fetch. Mirrors the dispatch in `toggle_node`.
    fn ancestor_loaded(&self, node_id: &str) -> bool {
        use crate::tree::ObjectGroupKind;
        let segments: Vec<&str> = node_id.split('/').collect();
        match segments.as_slice() {
            [conn_id] => self.tree.databases_loaded(conn_id),
            [conn_id, db_name] => self.tree.schemas_loaded(&format!("{conn_id}/{db_name}")),
            [conn_id, db_name, schema_name] => self
                .tree
                .relations_loaded(&format!("{conn_id}/{db_name}/{schema_name}")),
            [conn_id, db_name, schema_name, group] => {
                let schema_id = format!("{conn_id}/{db_name}/{schema_name}");
                match ObjectGroupKind::from_key(group) {
                    Some(ObjectGroupKind::Tables) | Some(ObjectGroupKind::Views) => {
                        self.tree.relations_loaded(&schema_id)
                    }
                    Some(ObjectGroupKind::MaterializedViews) => {
                        self.tree.materialized_views_loaded(&schema_id)
                    }
                    Some(ObjectGroupKind::Functions) => self.tree.functions_loaded(&schema_id),
                    Some(ObjectGroupKind::Sequences) => self.tree.sequences_loaded(&schema_id),
                    _ => true,
                }
            }
            _ => true,
        }
    }

    /// Kicks off the async load for `node_id`'s children, reusing the same
    /// per-kind expand methods `toggle_node` dispatches to.
    fn ensure_children_loaded(&mut self, node_id: &str, cx: &mut Context<Self>) {
        use crate::tree::ObjectGroupKind;
        let segments: Vec<&str> = node_id.split('/').collect();
        match segments.as_slice() {
            [_conn_id] => self.expand_connection(node_id.to_string(), cx),
            [_conn_id, _db_name] => self.expand_database(node_id.to_string(), cx),
            [_conn_id, _db_name, _schema_name] => self.expand_schema(node_id.to_string(), cx),
            [_conn_id, _db_name, _schema_name, group] => {
                if let Some(kind) = ObjectGroupKind::from_key(group) {
                    self.expand_schema_group(node_id.to_string(), kind, cx);
                }
            }
            _ => {}
        }
    }

    /// Expands every ancestor of `node_id` (shallowest first), loading
    /// children lazily as needed, then selects `node_id`. When `open_data` is
    /// set (⌥Enter from the `SearchObjects` picker), also opens its data tab
    /// once revealed.
    pub fn reveal_object(
        &mut self,
        node_id: &str,
        open_data: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let node_id = node_id.to_string();
        let ancestors = object_search::ancestor_ids(&node_id);
        cx.spawn_in(window, async move |this, cx| {
            for ancestor in ancestors {
                let loaded = this.read_with(cx, |panel, _| panel.ancestor_loaded(&ancestor))?;
                if !loaded {
                    this.update(cx, |panel, cx| {
                        if !panel.tree.is_expanded(&ancestor) {
                            panel.tree.toggle(&ancestor);
                        }
                        panel.ensure_children_loaded(&ancestor, cx);
                        cx.notify();
                    })?;
                    // The expand methods above spawn their own async fetch;
                    // poll until it lands (or give up) before moving to the
                    // next, deeper ancestor, which may depend on this one's
                    // connection/schema having been loaded.
                    for _ in 0..200 {
                        if this.read_with(cx, |panel, _| panel.ancestor_loaded(&ancestor))? {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(25))
                            .await;
                    }
                } else if !this.read_with(cx, |panel, _| panel.tree.is_expanded(&ancestor))? {
                    this.update(cx, |panel, cx| {
                        panel.tree.toggle(&ancestor);
                        cx.notify();
                    })?;
                }
            }
            this.update_in(cx, |panel, window, cx| {
                match panel
                    .filtered_nodes()
                    .iter()
                    .position(|node| node.id == node_id)
                {
                    Some(index) => panel.set_selected(node_id.clone(), index, cx),
                    None => {
                        panel.selected_node_id = Some(node_id.clone());
                        cx.notify();
                    }
                }
                if open_data {
                    panel.open_query_for_relation(&node_id, window, cx);
                }
            })
        })
        .detach();
    }

    fn open_query_for_relation(&self, rel_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let segments: Vec<&str> = rel_id.split('/').collect();
        let [conn_id, db_name, schema_name, _group, table_name] = segments.as_slice() else {
            return;
        };

        let db_key = format!("{conn_id}/{db_name}");
        let Some(connection) = self.connections.get(&db_key).cloned() else {
            return;
        };

        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == *conn_id)
            .cloned()
            .map(|mut p| {
                p.database = db_name.to_string();
                p
            });
        let initial_sql = format!(
            "SELECT * FROM {}.{}",
            crate::connection::introspect::quote_ident(schema_name),
            crate::connection::introspect::quote_ident(table_name),
        );

        self.workspace
            .update(cx, |workspace, cx| {
                crate::query_view::QueryView::open(
                    workspace,
                    profile,
                    Some(connection),
                    initial_sql,
                    Some(table_name.to_string()),
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn open_ddl_for_relation(&self, rel_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let segments: Vec<&str> = rel_id.split('/').collect();
        let [conn_id, db_name, schema_name, _group, table_name] = segments.as_slice() else {
            return;
        };

        let db_key = format!("{conn_id}/{db_name}");
        let Some(connection) = self.connections.get(&db_key).cloned() else {
            return;
        };
        let relation_kind = self
            .tree
            .visible_nodes()
            .iter()
            .find(|node| node.id == rel_id)
            .and_then(|node| node.relation_kind)
            .unwrap_or(RelationKind::Table);
        let target = ddl_target_for_relation(relation_kind);
        let title: SharedString = format!("{table_name} DDL").into();
        self.open_ddl_tab(
            connection,
            target,
            schema_name.to_string(),
            table_name.to_string(),
            title,
            window,
            cx,
        );
    }

    /// Streams a whole relation (all rows, via `Connection::export_relation`)
    /// to a user-chosen file. `node_id` is a relation node id in the
    /// `"{profile}/{db}/{schema}/{group}/{relation}"` scheme.
    fn export_relation_to_file(
        &mut self,
        node_id: String,
        format: crate::export::ExportFormat,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((connection_key, schema, table)) = relation_export_target(&node_id) else {
            return;
        };
        let Some(connection) = self.connections.get(&connection_key).cloned() else {
            return;
        };
        let extension = match format {
            crate::export::ExportFormat::Csv => "csv",
            crate::export::ExportFormat::Json => "json",
            crate::export::ExportFormat::Sql => "sql",
        };
        let suggested = format!("{table}.{extension}");
        let directory = std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from(""));
        let save = cx.prompt_for_new_path(&directory, Some(&suggested));
        let workspace = self.workspace.clone();
        cx.spawn(async move |_this, cx| {
            let path = match save.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::error!("export: choosing a path failed: {error:#}");
                    return;
                }
                Err(_canceled) => return,
            };
            let export = cx.update(|cx| connection.export_relation(schema, table, format, cx));
            let payload = match export.await {
                Ok(payload) => payload,
                Err(error) => {
                    notify_export(&workspace, cx, format!("Export failed: {error:#}"));
                    return;
                }
            };
            let write = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, payload) }
                })
                .await;
            match write {
                Ok(()) => notify_export(&workspace, cx, format!("Exported to {}", path.display())),
                Err(error) => {
                    // Best-effort cleanup of the partial file.
                    std::fs::remove_file(&path).log_err();
                    notify_export(&workspace, cx, format!("Export failed: {error:#}"));
                }
            }
        })
        .detach();
    }

    fn open_ddl_for_object(
        &self,
        node_id: &str,
        target: DdlTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Object node id: "{conn}/{db}/{schema}/{group}/{name}" (functions,
        // sequences) or "{conn}/{db}/{schema}/{group}/{table}/{subkey}/{name}"
        // (indexes, triggers). Schema is always at index 2; the object name
        // is always the last segment.
        let mut parts = node_id.split('/');
        let (Some(conn_id), Some(db_name), Some(schema_name)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let Some(object_name) = node_id.rsplit('/').next() else {
            return;
        };
        let db_key = format!("{conn_id}/{db_name}");
        let Some(connection) = self.connections.get(&db_key).cloned() else {
            return;
        };
        let title: SharedString = format!("{object_name} DDL").into();
        self.open_ddl_tab(
            connection,
            target,
            schema_name.to_string(),
            object_name.to_string(),
            title,
            window,
            cx,
        );
    }

    fn open_ddl_tab(
        &self,
        connection: Connection,
        target: DdlTarget,
        schema: String,
        name: String,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_this, cx| {
            let ddl = match cx.update(|_window, cx| connection.object_ddl(target, schema, name, cx))
            {
                Ok(task) => task.await,
                Err(error) => Err(error),
            };
            let ddl = match ddl {
                Ok(ddl) => ddl,
                Err(error) => {
                    log::error!("object_ddl failed: {error}");
                    format!("-- Failed to load DDL: {error}")
                }
            };
            cx.update(|window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        DdlView::open(workspace, title, ddl.into(), window, cx);
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Open an empty query console bound to `profile_id`, targeting `database`
    /// when given (database node) or the profile's default database (connection
    /// node). Requires no live connection: `QueryView::run` lazily connects from
    /// the profile, so the tab opens even for offline connections.
    fn open_empty_query(
        &self,
        profile_id: &str,
        database: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut profile) = self.profiles.iter().find(|p| p.id == profile_id).cloned() else {
            return;
        };
        if let Some(database) = database {
            profile.database = database.to_string();
        }
        let connection = self
            .connections
            .get(&format!("{}/{}", profile.id, profile.database))
            .cloned();

        self.workspace
            .update(cx, |workspace, cx| {
                crate::query_view::QueryView::open(
                    workspace,
                    Some(profile),
                    connection,
                    String::new(),
                    None,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn expand_relation(&mut self, rel_id: String, cx: &mut Context<Self>) {
        if self.tree.columns_loaded(&rel_id) || self.loading.contains(&rel_id) {
            return;
        }

        let segments: Vec<&str> = rel_id.split('/').collect();
        let [conn_id, db_name, schema_name, _group, table_name] = segments.as_slice() else {
            return;
        };
        let conn_id = conn_id.to_string();
        let db_key = format!("{conn_id}/{db_name}");
        let schema_name = schema_name.to_string();
        let table_name = table_name.to_string();

        let Some(conn) = self.connections.get(&db_key).cloned() else {
            return;
        };

        self.loading.insert(rel_id.clone());

        cx.spawn(async move |this, cx| {
            let columns_result = cx
                .update(|cx| conn.list_columns(schema_name.clone(), table_name.clone(), cx))
                .await;
            this.update(cx, |this, cx| {
                this.loading.remove(&rel_id);
                match columns_result {
                    Ok(columns) => {
                        this.tree.set_columns(&rel_id, columns);
                        this.errors.remove(&rel_id);
                    }
                    Err(err) => {
                        log::error!("list_columns for {rel_id}: {err}");
                        this.mark_connection_stale(&conn_id);
                    }
                }
                cx.notify();
            })
            .ok();

            let count_result = cx
                .update(|cx| conn.approx_row_count(schema_name.clone(), table_name.clone(), cx))
                .await;
            this.update(cx, |this, cx| {
                if let Ok(count) = count_result {
                    this.tree.set_row_count(&rel_id, count);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drop all per-database state for a profile: live connections, cached
    /// password, and per-node errors under the connection's id prefix.
    fn drop_profile_state(&mut self, profile_id: &str) {
        let prefix = format!("{}/", profile_id);
        self.connections.retain(|key, _| !key.starts_with(&prefix));
        self.passwords.remove(profile_id);
        self.errors.retain(|key, _| !key.starts_with(&prefix));
    }

    /// Mark a connection as stale: drop all its per-database connections and
    /// cached data, collapse it, and record an error prompting the user to
    /// expand again to reconnect.
    fn mark_connection_stale(&mut self, conn_id: &str) {
        self.drop_profile_state(conn_id);
        self.loading.remove(conn_id);
        self.tree.clear_connection(conn_id);
        self.tree.collapse(conn_id);
        self.errors.insert(
            conn_id.to_string(),
            "Disconnected — expand to reconnect".to_string(),
        );
    }

    fn disconnect_connection(&mut self, profile_id: String, cx: &mut Context<Self>) {
        self.drop_profile_state(&profile_id);
        self.loading.remove(&profile_id);
        self.tree.clear_connection(&profile_id);
        self.tree.collapse(&profile_id);
        self.errors.remove(&profile_id);
        cx.notify();
    }

    fn delete_connection(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let profile = self.profiles.iter().find(|p| p.id == profile_id).cloned();
        self.profiles.retain(|p| p.id != profile_id);
        if let Err(e) = store::save_profiles(&self.profiles) {
            log::error!("failed to save profiles after delete: {e}");
        }
        if let Some(profile) = profile {
            store::delete_password(cx, &profile).detach_and_log_err(cx);
        }
        self.drop_profile_state(&profile_id);
        self.loading.remove(&profile_id);
        self.errors.remove(&profile_id);
        self.tree = Self::build_tree(&self.profiles);
        cx.notify();
    }

    fn edit_connection(&mut self, profile_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.iter().find(|p| p.id == profile_id).cloned() else {
            return;
        };
        let panel = cx.entity();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    crate::connection_modal::ConnectionModal::edit(
                        panel.clone(),
                        profile.clone(),
                        window,
                        cx,
                    )
                });
            })
            .ok();
    }

    fn deploy_context_menu(
        &mut self,
        node_id: String,
        kind: NodeKind,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity();

        let context_menu = match kind {
            NodeKind::Connection => {
                let profile_id = node_id;
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.item(ContextMenuEntry::new("New Query").handler({
                        let entity = entity.clone();
                        let profile_id = profile_id.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.open_empty_query(&profile_id, None, window, cx);
                            });
                        }
                    }))
                    .separator()
                    .item(ContextMenuEntry::new("Disconnect").handler({
                        let entity = entity.clone();
                        let profile_id = profile_id.clone();
                        move |_window, cx| {
                            entity.update(cx, |this, cx| {
                                this.disconnect_connection(profile_id.clone(), cx);
                            });
                        }
                    }))
                    .item(ContextMenuEntry::new("Edit").handler({
                        let entity = entity.clone();
                        let profile_id = profile_id.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.edit_connection(profile_id.clone(), window, cx);
                            });
                        }
                    }))
                    .item(ContextMenuEntry::new("Delete").handler({
                        let entity = entity.clone();
                        let profile_id = profile_id.clone();
                        move |_window, cx| {
                            entity.update(cx, |this, cx| {
                                this.delete_connection(profile_id.clone(), cx);
                            });
                        }
                    }))
                })
            }
            NodeKind::Database => {
                // "{conn}/{db}"
                let mut parts = node_id.splitn(2, '/');
                let (Some(profile_id), Some(db_name)) = (parts.next(), parts.next()) else {
                    return;
                };
                let profile_id = profile_id.to_string();
                let db_name = db_name.to_string();
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.item(ContextMenuEntry::new("New Query").handler({
                        let entity = entity.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.open_empty_query(&profile_id, Some(&db_name), window, cx);
                            });
                        }
                    }))
                })
            }
            NodeKind::Relation => {
                // "{conn}/{db}/{schema}/{group}/{rel}"
                let segments: Vec<&str> = node_id.split('/').collect();
                let [_, _, schema_name, _group, table_name] = segments.as_slice() else {
                    return;
                };
                let table_name = table_name.to_string();
                let qualified = qualified_relation_name(schema_name, &table_name);
                let rel_id = node_id.clone();
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.item(ContextMenuEntry::new("Open").handler({
                        let entity = entity.clone();
                        let rel_id = rel_id.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.open_query_for_relation(&rel_id, window, cx);
                            });
                        }
                    }))
                    .item(ContextMenuEntry::new("View DDL").handler({
                        let entity = entity.clone();
                        let rel_id = rel_id.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.open_ddl_for_relation(&rel_id, window, cx);
                            });
                        }
                    }))
                    .separator()
                    .item(ContextMenuEntry::new("Copy Name").handler({
                        let table_name = table_name.clone();
                        move |_window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(table_name.clone()));
                        }
                    }))
                    .item(ContextMenuEntry::new("Copy Qualified Name").handler({
                        move |_window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(qualified.clone()));
                        }
                    }))
                    .submenu("Export Data", {
                        let entity = entity.clone();
                        let rel_id = rel_id.clone();
                        move |menu, _window, _cx| {
                            menu.item(ContextMenuEntry::new("Export Data as CSV").handler({
                                let entity = entity.clone();
                                let rel_id = rel_id.clone();
                                move |window, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.export_relation_to_file(
                                            rel_id.clone(),
                                            crate::export::ExportFormat::Csv,
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            }))
                            .item(ContextMenuEntry::new("Export Data as JSON").handler({
                                let entity = entity.clone();
                                let rel_id = rel_id.clone();
                                move |window, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.export_relation_to_file(
                                            rel_id.clone(),
                                            crate::export::ExportFormat::Json,
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            }))
                            .item(
                                ContextMenuEntry::new("Export Data as SQL").handler({
                                    let entity = entity.clone();
                                    let rel_id = rel_id.clone();
                                    move |window, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.export_relation_to_file(
                                                rel_id.clone(),
                                                crate::export::ExportFormat::Sql,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                            )
                        }
                    })
                    .item(ContextMenuEntry::new("Refresh").handler({
                        let entity = entity.clone();
                        let rel_id = rel_id.clone();
                        move |_window, cx| {
                            entity.update(cx, |this, cx| {
                                this.refresh_node(rel_id.clone(), NodeKind::Relation, cx);
                            });
                        }
                    }))
                })
            }
            NodeKind::Schema | NodeKind::ObjectGroup(_) => {
                let refresh_id = node_id;
                let refresh_kind = kind;
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.item(ContextMenuEntry::new("Refresh").handler({
                        let entity = entity.clone();
                        move |_window, cx| {
                            entity.update(cx, |this, cx| {
                                this.refresh_node(refresh_id.clone(), refresh_kind.clone(), cx);
                            });
                        }
                    }))
                })
            }
            NodeKind::Function | NodeKind::Sequence | NodeKind::Index | NodeKind::Trigger => {
                let Some(target) = crate::connection::ddl::ddl_target_for_node_kind(kind) else {
                    return;
                };
                ContextMenu::build(window, cx, |menu, _, _| {
                    menu.item(ContextMenuEntry::new("View DDL").handler({
                        let entity = entity.clone();
                        let node_id = node_id.clone();
                        move |window, cx| {
                            entity.update(cx, |this, cx| {
                                this.open_ddl_for_object(&node_id, target, window, cx);
                            });
                        }
                    }))
                })
            }
            NodeKind::Column | NodeKind::ForeignKey | NodeKind::Constraint | NodeKind::Empty => {
                return;
            }
        };

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu.take();
            cx.notify();
        });
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn selected_visible_index(&self, nodes: &[TreeNode]) -> Option<usize> {
        let selected = self.selected_node_id.as_deref()?;
        nodes.iter().position(|node| node.id == selected)
    }

    fn set_selected(&mut self, node_id: String, index: usize, cx: &mut Context<Self>) {
        self.selected_node_id = Some(node_id);
        self.scroll_handle
            .scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        let nodes = self.filtered_nodes();
        if nodes.is_empty() {
            return;
        }
        let next_ix = match self.selected_visible_index(&nodes) {
            Some(ix) => (ix + 1).min(nodes.len() - 1),
            None => 0,
        };
        if let Some(node) = nodes.get(next_ix) {
            self.set_selected(node.id.clone(), next_ix, cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nodes = self.filtered_nodes();
        if nodes.is_empty() {
            return;
        }
        let prev_ix = match self.selected_visible_index(&nodes) {
            Some(ix) => ix.saturating_sub(1),
            None => 0,
        };
        if let Some(node) = nodes.get(prev_ix) {
            self.set_selected(node.id.clone(), prev_ix, cx);
        }
    }

    fn expand_selected_entry(
        &mut self,
        _: &ExpandSelectedEntry,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nodes = self.filtered_nodes();
        let Some(ix) = self.selected_visible_index(&nodes) else {
            return;
        };
        let Some(node) = nodes.get(ix) else { return };
        if matches!(node.kind, NodeKind::Column | NodeKind::Empty) {
            return;
        }
        if node.expanded {
            let child_ix = ix + 1;
            if let Some(child) = nodes.get(child_ix).filter(|child| child.depth > node.depth) {
                self.set_selected(child.id.clone(), child_ix, cx);
            }
        } else if node.has_children {
            self.toggle_node(node.id.clone(), node.kind.clone(), cx);
        }
    }

    fn collapse_selected_entry(
        &mut self,
        _: &CollapseSelectedEntry,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let nodes = self.filtered_nodes();
        let Some(ix) = self.selected_visible_index(&nodes) else {
            return;
        };
        let Some(node) = nodes.get(ix) else { return };
        if node.expanded && !matches!(node.kind, NodeKind::Column | NodeKind::Empty) {
            self.tree.collapse(&node.id);
            cx.notify();
        } else if let Some(parent_ix) = nodes.get(..ix).and_then(|prefix| {
            prefix
                .iter()
                .rposition(|candidate| candidate.depth < node.depth)
        }) {
            if let Some(parent) = nodes.get(parent_ix) {
                self.set_selected(parent.id.clone(), parent_ix, cx);
            }
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let nodes = self.filtered_nodes();
        let Some(node) = self
            .selected_visible_index(&nodes)
            .and_then(|ix| nodes.get(ix))
        else {
            return;
        };
        match node.kind {
            NodeKind::Relation => self.open_query_for_relation(&node.id, window, cx),
            NodeKind::Column | NodeKind::Empty => {}
            _ => self.toggle_node(node.id.clone(), node.kind.clone(), cx),
        }
    }

    fn is_profile_connected(&self, profile_id: &str) -> bool {
        let prefix = format!("{}/", profile_id);
        self.connections.keys().any(|key| key.starts_with(&prefix))
    }

    fn node_icon_color(&self, node: &TreeNode) -> Color {
        match node.kind {
            NodeKind::Connection => {
                if self.errors.contains_key(&node.id) {
                    Color::Error
                } else if self.is_profile_connected(&node.id) {
                    Color::Default
                } else {
                    Color::Muted
                }
            }
            NodeKind::Column if node.is_primary_key => Color::Warning,
            _ => Color::Muted,
        }
    }

    fn render_tree_node(&self, ix: usize, node: &TreeNode, cx: &mut Context<Self>) -> AnyElement {
        let theme_settings = ThemeSettings::get_global(cx);
        let sidebar_settings = DatabaseClientSettings::get_global(cx).sidebar;
        let ui_font_size = f32::from(theme_settings.ui_font_size(cx));
        let row_rem_size = sidebar_settings.font_size.unwrap_or(ui_font_size);
        let row_font_size = px(row_rem_size);
        let row_icon_size = sidebar_settings
            .icon_size
            .map(|size| ui::bar_icon_size(size, row_rem_size))
            .unwrap_or(IconSize::Small);
        let row_padding = sidebar_settings
            .padding
            .map(|padding| px(ui::clamp_bar_padding(padding)));
        let row_height = match row_padding {
            Some(padding) => row_font_size * theme_settings.ui_line_height() + 2.0 * padding,
            None => theme_settings.ui_font_size(cx) * theme_settings.ui_line_height(),
        };

        let is_empty = matches!(node.kind, NodeKind::Empty);
        let has_context_menu = matches!(
            node.kind,
            NodeKind::Connection
                | NodeKind::Database
                | NodeKind::Relation
                | NodeKind::Schema
                | NodeKind::ObjectGroup(_)
                | NodeKind::Function
                | NodeKind::Sequence
                | NodeKind::Index
                | NodeKind::Trigger
        );
        let is_loading = self.loading.contains(&node.id);
        let is_selected = self.selected_node_id.as_deref() == Some(node.id.as_str());
        let error = self.errors.get(&node.id).cloned();
        let icon = node_icon(node);
        let icon_color = self.node_icon_color(node);

        let node_id_for_toggle = node.id.clone();
        let node_id_for_click = node.id.clone();
        let node_id_for_menu = node.id.clone();
        let kind_for_toggle = node.kind.clone();
        let kind_for_click = node.kind.clone();
        let kind_for_menu = node.kind.clone();

        let label_text = if is_loading {
            format!("{} (loading…)", node.label)
        } else {
            node.label.clone()
        };

        let content = h_flex()
            .h(row_height)
            .gap_1()
            .overflow_hidden()
            .child(
                h_flex()
                    .w(px(TREE_CHEVRON_SLOT))
                    .flex_none()
                    .justify_center()
                    .when(node.has_children && !is_empty, |slot| {
                        slot.child(Disclosure::new(("disclosure", ix), node.expanded).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_node(
                                    node_id_for_toggle.clone(),
                                    kind_for_toggle.clone(),
                                    cx,
                                );
                            }),
                        ))
                    }),
            )
            .when_some(icon, |this, icon| {
                this.child(Icon::new(icon).size(row_icon_size).color(icon_color))
            })
            .child(
                Label::new(label_text)
                    .size(LabelSize::Small)
                    .truncate()
                    .when(is_loading || is_empty, |label| label.color(Color::Muted)),
            )
            .when_some(
                node.row_count.and_then(crate::tree::row_count_suffix),
                |this, suffix| {
                    this.child(
                        Label::new(suffix)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                },
            )
            .when_some(error, |this, err_text| {
                this.child(
                    Label::new(format!("— {err_text}"))
                        .size(LabelSize::Small)
                        .truncate()
                        .color(Color::Error),
                )
            });

        ListItem::new(ix)
            .indent_level(node.depth)
            .indent_step_size(px(TREE_INDENT_SIZE))
            .spacing(ListItemSpacing::Dense)
            .selectable(!is_empty)
            .toggle_state(is_selected)
            .child(
                ui::utils::WithRemSize::new(row_font_size)
                    .w_full()
                    .text_size(row_font_size)
                    .child(content),
            )
            .when(!is_empty, |item| {
                item.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.selected_node_id = Some(node_id_for_click.clone());
                    if !this.focus_handle.is_focused(window) {
                        window.focus(&this.focus_handle, cx);
                    }
                    if event.click_count() >= 2 {
                        if matches!(kind_for_click, NodeKind::Relation) {
                            this.open_query_for_relation(&node_id_for_click, window, cx);
                        } else {
                            this.toggle_node(node_id_for_click.clone(), kind_for_click.clone(), cx);
                        }
                    }
                    cx.notify();
                }))
            })
            .when(has_context_menu, |item| {
                item.on_secondary_mouse_down(cx.listener(
                    move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.deploy_context_menu(
                            node_id_for_menu.clone(),
                            kind_for_menu.clone(),
                            event.position,
                            window,
                            cx,
                        );
                    },
                ))
            })
            .into_any_element()
    }

    fn open_new_connection_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.entity();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    crate::connection_modal::ConnectionModal::new(panel.clone(), window, cx)
                });
            })
            .ok();
    }
}

/// Indent step for tree rows and indent guides.
const TREE_INDENT_SIZE: f32 = 16.0;
/// Fixed-width leading slot that holds the disclosure chevron.
const TREE_CHEVRON_SLOT: f32 = 16.0;

fn qualified_relation_name(schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        crate::connection::introspect::quote_ident(schema),
        crate::connection::introspect::quote_ident(table),
    )
}

/// From a relation node id `"{profile_id}/{database}/{schema}/{group}/{relation}"`
/// (mirroring `open_query_for_relation`/`open_ddl_for_relation`), returns
/// `(connection_key, schema, table)` where `connection_key` is the
/// `"{profile_id}/{database}"` used by `DatabasePanel::connections`.
fn relation_export_target(node_id: &str) -> Option<(String, String, String)> {
    let segments: Vec<&str> = node_id.split('/').collect();
    let [profile_id, database, schema, _group, table] = segments.as_slice() else {
        return None;
    };
    Some((
        format!("{profile_id}/{database}"),
        schema.to_string(),
        table.to_string(),
    ))
}

struct ExportNotice;

/// Shows a status toast on the workspace (best-effort; ignored if the
/// workspace is gone).
fn notify_export(workspace: &WeakEntity<Workspace>, cx: &mut gpui::AsyncApp, message: String) {
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_toast(
                workspace::Toast::new(
                    workspace::notifications::NotificationId::unique::<ExportNotice>(),
                    message,
                ),
                cx,
            );
        })
        .log_err();
}

fn node_icon(node: &TreeNode) -> Option<IconName> {
    match node.kind {
        NodeKind::Connection | NodeKind::Database => Some(IconName::Database),
        NodeKind::Schema => Some(IconName::Folder),
        NodeKind::Relation => match node.relation_kind {
            Some(RelationKind::View) | Some(RelationKind::MaterializedView) => Some(IconName::Eye),
            _ => Some(IconName::Table),
        },
        NodeKind::Column => {
            if node.is_primary_key {
                Some(IconName::Key)
            } else {
                Some(crate::column_meta::column_type_icon(
                    node.column_data_type.as_deref().unwrap_or(""),
                ))
            }
        }
        NodeKind::ObjectGroup(_) => Some(IconName::Folder),
        NodeKind::Index => Some(IconName::ListTree),
        NodeKind::ForeignKey => Some(IconName::Link),
        NodeKind::Constraint => Some(IconName::Sliders),
        NodeKind::Function => Some(IconName::Code),
        NodeKind::Sequence => Some(IconName::Hash),
        NodeKind::Trigger => Some(IconName::BoltFilled),
        NodeKind::Empty => None,
    }
}

impl Focusable for DatabasePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for DatabasePanel {}

impl Panel for DatabasePanel {
    fn persistent_name() -> &'static str {
        "Database Panel"
    }

    fn panel_key() -> &'static str {
        "DatabasePanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> gpui::Pixels {
        px(260.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::DatabaseZap)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Database")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .p_2()
                    .justify_between()
                    .child(Label::new(format!("{} connection(s)", self.profiles.len())))
                    .child(
                        IconButton::new("new-connection", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("New Connection"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_new_connection_modal(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .w_full()
                    .child(self.filter_editor.clone()),
            );

        let content = if self.profiles.is_empty() {
            div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    Label::new("No connections")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Button::new("add-connection-empty", "Add connection").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.open_new_connection_modal(window, cx);
                        },
                    )),
                )
                .into_any_element()
        } else {
            let item_count = self.filtered_nodes().len();

            let list = uniform_list(
                "db-tree",
                item_count,
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let nodes = this.filtered_nodes();
                    let mut items = Vec::with_capacity(range.len());
                    for ix in range {
                        if let Some(node) = nodes.get(ix) {
                            items.push(this.render_tree_node(ix, node, cx));
                        }
                    }
                    items
                }),
            )
            .size_full()
            .track_scroll(&self.scroll_handle)
            .with_decoration(
                ui::indent_guides(px(TREE_INDENT_SIZE), IndentGuideColors::panel(cx))
                    .with_compute_indents_fn(cx.entity(), |this, range, _window, _cx| {
                        let nodes = this.filtered_nodes();
                        nodes
                            .get(range)
                            .map(|slice| {
                                slice
                                    .iter()
                                    .map(|node| node.depth)
                                    .collect::<SmallVec<[usize; 64]>>()
                            })
                            .unwrap_or_default()
                    })
                    .with_render_fn(cx.entity(), move |_, params, _, _| {
                        // Shift guides under the chevron column, like the project panel
                        // (crates/project_panel/src/project_panel.rs).
                        const LEFT_OFFSET: Pixels = px(14.);
                        let indent_size = params.indent_size;
                        let item_height = params.item_height;
                        params
                            .indent_guides
                            .into_iter()
                            .map(|layout| {
                                let bounds = Bounds::new(
                                    point(
                                        layout.offset.x * indent_size + LEFT_OFFSET,
                                        layout.offset.y * item_height,
                                    ),
                                    size(px(1.), layout.length * item_height),
                                );
                                ui::RenderedIndentGuide {
                                    bounds,
                                    layout,
                                    is_active: false,
                                    hitbox: None,
                                }
                            })
                            .collect()
                    }),
            );

            v_flex()
                .flex_1()
                .size_full()
                .overflow_hidden()
                .child(list)
                .custom_scrollbars(
                    Scrollbars::for_settings::<EditorSettingsScrollbarProxy>()
                        .tracked_scroll_handle(&self.scroll_handle)
                        .notify_content(),
                    window,
                    cx,
                )
                .into_any_element()
        };

        div()
            .key_context("DatabasePanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::expand_selected_entry))
            .on_action(cx.listener(Self::collapse_selected_entry))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(content)
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::introspect::RelationInfo;
    use crate::connection::profile::{ConnectionProfile, SslMode};
    use gpui::TestAppContext;
    use std::time::Duration;

    #[test]
    fn relation_export_target_parses_node_id() {
        let target = relation_export_target("prof/mydb/public/tables/orders");
        assert_eq!(
            target,
            Some((
                "prof/mydb".to_string(),
                "public".to_string(),
                "orders".to_string()
            ))
        );
        assert_eq!(relation_export_target("prof/mydb/public"), None);
    }

    fn test_profile() -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "Local".into(),
            host: "localhost".into(),
            port: 5432,
            database: "db1".into(),
            user: "u".into(),
            ssl_mode: SslMode::Disable,
            read_only: false,
        }
    }

    /// Pre-load p1 -> db1 -> public -> users so expand/collapse does no IO
    /// (each expand_* early-returns when its `*_loaded()` check passes).
    fn inject_loaded_tree(panel: &Entity<DatabasePanel>, cx: &mut gpui::VisualTestContext) {
        panel.update(cx, |panel, cx| {
            panel.tree.toggle("p1");
            panel.tree.set_databases("p1", vec!["db1".into()]);
            panel.tree.toggle("p1/db1");
            panel.tree.set_schemas("p1/db1", vec!["public".into()]);
            panel.tree.toggle("p1/db1/public");
            panel.tree.set_relations(
                "p1/db1/public",
                vec![RelationInfo {
                    name: "users".into(),
                    kind: RelationKind::Table,
                }],
            );
            panel.tree.toggle("p1/db1/public/tables");
            cx.notify();
        });
    }

    fn selected(panel: &Entity<DatabasePanel>, cx: &mut gpui::VisualTestContext) -> Option<String> {
        panel.read_with(cx, |panel, _| panel.selected_node_id.clone())
    }

    #[gpui::test]
    async fn keyboard_selection_moves_and_clamps(cx: &mut TestAppContext) {
        init_test(cx);
        let (panel, _workspace, cx) = panel_with_profile(test_profile(), cx).await;
        inject_loaded_tree(&panel, cx);

        let node_ids = panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .map(|n| n.id.clone())
                .collect::<Vec<_>>()
        });
        // p1, db1, public, tables (1), users, views (0), matviews, functions, sequences
        assert_eq!(
            node_ids,
            vec![
                "p1",
                "p1/db1",
                "p1/db1/public",
                "p1/db1/public/tables",
                "p1/db1/public/tables/users",
                "p1/db1/public/views",
                "p1/db1/public/matviews",
                "p1/db1/public/functions",
                "p1/db1/public/sequences",
            ]
        );

        // No selection: down selects the first node.
        panel.update_in(cx, |panel, window, cx| {
            panel.select_next(&SelectNext, window, cx)
        });
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1"));

        for _ in 0..(node_ids.len() - 1) {
            panel.update_in(cx, |panel, window, cx| {
                panel.select_next(&SelectNext, window, cx)
            });
        }
        // Clamps at the last visible node.
        assert_eq!(
            selected(&panel, cx).as_deref(),
            Some("p1/db1/public/sequences")
        );

        panel.update_in(cx, |panel, window, cx| {
            panel.select_previous(&SelectPrevious, window, cx)
        });
        assert_eq!(
            selected(&panel, cx).as_deref(),
            Some("p1/db1/public/functions")
        );

        // Clamps at the first node.
        for _ in 0..node_ids.len() {
            panel.update_in(cx, |panel, window, cx| {
                panel.select_previous(&SelectPrevious, window, cx)
            });
        }
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1"));
    }

    #[gpui::test]
    async fn keyboard_selection_stays_within_active_filter(cx: &mut TestAppContext) {
        init_test(cx);
        let (panel, _workspace, cx) = panel_with_profile(test_profile(), cx).await;
        inject_loaded_tree(&panel, cx);

        // Filtering to "users" keeps only the ancestor chain down to the
        // "users" relation, dropping the sibling views/matviews/functions/sequences
        // nodes that are still present in the unfiltered tree.
        panel.update(cx, |panel, cx| {
            panel.tree_filter = "users".into();
            cx.notify();
        });

        let filtered_ids = panel.read_with(cx, |panel, _| {
            panel
                .filtered_nodes()
                .iter()
                .map(|n| n.id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            filtered_ids,
            vec![
                "p1",
                "p1/db1",
                "p1/db1/public",
                "p1/db1/public/tables",
                "p1/db1/public/tables/users",
            ]
        );

        // The unfiltered tree has more nodes; if select_next iterated over it
        // instead of the filtered list, it would be able to land on one of them.
        let unfiltered_ids = panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .map(|n| n.id.clone())
                .collect::<Vec<_>>()
        });
        assert!(unfiltered_ids.len() > filtered_ids.len());

        for _ in 0..(filtered_ids.len() + 3) {
            panel.update_in(cx, |panel, window, cx| {
                panel.select_next(&SelectNext, window, cx)
            });
            let current = selected(&panel, cx);
            assert!(
                current
                    .as_deref()
                    .is_some_and(|id| filtered_ids.iter().any(|filtered_id| filtered_id == id)),
                "selection {:?} escaped the active filter",
                current
            );
        }
        // Clamps at the last filtered node, not the last unfiltered node.
        assert_eq!(
            selected(&panel, cx).as_deref(),
            Some("p1/db1/public/tables/users")
        );

        for _ in 0..(filtered_ids.len() + 3) {
            panel.update_in(cx, |panel, window, cx| {
                panel.select_previous(&SelectPrevious, window, cx)
            });
            let current = selected(&panel, cx);
            assert!(
                current
                    .as_deref()
                    .is_some_and(|id| filtered_ids.iter().any(|filtered_id| filtered_id == id)),
                "selection {:?} escaped the active filter",
                current
            );
        }
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1"));
    }

    #[gpui::test]
    async fn expand_collapse_selected_navigates_hierarchy(cx: &mut TestAppContext) {
        init_test(cx);
        let (panel, _workspace, cx) = panel_with_profile(test_profile(), cx).await;
        inject_loaded_tree(&panel, cx);

        panel.update(cx, |panel, cx| {
            panel.selected_node_id = Some("p1".into());
            cx.notify();
        });

        // Right on an already-expanded node moves to its first child.
        panel.update_in(cx, |panel, window, cx| {
            panel.expand_selected_entry(&crate::ExpandSelectedEntry, window, cx)
        });
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1/db1"));

        // Left on an expanded node collapses it; selection stays put.
        panel.update_in(cx, |panel, window, cx| {
            panel.collapse_selected_entry(&crate::CollapseSelectedEntry, window, cx)
        });
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1/db1"));
        panel.read_with(cx, |panel, _| assert!(!panel.tree.is_expanded("p1/db1")));

        // Left on a collapsed node moves to the parent.
        panel.update_in(cx, |panel, window, cx| {
            panel.collapse_selected_entry(&crate::CollapseSelectedEntry, window, cx)
        });
        assert_eq!(selected(&panel, cx).as_deref(), Some("p1"));

        // Right on a collapsed node with loaded children re-expands without IO.
        panel.update(cx, |panel, cx| {
            panel.selected_node_id = Some("p1/db1".into());
            cx.notify();
        });
        panel.update_in(cx, |panel, window, cx| {
            panel.expand_selected_entry(&crate::ExpandSelectedEntry, window, cx)
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.tree.is_expanded("p1/db1"));
            assert!(
                panel.loading.is_empty(),
                "pre-loaded expand must not trigger loading"
            );
        });
    }

    #[gpui::test]
    async fn confirm_toggles_containers_and_ignores_unconnected_relations(cx: &mut TestAppContext) {
        init_test(cx);
        let (panel, workspace, cx) = panel_with_profile(test_profile(), cx).await;
        inject_loaded_tree(&panel, cx);

        // Enter on a schema toggles it.
        panel.update(cx, |panel, cx| {
            panel.selected_node_id = Some("p1/db1/public".into());
            cx.notify();
        });
        panel.update_in(cx, |panel, window, cx| panel.confirm(&Confirm, window, cx));
        panel.read_with(cx, |panel, _| {
            assert!(!panel.tree.is_expanded("p1/db1/public"))
        });
        panel.update_in(cx, |panel, window, cx| panel.confirm(&Confirm, window, cx));
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert!(panel.tree.is_expanded("p1/db1/public"))
        });

        // Enter on a relation without a live connection is a safe no-op.
        panel.update(cx, |panel, cx| {
            panel.selected_node_id = Some("p1/db1/public/tables/users".into());
            cx.notify();
        });
        panel.update_in(cx, |panel, window, cx| panel.confirm(&Confirm, window, cx));
        cx.run_until_parked();
        let query_views = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<crate::query_view::QueryView>(cx)
                .count()
        });
        assert_eq!(query_views, 0);
    }

    #[test]
    fn qualified_relation_name_quotes_and_escapes() {
        assert_eq!(
            qualified_relation_name("public", "users"),
            "\"public\".\"users\""
        );
        assert_eq!(
            qualified_relation_name("app", "we\"ird"),
            "\"app\".\"we\"\"ird\""
        );
    }

    #[test]
    fn column_node_icon_follows_data_type_and_primary_key() {
        fn column_node(data_type: &str, is_primary_key: bool) -> TreeNode {
            TreeNode {
                id: "p1/db1/public/users/c".into(),
                depth: 4,
                kind: NodeKind::Column,
                label: format!("c: {data_type}"),
                expanded: false,
                has_children: false,
                relation_kind: None,
                column_data_type: Some(data_type.to_string()),
                is_primary_key,
                row_count: None,
            }
        }

        assert_eq!(
            node_icon(&column_node("integer", false)),
            Some(IconName::Hash)
        );
        assert_eq!(
            node_icon(&column_node("timestamp with time zone", false)),
            Some(IconName::Calendar)
        );
        assert_eq!(
            node_icon(&column_node("jsonb", false)),
            Some(IconName::Json)
        );
        assert_eq!(
            node_icon(&column_node("boolean", false)),
            Some(IconName::ToggleLeft)
        );
        assert_eq!(
            node_icon(&column_node("integer", true)),
            Some(IconName::Key),
            "primary key wins over the type icon"
        );

        let view = TreeNode {
            id: "p1/db1/public/user_emails".into(),
            depth: 3,
            kind: NodeKind::Relation,
            label: "user_emails".into(),
            expanded: false,
            has_children: true,
            relation_kind: Some(RelationKind::View),
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        };
        assert_eq!(node_icon(&view), Some(IconName::Eye));
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            gpui_tokio::init(cx);
        });
    }

    /// Build a panel backed by a single injected profile, bypassing on-disk profile loading.
    async fn panel_with_profile(
        profile: ConnectionProfile,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DatabasePanel>,
        Entity<Workspace>,
        &mut gpui::VisualTestContext,
    ) {
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = workspace.update_in(cx, |workspace, window, cx| {
            DatabasePanel::new(workspace, window, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.profiles = vec![profile];
            panel.tree = DatabasePanel::build_tree(&panel.profiles);
            panel.connections.clear();
            cx.notify();
        });
        (panel, workspace, cx)
    }

    async fn wait_until(
        panel: &Entity<DatabasePanel>,
        cx: &mut gpui::VisualTestContext,
        description: &str,
        predicate: impl Fn(&DatabasePanel) -> bool,
    ) {
        for _ in 0..200 {
            cx.run_until_parked();
            if panel.read_with(cx, |panel, _| predicate(panel)) {
                return;
            }
            cx.background_executor
                .timer(Duration::from_millis(25))
                .await;
        }
        let labels = panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .map(|n| n.label.clone())
                .collect::<Vec<_>>()
        });
        let errors = panel.read_with(cx, |panel, _| panel.errors.clone());
        panic!("timed out waiting for {description}; visible={labels:?} errors={errors:?}");
    }

    fn labels(panel: &Entity<DatabasePanel>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .map(|n| n.label.clone())
                .collect()
        })
    }

    #[gpui::test]
    async fn disconnect_prunes_connections_passwords_and_node_errors(cx: &mut TestAppContext) {
        init_test(cx);
        let profile = ConnectionProfile {
            id: "p1".into(),
            name: "Test".into(),
            host: "localhost".into(),
            port: 5432,
            database: "db".into(),
            user: "u".into(),
            ssl_mode: SslMode::Disable,
            read_only: false,
        };
        let (panel, _workspace, cx) = panel_with_profile(profile, cx).await;

        panel.update(cx, |panel, cx| {
            // Top-level and per-node errors under p1, plus an unrelated
            // profile's error that must survive the prune.
            panel.errors.insert("p1".into(), "stale top-level".into());
            panel.errors.insert("p1/db".into(), "stale node".into());
            panel.errors.insert("p2/otherdb".into(), "unrelated".into());
            panel.passwords.insert("p1".into(), Some("secret".into()));
            cx.notify();
        });

        panel.update(cx, |panel, cx| {
            panel.disconnect_connection("p1".into(), cx);
        });

        panel.read_with(cx, |panel, _| {
            assert!(!panel.errors.contains_key("p1"));
            assert!(!panel.errors.contains_key("p1/db"));
            assert!(panel.errors.contains_key("p2/otherdb"));
            assert!(!panel.passwords.contains_key("p1"));
        });
    }

    #[gpui::test]
    async fn context_menu_deploys_for_connection_database_relation_schema_and_group(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let profile = ConnectionProfile {
            id: "p1".into(),
            name: "Test".into(),
            host: "127.0.0.1".into(),
            port: 1,
            database: "db".into(),
            user: "u".into(),
            ssl_mode: SslMode::Disable,
            read_only: false,
        };
        let (panel, _workspace, cx) = panel_with_profile(profile, cx).await;

        let deploys = |panel: &Entity<DatabasePanel>,
                       cx: &mut gpui::VisualTestContext,
                       node_id: &str,
                       kind: NodeKind| {
            let node_id = node_id.to_string();
            panel.update_in(cx, |panel, window, cx| {
                panel.context_menu.take();
                panel.deploy_context_menu(node_id, kind, Point::default(), window, cx);
                panel.context_menu.is_some()
            })
        };

        assert!(deploys(&panel, cx, "p1", NodeKind::Connection));
        assert!(deploys(&panel, cx, "p1/db", NodeKind::Database));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/tables/users",
            NodeKind::Relation
        ));
        assert!(deploys(&panel, cx, "p1/db/public", NodeKind::Schema));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/tables",
            NodeKind::ObjectGroup(crate::tree::ObjectGroupKind::Tables)
        ));
        assert!(!deploys(
            &panel,
            cx,
            "p1/db/public/tables/users/id",
            NodeKind::Column
        ));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/functions/add",
            NodeKind::Function
        ));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/sequences/users_id_seq",
            NodeKind::Sequence
        ));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/tables/users/indexes/users_pkey",
            NodeKind::Index
        ));
        assert!(deploys(
            &panel,
            cx,
            "p1/db/public/tables/users/triggers/audit_trg",
            NodeKind::Trigger
        ));
    }

    #[gpui::test]
    async fn new_query_opens_empty_query_view_bound_to_node_database(cx: &mut TestAppContext) {
        init_test(cx);
        let profile = ConnectionProfile {
            id: "p1".into(),
            name: "Test".into(),
            host: "127.0.0.1".into(),
            port: 1,
            database: "defaultdb".into(),
            user: "u".into(),
            ssl_mode: SslMode::Disable,
            read_only: false,
        };
        let (panel, workspace, cx) = panel_with_profile(profile, cx).await;

        // Database node: "p1/analytics" → New Query bound to that database.
        panel.update_in(cx, |panel, window, cx| {
            panel.open_empty_query("p1", Some("analytics"), window, cx);
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            let views: Vec<_> = workspace
                .items_of_type::<crate::query_view::QueryView>(cx)
                .collect();
            assert_eq!(views.len(), 1, "New Query should open exactly one tab");
            let view = views[0].read(cx);
            assert_eq!(view.sql(cx), "", "New Query console must start empty");
            assert!(!view.is_running(), "New Query must not auto-run");
            let bound = view.profile().expect("tab must be bound to the profile");
            assert_eq!(bound.id, "p1");
            assert_eq!(bound.database, "analytics");
            let active = workspace
                .active_item_as::<crate::query_view::QueryView>(cx)
                .expect("new tab should be the active item");
            assert!(
                active.focus_handle(cx).contains_focused(window, cx),
                "editor should be focused"
            );
        });

        // Connection node: no database override → profile default database.
        panel.update_in(cx, |panel, window, cx| {
            panel.open_empty_query("p1", None, window, cx);
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            let count = workspace
                .items_of_type::<crate::query_view::QueryView>(cx)
                .count();
            assert_eq!(count, 2);
            let active = workspace
                .active_item_as::<crate::query_view::QueryView>(cx)
                .expect("second tab active");
            let active = active.read(cx);
            assert_eq!(
                active.profile().expect("bound profile").database,
                "defaultdb"
            );
        });
    }

    // Drives the real panel expand orchestration against a live Postgres. Run with:
    // DATABASE_CLIENT_TEST_PG_URL=postgres://postgres@localhost:55432/testdb?sslmode=disable \
    //   cargo test -p database_client --lib -- --ignored live_panel
    // Requires databases `testdb` (seeded) and `emptydb` (no tables) to exist.
    #[gpui::test]
    #[ignore]
    async fn live_panel_expands_databases_schemas_relations_columns(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        init_test(cx);

        let mut profile = ConnectionProfile::from_url(&url).expect("valid test url");
        profile.id = "p1".into();
        profile.ssl_mode = SslMode::Disable;

        let (panel, workspace, cx) = panel_with_profile(profile, cx).await;

        // Expand connection -> databases load.
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1".into(), NodeKind::Connection, cx)
        });
        wait_until(&panel, cx, "databases to load", |panel| {
            panel.tree.databases_loaded("p1")
        })
        .await;
        let db_labels = labels(&panel, cx);
        assert!(
            db_labels.contains(&"testdb".to_string()),
            "expected testdb database, got {db_labels:?}"
        );
        assert!(
            db_labels.contains(&"emptydb".to_string()),
            "expected emptydb database, got {db_labels:?}"
        );

        // Expand the testdb database -> schemas load (second connection reused from bootstrap).
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/testdb".into(), NodeKind::Database, cx)
        });
        wait_until(&panel, cx, "testdb schemas to load", |panel| {
            panel.tree.schemas_loaded("p1/testdb")
        })
        .await;
        let schema_labels = labels(&panel, cx);
        assert!(
            schema_labels.contains(&"public".to_string()),
            "expected public schema, got {schema_labels:?}"
        );
        assert!(
            !schema_labels.iter().any(|l| l.starts_with("pg_")),
            "internal pg_* schemas should not appear, got {schema_labels:?}"
        );

        // Expand public -> relations load.
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/testdb/public".into(), NodeKind::Schema, cx)
        });
        wait_until(&panel, cx, "relations to load", |panel| {
            panel.tree.relations_loaded("p1/testdb/public")
        })
        .await;

        // Expand the Tables group -> the users table is revealed underneath it.
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/testdb/public/tables".into(),
                NodeKind::ObjectGroup(crate::tree::ObjectGroupKind::Tables),
                cx,
            )
        });
        cx.run_until_parked();
        let relation_labels = labels(&panel, cx);
        assert!(
            relation_labels.contains(&"users".to_string()),
            "expected users table under Tables group, got {relation_labels:?}"
        );

        // Expand users -> columns load.
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/testdb/public/tables/users".into(),
                NodeKind::Relation,
                cx,
            )
        });
        wait_until(&panel, cx, "columns to load", |panel| {
            panel.tree.columns_loaded("p1/testdb/public/tables/users")
        })
        .await;
        let column_labels = labels(&panel, cx);
        assert!(
            column_labels.iter().any(|l| l.starts_with("id:")),
            "expected id column under users, got {column_labels:?}"
        );

        // Expand emptydb -> its own connection; public schema exists but has no tables.
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/emptydb".into(), NodeKind::Database, cx)
        });
        wait_until(&panel, cx, "emptydb schemas to load", |panel| {
            panel.tree.schemas_loaded("p1/emptydb")
        })
        .await;
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/emptydb/public".into(), NodeKind::Schema, cx)
        });
        wait_until(&panel, cx, "emptydb relations to load", |panel| {
            panel.tree.relations_loaded("p1/emptydb/public")
        })
        .await;
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/emptydb/public/tables".into(),
                NodeKind::ObjectGroup(crate::tree::ObjectGroupKind::Tables),
                cx,
            )
        });
        cx.run_until_parked();
        // Two live connections now: p1/testdb and p1/emptydb.
        let conn_keys = panel.read_with(cx, |panel, _| {
            let mut keys: Vec<_> = panel.connections.keys().cloned().collect();
            keys.sort();
            keys
        });
        assert_eq!(
            conn_keys,
            vec!["p1/emptydb".to_string(), "p1/testdb".to_string()]
        );
        // Empty schema shows the explicit placeholder.
        let empty_present = panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .any(|n| n.kind == NodeKind::Empty && n.label == "(no tables)")
        });
        assert!(
            empty_present,
            "expected '(no tables)' placeholder under emptydb/public"
        );

        // Double-click open: must not read Workspace inside its own update
        // (regression: opening a table panicked with "cannot read Workspace
        // while it is already being updated").
        panel.update_in(cx, |panel, window, cx| {
            panel.open_query_for_relation("p1/testdb/public/tables/users", window, cx)
        });
        cx.run_until_parked();
        let query_views = workspace.update(cx, |workspace, cx| {
            workspace
                .items_of_type::<crate::query_view::QueryView>(cx)
                .count()
        });
        assert_eq!(query_views, 1, "double-click should open one QueryView tab");
    }

    // Drives `SearchObjects`' reveal path against a live Postgres, starting
    // from a fresh panel with nothing expanded and no live connection:
    // `reveal_object` must lazily expand connection -> database -> schema ->
    // Tables group, in order, before selecting the target relation. Run with:
    // DATABASE_CLIENT_TEST_PG_URL=postgres://postgres@localhost:55432/testdb?sslmode=disable \
    //   cargo test -p database_client --lib -- --ignored live_panel
    #[gpui::test]
    #[ignore]
    async fn live_reveal_object_expands_ancestors_and_selects_relation(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        init_test(cx);
        let mut profile = ConnectionProfile::from_url(&url).expect("valid test url");
        profile.id = "p1".into();
        profile.ssl_mode = SslMode::Disable;
        let (panel, _workspace, cx) = panel_with_profile(profile, cx).await;

        panel.update_in(cx, |panel, window, cx| {
            panel.reveal_object("p1/testdb/public/tables/users", false, window, cx);
        });
        wait_until(&panel, cx, "users to be revealed and selected", |panel| {
            panel.selected_node_id.as_deref() == Some("p1/testdb/public/tables/users")
        })
        .await;

        let visible = labels(&panel, cx);
        assert!(
            visible.contains(&"users".to_string()),
            "expected users table visible after reveal, got {visible:?}"
        );
        panel.read_with(cx, |panel, _| {
            assert!(panel.tree.is_expanded("p1"));
            assert!(panel.tree.is_expanded("p1/testdb"));
            assert!(panel.tree.is_expanded("p1/testdb/public"));
            assert!(panel.tree.is_expanded("p1/testdb/public/tables"));
        });
    }

    // Drives table-level object-group expansion (indexes/fkeys/constraints/
    // triggers) against a live Postgres. Run with:
    // DATABASE_CLIENT_TEST_PG_URL=postgres://postgres@localhost:55432/testdb?sslmode=disable \
    //   cargo test -p database_client --lib -- --ignored live_panel
    // Requires the `testdb` seed extended with: an extra index on `users`
    // (besides its pkey), a foreign key on `orders`, a function, a sequence,
    // a trigger on `users`, and a materialized view.
    #[gpui::test]
    #[ignore]
    async fn live_panel_expands_table_object_groups(cx: &mut TestAppContext) {
        let Ok(url) = std::env::var("DATABASE_CLIENT_TEST_PG_URL") else {
            return;
        };
        cx.executor().allow_parking();
        init_test(cx);
        let mut profile = ConnectionProfile::from_url(&url).expect("valid test url");
        profile.id = "p1".into();
        profile.ssl_mode = SslMode::Disable;
        let (panel, _workspace, cx) = panel_with_profile(profile, cx).await;

        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1".into(), NodeKind::Connection, cx)
        });
        wait_until(&panel, cx, "databases", |p| p.tree.databases_loaded("p1")).await;
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/testdb".into(), NodeKind::Database, cx)
        });
        wait_until(&panel, cx, "schemas", |p| {
            p.tree.schemas_loaded("p1/testdb")
        })
        .await;
        panel.update(cx, |panel, cx| {
            panel.toggle_node("p1/testdb/public".into(), NodeKind::Schema, cx)
        });
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/testdb/public/tables".into(),
                NodeKind::ObjectGroup(crate::tree::ObjectGroupKind::Tables),
                cx,
            )
        });
        wait_until(&panel, cx, "tables", |p| {
            p.tree.relations_loaded("p1/testdb/public")
        })
        .await;
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/testdb/public/tables/users".into(),
                NodeKind::Relation,
                cx,
            )
        });
        panel.update(cx, |panel, cx| {
            panel.toggle_node(
                "p1/testdb/public/tables/users/indexes".into(),
                NodeKind::ObjectGroup(crate::tree::ObjectGroupKind::Indexes),
                cx,
            )
        });
        wait_until(&panel, cx, "indexes", |p| {
            p.tree.indexes_loaded("p1/testdb/public/tables/users")
        })
        .await;
        let has_pkey = panel.read_with(cx, |panel, _| {
            panel
                .tree
                .visible_nodes()
                .iter()
                .any(|n| n.kind == NodeKind::Index && n.label.starts_with("users_pkey"))
        });
        assert!(has_pkey, "expected users_pkey index leaf");
    }
}
