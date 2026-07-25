use crate::connection::{introspect::RelationKind, metadata_cache::RelationMeta};
use crate::panel::DatabasePanel;
use crate::tree::TreeNode;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, Styled, Task, WeakEntity, Window, rems,
};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{Color, Label, LabelSize, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, Workspace};

/// The chain of ancestor node ids for a slash-delimited tree id, shallowest
/// first, excluding the node itself. e.g. "p1/db1/public/users" ->
/// ["p1", "p1/db1", "p1/db1/public"].
pub fn ancestor_ids(node_id: &str) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut prefix = String::new();
    let segments: Vec<&str> = node_id.split('/').collect();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        ancestors.push(prefix.clone());
    }
    ancestors
}

/// A searchable database object, addressed by its full tree node id
/// (including the object-group segment, e.g. `"p1/db1/public/tables/users"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectCandidate {
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    pub node_id: String,
}

/// Maps a connection's cached relations into search candidates, mirroring
/// how `TreeState::push_relation` addresses the same objects: `"{conn_key}/{schema}/{group}/{name}"`.
fn candidates_for_connection(conn_key: &str, relations: &[RelationMeta]) -> Vec<ObjectCandidate> {
    relations
        .iter()
        .map(|rel| {
            let group = match rel.kind {
                RelationKind::Table => "tables",
                RelationKind::View => "views",
                RelationKind::MaterializedView => "matviews",
            };
            ObjectCandidate {
                schema: rel.schema.clone(),
                name: rel.name.clone(),
                kind: rel.kind,
                node_id: format!("{conn_key}/{}/{group}/{}", rel.schema, rel.name),
            }
        })
        .collect()
}

fn candidate_label(candidate: &ObjectCandidate) -> String {
    format!("{}.{}", candidate.schema, candidate.name)
}

/// Indices of `candidates` whose `schema.name` label fuzzy-matches `query`.
fn filter_candidates(candidates: &[ObjectCandidate], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..candidates.len()).collect();
    }
    let query = query.trim();
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| fuzzy_matches(&candidate_label(candidate), query))
        .map(|(index, _)| index)
        .collect()
}

pub(crate) struct SearchObjectsModal {
    picker: Entity<Picker<SearchObjectsDelegate>>,
}

impl SearchObjectsModal {
    fn new(
        candidates: Vec<ObjectCandidate>,
        panel: WeakEntity<DatabasePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let matches = filter_candidates(&candidates, "");
        let delegate = SearchObjectsDelegate {
            modal: cx.entity().downgrade(),
            panel,
            candidates,
            matches,
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for SearchObjectsModal {}
impl EventEmitter<DismissEvent> for SearchObjectsModal {}

impl Focusable for SearchObjectsModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for SearchObjectsModal {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SearchObjectsModal")
            .w(rems(44.))
            .child(self.picker.clone())
    }
}

pub(crate) struct SearchObjectsDelegate {
    modal: WeakEntity<SearchObjectsModal>,
    panel: WeakEntity<DatabasePanel>,
    candidates: Vec<ObjectCandidate>,
    matches: Vec<usize>,
    selected_index: usize,
}

impl PickerDelegate for SearchObjectsDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "database object search"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search objects by name…".into()
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
        self.matches = filter_candidates(&self.candidates, &query);
        self.selected_index = 0;
        Task::ready(())
    }

    fn confirm(&mut self, secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(&candidate_index) = self.matches.get(self.selected_index) else {
            return;
        };
        let Some(candidate) = self.candidates.get(candidate_index) else {
            return;
        };
        let node_id = candidate.node_id.clone();
        self.panel
            .update(cx, |panel, cx| {
                panel.reveal_object(&node_id, secondary, window, cx)
            })
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
        let candidate_index = *self.matches.get(index)?;
        let candidate = self.candidates.get(candidate_index)?;
        let kind_label = match candidate.kind {
            RelationKind::Table => "table",
            RelationKind::View => "view",
            RelationKind::MaterializedView => "materialized view",
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
                        .child(Label::new(candidate_label(candidate)).single_line())
                        .child(
                            Label::new(kind_label)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                ),
        )
    }
}

/// Opens the `SearchObjects` picker, loading each live connection's metadata
/// (Plan 3's `MetadataCache`) to build the candidate list.
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(panel) = workspace.panel::<DatabasePanel>(cx) else {
        return;
    };
    let connections = panel.read(cx).live_connections();
    if connections.is_empty() {
        return;
    }
    let panel_weak = panel.downgrade();
    cx.spawn_in(window, async move |workspace, cx| {
        let mut candidates = Vec::new();
        for (conn_key, connection) in connections {
            let metadata = cx.update(|_window, cx| connection.load_metadata(cx))?.await;
            match metadata {
                Ok(metadata) => {
                    candidates.extend(candidates_for_connection(&conn_key, &metadata.relations));
                }
                Err(err) => log::error!("load_metadata for {conn_key}: {err}"),
            }
        }
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                SearchObjectsModal::new(candidates, panel_weak, window, cx)
            });
        })
    })
    .detach();
}

/// Case-insensitive subsequence match of `query` against `label`.
pub fn fuzzy_matches(label: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut query_chars = query.chars().map(|c| c.to_ascii_lowercase()).peekable();
    for label_char in label.chars().map(|c| c.to_ascii_lowercase()) {
        if query_chars.peek() == Some(&label_char) {
            query_chars.next();
        }
    }
    query_chars.peek().is_none()
}

/// Keep nodes whose label fuzzy-matches `query`, always retaining an
/// ancestor of any kept node so the tree stays structurally valid.
///
/// Takes ownership of `nodes` (rather than `&[TreeNode]`) because `TreeNode`
/// does not implement `Clone`, and every caller already holds a freshly
/// built, owned `Vec<TreeNode>` from `TreeState::visible_nodes`.
pub fn filter_visible_nodes(nodes: Vec<TreeNode>, query: &str) -> Vec<TreeNode> {
    if query.trim().is_empty() {
        return nodes;
    }
    let query = query.trim();
    let mut keep = vec![false; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if fuzzy_matches(&node.label, query) {
            keep[index] = true;
            // Retain the nearest shallower ancestor chain (monotonic depth).
            let mut depth = node.depth;
            for prior in (0..index).rev() {
                if nodes[prior].depth < depth {
                    keep[prior] = true;
                    depth = nodes[prior].depth;
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
    }
    nodes
        .into_iter()
        .zip(keep)
        .filter_map(|(node, keep)| keep.then_some(node))
        .collect()
}

#[cfg(test)]
mod reveal_tests {
    use super::{ancestor_ids, candidates_for_connection, filter_candidates};
    use crate::connection::introspect::RelationKind;
    use crate::connection::metadata_cache::{ColumnMeta, RelationMeta};

    fn relation(schema: &str, name: &str, kind: RelationKind) -> RelationMeta {
        RelationMeta {
            schema: schema.to_string(),
            name: name.to_string(),
            kind,
            columns: vec![ColumnMeta {
                name: "id".to_string(),
                data_type: "integer".to_string(),
                is_primary_key: true,
            }],
        }
    }

    #[test]
    fn ancestor_ids_are_shallowest_first() {
        assert_eq!(
            ancestor_ids("p1/db1/public/users"),
            vec!["p1", "p1/db1", "p1/db1/public"]
        );
        assert!(ancestor_ids("p1").is_empty());
        assert_eq!(ancestor_ids("p1/db1"), vec!["p1".to_string()]);
    }

    #[test]
    fn ancestor_ids_handle_object_group_segment() {
        // Object node ids include the object-group segment (e.g. "tables"),
        // so a table's ancestors include the group folder itself.
        assert_eq!(
            ancestor_ids("p1/db1/public/tables/users"),
            vec!["p1", "p1/db1", "p1/db1/public", "p1/db1/public/tables"]
        );
    }

    #[test]
    fn candidates_address_objects_by_their_full_tree_node_id() {
        let relations = vec![
            relation("public", "users", RelationKind::Table),
            relation("public", "users_view", RelationKind::View),
            relation("public", "users_summary", RelationKind::MaterializedView),
        ];
        let candidates = candidates_for_connection("p1/db1", &relations);
        let node_ids: Vec<_> = candidates.iter().map(|c| c.node_id.as_str()).collect();
        assert_eq!(
            node_ids,
            vec![
                "p1/db1/public/tables/users",
                "p1/db1/public/views/users_view",
                "p1/db1/public/matviews/users_summary",
            ]
        );
    }

    #[test]
    fn filter_candidates_matches_schema_dot_name_fuzzily() {
        let relations = vec![
            relation("public", "users", RelationKind::Table),
            relation("public", "orders", RelationKind::Table),
        ];
        let candidates = candidates_for_connection("p1/db1", &relations);
        assert_eq!(filter_candidates(&candidates, "usr"), vec![0]);
        assert_eq!(filter_candidates(&candidates, ""), vec![0, 1]);
        assert!(filter_candidates(&candidates, "zzz").is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{NodeKind, TreeNode};

    fn node(id: &str, depth: usize, label: &str) -> TreeNode {
        TreeNode {
            id: id.into(),
            depth,
            kind: NodeKind::Relation,
            label: label.into(),
            expanded: true,
            has_children: false,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        }
    }

    #[test]
    fn fuzzy_matches_is_subsequence_case_insensitive() {
        assert!(fuzzy_matches("users", "usr"));
        assert!(fuzzy_matches("OrderItems", "orit"));
        assert!(!fuzzy_matches("users", "xyz"));
        assert!(fuzzy_matches("anything", ""));
    }

    #[test]
    fn filter_keeps_matches_and_their_ancestors() {
        let nodes = vec![
            node("s", 0, "public"),
            node("s/users", 1, "users"),
            node("s/orders", 1, "orders"),
        ];
        let filtered = filter_visible_nodes(nodes, "usr");
        let labels: Vec<_> = filtered.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["public", "users"]);
    }

    #[test]
    fn empty_query_returns_all() {
        let nodes = vec![node("s", 0, "public"), node("s/users", 1, "users")];
        assert_eq!(filter_visible_nodes(nodes, "  ").len(), 2);
    }

    #[test]
    fn filter_with_no_matches_returns_empty() {
        let nodes = vec![node("s", 0, "public"), node("s/users", 1, "users")];
        assert!(filter_visible_nodes(nodes, "zzz").is_empty());
    }

    #[test]
    fn filter_matches_multiple_siblings_under_same_ancestor() {
        let nodes = vec![
            node("s", 0, "public"),
            node("s/users", 1, "users"),
            node("s/user_roles", 1, "user_roles"),
            node("s/orders", 1, "orders"),
        ];
        let filtered = filter_visible_nodes(nodes, "user");
        let labels: Vec<_> = filtered.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["public", "users", "user_roles"]);
    }
}
