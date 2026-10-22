//! Request handlers.
//!
//! Each is deliberately thin: validate, delegate to the store, shape the
//! response. Authorisation is not performed here — it happens inside
//! [`crate::store::SyncStore`], which takes an `Actor` on every method, so
//! a handler cannot forget it.

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use relay_models::Timestamp;
use relay_sync::{
    ApplyResult, ChangePage, LoginRequest, LoginResponse, MutationBatch, Role, UserProfile,
};
use serde::{Deserialize, Serialize};

use crate::app::{AppState, AuthenticatedUser, Health};
use crate::auth;
use crate::error::{ServerError, ServerResult};

/// `GET /health`
pub async fn health(state: State<AppState>) -> (StatusCode, Json<Health>) {
    crate::app::health_check(state).await
}

// --- authentication --------------------------------------------------------

/// What a registration submits.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    /// The address to register.
    pub email: String,
    /// The chosen password, at least 8 characters.
    pub password: String,
    /// Shown to teammates; defaults to the local part of the address.
    #[serde(default)]
    pub display_name: String,
}

/// `POST /v1/auth/register`
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> ServerResult<Json<UserProfile>> {
    let display_name = if body.display_name.trim().is_empty() {
        body.email.split('@').next().unwrap_or("User").to_string()
    } else {
        body.display_name.clone()
    };

    let profile =
        crate::app::register_user(&state, &body.email, &body.password, &display_name).await?;
    Ok(Json(profile))
}

/// `POST /v1/auth/login`
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> ServerResult<Json<LoginResponse>> {
    let (token, expires_at, user) =
        crate::app::sign_in(&state, &body.email, &body.password).await?;

    Ok(Json(LoginResponse {
        token,
        expires_at,
        user,
    }))
}

/// `GET /v1/auth/me`
pub async fn me(user: AuthenticatedUser) -> Json<UserProfile> {
    Json(user.0)
}

/// `POST /v1/auth/logout`
///
/// Revokes the presented token immediately — the point of opaque tokens
/// over JWTs is that signing out actually signs you out.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    _user: AuthenticatedUser,
) -> ServerResult<StatusCode> {
    if let Some(header) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = auth::bearer_token(header)
    {
        state.store.delete_session(&auth::hash_token(token)).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// An address, for the endpoints that take nothing else.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailRequest {
    /// The address to send to.
    pub email: String,
}

/// A token from an e-mailed link.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRequest {
    /// The token as it appeared in the link.
    pub token: String,
}

/// A token and the password to set with it.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequest {
    /// The token as it appeared in the link.
    pub token: String,
    /// The new password.
    pub password: String,
}

/// `POST /v1/auth/verify/request`
///
/// Always answers 204, whether or not the address is registered and
/// whether or not a message went out. Anything else turns this endpoint
/// into a way to ask who has an account here.
pub async fn request_verification(
    State(state): State<AppState>,
    Json(body): Json<EmailRequest>,
) -> ServerResult<StatusCode> {
    crate::app::request_verification(&state, &body.email).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/verify/confirm`
pub async fn confirm_verification(
    State(state): State<AppState>,
    Json(body): Json<TokenRequest>,
) -> ServerResult<StatusCode> {
    crate::app::confirm_verification(&state, &body.token).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/password/reset`
///
/// Answers 204 regardless, for the reason above.
pub async fn request_password_reset(
    State(state): State<AppState>,
    Json(body): Json<EmailRequest>,
) -> ServerResult<StatusCode> {
    crate::app::request_password_reset(&state, &body.email).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/password/confirm`
pub async fn confirm_password_reset(
    State(state): State<AppState>,
    Json(body): Json<ResetRequest>,
) -> ServerResult<StatusCode> {
    crate::app::confirm_password_reset(&state, &body.token, &body.password).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- workspaces ------------------------------------------------------------

/// A workspace as the API reports it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    /// The workspace id, matching the client's own.
    pub id: String,
    /// Its display name.
    pub name: String,
    /// Its current revision.
    pub head: i64,
    /// When it was created.
    pub created_at: Timestamp,
}

/// `GET /v1/workspaces`
pub async fn list_workspaces(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> ServerResult<Json<Vec<WorkspaceSummary>>> {
    let workspaces = state.store.list_workspaces(&user.actor()).await?;

    Ok(Json(
        workspaces
            .into_iter()
            .map(|w| WorkspaceSummary {
                id: w.id,
                name: w.name,
                head: w.change_seq,
                created_at: Timestamp(w.created_at),
            })
            .collect(),
    ))
}

/// What creating a workspace submits.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspaceRequest {
    /// The client's own id for it, so local and remote ids match.
    pub id: String,
    /// Its display name.
    pub name: String,
}

/// `POST /v1/workspaces`
pub async fn create_workspace(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(body): Json<CreateWorkspaceRequest>,
) -> ServerResult<(StatusCode, Json<WorkspaceSummary>)> {
    if body.id.trim().is_empty() {
        return Err(ServerError::BadRequest("the workspace id is empty".into()));
    }

    let workspace = state
        .store
        .create_workspace(&user.actor(), body.id.trim(), &body.name)
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(WorkspaceSummary {
            id: workspace.id,
            name: workspace.name,
            head: workspace.change_seq,
            created_at: Timestamp(workspace.created_at),
        }),
    ))
}

/// The current revision.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadResponse {
    /// The workspace's current revision.
    pub head: i64,
}

/// `GET /v1/workspaces/{id}/head`
pub async fn head(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
) -> ServerResult<Json<HeadResponse>> {
    let head = state.store.head(&user.actor(), &workspace_id).await?;
    Ok(Json(HeadResponse { head }))
}

// --- synchronisation -------------------------------------------------------

/// Query parameters for a pull.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullQuery {
    /// Return objects with a revision above this. Zero fetches everything.
    #[serde(default)]
    pub since: i64,
    /// How many objects to return; clamped to the protocol maximum.
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    relay_sync::MAX_PAGE_SIZE
}

/// `GET /v1/workspaces/{id}/changes?since=N&limit=M`
pub async fn pull(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
    Query(query): Query<PullQuery>,
) -> ServerResult<Json<ChangePage>> {
    if query.since < 0 {
        return Err(ServerError::BadRequest("'since' cannot be negative".into()));
    }

    let page = state
        .store
        .changes_since(&user.actor(), &workspace_id, query.since, query.limit)
        .await?;

    Ok(Json(page))
}

/// `POST /v1/workspaces/{id}/changes`
pub async fn push(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
    Json(batch): Json<MutationBatch>,
) -> ServerResult<Json<ApplyResult>> {
    let before = state.store.head(&user.actor(), &workspace_id).await?;
    let result = state
        .store
        .apply(&user.actor(), &workspace_id, batch)
        .await?;

    // Only disturb listeners if something actually changed. A batch of
    // pure duplicates advances nothing.
    if result.head != before {
        state.hub.publish(&workspace_id, result.head);
    }

    Ok(Json(result))
}

/// `GET /v1/workspaces/{id}/export`
pub async fn export(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
) -> ServerResult<Json<ChangePage>> {
    let page = state.store.export(&user.actor(), &workspace_id).await?;
    Ok(Json(page))
}

// --- membership ------------------------------------------------------------

/// A member and who they are.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberEntry {
    /// Who they are.
    pub user: UserProfile,
    /// What they may do.
    pub role: Role,
    /// When they were added.
    pub joined_at: Timestamp,
}

/// `GET /v1/workspaces/{id}/members`
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
) -> ServerResult<Json<Vec<MemberEntry>>> {
    let members = state
        .store
        .list_members(&user.actor(), &workspace_id)
        .await?;

    Ok(Json(
        members
            .into_iter()
            .map(|(membership, profile)| MemberEntry {
                user: profile,
                role: membership.role,
                joined_at: membership.created_at,
            })
            .collect(),
    ))
}

/// The role to grant.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutMemberRequest {
    /// The role to grant.
    pub role: Role,
}

/// `PUT /v1/workspaces/{id}/members/{user_id}`
pub async fn put_member(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(body): Json<PutMemberRequest>,
) -> ServerResult<StatusCode> {
    state
        .store
        .upsert_membership(&user.actor(), &workspace_id, &target, body.role)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/workspaces/{id}/members/{user_id}`
pub async fn delete_member(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((workspace_id, target)): Path<(String, String)>,
) -> ServerResult<StatusCode> {
    state
        .store
        .remove_membership(&user.actor(), &workspace_id, &target)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- live notifications ----------------------------------------------------

/// `GET /v1/workspaces/{id}/ws`
///
/// Membership is checked **before** the upgrade, so a non-member never
/// gets a socket at all — rather than being connected and then silently
/// receiving nothing, which would leak that the workspace exists.
pub async fn websocket(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> ServerResult<Response> {
    let head = state.store.head(&user.actor(), &workspace_id).await?;

    Ok(upgrade.on_upgrade(move |socket| handle_socket(socket, state, workspace_id, head)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, workspace_id: String, head: i64) {
    let mut receiver = state.hub.subscribe(&workspace_id);

    // Send the current head immediately, so a client that reconnects after
    // missing notifications knows at once whether it is behind.
    let hello = serde_json::json!({ "type": "head", "seq": head }).to_string();
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            // A notification to forward.
            received = receiver.recv() => {
                match received {
                    Ok(notification) => {
                        let Ok(text) = serde_json::to_string(&notification) else {
                            continue;
                        };
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    // Lagged: this client missed some. It only ever needed
                    // the latest number, so carry on rather than
                    // disconnecting — the next notification corrects it.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::debug!(%workspace_id, skipped, "a websocket fell behind");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }

            // Anything from the client. The socket is notification-only, so
            // this exists to notice a close and to answer pings.
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                }
            }
        }
    }

    state.hub.prune();
}
