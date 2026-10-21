//! What an import produced, and what it could not bring across.

use relay_models::{Collection, Environment, RequestDoc, WorkspaceId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Everything an import produced, plus an honest account of what it lost.
///
/// Nothing here is persisted yet: the caller shows the report, the user
/// accepts it, and only then is it written. Importing a 200-request
/// collection and discovering afterwards that the auth did not come across
/// is worse than being told first.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportReport {
    /// A short description of what was recognised, e.g. "Postman Collection v2.1".
    pub source_format: String,
    /// The collections and folders to create, parents before children.
    pub collections: Vec<Collection>,
    /// The requests to create.
    pub requests: Vec<RequestDoc>,
    /// The environments to create.
    pub environments: Vec<Environment>,
    /// Things that could not be represented, each tied to where it came from.
    pub warnings: Vec<ImportWarning>,
}

impl ImportReport {
    /// A report for a recognised format with nothing in it yet.
    pub fn new(source_format: impl Into<String>) -> Self {
        Self {
            source_format: source_format.into(),
            ..Default::default()
        }
    }

    /// Records something that did not come across.
    pub fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(ImportWarning {
            path: path.into(),
            message: message.into(),
        });
    }

    /// Total number of objects that would be created.
    pub fn object_count(&self) -> usize {
        self.collections.len() + self.requests.len() + self.environments.len()
    }

    /// True when nothing at all was recognised.
    pub fn is_empty(&self) -> bool {
        self.object_count() == 0
    }

    /// Re-targets everything at a workspace.
    ///
    /// Importers build their output without knowing where it will land, so
    /// this is the single place that stamps the workspace onto every object.
    pub fn retarget(&mut self, workspace_id: &WorkspaceId) {
        for collection in &mut self.collections {
            collection.workspace_id = workspace_id.clone();
        }
        for request in &mut self.requests {
            request.workspace_id = workspace_id.clone();
        }
        for environment in &mut self.environments {
            environment.workspace_id = workspace_id.clone();
        }
    }
}

/// One thing an import could not represent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportWarning {
    /// Where in the source it came from, e.g. `My API / Users / Get user`.
    pub path: String,
    /// What was lost, and what the user can do about it.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retargeting_stamps_every_object() {
        let origin = WorkspaceId::from("origin");
        let mut report = ImportReport::new("test");
        report
            .collections
            .push(Collection::new(origin.clone(), "C"));
        report.requests.push(RequestDoc::new(origin.clone(), "R"));
        report.environments.push(Environment::new(origin, "E"));

        let target = WorkspaceId::from("target");
        report.retarget(&target);

        assert_eq!(report.collections[0].workspace_id, target);
        assert_eq!(report.requests[0].workspace_id, target);
        assert_eq!(report.environments[0].workspace_id, target);
        assert_eq!(report.object_count(), 3);
    }

    #[test]
    fn an_empty_report_is_empty() {
        assert!(ImportReport::new("test").is_empty());
    }
}
