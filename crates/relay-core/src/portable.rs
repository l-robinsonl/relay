//! Relay's own export format.
//!
//! This is the file behind "no functional lock-in". It carries a whole
//! workspace — collections, requests, environments — in a documented JSON
//! shape, with object ids preserved so that re-importing it somewhere else
//! keeps every reference intact rather than producing a disconnected copy.
//!
//! Secret values are never included. Nor are cookies, history or tokens:
//! those are local to a device by design, and an export is a file that gets
//! e-mailed, committed and copied onto shared drives.

use relay_models::{
    Collection, CollectionId, Environment, EnvironmentId, RequestDoc, RequestId, Revision,
    Timestamp, Workspace, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::import::ImportReport;

/// Identifies the file as a Relay export.
pub const FORMAT: &str = "relay.workspace";

/// The export schema version. Bumped only on a breaking change.
pub const FORMAT_VERSION: u32 = 1;

/// A complete, portable workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceExport {
    /// Always [`FORMAT`]; lets a reader reject the wrong kind of file fast.
    pub format: String,
    /// The schema version this file was written against.
    pub version: u32,
    /// When it was written.
    pub exported_at: Timestamp,
    /// The version of Relay that wrote it, for diagnosing odd files.
    pub exported_by: String,
    /// The workspace itself.
    pub workspace: Workspace,
    /// Collections and folders. Parents always precede their children.
    #[serde(default)]
    pub collections: Vec<Collection>,
    /// Saved requests.
    #[serde(default)]
    pub requests: Vec<RequestDoc>,
    /// Environments, with every secret value already removed.
    #[serde(default)]
    pub environments: Vec<Environment>,
}

impl WorkspaceExport {
    /// Builds an export, stripping anything that must not leave the device.
    ///
    /// Redaction happens here rather than at the call site, so there is one
    /// place to audit and no way to add a second exporter that forgets.
    pub fn build(
        workspace: &Workspace,
        collections: &[Collection],
        requests: &[RequestDoc],
        environments: &[Environment],
    ) -> Self {
        let mut workspace = workspace.clone();
        workspace.variables = workspace.variables.iter().map(|v| v.redacted()).collect();
        // Sync configuration points at one team's server; it is not part of
        // the data and would be wrong on any other installation.
        workspace.sync = None;

        Self {
            format: FORMAT.to_string(),
            version: FORMAT_VERSION,
            exported_at: Timestamp::now(),
            exported_by: format!("Relay {}", env!("CARGO_PKG_VERSION")),
            workspace,
            collections: collections
                .iter()
                .map(|collection| {
                    let mut copy = collection.clone();
                    copy.variables = copy.variables.iter().map(|v| v.redacted()).collect();
                    copy
                })
                .collect(),
            requests: requests.to_vec(),
            environments: environments.iter().map(Environment::redacted).collect(),
        }
    }

    /// Serialises to indented JSON, so the file diffs usefully in Git.
    pub fn to_json(&self) -> CoreResult<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| CoreError::Export(format!("could not write the export: {e}")))
    }

    /// Parses an export, checking it really is one.
    pub fn from_json(json: &str) -> CoreResult<Self> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| CoreError::Import(format!("this is not valid JSON: {e}")))?;

        match value.get("format").and_then(serde_json::Value::as_str) {
            Some(FORMAT) => {}
            Some(other) => {
                return Err(CoreError::Import(format!(
                    "this is a '{other}' file, not a Relay workspace export"
                )));
            }
            None => {
                return Err(CoreError::Import(
                    "this is not a Relay workspace export — it has no \"format\" field. \
                     If it came from Postman, use the Postman importer instead."
                        .into(),
                ));
            }
        }

        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;

        if version > FORMAT_VERSION {
            // Reading a newer file with older code would drop whatever it
            // does not know about, and the user would not find out until
            // something was missing.
            return Err(CoreError::Import(format!(
                "this export was written by a newer version of Relay (format {version}, \
                 this build reads {FORMAT_VERSION}). Upgrade Relay to import it."
            )));
        }

        serde_json::from_str(json)
            .map_err(|e| CoreError::Import(format!("this export is malformed: {e}")))
    }

    /// Converts the export into something importable into a workspace.
    ///
    /// Two cases, distinguished by whether the target is the workspace the
    /// file came from:
    ///
    /// * **Restoring** into the same workspace id keeps every object id, so
    ///   the restore lands exactly on top of what was there and revision
    ///   history still lines up.
    /// * **Copying** into a different workspace mints fresh ids and rewrites
    ///   the references between them. Without this, importing an export into
    ///   a second workspace on the same machine would move the originals out
    ///   of the first rather than duplicating them.
    pub fn into_report(mut self, into_workspace: &WorkspaceId) -> ImportReport {
        let restoring = self.workspace.id == *into_workspace;

        let mut report = ImportReport::new(format!(
            "Relay workspace export (format {}, written by {})",
            self.version, self.exported_by
        ));

        let secret_count = self
            .environments
            .iter()
            .flat_map(|e| &e.variables)
            .filter(|v| v.secret)
            .count();

        if !restoring {
            self.remint_ids();
        }

        report.collections = self.collections;
        report.requests = self.requests;
        report.environments = self.environments;
        report.retarget(into_workspace);

        if secret_count > 0 {
            report.warn(
                &self.workspace.name,
                format!(
                    "{secret_count} secret variable{} imported by name only. Exports never \
                     carry secret values — enter {} again on this machine.",
                    if secret_count == 1 { " was" } else { "s were" },
                    if secret_count == 1 { "it" } else { "them" },
                ),
            );
        }

        report
    }

    /// Replaces every object id with a fresh one, keeping references intact.
    ///
    /// A copy has never been synchronised anywhere, so its revision metadata
    /// is reset too; leaving a server revision on a brand-new object would
    /// make the next push claim to be an edit of something it is not.
    fn remint_ids(&mut self) {
        use std::collections::HashMap;

        let collection_map: HashMap<CollectionId, CollectionId> = self
            .collections
            .iter()
            .map(|c| (c.id.clone(), CollectionId::new()))
            .collect();

        for collection in &mut self.collections {
            collection.id = collection_map
                .get(&collection.id)
                .cloned()
                .unwrap_or_else(CollectionId::new);
            // A parent outside this export cannot be resolved, so the folder
            // becomes a root rather than pointing at a row that is not there.
            collection.parent_id = collection
                .parent_id
                .as_ref()
                .and_then(|parent| collection_map.get(parent).cloned());
            collection.revision = Revision::default();
        }

        for request in &mut self.requests {
            request.id = RequestId::new();
            request.collection_id = request
                .collection_id
                .as_ref()
                .and_then(|parent| collection_map.get(parent).cloned());
            request.revision = Revision::default();
        }

        for environment in &mut self.environments {
            environment.id = EnvironmentId::new();
            environment.revision = Revision::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_models::{RequestBody, Variable};

    fn fixture() -> (
        Workspace,
        Vec<Collection>,
        Vec<RequestDoc>,
        Vec<Environment>,
    ) {
        let mut workspace = Workspace::new("Team API");
        workspace.variables = vec![
            Variable::new("org", "acme"),
            Variable {
                key: "rootSecret".into(),
                value: "WORKSPACE-SECRET".into(),
                enabled: true,
                secret: true,
                description: None,
            },
        ];
        workspace.sync = Some(relay_models::SyncConfig {
            server_url: "https://relay.internal".into(),
            remote_workspace_id: "remote-1".into(),
            enabled: true,
        });

        let mut collection = Collection::new(workspace.id.clone(), "Users");
        collection.variables = vec![Variable {
            key: "folderSecret".into(),
            value: "FOLDER-SECRET".into(),
            enabled: true,
            secret: true,
            description: None,
        }];

        let mut request = RequestDoc::new(workspace.id.clone(), "List users");
        request.collection_id = Some(collection.id.clone());
        request.url = "{{baseUrl}}/users".into();
        request.body = RequestBody::Json {
            text: r#"{"a":1}"#.into(),
        };

        let mut environment = Environment::new(workspace.id.clone(), "Production");
        environment.variables = vec![
            Variable::new("baseUrl", "https://api.acme.com"),
            Variable {
                key: "apiToken".into(),
                value: "ENV-SECRET".into(),
                enabled: true,
                secret: true,
                description: None,
            },
        ];

        (
            workspace,
            vec![collection],
            vec![request],
            vec![environment],
        )
    }

    #[test]
    fn an_export_round_trips() {
        let (workspace, collections, requests, environments) = fixture();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let json = export.to_json().unwrap();
        let parsed = WorkspaceExport::from_json(&json).unwrap();

        assert_eq!(parsed.format, FORMAT);
        assert_eq!(parsed.version, FORMAT_VERSION);
        assert_eq!(parsed.workspace.name, "Team API");
        assert_eq!(parsed.requests.len(), 1);
        assert_eq!(parsed.requests[0].url, "{{baseUrl}}/users");
        assert_eq!(parsed.requests[0].body, requests[0].body);
    }

    #[test]
    fn ids_survive_so_references_still_resolve() {
        let (workspace, collections, requests, environments) = fixture();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);
        let parsed = WorkspaceExport::from_json(&export.to_json().unwrap()).unwrap();

        assert_eq!(parsed.collections[0].id, collections[0].id);
        assert_eq!(parsed.requests[0].id, requests[0].id);
        assert_eq!(
            parsed.requests[0].collection_id.as_ref(),
            Some(&collections[0].id),
            "the request lost its link to its folder"
        );
    }

    #[test]
    fn no_secret_from_any_scope_reaches_the_file() {
        let (workspace, collections, requests, environments) = fixture();
        let json = WorkspaceExport::build(&workspace, &collections, &requests, &environments)
            .to_json()
            .unwrap();

        for secret in ["WORKSPACE-SECRET", "FOLDER-SECRET", "ENV-SECRET"] {
            assert!(!json.contains(secret), "{secret} was exported");
        }

        // Names survive, so the recipient knows what to supply.
        assert!(json.contains("rootSecret"));
        assert!(json.contains("folderSecret"));
        assert!(json.contains("apiToken"));
        assert!(json.contains("https://api.acme.com"));
    }

    #[test]
    fn sync_configuration_is_not_exported() {
        // It names one team's server and would be wrong anywhere else.
        let (workspace, collections, requests, environments) = fixture();
        let json = WorkspaceExport::build(&workspace, &collections, &requests, &environments)
            .to_json()
            .unwrap();
        assert!(!json.contains("relay.internal"));
    }

    #[test]
    fn importing_retargets_everything_at_the_chosen_workspace() {
        let (workspace, collections, requests, environments) = fixture();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let target = WorkspaceId::from("somewhere-else");
        let report = export.into_report(&target);

        assert!(report.collections.iter().all(|c| c.workspace_id == target));
        assert!(report.requests.iter().all(|r| r.workspace_id == target));
        assert!(report.environments.iter().all(|e| e.workspace_id == target));
    }

    #[test]
    fn copying_into_another_workspace_mints_new_ids_but_keeps_the_links() {
        let (workspace, collections, requests, environments) = fixture();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let report = export.into_report(&WorkspaceId::from("a-different-workspace"));

        // New identities, so the originals are untouched.
        assert_ne!(report.collections[0].id, collections[0].id);
        assert_ne!(report.requests[0].id, requests[0].id);
        assert_ne!(report.environments[0].id, environments[0].id);

        // But the request still points at its folder.
        assert_eq!(
            report.requests[0].collection_id.as_ref(),
            Some(&report.collections[0].id),
            "the copy lost the link between request and folder"
        );
    }

    #[test]
    fn a_copy_has_no_revision_history() {
        let (workspace, mut collections, requests, environments) = fixture();
        collections[0].revision = Revision {
            rev: 42,
            dirty: false,
            deleted_at: None,
        };
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let report = export.into_report(&WorkspaceId::from("elsewhere"));
        assert_eq!(
            report.collections[0].revision.rev, 0,
            "a brand-new copy claimed a server revision it does not have"
        );
    }

    #[test]
    fn restoring_into_the_same_workspace_keeps_every_id() {
        let (workspace, collections, requests, environments) = fixture();
        let workspace_id = workspace.id.clone();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let report = export.into_report(&workspace_id);

        assert_eq!(report.collections[0].id, collections[0].id);
        assert_eq!(report.requests[0].id, requests[0].id);
        assert_eq!(report.environments[0].id, environments[0].id);
    }

    #[test]
    fn a_folder_whose_parent_is_missing_becomes_a_root() {
        let (workspace, mut collections, requests, environments) = fixture();
        // Point at a folder that is not in the export.
        collections[0].parent_id = Some(CollectionId::from("not-in-this-file"));
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let report = export.into_report(&WorkspaceId::from("elsewhere"));
        assert!(
            report.collections[0].parent_id.is_none(),
            "a dangling parent reference survived and would break the tree"
        );
    }

    #[test]
    fn importing_reports_that_secrets_need_re_entering() {
        let (workspace, collections, requests, environments) = fixture();
        let export = WorkspaceExport::build(&workspace, &collections, &requests, &environments);

        let report = export.into_report(&WorkspaceId::from("target"));
        assert!(
            report.warnings.iter().any(|w| w.message.contains("secret")),
            "the recipient was not told to re-enter secrets"
        );
    }

    #[test]
    fn a_file_from_another_tool_is_rejected_with_a_pointer() {
        let err = WorkspaceExport::from_json(r#"{"info":{"name":"x"},"item":[]}"#).unwrap_err();
        assert!(err.to_string().contains("Postman"), "got {err}");
    }

    #[test]
    fn a_file_claiming_a_different_format_is_rejected() {
        let err = WorkspaceExport::from_json(r#"{"format":"something.else"}"#).unwrap_err();
        assert!(err.to_string().contains("something.else"), "got {err}");
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_silently_truncated() {
        let json = format!(r#"{{"format":"{FORMAT}","version":999}}"#);
        let err = WorkspaceExport::from_json(&json).unwrap_err();
        assert!(err.to_string().contains("Upgrade Relay"), "got {err}");
    }

    #[test]
    fn invalid_json_is_rejected_readably() {
        let err = WorkspaceExport::from_json("{not json").unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn the_file_is_indented_so_it_diffs_in_version_control() {
        let (workspace, collections, requests, environments) = fixture();
        let json = WorkspaceExport::build(&workspace, &collections, &requests, &environments)
            .to_json()
            .unwrap();
        assert!(json.contains("\n  "), "the export is not pretty-printed");
    }
}
