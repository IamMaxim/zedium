use anyhow::Result;
use db::{
    query,
    sqlez::{domain::Domain, statement::Statement, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use workspace::{ItemId, WorkspaceDb, WorkspaceId};

pub struct QueryDb(ThreadSafeConnection);

impl Domain for QueryDb {
    const NAME: &str = stringify!(QueryDb);

    const MIGRATIONS: &[&str] = &[
        sql!(
            CREATE TABLE database_query_views (
                item_id INTEGER,
                workspace_id INTEGER,
                profile_id TEXT NOT NULL,
                sql TEXT NOT NULL,
                PRIMARY KEY(item_id, workspace_id),
                FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                ON DELETE CASCADE
            ) STRICT;
        ),
        // Persist the database a query tab targets, so a tab opened on a
        // non-default database (e.g. via double-click on a table) restores
        // against the same database rather than the profile's default.
        sql!(
            ALTER TABLE database_query_views ADD COLUMN database TEXT;
        ),
        // Persist the editor/results split ratio of a query tab (NULL = default).
        sql!(
            ALTER TABLE database_query_views ADD COLUMN editor_ratio REAL;
        ),
        // Persist the tab's commit mode ("auto"/"manual"); NULL = default (auto).
        sql!(
            ALTER TABLE database_query_views ADD COLUMN commit_mode TEXT;
        ),
    ];
}

db::static_connection!(QueryDb, [WorkspaceDb]);

impl QueryDb {
    pub async fn save_query(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
        profile_id: String,
        sql: String,
        database: Option<String>,
        editor_ratio: Option<f64>,
        commit_mode: Option<String>,
    ) -> Result<()> {
        self.write(move |conn| {
            let query_str =
                "INSERT INTO database_query_views (item_id, workspace_id, profile_id, sql, database, editor_ratio, commit_mode)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT (item_id, workspace_id) DO UPDATE SET
                    profile_id = excluded.profile_id,
                    sql = excluded.sql,
                    database = excluded.database,
                    editor_ratio = excluded.editor_ratio,
                    commit_mode = excluded.commit_mode";
            let mut statement = Statement::prepare(conn, query_str)?;
            let mut next_index = statement.bind(&item_id, 1)?;
            next_index = statement.bind(&workspace_id, next_index)?;
            next_index = statement.bind(&profile_id, next_index)?;
            next_index = statement.bind(&sql, next_index)?;
            next_index = statement.bind(&database, next_index)?;
            next_index = statement.bind(&editor_ratio, next_index)?;
            statement.bind(&commit_mode, next_index)?;
            statement.exec()
        })
        .await
    }

    query! {
        pub fn get_query(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<(String, String, Option<String>, Option<f64>, Option<String>)>> {
            SELECT profile_id, sql, database, editor_ratio, commit_mode
            FROM database_query_views
            WHERE item_id = ? AND workspace_id = ?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(db::AppDatabase::test_new()));
    }

    #[gpui::test]
    async fn round_trips_persisted_database(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        query_db
            .save_query(
                1,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                Some("otherdb".to_string()),
                None,
                None,
            )
            .await
            .unwrap();

        let (profile_id, sql, database, _, _) =
            query_db.get_query(1, workspace_id).unwrap().unwrap();
        assert_eq!(profile_id, "p1");
        assert_eq!(sql, "select 1");
        assert_eq!(database, Some("otherdb".to_string()));
    }

    #[gpui::test]
    async fn missing_database_loads_as_none(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        query_db
            .save_query(
                2,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let (_, _, database, _, _) = query_db.get_query(2, workspace_id).unwrap().unwrap();
        assert_eq!(database, None);
    }

    #[gpui::test]
    async fn round_trips_editor_ratio(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        query_db
            .save_query(
                3,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                None,
                Some(0.42),
                None,
            )
            .await
            .unwrap();

        let (_, _, _, editor_ratio, _) = query_db.get_query(3, workspace_id).unwrap().unwrap();
        assert_eq!(editor_ratio, Some(0.42));
    }

    #[gpui::test]
    async fn missing_editor_ratio_loads_as_none(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        // NULL back-compat: rows written before the editor_ratio migration have NULL.
        query_db
            .save_query(
                4,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let (_, _, _, editor_ratio, _) = query_db.get_query(4, workspace_id).unwrap().unwrap();
        assert_eq!(editor_ratio, None);
    }

    #[gpui::test]
    async fn round_trips_commit_mode(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        query_db
            .save_query(
                5,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                None,
                None,
                Some("manual".to_string()),
            )
            .await
            .unwrap();

        let (_, _, _, _, commit_mode) = query_db.get_query(5, workspace_id).unwrap().unwrap();
        assert_eq!(commit_mode, Some("manual".to_string()));
    }

    #[gpui::test]
    async fn missing_commit_mode_loads_as_none(cx: &mut TestAppContext) {
        init_test(cx);
        let workspace_db = cx.update(|cx| WorkspaceDb::global(cx));
        let workspace_id = workspace_db.next_id().await.unwrap();
        let query_db = cx.update(|cx| QueryDb::global(cx));

        // NULL back-compat: rows written before the commit_mode migration have NULL.
        query_db
            .save_query(
                6,
                workspace_id,
                "p1".to_string(),
                "select 1".to_string(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let (_, _, _, _, commit_mode) = query_db.get_query(6, workspace_id).unwrap().unwrap();
        assert_eq!(commit_mode, None);
    }
}
