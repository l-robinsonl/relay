//! One behaviour suite, run against every backend.
//!
//! Two store implementations only stay equivalent if something checks. This
//! suite is written once against the [`SyncStore`] trait and executed
//! against SQLite always, and against PostgreSQL whenever
//! `RELAY_TEST_POSTGRES_URL` is set — which is how the Neon-backed
//! deployment is verified.

use relay_server::store::{Actor, NewUser, SyncStore};
use relay_server::{PostgresStore, ServerError, SqliteStore};
use relay_sync::{EntityKind, Mutation, MutationBatch, MutationOutcome, Role};
use std::sync::Arc;

/// Creates an isolated store for one test.
async fn sqlite_store() -> Arc<dyn SyncStore> {
    // A private in-memory database per test: shared state between tests
    // turns a real failure into a confusing one.
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    Arc::new(store)
}

/// A Postgres store against a scratch schema, when one is configured.
///
/// Absent configuration means "do not test Postgres" and is fine.
/// Configuration that does not work is a failure, loudly: this returned
/// `None` on a connection error for months, so the whole suite quietly
/// ran SQLite-only against a database that had been deleted, and every
/// run still said it passed.
async fn postgres_store() -> Option<Arc<dyn SyncStore>> {
    let url = std::env::var("RELAY_TEST_POSTGRES_URL").ok()?;
    if url.trim().is_empty() {
        return None;
    }

    // This suite drops and recreates the `relay` schema. Pointed at a
    // database holding real data it would destroy it, and the variable
    // being named TEST is not much of a guard when a connection string
    // is copied from somewhere else. Insist the database name says so.
    let database = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    assert!(
        database.contains("test"),
        "RELAY_TEST_POSTGRES_URL points at a database called '{database}'. \
         These tests DROP the relay schema, so the name has to contain \
         'test' — create a scratch database rather than aiming this at a \
         real one."
    );

    let store = PostgresStore::connect(&url).await.unwrap_or_else(|e| {
        panic!(
            "RELAY_TEST_POSTGRES_URL is set but unusable, so these tests would \
             silently cover SQLite only: {e}"
        )
    });

    // Tests share one database, so start from a clean slate each run.
    //
    // This drops Relay's own schema and nothing else. The target database
    // may be shared with other applications, so a broader reset — or one
    // naming bare tables in `public` — would be unacceptable here.
    let _ = sqlx::raw_sql("DROP SCHEMA IF EXISTS relay CASCADE")
        .execute(store.pool())
        .await;

    store
        .migrate()
        .await
        .expect("the configured Postgres could not be migrated");

    Some(Arc::new(store))
}

/// Runs `body` against every available backend.
async fn for_each_backend<F, Fut>(body: F)
where
    F: Fn(Arc<dyn SyncStore>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    body(sqlite_store().await).await;

    if let Some(store) = postgres_store().await {
        eprintln!("  (also running against postgres)");
        body(store).await;
    }
}

async fn seed_user(store: &Arc<dyn SyncStore>, email: &str) -> Actor {
    let profile = store
        .create_user(NewUser {
            email: email.into(),
            password_hash: "not-a-real-hash".into(),
            display_name: email.into(),
            is_server_admin: false,
        })
        .await
        .unwrap();
    Actor::user(profile.id)
}

fn write(mutation_id: &str, id: &str, base_rev: i64, name: &str) -> Mutation {
    Mutation {
        mutation_id: mutation_id.into(),
        kind: EntityKind::Request,
        id: id.into(),
        base_rev,
        deleted: false,
        payload: Some(serde_json::json!({ "name": name })),
    }
}

fn batch(client: &str, mutations: Vec<Mutation>) -> MutationBatch {
    MutationBatch {
        client_id: client.into(),
        mutations,
    }
}

#[tokio::test]
async fn a_new_workspace_starts_at_revision_zero() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        let workspace = store.create_workspace(&actor, "w1", "Team").await.unwrap();

        assert_eq!(workspace.change_seq, 0);
        assert_eq!(store.head(&actor, "w1").await.unwrap(), 0);
    })
    .await;
}

#[tokio::test]
async fn the_creator_becomes_an_administrator() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        let membership = store
            .membership("w1", &actor.user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(membership.role, Role::Admin);
    })
    .await;
}

#[tokio::test]
async fn pushing_then_pulling_returns_what_was_pushed() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        let result = store
            .apply(
                &actor,
                "w1",
                batch("device-a", vec![write("m1", "r1", 0, "First")]),
            )
            .await
            .unwrap();

        assert!(result.is_clean());
        assert_eq!(result.head, 1);

        let page = store.changes_since(&actor, "w1", 0, 100).await.unwrap();
        assert_eq!(page.objects.len(), 1);
        assert_eq!(page.objects[0].id, "r1");
        assert_eq!(page.objects[0].rev, 1);
        assert_eq!(page.objects[0].payload.as_ref().unwrap()["name"], "First");
        assert_eq!(page.head, 1);
        assert!(!page.has_more);
    })
    .await;
}

#[tokio::test]
async fn pulling_from_a_revision_returns_only_what_is_newer() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        store
            .apply(&actor, "w1", batch("a", vec![write("m1", "r1", 0, "One")]))
            .await
            .unwrap();
        store
            .apply(&actor, "w1", batch("a", vec![write("m2", "r2", 0, "Two")]))
            .await
            .unwrap();

        let page = store.changes_since(&actor, "w1", 1, 100).await.unwrap();
        assert_eq!(page.objects.len(), 1);
        assert_eq!(page.objects[0].id, "r2");
    })
    .await;
}

#[tokio::test]
async fn a_stale_edit_conflicts_and_returns_the_servers_copy() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        store
            .apply(
                &actor,
                "w1",
                batch("a", vec![write("m1", "r1", 0, "Original")]),
            )
            .await
            .unwrap();
        // Someone else moves it on.
        store
            .apply(
                &actor,
                "w1",
                batch("a", vec![write("m2", "r1", 1, "Theirs")]),
            )
            .await
            .unwrap();

        // Our edit was made against revision 1, which is now stale.
        let result = store
            .apply(&actor, "w1", batch("b", vec![write("m3", "r1", 1, "Mine")]))
            .await
            .unwrap();

        assert!(!result.is_clean());
        match &result.outcomes[0] {
            MutationOutcome::Conflict { server, .. } => {
                // The server's copy comes back so the UI can show a diff
                // rather than making the user guess what changed.
                assert_eq!(server.payload.as_ref().unwrap()["name"], "Theirs");
                assert_eq!(server.rev, 2);
            }
            other => panic!("expected a conflict, got {other:?}"),
        }

        // The losing edit changed nothing.
        let page = store.changes_since(&actor, "w1", 0, 100).await.unwrap();
        assert_eq!(page.objects[0].payload.as_ref().unwrap()["name"], "Theirs");
    })
    .await;
}

#[tokio::test]
async fn resending_a_batch_applies_it_once() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        let first = store
            .apply(&actor, "w1", batch("a", vec![write("m1", "r1", 0, "Once")]))
            .await
            .unwrap();
        // The connection dropped; the client re-sends the identical batch.
        let second = store
            .apply(&actor, "w1", batch("a", vec![write("m1", "r1", 0, "Once")]))
            .await
            .unwrap();

        assert!(matches!(
            second.outcomes[0],
            MutationOutcome::Duplicate { .. }
        ));
        assert!(second.outcomes[0].is_accepted());
        // The revision did not advance a second time.
        assert_eq!(second.head, first.head);

        let page = store.changes_since(&actor, "w1", 0, 100).await.unwrap();
        assert_eq!(page.objects.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn one_conflict_does_not_block_the_rest_of_the_batch() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();
        store
            .apply(
                &actor,
                "w1",
                batch("a", vec![write("m1", "r1", 0, "Server")]),
            )
            .await
            .unwrap();

        let result = store
            .apply(
                &actor,
                "w1",
                batch(
                    "b",
                    vec![
                        write("m2", "r1", 0, "Conflicting"), // base_rev 0 but it exists
                        write("m3", "r2", 0, "Fine"),
                    ],
                ),
            )
            .await
            .unwrap();

        assert!(result.outcomes[0].is_conflict());
        assert!(result.outcomes[1].is_accepted());

        // The unrelated object really was written.
        let page = store.changes_since(&actor, "w1", 0, 100).await.unwrap();
        assert!(page.objects.iter().any(|o| o.id == "r2"));
    })
    .await;
}

#[tokio::test]
async fn deletions_travel_as_tombstones_without_their_payload() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();
        store
            .apply(
                &actor,
                "w1",
                batch("a", vec![write("m1", "r1", 0, "Doomed")]),
            )
            .await
            .unwrap();

        store
            .apply(
                &actor,
                "w1",
                batch(
                    "a",
                    vec![Mutation {
                        mutation_id: "m2".into(),
                        kind: EntityKind::Request,
                        id: "r1".into(),
                        base_rev: 1,
                        deleted: true,
                        payload: None,
                    }],
                ),
            )
            .await
            .unwrap();

        let page = store.changes_since(&actor, "w1", 0, 100).await.unwrap();
        assert_eq!(page.objects.len(), 1);
        assert!(page.objects[0].deleted);
        assert!(
            page.objects[0].payload.is_none(),
            "a tombstone kept its payload"
        );

        // Export omits tombstones: a backup wants what exists.
        let exported = store.export(&actor, "w1").await.unwrap();
        assert!(exported.objects.is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_non_member_cannot_read_or_write_and_cannot_tell_the_workspace_exists() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "owner@example.com").await;
        let stranger = seed_user(&store, "stranger@example.com").await;
        store
            .create_workspace(&owner, "w1", "Private")
            .await
            .unwrap();
        store
            .apply(
                &owner,
                "w1",
                batch("a", vec![write("m1", "r1", 0, "Secret")]),
            )
            .await
            .unwrap();

        // Knowing the id grants nothing.
        assert!(matches!(
            store.head(&stranger, "w1").await,
            Err(ServerError::Forbidden)
        ));
        assert!(matches!(
            store.changes_since(&stranger, "w1", 0, 100).await,
            Err(ServerError::Forbidden)
        ));
        assert!(matches!(
            store
                .apply(
                    &stranger,
                    "w1",
                    batch("x", vec![write("m9", "r9", 0, "nope")])
                )
                .await,
            Err(ServerError::Forbidden)
        ));
        assert!(matches!(
            store.export(&stranger, "w1").await,
            Err(ServerError::Forbidden)
        ));

        // A workspace that does not exist reports the same thing, so the
        // two cannot be told apart by probing.
        assert!(matches!(
            store.head(&stranger, "does-not-exist").await,
            Err(ServerError::Forbidden)
        ));
    })
    .await;
}

#[tokio::test]
async fn a_viewer_may_read_but_every_write_is_refused() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "owner@example.com").await;
        let viewer = seed_user(&store, "viewer@example.com").await;
        store
            .create_workspace(&owner, "w1", "Shared")
            .await
            .unwrap();
        store
            .upsert_membership(&owner, "w1", &viewer.user_id, Role::Viewer)
            .await
            .unwrap();
        store
            .apply(
                &owner,
                "w1",
                batch("a", vec![write("m1", "r1", 0, "Readable")]),
            )
            .await
            .unwrap();

        let page = store.changes_since(&viewer, "w1", 0, 100).await.unwrap();
        assert_eq!(page.objects.len(), 1);

        assert!(matches!(
            store
                .apply(
                    &viewer,
                    "w1",
                    batch("v", vec![write("m2", "r2", 0, "nope")])
                )
                .await,
            Err(ServerError::Forbidden)
        ));
    })
    .await;
}

#[tokio::test]
async fn an_editor_may_write_but_not_manage_membership() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "owner@example.com").await;
        let editor = seed_user(&store, "editor@example.com").await;
        let outsider = seed_user(&store, "outsider@example.com").await;
        store
            .create_workspace(&owner, "w1", "Shared")
            .await
            .unwrap();
        store
            .upsert_membership(&owner, "w1", &editor.user_id, Role::Editor)
            .await
            .unwrap();

        assert!(
            store
                .apply(
                    &editor,
                    "w1",
                    batch("e", vec![write("m1", "r1", 0, "Mine")])
                )
                .await
                .unwrap()
                .is_clean()
        );

        assert!(matches!(
            store
                .upsert_membership(&editor, "w1", &outsider.user_id, Role::Editor)
                .await,
            Err(ServerError::Forbidden)
        ));
    })
    .await;
}

#[tokio::test]
async fn the_last_administrator_cannot_be_removed() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "owner@example.com").await;
        store
            .create_workspace(&owner, "w1", "Shared")
            .await
            .unwrap();

        // Otherwise the workspace would be left with nobody able to manage it.
        let err = store
            .remove_membership(&owner, "w1", &owner.user_id)
            .await
            .unwrap_err();
        assert!(matches!(err, ServerError::BadRequest(_)));
    })
    .await;
}

#[tokio::test]
async fn two_users_sharing_a_workspace_converge() {
    for_each_backend(|store| async move {
        let alice = seed_user(&store, "alice@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;
        store
            .create_workspace(&alice, "w1", "Shared")
            .await
            .unwrap();
        store
            .upsert_membership(&alice, "w1", &bob.user_id, Role::Editor)
            .await
            .unwrap();

        // Both work offline against revision 0 on different objects.
        store
            .apply(
                &alice,
                "w1",
                batch("alice", vec![write("a1", "r-alice", 0, "A")]),
            )
            .await
            .unwrap();
        store
            .apply(&bob, "w1", batch("bob", vec![write("b1", "r-bob", 0, "B")]))
            .await
            .unwrap();

        // Each then sees the other's work.
        let alice_view = store.changes_since(&alice, "w1", 0, 100).await.unwrap();
        let bob_view = store.changes_since(&bob, "w1", 0, 100).await.unwrap();

        assert_eq!(alice_view.objects.len(), 2);
        assert_eq!(bob_view.objects.len(), 2);
        assert_eq!(alice_view.head, bob_view.head);
    })
    .await;
}

#[tokio::test]
async fn paging_covers_every_object_exactly_once() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Big").await.unwrap();

        for i in 0..25 {
            store
                .apply(
                    &actor,
                    "w1",
                    batch("a", vec![write(&format!("m{i}"), &format!("r{i}"), 0, "x")]),
                )
                .await
                .unwrap();
        }

        // Walk the pages the way a client would.
        let mut seen = Vec::new();
        let mut cursor = 0;
        loop {
            let page = store.changes_since(&actor, "w1", cursor, 10).await.unwrap();
            if page.objects.is_empty() {
                break;
            }
            // A client advances to the highest revision it saw, not to head.
            cursor = page.highest_rev().unwrap();
            seen.extend(page.objects.iter().map(|o| o.id.clone()));
            if !page.has_more {
                break;
            }
        }

        assert_eq!(seen.len(), 25, "paging lost or repeated objects");
        let unique: std::collections::BTreeSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), 25);
    })
    .await;
}

#[tokio::test]
async fn an_empty_batch_is_refused() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        store.create_workspace(&actor, "w1", "Team").await.unwrap();

        assert!(matches!(
            store.apply(&actor, "w1", batch("a", vec![])).await,
            Err(ServerError::BadRequest(_))
        ));
    })
    .await;
}

#[tokio::test]
async fn a_duplicate_email_is_refused() {
    for_each_backend(|store| async move {
        seed_user(&store, "taken@example.com").await;

        let err = store
            .create_user(NewUser {
                email: "TAKEN@example.com".into(), // different case, same address
                password_hash: "x".into(),
                display_name: "Impostor".into(),
                is_server_admin: false,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, ServerError::Conflict(_)));
    })
    .await;
}

#[tokio::test]
async fn sessions_resolve_to_their_user_and_expire() {
    for_each_backend(|store| async move {
        let actor = seed_user(&store, "a@example.com").await;
        let future = relay_models::Timestamp::now().as_millis() + 60_000;
        let past = relay_models::Timestamp::now().as_millis() - 1;

        store
            .create_session(&actor.user_id, "live-hash", future)
            .await
            .unwrap();
        store
            .create_session(&actor.user_id, "dead-hash", past)
            .await
            .unwrap();

        let resolved = store.session_user("live-hash").await.unwrap();
        assert_eq!(resolved.unwrap().id, actor.user_id);

        assert!(
            store.session_user("dead-hash").await.unwrap().is_none(),
            "an expired session still authenticated"
        );
        assert!(store.session_user("never-existed").await.unwrap().is_none());

        store.delete_session("live-hash").await.unwrap();
        assert!(store.session_user("live-hash").await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn workspaces_are_listed_only_for_their_members() {
    for_each_backend(|store| async move {
        let alice = seed_user(&store, "alice@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;

        store
            .create_workspace(&alice, "w-alice", "Alice")
            .await
            .unwrap();
        store.create_workspace(&bob, "w-bob", "Bob").await.unwrap();

        let alice_list = store.list_workspaces(&alice).await.unwrap();
        assert_eq!(alice_list.len(), 1);
        assert_eq!(alice_list[0].id, "w-alice");

        let bob_list = store.list_workspaces(&bob).await.unwrap();
        assert_eq!(bob_list.len(), 1);
        assert_eq!(bob_list[0].id, "w-bob");
    })
    .await;
}

// --- e-mailed tokens -------------------------------------------------------

use relay_server::store::EmailTokenPurpose;

/// A user plus the id a token is issued against.
async fn seed_user_id(store: &Arc<dyn SyncStore>, email: &str) -> String {
    seed_user(store, email).await.user_id
}

fn in_an_hour() -> i64 {
    relay_models::Timestamp::now().as_millis() + 3_600_000
}

#[tokio::test]
async fn a_token_is_redeemable_once() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;

        store
            .create_email_token(
                &user,
                EmailTokenPurpose::VerifyEmail,
                "hash-1",
                in_an_hour(),
            )
            .await
            .unwrap();

        let now = relay_models::Timestamp::now().as_millis();
        let first = store
            .consume_email_token(EmailTokenPurpose::VerifyEmail, "hash-1", now)
            .await
            .unwrap();
        assert_eq!(first.as_deref(), Some(user.as_str()));

        // A link forwarded, cached by a scanner, or simply clicked twice
        // must not work a second time.
        let second = store
            .consume_email_token(EmailTokenPurpose::VerifyEmail, "hash-1", now)
            .await
            .unwrap();
        assert_eq!(second, None);
    })
    .await;
}

#[tokio::test]
async fn a_token_issued_for_one_purpose_cannot_be_used_for_the_other() {
    // Otherwise anyone able to read a verification mail — a shared
    // mailbox, a mailing list, a forwarded message — could redeem it as
    // a password reset and take the account.
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;
        let now = relay_models::Timestamp::now().as_millis();

        store
            .create_email_token(
                &user,
                EmailTokenPurpose::VerifyEmail,
                "hash-x",
                in_an_hour(),
            )
            .await
            .unwrap();

        let wrong = store
            .consume_email_token(EmailTokenPurpose::ResetPassword, "hash-x", now)
            .await
            .unwrap();
        assert_eq!(wrong, None, "a verification token reset a password");

        // And it is still there for its actual purpose.
        let right = store
            .consume_email_token(EmailTokenPurpose::VerifyEmail, "hash-x", now)
            .await
            .unwrap();
        assert_eq!(right.as_deref(), Some(user.as_str()));
    })
    .await;
}

#[tokio::test]
async fn an_expired_token_is_refused() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;
        let now = relay_models::Timestamp::now().as_millis();

        store
            .create_email_token(&user, EmailTokenPurpose::ResetPassword, "stale", now - 1)
            .await
            .unwrap();

        let result = store
            .consume_email_token(EmailTokenPurpose::ResetPassword, "stale", now)
            .await
            .unwrap();
        assert_eq!(result, None);
    })
    .await;
}

#[tokio::test]
async fn verification_is_recorded_on_the_user() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;

        let before = store
            .find_user_by_email("ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.profile.email_verified_at, None);

        store
            .mark_email_verified(&user, 1_700_000_000_000)
            .await
            .unwrap();

        let after = store
            .find_user_by_email("ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.profile.email_verified_at.map(|t| t.as_millis()),
            Some(1_700_000_000_000)
        );
    })
    .await;
}

#[tokio::test]
async fn changing_a_password_signs_every_device_out() {
    // Someone resetting a password may be doing it *because* an attacker
    // holds a session. Leaving that session alive makes the reset
    // pointless.
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;

        store
            .create_session(&user, "session-a", in_an_hour())
            .await
            .unwrap();
        store
            .create_session(&user, "session-b", in_an_hour())
            .await
            .unwrap();

        assert!(store.session_user("session-a").await.unwrap().is_some());

        let dropped = store.delete_sessions_for_user(&user).await.unwrap();
        assert_eq!(dropped, 2);

        assert!(store.session_user("session-a").await.unwrap().is_none());
        assert!(store.session_user("session-b").await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn one_users_sessions_survive_anothers_reset() {
    for_each_backend(|store| async move {
        let ada = seed_user_id(&store, "ada@example.com").await;
        let grace = seed_user_id(&store, "grace@example.com").await;

        store
            .create_session(&ada, "ada-s", in_an_hour())
            .await
            .unwrap();
        store
            .create_session(&grace, "grace-s", in_an_hour())
            .await
            .unwrap();

        store.delete_sessions_for_user(&ada).await.unwrap();

        assert!(store.session_user("ada-s").await.unwrap().is_none());
        assert!(store.session_user("grace-s").await.unwrap().is_some());
    })
    .await;
}

#[tokio::test]
async fn a_new_password_hash_replaces_the_old_one() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;

        store
            .set_password_hash(&user, "the-new-hash")
            .await
            .unwrap();

        let stored = store
            .find_user_by_email("ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.password_hash, "the-new-hash");
    })
    .await;
}

#[tokio::test]
async fn the_cooldown_can_see_when_the_last_token_went_out() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;

        assert_eq!(
            store
                .latest_email_token_at(&user, EmailTokenPurpose::VerifyEmail)
                .await
                .unwrap(),
            None
        );

        store
            .create_email_token(&user, EmailTokenPurpose::VerifyEmail, "h", in_an_hour())
            .await
            .unwrap();

        let at = store
            .latest_email_token_at(&user, EmailTokenPurpose::VerifyEmail)
            .await
            .unwrap();
        assert!(at.is_some());

        // Kept per purpose, so asking for a reset does not silence
        // verification mail or the other way round.
        assert_eq!(
            store
                .latest_email_token_at(&user, EmailTokenPurpose::ResetPassword)
                .await
                .unwrap(),
            None
        );
    })
    .await;
}

#[tokio::test]
async fn clearing_tokens_leaves_the_other_purpose_alone() {
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;
        let now = relay_models::Timestamp::now().as_millis();

        store
            .create_email_token(&user, EmailTokenPurpose::VerifyEmail, "v", in_an_hour())
            .await
            .unwrap();
        store
            .create_email_token(&user, EmailTokenPurpose::ResetPassword, "r", in_an_hour())
            .await
            .unwrap();

        store
            .clear_email_tokens(&user, EmailTokenPurpose::ResetPassword)
            .await
            .unwrap();

        assert_eq!(
            store
                .consume_email_token(EmailTokenPurpose::ResetPassword, "r", now)
                .await
                .unwrap(),
            None
        );
        assert!(
            store
                .consume_email_token(EmailTokenPurpose::VerifyEmail, "v", now)
                .await
                .unwrap()
                .is_some(),
            "clearing reset tokens should not revoke a verification link"
        );
    })
    .await;
}

#[tokio::test]
async fn deleting_a_user_takes_their_tokens_with_them() {
    // The foreign key is ON DELETE CASCADE; without it a token would
    // outlive its user and point at an id that could be reissued.
    // Postgres only: the SQLite schema has no enforced foreign keys.
    let Some(url) = std::env::var("RELAY_TEST_POSTGRES_URL").ok() else {
        return;
    };
    let store = PostgresStore::connect(&url).await.unwrap();
    let _ = sqlx::raw_sql("DROP SCHEMA IF EXISTS relay CASCADE")
        .execute(store.pool())
        .await;
    store.migrate().await.unwrap();

    let pool = store.pool().clone();
    let store: Arc<dyn SyncStore> = Arc::new(store);
    let user = seed_user_id(&store, "ada@example.com").await;

    store
        .create_email_token(
            &user,
            EmailTokenPurpose::VerifyEmail,
            "doomed",
            in_an_hour(),
        )
        .await
        .unwrap();

    sqlx::query("DELETE FROM relay.users WHERE id = $1")
        .bind(&user)
        .execute(&pool)
        .await
        .unwrap();

    let now = relay_models::Timestamp::now().as_millis();
    assert_eq!(
        store
            .consume_email_token(EmailTokenPurpose::VerifyEmail, "doomed", now)
            .await
            .unwrap(),
        None
    );
}

// --- invitations -----------------------------------------------------------

use relay_server::store::InvitationOffer;

async fn invited_workspace(store: &Arc<dyn SyncStore>) -> (Actor, Actor) {
    let owner = seed_user(store, "ada@example.com").await;
    store.create_workspace(&owner, "w1", "Team").await.unwrap();
    let guest = seed_user(store, "grace@example.com").await;
    (owner, guest)
}

async fn invite(store: &Arc<dyn SyncStore>, owner: &Actor, email: &str, role: Role, hash: &str) {
    store
        .create_invitation(owner, "w1", email, role, hash, in_an_hour())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_invitation_lets_someone_join_with_the_role_it_names() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        invite(&store, &owner, "grace@example.com", Role::Editor, "inv-1").await;

        let now = relay_models::Timestamp::now().as_millis();
        let membership = store
            .accept_invitation(&guest.user_id, "grace@example.com", "inv-1", now)
            .await
            .unwrap();

        assert_eq!(membership.role, Role::Editor);
        assert_eq!(
            store
                .membership("w1", &guest.user_id)
                .await
                .unwrap()
                .unwrap()
                .role,
            Role::Editor
        );
    })
    .await;
}

#[tokio::test]
async fn a_forwarded_invitation_does_not_admit_the_wrong_person() {
    // The token is in an e-mail, and e-mails get forwarded. Binding the
    // invitation to the address it was sent to is what stops a
    // forwarded message becoming access to someone else's workspace.
    for_each_backend(|store| async move {
        let (owner, _) = invited_workspace(&store).await;
        let bystander = seed_user(&store, "mallory@example.com").await;
        invite(&store, &owner, "grace@example.com", Role::Admin, "inv-2").await;

        let now = relay_models::Timestamp::now().as_millis();
        let error = store
            .accept_invitation(&bystander.user_id, "mallory@example.com", "inv-2", now)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("grace@example.com"), "{error}");
        assert!(
            store
                .membership("w1", &bystander.user_id)
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn an_invitation_is_accepted_once() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        invite(&store, &owner, "grace@example.com", Role::Viewer, "inv-3").await;

        let now = relay_models::Timestamp::now().as_millis();
        store
            .accept_invitation(&guest.user_id, "grace@example.com", "inv-3", now)
            .await
            .unwrap();

        let second = store
            .accept_invitation(&guest.user_id, "grace@example.com", "inv-3", now)
            .await;
        assert!(second.is_err(), "the same invitation was accepted twice");
    })
    .await;
}

#[tokio::test]
async fn only_an_administrator_may_invite() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        store
            .upsert_membership(&owner, "w1", &guest.user_id, Role::Editor)
            .await
            .unwrap();

        // An editor can write content but must not be able to widen who
        // else can.
        let error = store
            .create_invitation(
                &guest,
                "w1",
                "mallory@example.com",
                Role::Admin,
                "inv-4",
                in_an_hour(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ServerError::Forbidden), "{error:?}");
    })
    .await;
}

#[tokio::test]
async fn a_stranger_cannot_invite_themselves_in() {
    for_each_backend(|store| async move {
        let (_, outsider) = invited_workspace(&store).await;

        let error = store
            .create_invitation(
                &outsider,
                "w1",
                "grace@example.com",
                Role::Admin,
                "inv-5",
                in_an_hour(),
            )
            .await
            .unwrap_err();

        // 404-shaped, not 403: confirming the workspace exists would let
        // ids be enumerated.
        assert!(matches!(error, ServerError::Forbidden), "{error:?}");
    })
    .await;
}

#[tokio::test]
async fn an_expired_invitation_offers_nothing() {
    for_each_backend(|store| async move {
        let (owner, _) = invited_workspace(&store).await;
        let now = relay_models::Timestamp::now().as_millis();

        store
            .create_invitation(
                &owner,
                "w1",
                "grace@example.com",
                Role::Viewer,
                "old",
                now - 1,
            )
            .await
            .unwrap();

        assert_eq!(store.invitation_offer("old", now).await.unwrap(), None);
    })
    .await;
}

#[tokio::test]
async fn the_offer_says_what_is_being_given_away() {
    for_each_backend(|store| async move {
        let (owner, _) = invited_workspace(&store).await;
        invite(&store, &owner, "grace@example.com", Role::Editor, "inv-6").await;

        let offer = store
            .invitation_offer("inv-6", relay_models::Timestamp::now().as_millis())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            offer,
            InvitationOffer {
                workspace_id: "w1".into(),
                workspace_name: "Team".into(),
                email: "grace@example.com".into(),
                role: Role::Editor,
            }
        );
    })
    .await;
}

#[tokio::test]
async fn accepting_removes_it_from_the_pending_list() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        invite(&store, &owner, "grace@example.com", Role::Viewer, "inv-7").await;

        assert_eq!(store.list_invitations(&owner, "w1").await.unwrap().len(), 1);

        store
            .accept_invitation(
                &guest.user_id,
                "grace@example.com",
                "inv-7",
                relay_models::Timestamp::now().as_millis(),
            )
            .await
            .unwrap();

        assert!(
            store
                .list_invitations(&owner, "w1")
                .await
                .unwrap()
                .is_empty()
        );
    })
    .await;
}

#[tokio::test]
async fn a_revoked_invitation_stops_working() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        invite(&store, &owner, "grace@example.com", Role::Admin, "inv-8").await;

        let listed = store.list_invitations(&owner, "w1").await.unwrap();
        store
            .revoke_invitation(&owner, "w1", &listed[0].id)
            .await
            .unwrap();

        let now = relay_models::Timestamp::now().as_millis();
        assert_eq!(store.invitation_offer("inv-8", now).await.unwrap(), None);
        assert!(
            store
                .accept_invitation(&guest.user_id, "grace@example.com", "inv-8", now)
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn an_admin_of_one_workspace_cannot_revoke_anothers() {
    for_each_backend(|store| async move {
        let (owner, other) = invited_workspace(&store).await;
        store
            .create_workspace(&other, "w2", "Theirs")
            .await
            .unwrap();

        invite(&store, &owner, "grace@example.com", Role::Viewer, "inv-9").await;
        let listed = store.list_invitations(&owner, "w1").await.unwrap();

        // Guessing an id must not be enough; the workspace is checked too.
        let error = store.revoke_invitation(&other, "w2", &listed[0].id).await;
        assert!(error.is_ok(), "deleting nothing is not an error");

        assert_eq!(store.list_invitations(&owner, "w1").await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn issuing_a_token_sweeps_that_users_lapsed_ones() {
    // Keeps the table bounded without a scheduled job, which would mean
    // a second service to deploy and to forget about.
    for_each_backend(|store| async move {
        let user = seed_user_id(&store, "ada@example.com").await;
        let now = relay_models::Timestamp::now().as_millis();

        store
            .create_email_token(&user, EmailTokenPurpose::VerifyEmail, "dead", now - 1)
            .await
            .unwrap();
        store
            .create_email_token(&user, EmailTokenPurpose::VerifyEmail, "live", in_an_hour())
            .await
            .unwrap();

        // The second issue swept the first, expired one.
        assert_eq!(
            store
                .consume_email_token(EmailTokenPurpose::VerifyEmail, "dead", now)
                .await
                .unwrap(),
            None
        );
        // And left the good one alone.
        assert!(
            store
                .consume_email_token(EmailTokenPurpose::VerifyEmail, "live", now)
                .await
                .unwrap()
                .is_some()
        );
    })
    .await;
}

#[tokio::test]
async fn sweeping_does_not_touch_another_users_tokens() {
    for_each_backend(|store| async move {
        let ada = seed_user_id(&store, "ada@example.com").await;
        let grace = seed_user_id(&store, "grace@example.com").await;

        store
            .create_email_token(&grace, EmailTokenPurpose::VerifyEmail, "hers", in_an_hour())
            .await
            .unwrap();
        store
            .create_email_token(&ada, EmailTokenPurpose::VerifyEmail, "his", in_an_hour())
            .await
            .unwrap();

        let now = relay_models::Timestamp::now().as_millis();
        assert!(
            store
                .consume_email_token(EmailTokenPurpose::VerifyEmail, "hers", now)
                .await
                .unwrap()
                .is_some(),
            "one user's new token swept another's"
        );
    })
    .await;
}

#[tokio::test]
async fn inviting_sweeps_lapsed_and_accepted_invitations() {
    for_each_backend(|store| async move {
        let (owner, guest) = invited_workspace(&store).await;
        let now = relay_models::Timestamp::now().as_millis();

        // One expired, one accepted, one still open.
        store
            .create_invitation(&owner, "w1", "old@example.com", Role::Viewer, "x1", now - 1)
            .await
            .unwrap();
        invite(&store, &owner, "grace@example.com", Role::Viewer, "x2").await;
        store
            .accept_invitation(&guest.user_id, "grace@example.com", "x2", now)
            .await
            .unwrap();

        invite(&store, &owner, "fresh@example.com", Role::Viewer, "x3").await;

        let pending = store.list_invitations(&owner, "w1").await.unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].email, "fresh@example.com");
    })
    .await;
}

// --- billing ---------------------------------------------------------------

use relay_server::billing::{Subscription, SubscriptionStatus};

fn subscription(workspace: &str, status: SubscriptionStatus, seats: u32) -> Subscription {
    Subscription {
        workspace_id: workspace.into(),
        customer_id: format!("cus_{workspace}"),
        subscription_id: Some(format!("sub_{workspace}")),
        status,
        seats,
        current_period_end: Some(in_an_hour()),
        price_id: None,
    }
}

#[tokio::test]
async fn a_workspace_with_no_subscription_is_unmetered() {
    // Self-hosting never writes a row, and a workspace predating
    // billing must keep working rather than silently losing its seats.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();

        let entitlement = store.entitlement("w1").await.unwrap();
        assert_eq!(entitlement.seats, None);
        assert!(entitlement.active);
        assert!(entitlement.allows(10_000));
    })
    .await;
}

#[tokio::test]
async fn seats_stop_an_invitation_once_they_run_out() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();

        // One seat, and the owner already occupies it.
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Active, 1))
            .await
            .unwrap();

        let error = store
            .create_invitation(
                &owner,
                "w1",
                "grace@example.com",
                Role::Editor,
                "inv-seat",
                in_an_hour(),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(error, ServerError::LimitExceeded { limit: "seats", .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("Add a seat"), "{error}");
    })
    .await;
}

#[tokio::test]
async fn a_spare_seat_lets_the_invitation_through() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Active, 5))
            .await
            .unwrap();

        assert!(
            store
                .create_invitation(
                    &owner,
                    "w1",
                    "grace@example.com",
                    Role::Editor,
                    "inv-ok",
                    in_an_hour(),
                )
                .await
                .is_ok()
        );
    })
    .await;
}

#[tokio::test]
async fn a_failed_payment_does_not_stop_a_team_working() {
    // past_due still entitles. Stripe retries for days and the usual
    // cause is a bank's fraud check; locking a team out over it would
    // be the worse mistake.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::PastDue, 5))
            .await
            .unwrap();

        let entitlement = store.entitlement("w1").await.unwrap();
        assert!(entitlement.active);
        assert!(entitlement.notice.unwrap().contains("still works"));

        assert!(
            store
                .create_invitation(
                    &owner,
                    "w1",
                    "g@example.com",
                    Role::Viewer,
                    "i1",
                    in_an_hour()
                )
                .await
                .is_ok()
        );
    })
    .await;
}

#[tokio::test]
async fn a_cancelled_subscription_stops_growth_only() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Canceled, 5))
            .await
            .unwrap();

        // Cannot invite...
        assert!(
            store
                .create_invitation(
                    &owner,
                    "w1",
                    "g@example.com",
                    Role::Viewer,
                    "i1",
                    in_an_hour()
                )
                .await
                .is_err()
        );

        // ...but the workspace is entirely usable, which is the point.
        assert!(store.head(&owner, "w1").await.is_ok());
        assert!(store.list_workspaces(&owner).await.unwrap().len() == 1);
        assert!(store.export(&owner, "w1").await.is_ok());
    })
    .await;
}

#[tokio::test]
async fn assistant_turns_accumulate_rather_than_replace() {
    // Two turns finishing at the same moment must both be counted. A
    // read-then-write would lose one, and losing one is the wrong way
    // round for a cap to be wrong.
    use relay_server::billing::AssistantUsage;

    for_each_backend(|store| async move {
        let ada = seed_user(&store, "ada@example.com").await;
        let period = relay_server::billing::period_start(1_781_354_096_000);

        let first = store
            .record_assistant_turn(
                &ada.user_id,
                period,
                AssistantUsage {
                    turns: 1,
                    input_tokens: 8000,
                    output_tokens: 500,
                    cached_tokens: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(first.turns, 1);

        let second = store
            .record_assistant_turn(
                &ada.user_id,
                period,
                AssistantUsage {
                    turns: 1,
                    input_tokens: 6000,
                    output_tokens: 300,
                    cached_tokens: 5000,
                },
            )
            .await
            .unwrap();
        assert_eq!(second.turns, 2);
        assert_eq!(second.input_tokens, 14_000);
        assert_eq!(second.output_tokens, 800);
        assert_eq!(second.cached_tokens, 5_000);

        // And reading it back agrees with what the write returned.
        let read = store.assistant_usage(&ada.user_id, period).await.unwrap();
        assert_eq!(read, second);
    })
    .await;
}

#[tokio::test]
async fn each_month_starts_from_nothing() {
    use relay_server::billing::AssistantUsage;

    for_each_backend(|store| async move {
        let ada = seed_user(&store, "ada@example.com").await;
        let june = relay_server::billing::period_start(1_781_354_096_000);
        let july = relay_server::billing::period_start(1_781_354_096_000 + 86_400_000 * 40);
        assert_ne!(june, july);

        store
            .record_assistant_turn(
                &ada.user_id,
                june,
                AssistantUsage {
                    turns: 900,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .assistant_usage(&ada.user_id, june)
                .await
                .unwrap()
                .turns,
            900
        );
        assert_eq!(
            store
                .assistant_usage(&ada.user_id, july)
                .await
                .unwrap()
                .turns,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn usage_is_not_shared_between_people() {
    use relay_server::billing::AssistantUsage;

    for_each_backend(|store| async move {
        let ada = seed_user(&store, "ada@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;
        let period = relay_server::billing::period_start(1_781_354_096_000);

        store
            .record_assistant_turn(
                &ada.user_id,
                period,
                AssistantUsage {
                    turns: 5,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .assistant_usage(&ada.user_id, period)
                .await
                .unwrap()
                .turns,
            5
        );
        assert_eq!(
            store
                .assistant_usage(&bob.user_id, period)
                .await
                .unwrap()
                .turns,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn a_persons_subscriptions_are_every_workspace_they_are_in() {
    // The allowance follows the person, so this is what decides
    // whether they have one.
    for_each_backend(|store| async move {
        let ada = seed_user(&store, "ada@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;
        store.create_workspace(&ada, "paid", "Paid").await.unwrap();
        store.create_workspace(&ada, "free", "Free").await.unwrap();

        let mut paid = subscription("paid", SubscriptionStatus::Active, 5);
        paid.price_id = Some("price_with_ai".into());
        store.save_subscription(&paid).await.unwrap();

        let hers = store.subscriptions_for_user(&ada.user_id).await.unwrap();
        assert_eq!(hers.len(), 1, "only the workspace with a subscription");
        assert_eq!(hers[0].price_id.as_deref(), Some("price_with_ai"));

        // Somebody who is not a member of it gets nothing.
        assert!(
            store
                .subscriptions_for_user(&bob.user_id)
                .await
                .unwrap()
                .is_empty(),
            "a stranger's subscription leaked"
        );
    })
    .await;
}

#[tokio::test]
async fn the_price_a_workspace_pays_survives_a_round_trip() {
    // Which plan a subscription is on is read from its price, so the
    // price has to come back out of the database exactly as it went
    // in — on both backends, or the same subscription would include
    // the assistant self-hosted and not on Cloud.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();

        let mut paid = subscription("w1", SubscriptionStatus::Active, 3);
        paid.price_id = Some("price_with_ai".into());
        store.save_subscription(&paid).await.unwrap();

        let read = store.subscription("w1").await.unwrap().unwrap();
        assert_eq!(read.price_id.as_deref(), Some("price_with_ai"));
        assert_eq!(
            read.plan(Some("price_with_ai")),
            relay_server::billing::Plan::SyncAndAssistant
        );
        assert_eq!(
            read.plan(Some("price_other")),
            relay_server::billing::Plan::Sync,
            "an unrecognised price must not include the assistant"
        );
    })
    .await;
}

#[tokio::test]
async fn a_subscription_predating_prices_reads_as_sync_only() {
    // The column is new, so rows written before it exists come back
    // with no price. They must not be read as including anything.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Active, 1))
            .await
            .unwrap();

        let read = store.subscription("w1").await.unwrap().unwrap();
        assert_eq!(read.price_id, None);
        assert!(!read.plan(Some("price_with_ai")).includes_assistant());
    })
    .await;
}

#[tokio::test]
async fn seats_cannot_be_skipped_by_adding_a_member_directly() {
    // Inviting was gated from the start; adding by user id was not,
    // so an administrator could fill a workspace without paying for
    // any of it. There are two ways a person joins and both bind.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Active, 1))
            .await
            .unwrap();

        let refused = store
            .upsert_membership(&owner, "w1", &bob.user_id, Role::Editor)
            .await;
        assert!(refused.is_err(), "a second member was admitted on one seat");
        assert_eq!(store.member_count("w1").await.unwrap(), 1);
    })
    .await;
}

#[tokio::test]
async fn an_existing_member_can_still_be_re_roled_over_the_limit() {
    // The limit binds on growth. A workspace that has outgrown its
    // subscription must still be able to demote or promote the people
    // already in it — otherwise the only way out is to pay.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        let bob = seed_user(&store, "bob@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .upsert_membership(&owner, "w1", &bob.user_id, Role::Editor)
            .await
            .unwrap();

        // Now the subscription shrinks under them.
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Canceled, 1))
            .await
            .unwrap();

        store
            .upsert_membership(&owner, "w1", &bob.user_id, Role::Viewer)
            .await
            .expect("an existing member could not be demoted");

        assert_eq!(
            store
                .membership("w1", &bob.user_id)
                .await
                .unwrap()
                .unwrap()
                .role,
            Role::Viewer
        );
        // And removing someone is always allowed; it is how you get
        // back under the limit.
        store
            .remove_membership(&owner, "w1", &bob.user_id)
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_subscription_round_trips_including_an_unknown_status() {
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();

        let mut saved = subscription("w1", SubscriptionStatus::Active, 3);
        store.save_subscription(&saved).await.unwrap();
        assert_eq!(store.subscription("w1").await.unwrap().unwrap(), saved);

        // Updating in place rather than accumulating rows.
        saved.seats = 7;
        saved.status = SubscriptionStatus::PastDue;
        store.save_subscription(&saved).await.unwrap();

        let read = store.subscription("w1").await.unwrap().unwrap();
        assert_eq!(read.seats, 7);
        assert_eq!(read.status, SubscriptionStatus::PastDue);
    })
    .await;
}

#[tokio::test]
async fn a_customer_can_be_traced_back_to_its_workspace() {
    // A webhook names the customer, never the workspace.
    for_each_backend(|store| async move {
        let owner = seed_user(&store, "ada@example.com").await;
        store.create_workspace(&owner, "w1", "Team").await.unwrap();
        store
            .save_subscription(&subscription("w1", SubscriptionStatus::Active, 2))
            .await
            .unwrap();

        assert_eq!(
            store.workspace_for_customer("cus_w1").await.unwrap(),
            Some("w1".to_string())
        );
        assert_eq!(
            store.workspace_for_customer("cus_nobody").await.unwrap(),
            None
        );
    })
    .await;
}

#[tokio::test]
async fn an_event_is_applied_once_however_many_times_it_arrives() {
    // Stripe retries until acknowledged. Without this, a replayed
    // event could double a seat count.
    for_each_backend(|store| async move {
        assert!(store.claim_billing_event("evt_1").await.unwrap());
        assert!(!store.claim_billing_event("evt_1").await.unwrap());
        assert!(store.claim_billing_event("evt_2").await.unwrap());
    })
    .await;
}
