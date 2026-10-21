//! Workspaces, collections and environments.

use crate::common::{KeyValue, Revision, Timestamp, default_true};
use crate::ids::{CollectionId, EnvironmentId, RequestId, WorkspaceId};
use crate::request::{AuthConfig, HttpMethod};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The top-level organisational unit. Users may create as many as they like,
/// with no account and no network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Variables available to every request in the workspace. Lowest precedence.
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub sort_order: f64,
    /// Where this workspace synchronises, if anywhere. `None` is purely local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, flatten)]
    pub revision: Revision,
}

impl Workspace {
    /// A new local-only workspace.
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: WorkspaceId::new(),
            name: name.into(),
            description: String::new(),
            variables: Vec::new(),
            sort_order: 0.0,
            sync: None,
            created_at: now,
            updated_at: now,
            revision: Revision::default(),
        }
    }
}

/// Where a workspace synchronises to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SyncConfig {
    /// Base URL of a server the user controls, e.g. `https://relay.internal`.
    pub server_url: String,
    /// The workspace id on the server, which may differ from the local one.
    pub remote_workspace_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A collection or a folder inside one.
///
/// Both are the same shape: a collection is a node with no parent, a folder is
/// a node with one. Modelling them separately would duplicate every operation
/// — rename, move, delete, reorder — for no gain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Collection {
    pub id: CollectionId,
    pub workspace_id: WorkspaceId,
    /// `None` makes this a root collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<CollectionId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Inherited by requests whose auth is [`AuthConfig::Inherit`].
    #[serde(default)]
    pub auth: AuthConfig,
    /// Headers merged into every request beneath this node.
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub sort_order: f64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, flatten)]
    pub revision: Revision,
}

impl Collection {
    /// A new root collection in the given workspace.
    pub fn new(workspace_id: WorkspaceId, name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: CollectionId::new(),
            workspace_id,
            parent_id: None,
            name: name.into(),
            description: String::new(),
            auth: AuthConfig::None,
            headers: Vec::new(),
            variables: Vec::new(),
            sort_order: 0.0,
            created_at: now,
            updated_at: now,
            revision: Revision::default(),
        }
    }
}

/// A single variable binding.
///
/// `secret` variables carry no value in anything that is persisted to the
/// shared database, exported or synchronised: the value lives in the OS
/// credential store and is substituted only at send time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Variable {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Variable {
    /// A plain, enabled, non-secret variable.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            secret: false,
            description: None,
        }
    }

    /// Marks the variable secret and drops any inline value.
    pub fn into_secret(mut self) -> Self {
        self.secret = true;
        self.value = String::new();
        self
    }

    /// A copy safe to write to disk, export or send to a sync server.
    ///
    /// This is the single place that decides what leaves the device, so there
    /// is no way to add a new call site that forgets to redact.
    pub fn redacted(&self) -> Self {
        if self.secret {
            Self {
                value: String::new(),
                ..self.clone()
            }
        } else {
            self.clone()
        }
    }
}

/// A named set of variables, such as Development or Production.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Environment {
    pub id: EnvironmentId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub sort_order: f64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, flatten)]
    pub revision: Revision,
}

impl Environment {
    /// A new, empty environment.
    pub fn new(workspace_id: WorkspaceId, name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: EnvironmentId::new(),
            workspace_id,
            name: name.into(),
            variables: Vec::new(),
            color: None,
            sort_order: 0.0,
            created_at: now,
            updated_at: now,
            revision: Revision::default(),
        }
    }

    /// A copy with every secret value stripped.
    pub fn redacted(&self) -> Self {
        Self {
            variables: self.variables.iter().map(Variable::redacted).collect(),
            ..self.clone()
        }
    }
}

/// One node of the sidebar tree, flattened for transport.
///
/// The frontend receives a flat list with `parentId` links rather than a
/// nested structure: it is cheaper to diff, trivially virtualisable, and lets
/// a single query build the whole tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum TreeNode {
    #[serde(rename_all = "camelCase")]
    Folder {
        id: CollectionId,
        parent_id: Option<CollectionId>,
        name: String,
        sort_order: f64,
        child_count: u32,
    },
    #[serde(rename_all = "camelCase")]
    Request {
        id: RequestId,
        parent_id: Option<CollectionId>,
        name: String,
        method: HttpMethod,
        url: String,
        sort_order: f64,
    },
}

impl TreeNode {
    /// The node's parent folder, if it has one.
    pub fn parent_id(&self) -> Option<&CollectionId> {
        match self {
            Self::Folder { parent_id, .. } | Self::Request { parent_id, .. } => parent_id.as_ref(),
        }
    }

    /// The node's position among its siblings.
    pub fn sort_order(&self) -> f64 {
        match self {
            Self::Folder { sort_order, .. } | Self::Request { sort_order, .. } => *sort_order,
        }
    }

    /// The display name.
    pub fn name(&self) -> &str {
        match self {
            Self::Folder { name, .. } | Self::Request { name, .. } => name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_variables_redact_their_value() {
        let v = Variable::new("apiKey", "super-secret").into_secret();
        assert_eq!(v.value, "");
        let v2 = Variable {
            key: "apiKey".into(),
            value: "leaked".into(),
            enabled: true,
            secret: true,
            description: None,
        };
        assert_eq!(v2.redacted().value, "");
        // The key is kept so the UI can still show that the variable exists.
        assert_eq!(v2.redacted().key, "apiKey");
    }

    #[test]
    fn non_secret_variables_survive_redaction() {
        let v = Variable::new("baseUrl", "https://api.example.com");
        assert_eq!(v.redacted().value, "https://api.example.com");
    }

    #[test]
    fn redacting_an_environment_covers_every_variable() {
        let mut env = Environment::new(WorkspaceId::new(), "Production");
        env.variables = vec![
            Variable::new("baseUrl", "https://api.example.com"),
            Variable::new("token", "t0ps3cret").into_secret(),
            Variable {
                key: "password".into(),
                value: "hunter2".into(),
                enabled: true,
                secret: true,
                description: None,
            },
        ];

        let redacted = env.redacted();
        let serialised = serde_json::to_string(&redacted).unwrap();
        assert!(!serialised.contains("hunter2"));
        assert!(!serialised.contains("t0ps3cret"));
        assert!(serialised.contains("https://api.example.com"));
    }

    #[test]
    fn tree_nodes_discriminate_by_kind() {
        let node = TreeNode::Folder {
            id: CollectionId::from("c1"),
            parent_id: None,
            name: "Users".into(),
            sort_order: 1.0,
            child_count: 3,
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["kind"], "folder");
        assert_eq!(json["childCount"], 3);
    }

    #[test]
    fn workspaces_start_unsynced() {
        assert!(Workspace::new("Local").sync.is_none());
    }
}
