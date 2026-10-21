//! The HTTP client for a Relay sync server.

use relay_models::{ErrorKind, RelayError, RelayResult};
use relay_sync::{ApplyResult, ChangePage, LoginResponse, MutationBatch, UserProfile};
use serde::de::DeserializeOwned;

/// Talks to one server.
#[derive(Clone)]
pub struct SyncClient {
    base_url: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for SyncClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl SyncClient {
    /// Builds a client for `base_url`, e.g. `https://relay.internal`.
    pub fn new(base_url: impl Into<String>) -> RelayResult<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();

        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(RelayError::invalid(
                "the server address must start with http:// or https://",
            ));
        }

        let http = reqwest::Client::builder()
            .user_agent(concat!("Relay/", env!("CARGO_PKG_VERSION")))
            // A sync must not hang the app indefinitely; the user can
            // always retry, and a stuck request looks like a broken app.
            .timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| RelayError::new(ErrorKind::Sync, e.to_string()))?;

        Ok(Self { base_url, http })
    }

    /// The configured server address.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The WebSocket address for a workspace's change notifications.
    pub fn websocket_url(&self, workspace_id: &str) -> String {
        let ws = self
            .base_url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{ws}/v1/workspaces/{workspace_id}/ws")
    }

    /// Checks the server is reachable and speaks a protocol we understand.
    pub async fn health(&self) -> RelayResult<ServerHealth> {
        let response = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(transport_error)?;

        let health: ServerHealth = parse(response).await?;

        if health.protocol != relay_sync::PROTOCOL_VERSION {
            return Err(RelayError::new(
                ErrorKind::Sync,
                format!(
                    "that server speaks sync protocol {} but this version of Relay speaks {}",
                    health.protocol,
                    relay_sync::PROTOCOL_VERSION
                ),
            ));
        }

        Ok(health)
    }

    /// Signs in and returns a bearer token.
    pub async fn login(&self, email: &str, password: &str) -> RelayResult<LoginResponse> {
        let response = self
            .http
            .post(format!("{}/v1/auth/login", self.base_url))
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .map_err(transport_error)?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RelayError::new(
                ErrorKind::Forbidden,
                "that e-mail address and password were not accepted",
            ));
        }

        parse(response).await
    }

    /// Confirms a token is still valid and returns who it belongs to.
    pub async fn me(&self, token: &str) -> RelayResult<UserProfile> {
        let response = self
            .http
            .get(format!("{}/v1/auth/me", self.base_url))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Creates a workspace on the server, keeping the local id.
    pub async fn create_workspace(&self, token: &str, id: &str, name: &str) -> RelayResult<()> {
        let response = self
            .http
            .post(format!("{}/v1/workspaces", self.base_url))
            .bearer_auth(token)
            .json(&serde_json::json!({ "id": id, "name": name }))
            .send()
            .await
            .map_err(transport_error)?;

        // Already there is success as far as the client is concerned: it
        // means a previous attempt got through, or a teammate made it.
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }

        expect_success(response).await.map(|_| ())
    }

    /// Fetches changes above `since`.
    pub async fn pull(
        &self,
        token: &str,
        workspace_id: &str,
        since: i64,
        limit: u32,
    ) -> RelayResult<ChangePage> {
        let response = self
            .http
            .get(format!(
                "{}/v1/workspaces/{workspace_id}/changes",
                self.base_url
            ))
            .query(&[("since", since.to_string()), ("limit", limit.to_string())])
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Submits a batch of local changes.
    pub async fn push(
        &self,
        token: &str,
        workspace_id: &str,
        batch: &MutationBatch,
    ) -> RelayResult<ApplyResult> {
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/changes",
                self.base_url
            ))
            .bearer_auth(token)
            .json(batch)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }
}

/// What `/health` reports.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHealth {
    /// `"ok"` when the database answered.
    pub status: String,
    /// Which storage backend the server uses.
    pub backend: String,
    /// The sync protocol it speaks.
    pub protocol: String,
    /// Its version.
    pub version: String,
}

/// Turns a transport failure into something a user can act on.
fn transport_error(error: reqwest::Error) -> RelayError {
    let kind = ErrorKind::Sync;

    if error.is_timeout() {
        return RelayError::new(kind, "the server did not respond in time");
    }
    if error.is_connect() {
        return RelayError::new(
            kind,
            "could not reach the server. Check the address, and that you are online.",
        )
        .with_detail(error.to_string());
    }

    RelayError::new(kind, "the server could not be reached").with_detail(error.to_string())
}

/// Maps an unsuccessful response to an error, preserving what the server said.
async fn expect_success(response: reqwest::Response) -> RelayResult<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let kind = match status {
        reqwest::StatusCode::UNAUTHORIZED => ErrorKind::Forbidden,
        // The server reports an inaccessible workspace as 404 whether or
        // not it exists, so the two cannot be told apart here either.
        reqwest::StatusCode::NOT_FOUND => ErrorKind::Forbidden,
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => ErrorKind::Invalid,
        s if s.is_client_error() => ErrorKind::Invalid,
        _ => ErrorKind::Sync,
    };

    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<relay_sync::SyncError>(&body)
        .map(|e| e.to_string())
        .unwrap_or_else(|_| format!("the server returned {status}"));

    Err(RelayError::new(kind, message))
}

async fn parse<T: DeserializeOwned>(response: reqwest::Response) -> RelayResult<T> {
    let response = expect_success(response).await?;
    let body = response.text().await.map_err(transport_error)?;

    serde_json::from_str(&body).map_err(|e| {
        RelayError::new(
            ErrorKind::Sync,
            "the server sent a response this version of Relay does not understand",
        )
        .with_detail(e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_address_must_carry_a_scheme() {
        assert!(SyncClient::new("relay.internal").is_err());
        assert!(SyncClient::new("https://relay.internal").is_ok());
        assert!(SyncClient::new("http://localhost:8787").is_ok());
    }

    #[test]
    fn a_trailing_slash_is_trimmed_so_urls_do_not_double_up() {
        let client = SyncClient::new("https://relay.internal/").unwrap();
        assert_eq!(client.base_url(), "https://relay.internal");
    }

    #[test]
    fn the_websocket_url_matches_the_schemes_security() {
        // Downgrading to ws:// from an https:// server would send the
        // bearer token in clear text.
        let secure = SyncClient::new("https://relay.internal").unwrap();
        assert_eq!(
            secure.websocket_url("w1"),
            "wss://relay.internal/v1/workspaces/w1/ws"
        );

        let local = SyncClient::new("http://localhost:8787").unwrap();
        assert_eq!(
            local.websocket_url("w1"),
            "ws://localhost:8787/v1/workspaces/w1/ws"
        );
    }
}
