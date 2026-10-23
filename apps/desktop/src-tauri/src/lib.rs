//! The Relay desktop application.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::sync::Arc;
use std::time::Duration;

use relay_app::{MemorySecretStore, RelayService, paths};
use relay_http::{EngineConfig, HttpEngine};
use relay_storage::Database;

/// Builds and runs the application.
///
/// # Panics
///
/// Panics only if the local database cannot be opened or migrated, which
/// means Relay has no data layer and cannot usefully continue. The message
/// names the path so the user can act on it.
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RELAY_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let database_path = paths::database_path();
    let database = match Database::open(&database_path) {
        Ok(db) => db,
        Err(err) => {
            tracing::error!("could not open {}: {err}", database_path.display());
            panic!(
                "Relay could not open its database at {}: {err}",
                database_path.display()
            );
        }
    };

    // Response bodies spooled by a previous run are of no use now.
    if let Err(err) = paths::clean_spool() {
        tracing::warn!("could not clean the response spool directory: {err}");
    }

    let engine = HttpEngine::new(EngineConfig {
        default_timeout: Duration::from_secs(120),
        spool_dir: Some(paths::spool_dir()),
        ..Default::default()
    });

    let service = RelayService::new(database, engine, Arc::new(MemorySecretStore::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(service)
        .manage(commands::SyncWatchers::default())
        .invoke_handler(tauri::generate_handler![
            commands::workspace_list,
            commands::workspace_active,
            commands::workspace_create,
            commands::workspace_save,
            commands::workspace_delete,
            commands::collection_tree,
            commands::collection_list,
            commands::collection_create,
            commands::collection_save,
            commands::collection_delete,
            commands::request_get,
            commands::request_create,
            commands::request_save,
            commands::request_delete,
            commands::request_duplicate,
            commands::request_move,
            commands::request_resolve,
            commands::http_send,
            commands::http_send_with_tests,
            commands::http_cancel,
            commands::cookies_clear,
            commands::oauth_token_status,
            commands::oauth_fetch_token,
            commands::oauth_authorize,
            commands::oauth_forget_token,
            commands::oauth_forget_all_tokens,
            commands::environment_list,
            commands::environment_create,
            commands::environment_save,
            commands::environment_delete,
            commands::secret_set,
            commands::secret_delete,
            commands::secret_is_set,
            commands::secret_backend,
            commands::history_list,
            commands::history_clear,
            commands::import_curl,
            commands::import_preview,
            commands::import_preview_path,
            commands::export_workspace_to_path,
            commands::import_apply,
            commands::export_workspace,
            commands::export_curl,
            commands::sync_probe,
            commands::sync_register,
            commands::sync_invitation_offer,
            commands::sync_join_invited,
            commands::sync_members,
            commands::sync_set_member_role,
            commands::sync_remove_member,
            commands::sync_invite,
            commands::sync_invitations,
            commands::sync_revoke_invitation,
            commands::sync_request_verification,
            commands::sync_request_password_reset,
            commands::sync_connect,
            commands::sync_now,
            commands::sync_status,
            commands::sync_disconnect,
            commands::sync_watch,
            commands::sync_unwatch,
            commands::sync_conflicts,
            commands::sync_resolve,
            commands::state_get,
            commands::state_set,
            commands::database_backup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Relay");
}
