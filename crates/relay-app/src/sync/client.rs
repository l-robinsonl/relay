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

        // The password was right but the address is unconfirmed. Worth
        // its own message: "not accepted" would send someone hunting for
        // a typo in a password that is perfectly correct.
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(RelayError::new(
                ErrorKind::Forbidden,
                "confirm your e-mail address before signing in",
            )
            .with_detail(
                "The server sent you a link when you registered. Ask for                  another if you cannot find it."
                    .to_string(),
            ));
        }

        parse(response).await
    }

    /// Creates an account on the server.
    pub async fn register(
        &self,
        email: &str,
        password: &str,
        display_name: &str,
        invitation: Option<&str>,
    ) -> RelayResult<UserProfile> {
        let response = self
            .http
            .post(format!("{}/v1/auth/register", self.base_url))
            .json(&serde_json::json!({
                "email": email,
                "password": password,
                "displayName": display_name,
                // An invite-only server refuses registration without
                // this, which is most of the point of invite-only.
                "invitation": invitation,
            }))
            .send()
            .await
            .map_err(transport_error)?;

        match response.status() {
            reqwest::StatusCode::CONFLICT => Err(RelayError::new(
                ErrorKind::Conflict,
                "that e-mail address already has an account on this server",
            )
            .with_detail("Sign in instead, or reset the password.")),
            reqwest::StatusCode::FORBIDDEN => Err(RelayError::new(
                ErrorKind::Forbidden,
                "this server is not accepting new accounts",
            )
            .with_detail(
                "Its administrator sets who may register. Ask them for an \
                 account."
                    .to_string(),
            )),
            _ => parse(response).await,
        }
    }

    /// Asks for another confirmation e-mail.
    ///
    /// Succeeds whether or not the address is registered: the server
    /// answers the same either way, on purpose.
    pub async fn request_verification(&self, email: &str) -> RelayResult<()> {
        self.post_email("/v1/auth/verify/request", email).await
    }

    /// Asks for a password reset e-mail. Also deliberately uninformative.
    pub async fn request_password_reset(&self, email: &str) -> RelayResult<()> {
        self.post_email("/v1/auth/password/reset", email).await
    }

    async fn post_email(&self, path: &str, email: &str) -> RelayResult<()> {
        let response = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(transport_error)?;

        expect_success(response).await?;
        Ok(())
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

    /// Describes an invitation without accepting it.
    ///
    /// Needs no credentials: whoever holds the token is entitled to see
    /// what they have been offered, and will not have an account yet if
    /// the invitation is the reason they are here.
    pub async fn invitation_offer(&self, token: &str) -> RelayResult<relay_sync::InvitationOffer> {
        let response = self
            .http
            .get(format!("{}/v1/invitations/{token}", self.base_url))
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Accepts an invitation as the signed-in user.
    pub async fn accept_invitation(
        &self,
        token: &str,
        invitation: &str,
    ) -> RelayResult<relay_sync::Membership> {
        let response = self
            .http
            .post(format!(
                "{}/v1/invitations/{invitation}/accept",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Who is in a workspace, and as what.
    pub async fn list_members(
        &self,
        token: &str,
        workspace_id: &str,
    ) -> RelayResult<Vec<relay_sync::MemberView>> {
        let response = self
            .http
            .get(format!(
                "{}/v1/workspaces/{workspace_id}/members",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Changes what a member may do.
    pub async fn set_member_role(
        &self,
        token: &str,
        workspace_id: &str,
        user_id: &str,
        role: relay_sync::Role,
    ) -> RelayResult<()> {
        let response = self
            .http
            .put(format!(
                "{}/v1/workspaces/{workspace_id}/members/{user_id}",
                self.base_url
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({ "role": role }))
            .send()
            .await
            .map_err(transport_error)?;

        expect_success(response).await?;
        Ok(())
    }

    /// Removes someone from a workspace.
    pub async fn remove_member(
        &self,
        token: &str,
        workspace_id: &str,
        user_id: &str,
    ) -> RelayResult<()> {
        let response = self
            .http
            .delete(format!(
                "{}/v1/workspaces/{workspace_id}/members/{user_id}",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        expect_success(response).await?;
        Ok(())
    }

    /// Invites an address to a workspace with a role.
    pub async fn invite(
        &self,
        token: &str,
        workspace_id: &str,
        email: &str,
        role: relay_sync::Role,
    ) -> RelayResult<relay_sync::InvitationView> {
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/invitations",
                self.base_url
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({ "email": email, "role": role }))
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Invitations sent but not yet accepted.
    pub async fn list_invitations(
        &self,
        token: &str,
        workspace_id: &str,
    ) -> RelayResult<Vec<relay_sync::InvitationView>> {
        let response = self
            .http
            .get(format!(
                "{}/v1/workspaces/{workspace_id}/invitations",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Withdraws an invitation.
    pub async fn revoke_invitation(
        &self,
        token: &str,
        workspace_id: &str,
        invitation_id: &str,
    ) -> RelayResult<()> {
        let response = self
            .http
            .delete(format!(
                "{}/v1/workspaces/{workspace_id}/invitations/{invitation_id}",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        expect_success(response).await?;
        Ok(())
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

    /// What the workspace is entitled to, and whether it is paid for.
    pub async fn billing(
        &self,
        token: &str,
        workspace_id: &str,
    ) -> RelayResult<relay_sync::BillingView> {
        let response = self
            .http
            .get(format!(
                "{}/v1/workspaces/{workspace_id}/billing",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        parse(response).await
    }

    /// Starts a checkout, returning the URL to open in a browser.
    ///
    /// Relay never renders a payment form. The card details belong to
    /// Stripe's page, in the system browser, where the person can see
    /// the address bar — not inside a desktop app's webview.
    pub async fn billing_checkout(
        &self,
        token: &str,
        workspace_id: &str,
        seats: Option<u32>,
    ) -> RelayResult<String> {
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/billing/checkout",
                self.base_url
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({ "seats": seats }))
            .send()
            .await
            .map_err(transport_error)?;

        let view: RedirectView = parse(response).await?;
        checked_payment_url(view.url)
    }

    /// Opens Stripe's customer portal, returning the URL.
    pub async fn billing_portal(&self, token: &str, workspace_id: &str) -> RelayResult<String> {
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/billing/portal",
                self.base_url
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(transport_error)?;

        let view: RedirectView = parse(response).await?;
        checked_payment_url(view.url)
    }
}

/// Refuses a payment URL that is not https.
///
/// This URL is handed straight to the operating system to open. The
/// sync server is one the user chose, but a compromised or hostile
/// one should not be able to make Relay launch `file:///…` or a
/// custom scheme registered by something else on the machine. Stripe
/// only ever answers with https.
fn checked_payment_url(url: String) -> RelayResult<String> {
    if url.starts_with("https://") {
        return Ok(url);
    }

    Err(RelayError::new(
        ErrorKind::Sync,
        "the server answered with a payment link that is not https",
    )
    .with_detail(
        "Relay will not open it. Check which server this workspace is connected to.".to_string(),
    ))
}

/// Where the server wants the browser sent.
#[derive(Debug, Clone, serde::Deserialize)]
struct RedirectView {
    url: String,
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
        // Seats, or any other limit the server bills for.
        reqwest::StatusCode::PAYMENT_REQUIRED => ErrorKind::Invalid,
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
    fn a_payment_link_that_is_not_https_is_refused() {
        // It goes straight to the OS to open.
        assert!(checked_payment_url("https://checkout.stripe.com/c/pay/x".into()).is_ok());
        for hostile in [
            "file:///etc/passwd",
            "http://checkout.stripe.com/c/pay/x",
            "javascript:alert(1)",
            "relay://something",
            " https://checkout.stripe.com",
        ] {
            assert!(
                checked_payment_url(hostile.into()).is_err(),
                "opened {hostile}"
            );
        }
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
