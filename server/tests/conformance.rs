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
