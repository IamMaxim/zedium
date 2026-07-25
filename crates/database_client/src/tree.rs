use std::collections::{HashMap, HashSet};

use crate::connection::introspect::{
    ColumnInfo, ConstraintInfo, ForeignKeyInfo, FunctionInfo, IndexInfo, RelationInfo,
    RelationKind, SequenceInfo, TriggerInfo,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectGroupKind {
    Tables,
    Views,
    MaterializedViews,
    Functions,
    Sequences,
    Columns,
    Indexes,
    ForeignKeys,
    Constraints,
    Triggers,
}

// Pinned cross-task API: the tasks that build the schema-object UI (indexes,
// foreign keys, functions, etc.) are the first non-test consumers, so
// dead_code fires until those tasks land.
#[allow(dead_code)]
impl ObjectGroupKind {
    /// Stable path segment used inside node ids (e.g. ".../tables").
    pub fn key(self) -> &'static str {
        match self {
            ObjectGroupKind::Tables => "tables",
            ObjectGroupKind::Views => "views",
            ObjectGroupKind::MaterializedViews => "matviews",
            ObjectGroupKind::Functions => "functions",
            ObjectGroupKind::Sequences => "sequences",
            ObjectGroupKind::Columns => "columns",
            ObjectGroupKind::Indexes => "indexes",
            ObjectGroupKind::ForeignKeys => "fkeys",
            ObjectGroupKind::Constraints => "constraints",
            ObjectGroupKind::Triggers => "triggers",
        }
    }

    /// Human-readable folder title.
    pub fn title(self) -> &'static str {
        match self {
            ObjectGroupKind::Tables => "Tables",
            ObjectGroupKind::Views => "Views",
            ObjectGroupKind::MaterializedViews => "Materialized Views",
            ObjectGroupKind::Functions => "Functions",
            ObjectGroupKind::Sequences => "Sequences",
            ObjectGroupKind::Columns => "Columns",
            ObjectGroupKind::Indexes => "Indexes",
            ObjectGroupKind::ForeignKeys => "Foreign Keys",
            ObjectGroupKind::Constraints => "Constraints",
            ObjectGroupKind::Triggers => "Triggers",
        }
    }

    /// Fragment used inside the "(no …)" empty placeholder.
    pub fn empty_what(self) -> &'static str {
        match self {
            ObjectGroupKind::Tables => "tables",
            ObjectGroupKind::Views => "views",
            ObjectGroupKind::MaterializedViews => "materialized views",
            ObjectGroupKind::Functions => "functions",
            ObjectGroupKind::Sequences => "sequences",
            ObjectGroupKind::Columns => "columns",
            ObjectGroupKind::Indexes => "indexes",
            ObjectGroupKind::ForeignKeys => "foreign keys",
            ObjectGroupKind::Constraints => "constraints",
            ObjectGroupKind::Triggers => "triggers",
        }
    }

    /// Look up a schema-level group by its path segment.
    pub fn from_key(key: &str) -> Option<ObjectGroupKind> {
        [
            ObjectGroupKind::Tables,
            ObjectGroupKind::Views,
            ObjectGroupKind::MaterializedViews,
            ObjectGroupKind::Functions,
            ObjectGroupKind::Sequences,
            ObjectGroupKind::Columns,
            ObjectGroupKind::Indexes,
            ObjectGroupKind::ForeignKeys,
            ObjectGroupKind::Constraints,
            ObjectGroupKind::Triggers,
        ]
        .into_iter()
        .find(|kind| kind.key() == key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Connection,
    Database,
    Schema,
    // Pinned cross-task API: the tasks that build the schema-object UI are
    // the first non-test consumers to construct these variants, so
    // dead_code fires until those tasks land.
    #[allow(dead_code)]
    ObjectGroup(ObjectGroupKind),
    Relation,
    Column,
    #[allow(dead_code)]
    Index,
    #[allow(dead_code)]
    ForeignKey,
    #[allow(dead_code)]
    Constraint,
    #[allow(dead_code)]
    Function,
    #[allow(dead_code)]
    Sequence,
    #[allow(dead_code)]
    Trigger,
    /// Synthetic placeholder rendered under an expanded node whose children
    /// loaded empty, e.g. "(no tables)". Non-interactive.
    Empty,
}

pub struct TreeNode {
    pub id: String,
    pub depth: usize,
    pub kind: NodeKind,
    pub label: String,
    pub expanded: bool,
    pub has_children: bool,
    pub relation_kind: Option<RelationKind>,
    /// For `NodeKind::Column`: the raw Postgres type name (e.g. "integer").
    pub column_data_type: Option<String>,
    /// For `NodeKind::Column`: whether the column is part of the primary key.
    pub is_primary_key: bool,
    /// For `NodeKind::Relation`: the row count, once loaded.
    pub row_count: Option<i64>,
}

struct ConnectionEntry {
    id: String,
    name: String,
}

pub struct TreeState {
    connections: Vec<ConnectionEntry>,
    /// conn-id -> database names
    databases: HashMap<String, Vec<String>>,
    /// "{conn}/{db}" -> schema names
    schemas: HashMap<String, Vec<String>>,
    /// "{conn}/{db}/{schema}" -> relations
    relations: HashMap<String, Vec<RelationInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> columns
    columns: HashMap<String, Vec<ColumnInfo>>,
    /// "{conn}/{db}/{schema}" -> materialized views
    materialized_views: HashMap<String, Vec<RelationInfo>>,
    /// "{conn}/{db}/{schema}" -> functions
    functions: HashMap<String, Vec<FunctionInfo>>,
    /// "{conn}/{db}/{schema}" -> sequences
    sequences: HashMap<String, Vec<SequenceInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> indexes
    indexes: HashMap<String, Vec<IndexInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> foreign keys
    foreign_keys: HashMap<String, Vec<ForeignKeyInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> constraints
    constraints: HashMap<String, Vec<ConstraintInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> triggers
    triggers: HashMap<String, Vec<TriggerInfo>>,
    /// "{conn}/{db}/{schema}/{rel}" -> row count
    row_counts: HashMap<String, i64>,
    expanded: HashSet<String>,
}

/// Unloaded children (None) keep the disclosure arrow; loaded-empty hides it.
fn loaded_has_children<T>(loaded: Option<&Vec<T>>) -> bool {
    match loaded {
        None => true,
        Some(v) => !v.is_empty(),
    }
}

fn empty_node(parent_id: &str, depth: usize, what: &str) -> TreeNode {
    TreeNode {
        id: format!("{parent_id}/(empty)"),
        depth,
        kind: NodeKind::Empty,
        label: format!("(no {what})"),
        expanded: false,
        has_children: false,
        relation_kind: None,
        column_data_type: None,
        is_primary_key: false,
        row_count: None,
    }
}

fn leaf_node(group_id: &str, name: &str, kind: NodeKind, label: String) -> TreeNode {
    TreeNode {
        id: format!("{group_id}/{name}"),
        depth: 6,
        kind,
        label,
        expanded: false,
        has_children: false,
        relation_kind: None,
        column_data_type: None,
        is_primary_key: false,
        row_count: None,
    }
}

impl TreeState {
    pub fn new(connections: Vec<(String, String)>) -> Self {
        TreeState {
            connections: connections
                .into_iter()
                .map(|(id, name)| ConnectionEntry { id, name })
                .collect(),
            databases: HashMap::new(),
            schemas: HashMap::new(),
            relations: HashMap::new(),
            columns: HashMap::new(),
            materialized_views: HashMap::new(),
            functions: HashMap::new(),
            sequences: HashMap::new(),
            indexes: HashMap::new(),
            foreign_keys: HashMap::new(),
            constraints: HashMap::new(),
            triggers: HashMap::new(),
            row_counts: HashMap::new(),
            expanded: HashSet::new(),
        }
    }

    pub fn visible_nodes(&self) -> Vec<TreeNode> {
        let mut nodes = Vec::new();
        for conn in &self.connections {
            let expanded = self.expanded.contains(&conn.id);
            nodes.push(TreeNode {
                id: conn.id.clone(),
                depth: 0,
                kind: NodeKind::Connection,
                label: conn.name.clone(),
                expanded,
                has_children: loaded_has_children(self.databases.get(&conn.id)),
                relation_kind: None,
                column_data_type: None,
                is_primary_key: false,
                row_count: None,
            });
            if expanded {
                match self.databases.get(&conn.id) {
                    None => {}
                    Some(dbs) if dbs.is_empty() => {
                        nodes.push(empty_node(&conn.id, 1, "databases"));
                    }
                    Some(dbs) => {
                        for db_name in dbs {
                            self.push_database(&mut nodes, &conn.id, db_name);
                        }
                    }
                }
            }
        }
        nodes
    }

    fn push_database(&self, nodes: &mut Vec<TreeNode>, conn_id: &str, db_name: &str) {
        let db_id = format!("{conn_id}/{db_name}");
        let expanded = self.expanded.contains(&db_id);
        nodes.push(TreeNode {
            id: db_id.clone(),
            depth: 1,
            kind: NodeKind::Database,
            label: db_name.to_string(),
            expanded,
            has_children: loaded_has_children(self.schemas.get(&db_id)),
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
        if expanded {
            match self.schemas.get(&db_id) {
                None => {}
                Some(schemas) if schemas.is_empty() => {
                    nodes.push(empty_node(&db_id, 2, "schemas"));
                }
                Some(schemas) => {
                    for schema_name in schemas {
                        self.push_schema(nodes, &db_id, schema_name);
                    }
                }
            }
        }
    }

    fn push_schema(&self, nodes: &mut Vec<TreeNode>, db_id: &str, schema_name: &str) {
        let schema_id = format!("{db_id}/{schema_name}");
        let expanded = self.expanded.contains(&schema_id);
        nodes.push(TreeNode {
            id: schema_id.clone(),
            depth: 2,
            kind: NodeKind::Schema,
            label: schema_name.to_string(),
            expanded,
            has_children: true,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
        if expanded {
            for kind in [
                ObjectGroupKind::Tables,
                ObjectGroupKind::Views,
                ObjectGroupKind::MaterializedViews,
                ObjectGroupKind::Functions,
                ObjectGroupKind::Sequences,
            ] {
                self.push_schema_group(nodes, &schema_id, kind);
            }
        }
    }

    #[allow(dead_code)]
    fn schema_group_loaded(&self, schema_id: &str, kind: ObjectGroupKind) -> bool {
        match kind {
            ObjectGroupKind::Tables | ObjectGroupKind::Views => {
                self.relations.contains_key(schema_id)
            }
            ObjectGroupKind::MaterializedViews => self.materialized_views.contains_key(schema_id),
            ObjectGroupKind::Functions => self.functions.contains_key(schema_id),
            ObjectGroupKind::Sequences => self.sequences.contains_key(schema_id),
            _ => false,
        }
    }

    fn schema_group_count(&self, schema_id: &str, kind: ObjectGroupKind) -> Option<usize> {
        match kind {
            ObjectGroupKind::Tables => self
                .relations
                .get(schema_id)
                .map(|r| r.iter().filter(|x| x.kind == RelationKind::Table).count()),
            ObjectGroupKind::Views => self
                .relations
                .get(schema_id)
                .map(|r| r.iter().filter(|x| x.kind == RelationKind::View).count()),
            ObjectGroupKind::MaterializedViews => {
                self.materialized_views.get(schema_id).map(|v| v.len())
            }
            ObjectGroupKind::Functions => self.functions.get(schema_id).map(|v| v.len()),
            ObjectGroupKind::Sequences => self.sequences.get(schema_id).map(|v| v.len()),
            _ => None,
        }
    }

    fn push_schema_group(&self, nodes: &mut Vec<TreeNode>, schema_id: &str, kind: ObjectGroupKind) {
        let group_id = format!("{schema_id}/{}", kind.key());
        let expanded = self.expanded.contains(&group_id);
        let count = self.schema_group_count(schema_id, kind);
        let has_children = match count {
            None => true,
            Some(n) => n > 0,
        };
        let label = match count {
            Some(n) => format!("{} ({n})", kind.title()),
            None => kind.title().to_string(),
        };
        nodes.push(TreeNode {
            id: group_id.clone(),
            depth: 3,
            kind: NodeKind::ObjectGroup(kind),
            label,
            expanded,
            has_children,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
        if !expanded {
            return;
        }
        match kind {
            ObjectGroupKind::Tables | ObjectGroupKind::Views => {
                let Some(relations) = self.relations.get(schema_id) else {
                    return;
                };
                let wanted = if kind == ObjectGroupKind::Tables {
                    RelationKind::Table
                } else {
                    RelationKind::View
                };
                let mut any = false;
                for rel in relations.iter().filter(|r| r.kind == wanted) {
                    any = true;
                    self.push_relation(nodes, &group_id, rel);
                }
                if !any {
                    nodes.push(empty_node(&group_id, 4, kind.empty_what()));
                }
            }
            ObjectGroupKind::MaterializedViews => match self.materialized_views.get(schema_id) {
                None => {}
                Some(views) if views.is_empty() => {
                    nodes.push(empty_node(&group_id, 4, kind.empty_what()));
                }
                Some(views) => {
                    for view in views {
                        self.push_relation(nodes, &group_id, view);
                    }
                }
            },
            ObjectGroupKind::Functions => match self.functions.get(schema_id) {
                None => {}
                Some(functions) if functions.is_empty() => {
                    nodes.push(empty_node(&group_id, 4, kind.empty_what()));
                }
                Some(functions) => {
                    for function in functions {
                        self.push_function(nodes, &group_id, function);
                    }
                }
            },
            ObjectGroupKind::Sequences => match self.sequences.get(schema_id) {
                None => {}
                Some(sequences) if sequences.is_empty() => {
                    nodes.push(empty_node(&group_id, 4, kind.empty_what()));
                }
                Some(sequences) => {
                    for sequence in sequences {
                        self.push_sequence(nodes, &group_id, sequence);
                    }
                }
            },
            _ => {}
        }
    }

    fn push_function(&self, nodes: &mut Vec<TreeNode>, group_id: &str, function: &FunctionInfo) {
        nodes.push(TreeNode {
            id: format!("{group_id}/{}", function.name),
            depth: 4,
            kind: NodeKind::Function,
            label: format!(
                "{}({}): {}",
                function.name, function.signature, function.returns
            ),
            expanded: false,
            has_children: false,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
    }

    fn push_sequence(&self, nodes: &mut Vec<TreeNode>, group_id: &str, sequence: &SequenceInfo) {
        nodes.push(TreeNode {
            id: format!("{group_id}/{}", sequence.name),
            depth: 4,
            kind: NodeKind::Sequence,
            label: sequence.name.clone(),
            expanded: false,
            has_children: false,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
    }

    fn push_relation(&self, nodes: &mut Vec<TreeNode>, group_id: &str, rel: &RelationInfo) {
        let rel_id = format!("{group_id}/{}", rel.name);
        let expanded = self.expanded.contains(&rel_id);
        nodes.push(TreeNode {
            id: rel_id.clone(),
            depth: 4,
            kind: NodeKind::Relation,
            label: rel.name.clone(),
            expanded,
            has_children: loaded_has_children(self.columns.get(&rel_id)),
            relation_kind: Some(rel.kind),
            column_data_type: None,
            is_primary_key: false,
            row_count: if rel.kind == RelationKind::Table {
                self.row_counts.get(&rel_id).copied()
            } else {
                None
            },
        });
        // Views and materialized views only ever expose their Columns; the
        // remaining sub-groups (Indexes/FKs/Constraints/Triggers) are
        // table-only concepts that don't exist for a view/matview relation.
        if expanded {
            self.push_table_group(nodes, &rel_id, ObjectGroupKind::Columns);
            if rel.kind == RelationKind::Table {
                self.push_table_group(nodes, &rel_id, ObjectGroupKind::Indexes);
                self.push_table_group(nodes, &rel_id, ObjectGroupKind::ForeignKeys);
                self.push_table_group(nodes, &rel_id, ObjectGroupKind::Constraints);
                self.push_table_group(nodes, &rel_id, ObjectGroupKind::Triggers);
            }
        }
    }

    pub fn table_group_loaded(&self, rel_id: &str, kind: ObjectGroupKind) -> bool {
        match kind {
            ObjectGroupKind::Columns => self.columns.contains_key(rel_id),
            ObjectGroupKind::Indexes => self.indexes.contains_key(rel_id),
            ObjectGroupKind::ForeignKeys => self.foreign_keys.contains_key(rel_id),
            ObjectGroupKind::Constraints => self.constraints.contains_key(rel_id),
            ObjectGroupKind::Triggers => self.triggers.contains_key(rel_id),
            _ => false,
        }
    }

    fn table_group_count(&self, rel_id: &str, kind: ObjectGroupKind) -> Option<usize> {
        match kind {
            ObjectGroupKind::Columns => self.columns.get(rel_id).map(|v| v.len()),
            ObjectGroupKind::Indexes => self.indexes.get(rel_id).map(|v| v.len()),
            ObjectGroupKind::ForeignKeys => self.foreign_keys.get(rel_id).map(|v| v.len()),
            ObjectGroupKind::Constraints => self.constraints.get(rel_id).map(|v| v.len()),
            ObjectGroupKind::Triggers => self.triggers.get(rel_id).map(|v| v.len()),
            _ => None,
        }
    }

    fn push_table_group(&self, nodes: &mut Vec<TreeNode>, rel_id: &str, kind: ObjectGroupKind) {
        let group_id = format!("{rel_id}/{}", kind.key());
        let expanded = self.expanded.contains(&group_id);
        let count = self.table_group_count(rel_id, kind);
        let has_children = match count {
            None => true,
            Some(n) => n > 0,
        };
        let label = match count {
            Some(n) => format!("{} ({n})", kind.title()),
            None => kind.title().to_string(),
        };
        nodes.push(TreeNode {
            id: group_id.clone(),
            depth: 5,
            kind: NodeKind::ObjectGroup(kind),
            label,
            expanded,
            has_children,
            relation_kind: None,
            column_data_type: None,
            is_primary_key: false,
            row_count: None,
        });
        if !expanded {
            return;
        }
        match kind {
            ObjectGroupKind::Columns => match self.columns.get(rel_id) {
                None => {}
                Some(cols) if cols.is_empty() => {
                    nodes.push(empty_node(&group_id, 6, "columns"));
                }
                Some(cols) => {
                    for col in cols {
                        nodes.push(TreeNode {
                            id: format!("{group_id}/{}", col.name),
                            depth: 6,
                            kind: NodeKind::Column,
                            label: format!("{}: {}", col.name, col.data_type),
                            expanded: false,
                            has_children: false,
                            relation_kind: None,
                            column_data_type: Some(col.data_type.clone()),
                            is_primary_key: col.is_primary_key,
                            row_count: None,
                        });
                    }
                }
            },
            ObjectGroupKind::Indexes => match self.indexes.get(rel_id) {
                None => {}
                Some(items) if items.is_empty() => {
                    nodes.push(empty_node(&group_id, 6, kind.empty_what()));
                }
                Some(items) => {
                    for index in items {
                        let label = if index.is_primary {
                            format!("{} (primary)", index.name)
                        } else if index.is_unique {
                            format!("{} (unique)", index.name)
                        } else {
                            index.name.clone()
                        };
                        nodes.push(leaf_node(&group_id, &index.name, NodeKind::Index, label));
                    }
                }
            },
            ObjectGroupKind::ForeignKeys => match self.foreign_keys.get(rel_id) {
                None => {}
                Some(items) if items.is_empty() => {
                    nodes.push(empty_node(&group_id, 6, kind.empty_what()));
                }
                Some(items) => {
                    for fk in items {
                        let label = format!("{} → {}", fk.name, fk.referenced_table);
                        nodes.push(leaf_node(&group_id, &fk.name, NodeKind::ForeignKey, label));
                    }
                }
            },
            ObjectGroupKind::Constraints => match self.constraints.get(rel_id) {
                None => {}
                Some(items) if items.is_empty() => {
                    nodes.push(empty_node(&group_id, 6, kind.empty_what()));
                }
                Some(items) => {
                    for constraint in items {
                        let label = format!("{} [{}]", constraint.name, constraint.kind);
                        nodes.push(leaf_node(
                            &group_id,
                            &constraint.name,
                            NodeKind::Constraint,
                            label,
                        ));
                    }
                }
            },
            ObjectGroupKind::Triggers => match self.triggers.get(rel_id) {
                None => {}
                Some(items) if items.is_empty() => {
                    nodes.push(empty_node(&group_id, 6, kind.empty_what()));
                }
                Some(items) => {
                    for trigger in items {
                        nodes.push(leaf_node(
                            &group_id,
                            &trigger.name,
                            NodeKind::Trigger,
                            trigger.name.clone(),
                        ));
                    }
                }
            },
            _ => {}
        }
    }

    pub fn toggle(&mut self, id: &str) {
        if self.expanded.contains(id) {
            self.expanded.remove(id);
        } else {
            self.expanded.insert(id.to_string());
        }
    }

    pub fn set_databases(&mut self, conn_id: &str, databases: Vec<String>) {
        self.databases.insert(conn_id.to_string(), databases);
    }

    pub fn set_schemas(&mut self, db_id: &str, schemas: Vec<String>) {
        self.schemas.insert(db_id.to_string(), schemas);
    }

    pub fn set_relations(&mut self, schema_id: &str, relations: Vec<RelationInfo>) {
        self.relations.insert(schema_id.to_string(), relations);
    }

    pub fn set_columns(&mut self, rel_id: &str, columns: Vec<ColumnInfo>) {
        self.columns.insert(rel_id.to_string(), columns);
    }

    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    pub fn databases_loaded(&self, conn_id: &str) -> bool {
        self.databases.contains_key(conn_id)
    }

    pub fn schemas_loaded(&self, db_id: &str) -> bool {
        self.schemas.contains_key(db_id)
    }

    pub fn relations_loaded(&self, schema_id: &str) -> bool {
        self.relations.contains_key(schema_id)
    }

    pub fn columns_loaded(&self, rel_id: &str) -> bool {
        self.columns.contains_key(rel_id)
    }

    // Pinned cross-task API: the tasks that build the schema-object UI
    // (indexes, foreign keys, functions, etc.) are the first non-test
    // consumers, so dead_code fires until those tasks land.
    #[allow(dead_code)]
    pub fn set_materialized_views(&mut self, schema_id: &str, views: Vec<RelationInfo>) {
        self.materialized_views.insert(schema_id.to_string(), views);
    }

    #[allow(dead_code)]
    pub fn materialized_views_loaded(&self, schema_id: &str) -> bool {
        self.materialized_views.contains_key(schema_id)
    }

    #[allow(dead_code)]
    pub fn set_functions(&mut self, schema_id: &str, functions: Vec<FunctionInfo>) {
        self.functions.insert(schema_id.to_string(), functions);
    }

    #[allow(dead_code)]
    pub fn functions_loaded(&self, schema_id: &str) -> bool {
        self.functions.contains_key(schema_id)
    }

    #[allow(dead_code)]
    pub fn set_sequences(&mut self, schema_id: &str, sequences: Vec<SequenceInfo>) {
        self.sequences.insert(schema_id.to_string(), sequences);
    }

    #[allow(dead_code)]
    pub fn sequences_loaded(&self, schema_id: &str) -> bool {
        self.sequences.contains_key(schema_id)
    }

    #[allow(dead_code)]
    pub fn set_indexes(&mut self, rel_id: &str, indexes: Vec<IndexInfo>) {
        self.indexes.insert(rel_id.to_string(), indexes);
    }

    #[allow(dead_code)]
    pub fn indexes_loaded(&self, rel_id: &str) -> bool {
        self.indexes.contains_key(rel_id)
    }

    #[allow(dead_code)]
    pub fn set_foreign_keys(&mut self, rel_id: &str, foreign_keys: Vec<ForeignKeyInfo>) {
        self.foreign_keys.insert(rel_id.to_string(), foreign_keys);
    }

    #[allow(dead_code)]
    pub fn foreign_keys_loaded(&self, rel_id: &str) -> bool {
        self.foreign_keys.contains_key(rel_id)
    }

    #[allow(dead_code)]
    pub fn set_constraints(&mut self, rel_id: &str, constraints: Vec<ConstraintInfo>) {
        self.constraints.insert(rel_id.to_string(), constraints);
    }

    #[allow(dead_code)]
    pub fn constraints_loaded(&self, rel_id: &str) -> bool {
        self.constraints.contains_key(rel_id)
    }

    #[allow(dead_code)]
    pub fn set_triggers(&mut self, rel_id: &str, triggers: Vec<TriggerInfo>) {
        self.triggers.insert(rel_id.to_string(), triggers);
    }

    #[allow(dead_code)]
    pub fn triggers_loaded(&self, rel_id: &str) -> bool {
        self.triggers.contains_key(rel_id)
    }

    pub fn set_row_count(&mut self, rel_id: &str, count: i64) {
        self.row_counts.insert(rel_id.to_string(), count);
    }

    #[allow(dead_code)]
    pub fn row_count(&self, rel_id: &str) -> Option<i64> {
        self.row_counts.get(rel_id).copied()
    }

    /// Remove all cached databases, schemas, relations, and columns for a
    /// connection and collapse its descendants. The connection node's own
    /// expanded state is preserved so callers can collapse it separately.
    pub fn clear_connection(&mut self, conn_id: &str) {
        self.databases.remove(conn_id);
        let prefix = format!("{}/", conn_id);
        self.schemas.retain(|k, _| !k.starts_with(&prefix));
        self.relations.retain(|k, _| !k.starts_with(&prefix));
        self.columns.retain(|k, _| !k.starts_with(&prefix));
        self.materialized_views
            .retain(|k, _| !k.starts_with(&prefix));
        self.functions.retain(|k, _| !k.starts_with(&prefix));
        self.sequences.retain(|k, _| !k.starts_with(&prefix));
        self.indexes.retain(|k, _| !k.starts_with(&prefix));
        self.foreign_keys.retain(|k, _| !k.starts_with(&prefix));
        self.constraints.retain(|k, _| !k.starts_with(&prefix));
        self.triggers.retain(|k, _| !k.starts_with(&prefix));
        self.row_counts.retain(|k, _| !k.starts_with(&prefix));
        self.expanded.retain(|k| !k.starts_with(&prefix));
    }

    /// Collapse a node (remove from the expanded set).
    pub fn collapse(&mut self, id: &str) {
        self.expanded.remove(id);
    }

    /// Drop cached children for `id` and every descendant so a subsequent
    /// expand re-introspects. The node's own expanded state is preserved.
    pub fn clear_subtree(&mut self, id: &str) {
        let prefix = format!("{id}/");
        let matches = |key: &str| key == id || key.starts_with(&prefix);
        self.schemas.retain(|k, _| !matches(k));
        self.relations.retain(|k, _| !matches(k));
        self.columns.retain(|k, _| !matches(k));
        self.materialized_views.retain(|k, _| !matches(k));
        self.functions.retain(|k, _| !matches(k));
        self.sequences.retain(|k, _| !matches(k));
        self.indexes.retain(|k, _| !matches(k));
        self.foreign_keys.retain(|k, _| !matches(k));
        self.constraints.retain(|k, _| !matches(k));
        self.triggers.retain(|k, _| !matches(k));
        self.row_counts.retain(|k, _| !matches(k));
        self.databases.retain(|k, _| !matches(k));
    }
}

/// Muted, right-aligned suffix for a table's approximate row count. A negative
/// `reltuples` (never analyzed) renders nothing.
pub fn row_count_suffix(count: i64) -> Option<String> {
    if count < 0 {
        return None;
    }
    let unit = if count == 1 { "row" } else { "rows" };
    Some(format!("~{count} {unit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_one_conn() -> TreeState {
        TreeState::new(vec![("p1".into(), "Local".into())])
    }

    /// Expand + load the path p1 -> db1 -> public -> users.
    fn loaded_tree() -> TreeState {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1/db1");
        t.set_schemas("p1/db1", vec!["public".into()]);
        t.toggle("p1/db1/public");
        t.set_relations(
            "p1/db1/public",
            vec![RelationInfo {
                name: "users".into(),
                kind: RelationKind::Table,
            }],
        );
        t
    }

    fn labels(t: &TreeState) -> Vec<String> {
        t.visible_nodes().iter().map(|n| n.label.clone()).collect()
    }

    #[test]
    fn collapsed_connection_shows_only_itself() {
        let t = tree_one_conn();
        let nodes = t.visible_nodes();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, NodeKind::Connection);
        assert!(!nodes[0].expanded);
    }

    #[test]
    fn expanding_connection_reveals_loaded_databases() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["app".into(), "postgres".into()]);
        assert_eq!(labels(&t), vec!["Local", "app", "postgres"]);
        let nodes = t.visible_nodes();
        assert_eq!(nodes[1].kind, NodeKind::Database);
        assert_eq!(nodes[1].id, "p1/app");
        assert_eq!(nodes[1].depth, 1);
    }

    #[test]
    fn expanded_but_not_yet_loaded_shows_only_parent() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        // databases not loaded yet -> no children, no empty placeholder
        assert_eq!(t.visible_nodes().len(), 1);
    }

    #[test]
    fn expanding_database_reveals_schemas() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1/db1");
        t.set_schemas("p1/db1", vec!["app".into(), "public".into()]);
        assert_eq!(labels(&t), vec!["Local", "db1", "app", "public"]);
        let nodes = t.visible_nodes();
        assert_eq!(nodes[2].kind, NodeKind::Schema);
        assert_eq!(nodes[2].id, "p1/db1/app");
        assert_eq!(nodes[2].depth, 2);
    }

    #[test]
    fn expanded_schema_shows_object_group_folders() {
        let mut t = loaded_tree(); // p1 -> db1 -> public (expanded), relations loaded with `users` table
        let labels = labels(&t);
        // The schema now yields the five group folders, not the relation directly.
        assert!(labels.contains(&"Tables (1)".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Views (0)".to_string()), "got {labels:?}");
        assert!(
            labels.contains(&"Materialized Views".to_string()),
            "got {labels:?}"
        );
        assert!(labels.contains(&"Functions".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Sequences".to_string()), "got {labels:?}");

        // Expanding the Tables group reveals the table under it at depth 4.
        t.toggle("p1/db1/public/tables");
        let nodes = t.visible_nodes();
        let table = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users")
            .expect("users under Tables group");
        assert_eq!(table.kind, NodeKind::Relation);
        assert_eq!(table.depth, 4);
        let group = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables")
            .expect("Tables group node");
        assert_eq!(group.kind, NodeKind::ObjectGroup(ObjectGroupKind::Tables));
        assert_eq!(group.depth, 3);
    }

    #[test]
    fn expanding_relation_reveals_columns() {
        let mut t = loaded_tree();
        t.toggle("p1/db1/public/tables");
        t.toggle("p1/db1/public/tables/users");
        t.toggle("p1/db1/public/tables/users/columns");
        t.set_columns(
            "p1/db1/public/tables/users",
            vec![ColumnInfo {
                name: "id".into(),
                data_type: "integer".into(),
                is_primary_key: true,
            }],
        );
        let nodes = t.visible_nodes();
        let column = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users/columns/id")
            .expect("id column under users");
        assert_eq!(column.kind, NodeKind::Column);
        assert_eq!(column.label, "id: integer");
        assert_eq!(column.depth, 6);
        assert!(!column.has_children);
    }

    #[test]
    fn column_nodes_carry_type_and_primary_key() {
        let mut t = loaded_tree();
        t.toggle("p1/db1/public/tables");
        t.toggle("p1/db1/public/tables/users");
        t.toggle("p1/db1/public/tables/users/columns");
        t.set_columns(
            "p1/db1/public/tables/users",
            vec![
                ColumnInfo {
                    name: "id".into(),
                    data_type: "integer".into(),
                    is_primary_key: true,
                },
                ColumnInfo {
                    name: "email".into(),
                    data_type: "text".into(),
                    is_primary_key: false,
                },
            ],
        );
        let nodes = t.visible_nodes();
        let id_col = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users/columns/id")
            .expect("id column");
        assert_eq!(id_col.column_data_type.as_deref(), Some("integer"));
        assert!(id_col.is_primary_key);
        let email_col = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users/columns/email")
            .expect("email column");
        assert_eq!(email_col.column_data_type.as_deref(), Some("text"));
        assert!(!email_col.is_primary_key);
        assert!(
            nodes[0].column_data_type.is_none(),
            "non-column nodes carry no type"
        );
    }

    #[test]
    fn expanding_then_collapsing_hides_children() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        assert_eq!(t.visible_nodes().len(), 2);
        t.toggle("p1");
        assert_eq!(t.visible_nodes().len(), 1);
    }

    #[test]
    fn re_expanding_after_collapse_does_not_lose_children() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1"); // collapse
        t.toggle("p1"); // re-expand, no set_databases call
        assert_eq!(labels(&t), vec!["Local", "db1"]);
    }

    #[test]
    fn multiple_connections_both_visible() {
        let t = TreeState::new(vec![
            ("p1".into(), "Local".into()),
            ("p2".into(), "Remote".into()),
        ]);
        assert_eq!(labels(&t), vec!["Local", "Remote"]);
    }

    // --- empty-state placeholders ---

    #[test]
    fn loaded_empty_databases_show_placeholder() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec![]);
        let nodes = t.visible_nodes();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].kind, NodeKind::Empty);
        assert_eq!(nodes[1].label, "(no databases)");
        assert_eq!(nodes[1].depth, 1);
        assert!(!nodes[1].has_children);
        // parent hides its disclosure arrow
        assert!(!nodes[0].has_children);
    }

    #[test]
    fn loaded_empty_schemas_show_placeholder() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1/db1");
        t.set_schemas("p1/db1", vec![]);
        let nodes = t.visible_nodes();
        assert_eq!(nodes[2].kind, NodeKind::Empty);
        assert_eq!(nodes[2].label, "(no schemas)");
        assert_eq!(nodes[2].depth, 2);
    }

    #[test]
    fn loaded_empty_relations_show_placeholder() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1/db1");
        t.set_schemas("p1/db1", vec!["public".into()]);
        t.toggle("p1/db1/public");
        t.set_relations("p1/db1/public", vec![]);
        t.toggle("p1/db1/public/tables");
        let nodes = t.visible_nodes();
        let placeholder = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/(empty)")
            .expect("empty placeholder under Tables group");
        assert_eq!(placeholder.kind, NodeKind::Empty);
        assert_eq!(placeholder.label, "(no tables)");
        assert_eq!(placeholder.depth, 4);
    }

    #[test]
    fn loaded_empty_columns_show_placeholder() {
        let mut t = loaded_tree();
        t.toggle("p1/db1/public/tables");
        t.toggle("p1/db1/public/tables/users");
        t.toggle("p1/db1/public/tables/users/columns");
        t.set_columns("p1/db1/public/tables/users", vec![]);
        let nodes = t.visible_nodes();
        let placeholder = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users/columns/(empty)")
            .expect("empty placeholder under users columns group");
        assert_eq!(placeholder.kind, NodeKind::Empty);
        assert_eq!(placeholder.label, "(no columns)");
        assert_eq!(placeholder.depth, 6);
    }

    #[test]
    fn expanded_table_shows_sub_group_folders() {
        let mut t = loaded_tree();
        t.toggle("p1/db1/public/tables");
        t.toggle("p1/db1/public/tables/users"); // expand the table
        let labels = labels(&t);
        assert!(labels.contains(&"Columns".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Indexes".to_string()), "got {labels:?}");
        assert!(
            labels.contains(&"Foreign Keys".to_string()),
            "got {labels:?}"
        );
        assert!(
            labels.contains(&"Constraints".to_string()),
            "got {labels:?}"
        );
        assert!(labels.contains(&"Triggers".to_string()), "got {labels:?}");

        // Load + expand Indexes.
        t.set_indexes(
            "p1/db1/public/tables/users",
            vec![IndexInfo {
                name: "users_pkey".into(),
                is_unique: true,
                is_primary: true,
                definition: "CREATE UNIQUE INDEX ...".into(),
            }],
        );
        t.toggle("p1/db1/public/tables/users/indexes");
        let nodes = t.visible_nodes();
        let index = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users/indexes/users_pkey")
            .expect("index leaf");
        assert_eq!(index.kind, NodeKind::Index);
        assert_eq!(index.depth, 6);
    }

    #[test]
    fn view_relation_shows_only_columns_sub_group() {
        let mut t = loaded_tree();
        t.set_relations(
            "p1/db1/public",
            vec![RelationInfo {
                name: "v_users".into(),
                kind: RelationKind::View,
            }],
        );
        t.toggle("p1/db1/public/views");
        t.toggle("p1/db1/public/views/v_users");
        let labels = labels(&t);
        assert!(labels.contains(&"Columns".to_string()), "got {labels:?}");
        assert!(
            !labels.contains(&"Indexes".to_string()),
            "views have no Indexes group"
        );
    }

    #[test]
    fn matview_relation_shows_only_columns_sub_group() {
        let mut t = loaded_tree();
        t.set_materialized_views(
            "p1/db1/public",
            vec![RelationInfo {
                name: "mv_users".into(),
                kind: RelationKind::MaterializedView,
            }],
        );
        t.toggle("p1/db1/public/matviews");
        t.toggle("p1/db1/public/matviews/mv_users");
        let labels = labels(&t);
        assert!(labels.contains(&"Columns".to_string()), "got {labels:?}");
        assert!(
            !labels.contains(&"Indexes".to_string()),
            "materialized views have no Indexes group"
        );
        assert!(
            !labels.contains(&"Foreign Keys".to_string()),
            "materialized views have no Foreign Keys group"
        );
        assert!(
            !labels.contains(&"Constraints".to_string()),
            "materialized views have no Constraints group"
        );
        assert!(
            !labels.contains(&"Triggers".to_string()),
            "materialized views have no Triggers group"
        );
    }

    #[test]
    fn unloaded_children_show_no_placeholder() {
        let mut t = tree_one_conn();
        t.toggle("p1");
        t.set_databases("p1", vec!["db1".into()]);
        t.toggle("p1/db1"); // schemas NOT loaded
        let nodes = t.visible_nodes();
        assert_eq!(nodes.len(), 2, "no placeholder while loading");
    }

    // --- lifecycle ---

    #[test]
    fn clear_connection_removes_all_child_data_and_keeps_connection_node() {
        let mut t = loaded_tree();
        assert!(t.databases_loaded("p1"));
        assert!(t.schemas_loaded("p1/db1"));
        assert!(t.relations_loaded("p1/db1/public"));

        t.clear_connection("p1");

        assert!(!t.databases_loaded("p1"));
        assert!(!t.schemas_loaded("p1/db1"));
        assert!(!t.relations_loaded("p1/db1/public"));
        assert!(!t.is_expanded("p1/db1"));
        let nodes = t.visible_nodes();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, NodeKind::Connection);
    }

    #[test]
    fn clear_subtree_invalidates_node_and_descendants() {
        let mut t = loaded_tree();
        t.set_columns(
            "p1/db1/public/tables/users",
            vec![ColumnInfo {
                name: "id".into(),
                data_type: "integer".into(),
                is_primary_key: true,
            }],
        );
        t.set_indexes("p1/db1/public/tables/users", vec![]);
        t.set_row_count("p1/db1/public/tables/users", 5);

        // Refreshing the schema drops relations + everything below it.
        t.clear_subtree("p1/db1/public");
        assert!(!t.relations_loaded("p1/db1/public"));
        assert!(!t.columns_loaded("p1/db1/public/tables/users"));
        assert!(!t.indexes_loaded("p1/db1/public/tables/users"));
        assert_eq!(t.row_count("p1/db1/public/tables/users"), None);
    }

    #[test]
    fn connection_node_has_children_before_load() {
        let t = tree_one_conn();
        assert!(t.visible_nodes()[0].has_children);
    }

    #[test]
    fn stores_and_reports_loaded_object_collections() {
        let mut t = tree_one_conn();
        let schema_id = "p1/db1/public";
        let rel_id = "p1/db1/public/tables/users";

        assert!(!t.functions_loaded(schema_id));
        t.set_functions(
            schema_id,
            vec![FunctionInfo {
                name: "f".into(),
                signature: String::new(),
                returns: "void".into(),
                language: "sql".into(),
            }],
        );
        assert!(t.functions_loaded(schema_id));

        assert!(!t.sequences_loaded(schema_id));
        t.set_sequences(schema_id, vec![]);
        assert!(t.sequences_loaded(schema_id));

        assert!(!t.materialized_views_loaded(schema_id));
        t.set_materialized_views(
            schema_id,
            vec![RelationInfo {
                name: "mv".into(),
                kind: RelationKind::MaterializedView,
            }],
        );
        assert!(t.materialized_views_loaded(schema_id));

        assert!(!t.indexes_loaded(rel_id));
        t.set_indexes(
            rel_id,
            vec![IndexInfo {
                name: "i".into(),
                is_unique: false,
                is_primary: false,
                definition: String::new(),
            }],
        );
        assert!(t.indexes_loaded(rel_id));

        t.set_foreign_keys(rel_id, vec![]);
        t.set_constraints(rel_id, vec![]);
        t.set_triggers(rel_id, vec![]);
        assert!(t.foreign_keys_loaded(rel_id));
        assert!(t.constraints_loaded(rel_id));
        assert!(t.triggers_loaded(rel_id));

        assert_eq!(t.row_count(rel_id), None);
        t.set_row_count(rel_id, 99);
        assert_eq!(t.row_count(rel_id), Some(99));
    }

    #[test]
    fn table_node_carries_row_count_and_negative_is_hidden() {
        let mut t = loaded_tree();
        t.set_row_count("p1/db1/public/tables/users", 1500);
        t.toggle("p1/db1/public/tables");
        let nodes = t.visible_nodes();
        let table = nodes
            .iter()
            .find(|n| n.id == "p1/db1/public/tables/users")
            .expect("users table");
        assert_eq!(table.row_count, Some(1500));

        assert_eq!(row_count_suffix(1500).as_deref(), Some("~1500 rows"));
        assert_eq!(row_count_suffix(1).as_deref(), Some("~1 row"));
        assert_eq!(
            row_count_suffix(-1),
            None,
            "unknown count renders no suffix"
        );
        assert_eq!(row_count_suffix(0).as_deref(), Some("~0 rows"));
    }
}
