//! Identity and membership, as they cross the wire.
//!
//! Relay's server authenticates its own users. There is no external
//! identity provider to configure, because requiring one would make a
//! self-hoster depend on a third party to let their own colleagues in.

use relay_models::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a member may do in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Role {
    /// May pull, but every push is refused.
    Viewer,
    /// May pull and push.
    Editor,
    /// May also manage membership and delete the workspace.
    Admin,
}

impl Role {
    /// Whether this role may modify workspace content.
    pub fn can_write(self) -> bool {
        matches!(self, Self::Editor | Self::Admin)
    }

    /// Whether this role may add, remove or re-role members.
    pub fn can_administer(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Editor => "editor",
            Self::Admin => "admin",
        }
    }

    /// Parses a stored name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "viewer" => Some(Self::Viewer),
            "editor" => Some(Self::Editor),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

/// One user's membership of one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Membership {
    pub workspace_id: String,
    pub user_id: String,
    pub role: Role,
    pub created_at: Timestamp,
}

/// A user, as other people see them.
///
/// Carries no password hash, no session token and no e-mail verification
/// state: this is the shape that goes into a members list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UserProfile {
    pub id: String,
    pub email: String,
    pub display_name: String,
    #[serde(default)]
    pub is_server_admin: bool,
    pub created_at: Timestamp,
    /// When the address was confirmed, if it has been. The client shows a
    /// prompt until it is, and the server may refuse to sign in without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_verified_at: Option<Timestamp>,
}

/// Credentials submitted to sign in.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// A successful sign-in.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LoginResponse {
    /// The bearer token for subsequent requests.
    ///
    /// Opaque and random rather than a JWT: a self-hosted server has no
    /// need to verify tokens without touching its database, and an opaque
    /// token can be revoked the instant a user signs out.
    pub token: String,
    pub expires_at: Timestamp,
    pub user: UserProfile,
}

/// An invitation to join a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Invitation {
    pub id: String,
    pub workspace_id: String,
    pub email: String,
    pub role: Role,
    pub invited_by: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    #[serde(default)]
    pub accepted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewers_cannot_write() {
        assert!(!Role::Viewer.can_write());
        assert!(Role::Editor.can_write());
        assert!(Role::Admin.can_write());
    }

    #[test]
    fn only_admins_administer() {
        assert!(!Role::Viewer.can_administer());
        assert!(!Role::Editor.can_administer());
        assert!(Role::Admin.can_administer());
    }

    #[test]
    fn roles_round_trip_through_their_stored_names() {
        for role in [Role::Viewer, Role::Editor, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("superuser"), None);
    }

    #[test]
    fn roles_are_ordered_by_privilege() {
        // Lets a check be written as `role >= Role::Editor`.
        assert!(Role::Admin > Role::Editor);
        assert!(Role::Editor > Role::Viewer);
    }

    #[test]
    fn a_user_profile_carries_no_credential_material() {
        let profile = UserProfile {
            id: "u1".into(),
            email: "a@example.com".into(),
            display_name: "Ada".into(),
            is_server_admin: false,
            created_at: Timestamp(1),
            email_verified_at: None,
        };
        let json = serde_json::to_value(&profile).unwrap();

        for forbidden in ["password", "passwordHash", "token", "hash"] {
            assert!(
                json.get(forbidden).is_none(),
                "a profile exposed {forbidden}"
            );
        }
    }
}
