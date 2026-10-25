//! Request handlers.
//!
//! Each is deliberately thin: validate, delegate to the store, shape the
//! response. Authorisation is not performed here — it happens inside
//! [`crate::store::SyncStore`], which takes an `Actor` on every method, so
//! a handler cannot forget it.

use crate::billing::{Allowance, AssistantUsage};
use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use relay_models::Timestamp;
use relay_sync::{
    ApplyResult, ChangePage, InvitationOffer, InvitationView, LoginRequest, LoginResponse,
    MemberView, Membership, MutationBatch, Role, UserProfile,
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
    /// A token from an invitation e-mail, when registering through one.
    #[serde(default)]
    pub invitation: Option<String>,
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

    let profile = crate::app::register_user(
        &state,
        &body.email,
        &body.password,
        &display_name,
        body.invitation.as_deref(),
    )
    .await?;
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

/// Turns a stored invitation into the shape the API reports.
fn invitation_view(invitation: crate::store::Invitation) -> InvitationView {
    InvitationView {
        id: invitation.id,
        email: invitation.email,
        role: invitation.role,
        created_at: Timestamp(invitation.created_at),
        expires_at: Timestamp(invitation.expires_at),
    }
}

/// What to invite, and as what.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteRequest {
    /// The address to invite.
    pub email: String,
    /// The role to grant. Defaults to the least privileged.
    #[serde(default)]
    pub role: Option<Role>,
}

/// `POST /v1/workspaces/{id}/invitations`
pub async fn create_invitation(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    user: AuthenticatedUser,
    Json(body): Json<InviteRequest>,
) -> ServerResult<Json<InvitationView>> {
    let invitation = crate::app::invite_to_workspace(
        &state,
        &user.actor(),
        &workspace_id,
        &body.email,
        // A caller who names no role gets the least privileged one.
        body.role.unwrap_or(Role::Viewer),
    )
    .await?;

    Ok(Json(invitation_view(invitation)))
}

/// `GET /v1/workspaces/{id}/invitations`
pub async fn list_invitations(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    user: AuthenticatedUser,
) -> ServerResult<Json<Vec<InvitationView>>> {
    let invitations = state
        .store
        .list_invitations(&user.actor(), &workspace_id)
        .await?;

    Ok(Json(invitations.into_iter().map(invitation_view).collect()))
}

/// `DELETE /v1/workspaces/{id}/invitations/{invitation_id}`
pub async fn revoke_invitation(
    State(state): State<AppState>,
    Path((workspace_id, invitation_id)): Path<(String, String)>,
    user: AuthenticatedUser,
) -> ServerResult<StatusCode> {
    state
        .store
        .revoke_invitation(&user.actor(), &workspace_id, &invitation_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/invitations/{token}`
///
/// Unauthenticated on purpose: the recipient may not have an account
/// yet, and holding the token is what entitles them to read this.
pub async fn invitation_offer(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> ServerResult<Json<InvitationOffer>> {
    let offer = state
        .store
        .invitation_offer(&auth::hash_token(&token), Timestamp::now().as_millis())
        .await?
        .ok_or_else(|| {
            ServerError::BadRequest("that invitation has expired or has already been used".into())
        })?;

    Ok(Json(InvitationOffer {
        workspace_id: offer.workspace_id,
        workspace_name: offer.workspace_name,
        email: offer.email,
        role: offer.role,
    }))
}

/// `POST /v1/invitations/{token}/accept`
pub async fn accept_invitation(
    State(state): State<AppState>,
    Path(token): Path<String>,
    user: AuthenticatedUser,
) -> ServerResult<Json<Membership>> {
    let membership = crate::app::accept_invitation(&state, &user.0, &token).await?;
    Ok(Json(membership))
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

/// `GET /v1/workspaces/{id}/members`
pub async fn list_members(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(workspace_id): Path<String>,
) -> ServerResult<Json<Vec<MemberView>>> {
    let members = state
        .store
        .list_members(&user.actor(), &workspace_id)
        .await?;

    Ok(Json(
        members
            .into_iter()
            .map(|(membership, profile)| MemberView {
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

// --- the hosted assistant ---------------------------------------------------

/// Works out what a person may do with the hosted assistant.
///
/// Shared by the usage endpoint and, later, by the proxy itself —
/// one place, so what the panel shows and what the proxy enforces
/// cannot drift apart.
pub async fn allowance_for(state: &AppState, user_id: &str) -> ServerResult<Allowance> {
    if !state.config.hosts_assistant() {
        return Ok(Allowance::decide(None, AssistantUsage::default()));
    }

    let assistant_prices = state.config.assistant_prices();
    let subscriptions = state.store.subscriptions_for_user(user_id).await?;

    // The best plan any of their workspaces is on. Somebody in a paid
    // team and a free side project gets the allowance; charging them
    // for a plan they already have would be the wrong answer.
    let included = subscriptions
        .iter()
        .any(|s| s.status.entitles() && s.plan(&assistant_prices).includes_assistant())
        .then_some(state.config.assistant_turns);

    let period = crate::billing::period_start(Timestamp::now().as_millis());
    let usage = state.store.assistant_usage(user_id, period).await?;

    Ok(Allowance::decide(included, usage))
}

/// `POST /v1/assistant/messages`
///
/// One request against the model, on Relay's key, for a subscriber
/// whose plan includes it. The loop stays on their machine; this is
/// the step that costs money, and so the step that is counted.
pub async fn assistant_messages(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(turn): Json<crate::assistant::TurnRequest>,
) -> ServerResult<Json<crate::assistant::TurnResponse>> {
    let assistant = state
        .assistant
        .as_ref()
        .ok_or_else(|| ServerError::BadRequest("this server does not host the assistant".into()))?;

    let allowance = allowance_for(&state, &user.0.id).await?;
    if !allowance.allowed {
        // 402, like the seat limit: this is a limit reached, not a
        // malformed request, and the notice says their own key works
        // with no limit at all.
        return Err(ServerError::LimitExceeded {
            limit: "assistantTurns",
            message: allowance
                .notice
                .unwrap_or_else(|| "No assistant requests left this month.".into()),
        });
    }

    let answer = assistant.run(&turn).await?;

    // Recorded after the fact, from what the provider says it cost.
    // A turn that generated tokens is billed whether or not the
    // caller is still listening, because the provider bills it
    // either way and not counting it is an obvious hole.
    let period = crate::billing::period_start(Timestamp::now().as_millis());
    if let Err(error) = state
        .store
        .record_assistant_turn(&user.0.id, period, answer.usage)
        .await
    {
        // Losing the count must not lose the answer the person has
        // already paid for. Loud, because a limiter that silently
        // stops counting is worse than no limiter.
        tracing::error!(%error, user = %user.0.id, "could not record assistant usage");
    }

    Ok(Json(answer))
}

/// `POST /v1/assistant/messages/stream`
///
/// The same turn, forwarded as it is written so the person watches
/// the answer appear rather than a spinner. The provider's events go
/// through byte for byte, which is what lets the client read them
/// with the same code it uses when talking to Anthropic directly.
pub async fn assistant_stream(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(turn): Json<crate::assistant::TurnRequest>,
) -> ServerResult<axum::response::Response> {
    use futures_util::StreamExt;

    let assistant = state
        .assistant
        .as_ref()
        .ok_or_else(|| ServerError::BadRequest("this server does not host the assistant".into()))?;

    let allowance = allowance_for(&state, &user.0.id).await?;
    if !allowance.allowed {
        return Err(ServerError::LimitExceeded {
            limit: "assistantTurns",
            message: allowance
                .notice
                .unwrap_or_else(|| "No assistant requests left this month.".into()),
        });
    }

    // Opened before anything is forwarded: a refusal from the
    // provider is a status while there is still a status to send. A
    // moment later the client holds a 200 and there is nowhere to put
    // the bad news but the stream itself.
    let upstream = assistant.open_stream(&turn).await?;

    // Records what the turn cost when the forwarding ends — whether
    // the provider finished, the client closed the window, or the
    // connection broke. The provider charges for what it generated in
    // every one of those cases, and a counter that only increments on
    // the happy path is a hole anybody could find.
    struct Billed {
        store: std::sync::Arc<dyn crate::store::SyncStore>,
        user: String,
        /// Shared with the forwarding closure rather than written
        /// through this struct. A closure captures disjoint fields,
        /// so a closure that only assigned `billed.usage` would
        /// capture that field alone and leave the guard to drop
        /// here, recording nothing — which is how this was first
        /// written, and what the test caught.
        usage: std::sync::Arc<std::sync::Mutex<crate::billing::AssistantUsage>>,
    }

    impl Drop for Billed {
        fn drop(&mut self) {
            let usage = self
                .usage
                .lock()
                .map(|u| *u)
                .unwrap_or_default();

            // Nothing was generated, so there is nothing to bill — an
            // upstream that refused before the first event must not
            // come out of somebody's allowance.
            if usage.turns == 0 {
                return;
            }

            let (store, user) = (self.store.clone(), self.user.clone());
            let period = crate::billing::period_start(Timestamp::now().as_millis());
            tokio::spawn(async move {
                if let Err(error) = store.record_assistant_turn(&user, period, usage).await {
                    // Loud: a limiter that silently stops counting is
                    // worse than no limiter.
                    tracing::error!(%error, user = %user, "could not record assistant usage");
                }
            });
        }
    }

    let counted = std::sync::Arc::new(std::sync::Mutex::new(
        crate::billing::AssistantUsage::default(),
    ));
    let billed = Billed {
        store: state.store.clone(),
        user: user.0.id.clone(),
        usage: counted.clone(),
    };

    let mut watcher = crate::assistant::UsageWatcher::new();
    let body = upstream.bytes_stream().map(move |chunk| {
        // Names the whole guard so the closure captures all of it.
        // Without this the guard is not moved into the stream, and
        // the turn is recorded when the handler returns instead of
        // when the answer finishes — which is to say, never.
        let _keep = &billed;

        if let Ok(bytes) = &chunk {
            watcher.observe(&String::from_utf8_lossy(bytes));
            if let Ok(mut total) = counted.lock() {
                *total = watcher.usage();
            }
        }
        chunk
    });

    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        // Without this a reverse proxy may buffer the whole thing and
        // hand it over at the end, which is the problem this endpoint
        // exists to solve.
        .header("x-accel-buffering", "no")
        .body(axum::body::Body::from_stream(body))
        .map_err(|e| ServerError::Assistant(e.to_string()))
}

/// `GET /v1/assistant/usage`
///
/// What the chat panel shows when somebody asks where they stand, and
/// what it shows them when they have run out.
pub async fn assistant_usage(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> ServerResult<Json<Allowance>> {
    Ok(Json(allowance_for(&state, &user.0.id).await?))
}

// --- billing ---------------------------------------------------------------

/// `GET /v1/workspaces/{id}/billing`
pub async fn billing_status(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    user: AuthenticatedUser,
) -> ServerResult<Json<relay_sync::BillingView>> {
    // Membership is the authorisation: the seat count is not secret
    // from the people occupying the seats.
    let membership = state
        .store
        .membership(&workspace_id, &user.0.id)
        .await?
        .ok_or(ServerError::Forbidden)?;

    let entitlement = state.store.entitlement(&workspace_id).await?;
    let subscription = state.store.subscription(&workspace_id).await?;

    Ok(Json(relay_sync::BillingView {
        metered: state.config.bills(),
        seats: entitlement.seats,
        members: state.store.member_count(&workspace_id).await?,
        active: entitlement.active,
        notice: entitlement.notice,
        has_subscription: subscription.is_some(),
        can_manage: membership.role == Role::Admin,
    }))
}

/// How many seats to buy.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutRequest {
    /// Seats. Defaults to the current member count.
    #[serde(default)]
    pub seats: Option<u32>,
}

/// Where to send the browser.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedirectView {
    /// The URL to open.
    pub url: String,
}

/// `POST /v1/workspaces/{id}/billing/checkout`
pub async fn billing_checkout(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    user: AuthenticatedUser,
    Json(body): Json<CheckoutRequest>,
) -> ServerResult<Json<RedirectView>> {
    let stripe = state
        .stripe
        .as_ref()
        .ok_or_else(|| ServerError::BadRequest("this server does not charge for seats".into()))?;

    // Only an administrator commits the workspace to a bill.
    let membership = state
        .store
        .membership(&workspace_id, &user.0.id)
        .await?
        .ok_or(ServerError::Forbidden)?;
    if membership.role != Role::Admin {
        return Err(ServerError::Forbidden);
    }

    let members = state.store.member_count(&workspace_id).await?;
    let seats = body.seats.unwrap_or(members).max(members).max(1);
    let base = state.config.public_url.trim_end_matches('/');

    let url = stripe
        .checkout_session(
            &workspace_id,
            seats,
            &user.0.email,
            &format!("{base}/billing/done"),
            &format!("{base}/billing/cancelled"),
            // Keyed on the workspace and the seat count, so a
            // double-click returns the same session instead of
            // starting a second subscription.
            &format!("checkout:{workspace_id}:{seats}"),
        )
        .await?;

    Ok(Json(RedirectView { url }))
}

/// `POST /v1/workspaces/{id}/billing/portal`
pub async fn billing_portal(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    user: AuthenticatedUser,
) -> ServerResult<Json<RedirectView>> {
    let stripe = state
        .stripe
        .as_ref()
        .ok_or_else(|| ServerError::BadRequest("this server does not charge for seats".into()))?;

    let membership = state
        .store
        .membership(&workspace_id, &user.0.id)
        .await?
        .ok_or(ServerError::Forbidden)?;
    if membership.role != Role::Admin {
        return Err(ServerError::Forbidden);
    }

    let subscription = state
        .store
        .subscription(&workspace_id)
        .await?
        .ok_or_else(|| ServerError::BadRequest("this workspace has no subscription".into()))?;

    let base = state.config.public_url.trim_end_matches('/');
    let return_url = state
        .config
        .stripe_return_url
        .clone()
        .unwrap_or_else(|| format!("{base}/billing/done"));

    let url = stripe
        .portal_session(&subscription.customer_id, &return_url)
        .await?;

    Ok(Json(RedirectView { url }))
}

/// `POST /v1/billing/webhook`
///
/// Unauthenticated, because Stripe has no account here. The signature
/// is the authentication, and nothing is read out of the body until it
/// has been checked.
pub async fn billing_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ServerResult<StatusCode> {
    let secret = state
        .config
        .stripe_webhook_secret
        .as_deref()
        .ok_or_else(|| ServerError::BadRequest("this server does not take webhooks".into()))?;

    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ServerError::BadRequest("no Stripe signature".into()))?;

    let now = Timestamp::now().as_millis() / 1000;
    let verified = crate::billing::webhook::verify(&body, signature, secret, now)?;

    let event = crate::billing::stripe::read_event(verified)?;

    // Stripe retries until acknowledged, so the same event arrives
    // more than once. Claiming it first makes a replay a no-op.
    if !event.event_id.is_empty() && !state.store.claim_billing_event(&event.event_id).await? {
        tracing::debug!(event = %event.event_id, "already applied");
        return Ok(StatusCode::NO_CONTENT);
    }

    let Some(status) = event.status else {
        // An event type this build does not act on. Acknowledged, or
        // Stripe retries it for days.
        return Ok(StatusCode::NO_CONTENT);
    };

    // A subscription event names a customer; the workspace comes from
    // metadata, or from whichever workspace that customer already has.
    let workspace_id = match &event.workspace_id {
        Some(id) => Some(id.clone()),
        None => match &event.customer_id {
            Some(customer) => state.store.workspace_for_customer(customer).await?,
            None => None,
        },
    };

    let Some(workspace_id) = workspace_id else {
        tracing::warn!(event = %event.event_id, "a billing event named no workspace");
        return Ok(StatusCode::NO_CONTENT);
    };

    let existing = state.store.subscription(&workspace_id).await?;

    state
        .store
        .save_subscription(&crate::billing::Subscription {
            workspace_id: workspace_id.clone(),
            customer_id: event
                .customer_id
                .clone()
                .or_else(|| existing.as_ref().map(|s| s.customer_id.clone()))
                .unwrap_or_default(),
            subscription_id: event
                .subscription_id
                .clone()
                .or_else(|| existing.as_ref().and_then(|s| s.subscription_id.clone())),
            status,
            // A checkout event carries no line items, so an existing
            // seat count is kept rather than reset to zero.
            seats: event
                .seats
                .or_else(|| existing.as_ref().map(|s| s.seats))
                .unwrap_or(1),
            current_period_end: event
                .current_period_end
                .or_else(|| existing.as_ref().and_then(|s| s.current_period_end)),
            // A checkout event carries no line items either, so the
            // price arrives with the subscription event that follows.
            price_id: event
                .price_id
                .clone()
                .or_else(|| existing.as_ref().and_then(|s| s.price_id.clone())),
        })
        .await?;

    tracing::info!(workspace = %workspace_id, ?status, "subscription updated");
    Ok(StatusCode::NO_CONTENT)
}
