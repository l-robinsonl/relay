//! Integration tests for local persistence.

use relay_models::{
    AuthConfig, Collection, Environment, HistoryEntry, HistoryId, HttpMethod, HttpRequestSpec,
    KeyValue, RequestBody, RequestDoc, RequestId, Timestamp, TreeNode, Variable, Workspace,
};
use relay_storage::{
    AppStateRepo, CollectionRepo, Database, EnvironmentRepo, HistoryRepo, RequestRepo,
    StorageError, TreeRepo, WorkspaceRepo,
};

fn db() -> Database {
    Database::open_in_memory().unwrap()
}

fn seeded_workspace(db: &Database) -> Workspace {
    let ws = Workspace::new("Test workspace");
    db.with(|c| WorkspaceRepo::save(c, &ws)).unwrap();
    ws
}

#[test]
fn workspace_crud_round_trips() {
    let db = db();
    let mut ws = Workspace::new("My APIs");
    ws.description = "Personal".into();
    ws.variables = vec![Variable::new("baseUrl", "https://api.example.com")];

    db.with(|c| WorkspaceRepo::save(c, &ws)).unwrap();

    let loaded = db.with(|c| WorkspaceRepo::get(c, &ws.id)).unwrap();
    assert_eq!(loaded.name, "My APIs");
    assert_eq!(loaded.description, "Personal");
    assert_eq!(loaded.variables.len(), 1);
    assert_eq!(loaded.variables[0].value, "https://api.example.com");
    assert!(loaded.revision.dirty, "a local save must be marked dirty");

    let mut updated = loaded.clone();
    updated.name = "Renamed".into();
    db.with(|c| WorkspaceRepo::save(c, &updated)).unwrap();
    assert_eq!(
        db.with(|c| WorkspaceRepo::get(c, &ws.id)).unwrap().name,
        "Renamed"
    );

    assert_eq!(db.with(WorkspaceRepo::list).unwrap().len(), 1);

    db.with(|c| WorkspaceRepo::delete(c, &ws.id)).unwrap();
    assert!(db.with(|c| WorkspaceRepo::get(c, &ws.id)).is_err());
    assert!(db.with(WorkspaceRepo::list).unwrap().is_empty());
}

#[test]
fn a_workspace_without_a_name_is_rejected() {
    let db = db();
    let mut ws = Workspace::new("   ");
    ws.name = "   ".into();
    assert!(db.with(|c| WorkspaceRepo::save(c, &ws)).is_err());
}

#[test]
fn deleting_a_workspace_tombstones_its_contents() {
    let db = db();
    let ws = seeded_workspace(&db);

    let collection = Collection::new(ws.id.clone(), "Users");
    db.with(|c| CollectionRepo::save(c, &collection)).unwrap();

    let mut request = RequestDoc::new(ws.id.clone(), "List users");
    request.collection_id = Some(collection.id.clone());
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    db.with(|c| WorkspaceRepo::delete(c, &ws.id)).unwrap();

    // Children must be tombstoned, not merely orphaned, so the deletion can
    // be synchronised to other devices.
    assert!(db.with(|c| RequestRepo::get(c, &request.id)).is_err());
    assert!(db.with(|c| CollectionRepo::get(c, &collection.id)).is_err());
}

#[test]
fn request_crud_round_trips_every_field() {
    let db = db();
    let ws = seeded_workspace(&db);

    let mut request = RequestDoc::new(ws.id.clone(), "Create user");
    request.method = HttpMethod::Post;
    request.url = "{{baseUrl}}/users".into();
    request.headers = vec![KeyValue::new("Accept", "application/json")];
    request.query_params = vec![KeyValue::new("verbose", "true")];
    request.body = RequestBody::Json {
        text: r#"{"name":"Ada"}"#.into(),
    };
    request.auth = AuthConfig::Bearer {
        token: "{{token}}".into(),
    };
    request.tags = vec!["users".into(), "write".into()];
    request.settings.timeout_ms = Some(5000);
    request.settings.verify_tls = false;

    db.with(|c| RequestRepo::save(c, &request)).unwrap();
    let loaded = db.with(|c| RequestRepo::get(c, &request.id)).unwrap();

    assert_eq!(loaded.method, HttpMethod::Post);
    assert_eq!(loaded.url, "{{baseUrl}}/users");
    assert_eq!(loaded.headers, request.headers);
    assert_eq!(loaded.query_params, request.query_params);
    assert_eq!(loaded.body, request.body);
    assert_eq!(loaded.auth, request.auth);
    assert_eq!(loaded.tags, request.tags);
    assert_eq!(loaded.settings.timeout_ms, Some(5000));
    assert!(!loaded.settings.verify_tls);
}

#[test]
fn a_saved_request_survives_closing_and_reopening_the_database() {
    // This is the Phase 1 exit criterion: create, save, quit, reopen.
    let dir = std::env::temp_dir().join(format!("relay-reopen-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("relay.db");

    let (workspace_id, request_id) = {
        let db = Database::open(&path).unwrap();
        let ws = Workspace::new("Persisted");
        db.with(|c| WorkspaceRepo::save(c, &ws)).unwrap();

        let mut request = RequestDoc::new(ws.id.clone(), "Health check");
        request.method = HttpMethod::Get;
        request.url = "https://api.example.com/health".into();
        request.headers = vec![KeyValue::new("X-Trace", "1")];
        db.with(|c| RequestRepo::save(c, &request)).unwrap();

        (ws.id, request.id)
    };

    // A completely fresh open, as though the application had been restarted.
    let db = Database::open(&path).unwrap();
    let workspaces = db.with(WorkspaceRepo::list).unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, workspace_id);

    let request = db.with(|c| RequestRepo::get(c, &request_id)).unwrap();
    assert_eq!(request.name, "Health check");
    assert_eq!(request.url, "https://api.example.com/health");
    assert_eq!(request.headers[0].key, "X-Trace");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn deleted_requests_are_tombstoned_not_removed() {
    let db = db();
    let ws = seeded_workspace(&db);
    let request = RequestDoc::new(ws.id.clone(), "Doomed");
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    db.with(|c| RequestRepo::delete(c, &request.id)).unwrap();
    assert!(db.with(|c| RequestRepo::get(c, &request.id)).is_err());

    // The row is still physically present with a tombstone, so the deletion
    // itself can be pushed to a sync server.
    let raw: i64 = db
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM requests WHERE id = ?1 AND deleted_at IS NOT NULL",
                [request.id.as_str()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(raw, 1, "the deletion left no tombstone to synchronise");
}

#[test]
fn deleting_a_missing_request_reports_not_found() {
    let db = db();
    match db.with(|c| RequestRepo::delete(c, &RequestId::from("nope"))) {
        Err(StorageError::NotFound { entity, .. }) => assert_eq!(entity, "Request"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn duplicating_a_request_produces_an_independent_copy() {
    let db = db();
    let ws = seeded_workspace(&db);
    let mut original = RequestDoc::new(ws.id.clone(), "Original");
    original.url = "https://example.com".into();
    db.with(|c| RequestRepo::save(c, &original)).unwrap();

    let copy = db
        .with(|c| RequestRepo::duplicate(c, &original.id))
        .unwrap();

    assert_ne!(copy.id, original.id);
    assert_eq!(copy.name, "Original copy");
    assert_eq!(copy.url, original.url);
    assert_eq!(copy.revision.rev, 0, "a copy has never been synced");

    // Editing the copy must not touch the original.
    let mut edited = copy.clone();
    edited.url = "https://changed.example".into();
    db.with(|c| RequestRepo::save(c, &edited)).unwrap();
    assert_eq!(
        db.with(|c| RequestRepo::get(c, &original.id)).unwrap().url,
        "https://example.com"
    );
}

#[test]
fn requests_can_be_moved_between_folders() {
    let db = db();
    let ws = seeded_workspace(&db);
    let a = Collection::new(ws.id.clone(), "A");
    let b = Collection::new(ws.id.clone(), "B");
    db.with(|c| CollectionRepo::save(c, &a)).unwrap();
    db.with(|c| CollectionRepo::save(c, &b)).unwrap();

    let mut request = RequestDoc::new(ws.id.clone(), "Movable");
    request.collection_id = Some(a.id.clone());
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    db.with(|c| RequestRepo::move_to(c, &request.id, Some(&b.id), 2.0))
        .unwrap();

    let moved = db.with(|c| RequestRepo::get(c, &request.id)).unwrap();
    assert_eq!(moved.collection_id, Some(b.id));
    assert_eq!(moved.sort_order, 2.0);
}

#[test]
fn nested_folders_build_a_tree() {
    let db = db();
    let ws = seeded_workspace(&db);

    let root = Collection::new(ws.id.clone(), "API");
    db.with(|c| CollectionRepo::save(c, &root)).unwrap();

    let mut child = Collection::new(ws.id.clone(), "Users");
    child.parent_id = Some(root.id.clone());
    db.with(|c| CollectionRepo::save(c, &child)).unwrap();

    let mut request = RequestDoc::new(ws.id.clone(), "List");
    request.collection_id = Some(child.id.clone());
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    let tree = db.with(|c| TreeRepo::tree(c, &ws.id)).unwrap();
    assert_eq!(tree.len(), 3);

    let root_node = tree
        .iter()
        .find(|n| n.name() == "API")
        .expect("root folder missing");
    assert!(root_node.parent_id().is_none());
    match root_node {
        TreeNode::Folder { child_count, .. } => assert_eq!(*child_count, 1),
        other => panic!("expected a folder, got {other:?}"),
    }

    let request_node = tree
        .iter()
        .find(|n| n.name() == "List")
        .expect("request missing");
    assert_eq!(request_node.parent_id(), Some(&child.id));
}

#[test]
fn deleting_a_folder_tombstones_its_whole_subtree() {
    let db = db();
    let ws = seeded_workspace(&db);

    let root = Collection::new(ws.id.clone(), "Root");
    db.with(|c| CollectionRepo::save(c, &root)).unwrap();
    let mut child = Collection::new(ws.id.clone(), "Child");
    child.parent_id = Some(root.id.clone());
    db.with(|c| CollectionRepo::save(c, &child)).unwrap();
    let mut grandchild = Collection::new(ws.id.clone(), "Grandchild");
    grandchild.parent_id = Some(child.id.clone());
    db.with(|c| CollectionRepo::save(c, &grandchild)).unwrap();

    let mut deep_request = RequestDoc::new(ws.id.clone(), "Deep");
    deep_request.collection_id = Some(grandchild.id.clone());
    db.with(|c| RequestRepo::save(c, &deep_request)).unwrap();

    db.with(|c| CollectionRepo::delete(c, &root.id)).unwrap();

    assert!(db.with(|c| CollectionRepo::get(c, &child.id)).is_err());
    assert!(db.with(|c| CollectionRepo::get(c, &grandchild.id)).is_err());
    assert!(db.with(|c| RequestRepo::get(c, &deep_request.id)).is_err());
    assert!(db.with(|c| TreeRepo::tree(c, &ws.id)).unwrap().is_empty());
}

#[test]
fn a_folder_cannot_be_moved_inside_itself() {
    let db = db();
    let ws = seeded_workspace(&db);

    let parent = Collection::new(ws.id.clone(), "Parent");
    db.with(|c| CollectionRepo::save(c, &parent)).unwrap();
    let mut child = Collection::new(ws.id.clone(), "Child");
    child.parent_id = Some(parent.id.clone());
    db.with(|c| CollectionRepo::save(c, &child)).unwrap();

    // Moving the parent under its own child would detach the subtree.
    let mut cyclic = parent.clone();
    cyclic.parent_id = Some(child.id.clone());
    assert!(db.with(|c| CollectionRepo::save(c, &cyclic)).is_err());

    // And a folder cannot be its own parent either.
    let mut self_parent = parent.clone();
    self_parent.parent_id = Some(parent.id.clone());
    assert!(db.with(|c| CollectionRepo::save(c, &self_parent)).is_err());
}

#[test]
fn secret_environment_values_are_never_written_to_the_database() {
    let db = db();
    let ws = seeded_workspace(&db);

    let mut env = Environment::new(ws.id.clone(), "Production");
    env.variables = vec![
        Variable::new("baseUrl", "https://api.example.com"),
        Variable {
            key: "apiToken".into(),
            value: "SUPER-SECRET-VALUE".into(),
            enabled: true,
            secret: true,
            description: None,
        },
    ];
    db.with(|c| EnvironmentRepo::save(c, &env)).unwrap();

    // Read the raw column, not the typed object: the secret must not be in
    // the file at all, because the file gets backed up and copied around.
    let raw: String = db
        .with(|c| {
            c.query_row(
                "SELECT variables_json FROM environments WHERE id = ?1",
                [env.id.as_str()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();

    assert!(
        !raw.contains("SUPER-SECRET-VALUE"),
        "a secret was persisted: {raw}"
    );
    assert!(raw.contains("apiToken"), "the variable name should survive");
    assert!(raw.contains("https://api.example.com"));

    let loaded = db.with(|c| EnvironmentRepo::get(c, &env.id)).unwrap();
    let secret = loaded
        .variables
        .iter()
        .find(|v| v.key == "apiToken")
        .unwrap();
    assert!(secret.secret);
    assert_eq!(secret.value, "");
}

#[test]
fn environments_round_trip_and_delete() {
    let db = db();
    let ws = seeded_workspace(&db);

    let mut dev = Environment::new(ws.id.clone(), "Development");
    dev.variables = vec![Variable::new("baseUrl", "http://localhost:3000")];
    let mut prod = Environment::new(ws.id.clone(), "Production");
    prod.sort_order = 1.0;
    prod.variables = vec![Variable::new("baseUrl", "https://api.example.com")];

    db.with(|c| EnvironmentRepo::save(c, &dev)).unwrap();
    db.with(|c| EnvironmentRepo::save(c, &prod)).unwrap();

    let list = db.with(|c| EnvironmentRepo::list(c, &ws.id)).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "Development");

    db.with(|c| EnvironmentRepo::delete(c, &dev.id)).unwrap();
    assert_eq!(
        db.with(|c| EnvironmentRepo::list(c, &ws.id)).unwrap().len(),
        1
    );
}

#[test]
fn history_records_executions_newest_first() {
    let db = db();
    let ws = seeded_workspace(&db);

    for i in 0..5 {
        let entry = HistoryEntry {
            id: HistoryId::new(),
            workspace_id: ws.id.clone(),
            request_id: None,
            name: format!("Request {i}"),
            method: HttpMethod::Get,
            url: format!("https://example.com/{i}"),
            status: Some(200),
            status_text: Some("OK".into()),
            duration_ms: 12.5,
            response_size: 100,
            executed_at: Timestamp(1000 + i),
            error: None,
            request_snapshot: Box::new(HttpRequestSpec::new(
                HttpMethod::Get,
                format!("https://example.com/{i}"),
            )),
        };
        db.with(|c| HistoryRepo::insert(c, &entry)).unwrap();
    }

    let entries = db.with(|c| HistoryRepo::list(c, &ws.id, 10, 0)).unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0].name, "Request 4", "history must be newest first");

    let page = db.with(|c| HistoryRepo::list(c, &ws.id, 2, 2)).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].name, "Request 2");
}

#[test]
fn history_redacts_credentials_even_if_the_caller_forgets() {
    let db = db();
    let ws = seeded_workspace(&db);

    let mut spec = HttpRequestSpec::new(HttpMethod::Get, "https://api.example.com");
    spec.auth = AuthConfig::Bearer {
        token: "sk-live-LEAKED".into(),
    };
    spec.headers = vec![KeyValue::new("Authorization", "Bearer sk-live-LEAKED")];

    let entry = HistoryEntry {
        id: HistoryId::new(),
        workspace_id: ws.id.clone(),
        request_id: None,
        name: "Unredacted".into(),
        method: HttpMethod::Get,
        url: "https://api.example.com".into(),
        status: Some(200),
        status_text: Some("OK".into()),
        duration_ms: 1.0,
        response_size: 0,
        executed_at: Timestamp::now(),
        error: None,
        // Deliberately NOT redacted by the caller.
        request_snapshot: Box::new(spec),
    };
    db.with(|c| HistoryRepo::insert(c, &entry)).unwrap();

    let raw: String = db
        .with(|c| {
            c.query_row(
                "SELECT request_snapshot_json FROM history_entries WHERE id = ?1",
                [entry.id.as_str()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();

    assert!(
        !raw.contains("sk-live-LEAKED"),
        "history stored a secret: {raw}"
    );
}

#[test]
fn history_can_be_pruned_and_cleared() {
    let db = db();
    let ws = seeded_workspace(&db);

    for i in 0..20 {
        let entry = HistoryEntry {
            id: HistoryId::new(),
            workspace_id: ws.id.clone(),
            request_id: None,
            name: format!("R{i}"),
            method: HttpMethod::Get,
            url: "https://example.com".into(),
            status: Some(200),
            status_text: None,
            duration_ms: 1.0,
            response_size: 0,
            executed_at: Timestamp(1000 + i),
            error: None,
            request_snapshot: Box::new(HttpRequestSpec::new(
                HttpMethod::Get,
                "https://example.com",
            )),
        };
        db.with(|c| HistoryRepo::insert(c, &entry)).unwrap();
    }

    db.with(|c| HistoryRepo::prune(c, &ws.id, 5)).unwrap();
    let kept = db.with(|c| HistoryRepo::list(c, &ws.id, 100, 0)).unwrap();
    assert_eq!(kept.len(), 5);
    assert_eq!(kept[0].name, "R19", "pruning kept the wrong end");

    db.with(|c| HistoryRepo::clear(c, &ws.id)).unwrap();
    assert!(
        db.with(|c| HistoryRepo::list(c, &ws.id, 100, 0))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn app_state_round_trips() {
    let db = db();
    assert_eq!(db.with(|c| AppStateRepo::get(c, "theme")).unwrap(), None);

    db.with(|c| AppStateRepo::set(c, "theme", "dark")).unwrap();
    assert_eq!(
        db.with(|c| AppStateRepo::get(c, "theme"))
            .unwrap()
            .as_deref(),
        Some("dark")
    );

    db.with(|c| AppStateRepo::set(c, "theme", "light")).unwrap();
    assert_eq!(
        db.with(|c| AppStateRepo::get(c, "theme"))
            .unwrap()
            .as_deref(),
        Some("light")
    );
}

#[test]
fn malformed_json_in_a_column_names_the_column() {
    let db = db();
    let ws = seeded_workspace(&db);
    let request = RequestDoc::new(ws.id.clone(), "Corruptible");
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    db.with(|c| {
        c.execute(
            "UPDATE requests SET body_json = 'not json' WHERE id = ?1",
            [request.id.as_str()],
        )
        .map_err(Into::into)
    })
    .unwrap();

    let err = db.with(|c| RequestRepo::get(c, &request.id)).unwrap_err();
    assert!(
        err.to_string().contains("body_json"),
        "the error should name the column: {err}"
    );
}

#[test]
fn workspaces_are_isolated_from_each_other() {
    let db = db();
    let a = Workspace::new("A");
    let b = Workspace::new("B");
    db.with(|c| WorkspaceRepo::save(c, &a)).unwrap();
    db.with(|c| WorkspaceRepo::save(c, &b)).unwrap();

    let request = RequestDoc::new(a.id.clone(), "Only in A");
    db.with(|c| RequestRepo::save(c, &request)).unwrap();

    assert_eq!(db.with(|c| RequestRepo::list(c, &a.id)).unwrap().len(), 1);
    assert!(db.with(|c| RequestRepo::list(c, &b.id)).unwrap().is_empty());
    assert!(db.with(|c| TreeRepo::tree(c, &b.id)).unwrap().is_empty());
}
