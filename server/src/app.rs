//! The HTTP application: state, authentication extractor and router.

use std::sync::Arc;

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use relay_models::Timestamp;
use relay_sync::UserProfile;

use crate::auth;
use crate::config::{Config, SignupMode};
use crate::error::{ServerError, ServerResult};
use crate::hub::Hub;
use crate::routes;
use crate::store::{Actor, NewUser, SyncStore};

/// Everything a handler needs.
#[derive(Clone)]
pub struct AppState {
    /// The configured persistence backend.
    pub store: Arc<dyn SyncStore>,
    /// Live change notifications.
    pub hub: Hub,
    /// The server's settings.
    pub config: Arc<Config>,
    /// Where transactional e-mail goes.
    pub mailer: Arc<dyn crate::mail::Mailer>,
    /// Stripe, when this deployment charges. `None` is self-hosting.
    pub stripe: Option<Arc<crate::billing::stripe::Stripe>>,
    /// Throttles the routes that cost something to abuse.
    pub limiter: crate::ratelimit::RateLimiter,
    /// The model provider, when this deployment hosts the assistant.
    pub assistant: Option<Arc<crate::assistant::Assistant>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("backend", &self.store.backend())
            .field("mailer", &self.mailer.name())
            .finish()
    }
}

impl AppState {
    /// Builds the state, choosing a mailer from the configuration.
    ///
    /// Falls back to the logging mailer rather than refusing to start:
    /// a misconfigured mail provider should not take the sync server
    /// down with it, and the startup log says which one is in use.
    pub fn new(store: Arc<dyn SyncStore>, config: Config) -> Self {
        let mailer = crate::mail::from_config(&config).unwrap_or_else(|error| {
            tracing::error!(%error, "falling back to logging e-mail instead of sending it");
            Arc::new(crate::mail::LogMailer)
        });

        Self::with_mailer(store, config, mailer)
    }

    /// Builds the state with a given mailer. Used by tests.
    pub fn with_mailer(
        store: Arc<dyn SyncStore>,
        config: Config,
        mailer: Arc<dyn crate::mail::Mailer>,
    ) -> Self {
        // Billing is off unless every piece is configured. A key with
        // no webhook secret would take payments and never hear that
        // they succeeded, which is worse than not charging.
        let stripe = config
            .bills()
            .then(|| {
                crate::billing::stripe::Stripe::new(
                    config.stripe_secret_key.clone().unwrap_or_default(),
                    config.stripe_price_id.clone().unwrap_or_default(),
                )
            })
            .transpose()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "Stripe is misconfigured; seats will not be charged");
                None
            })
            .map(Arc::new);

        // Both halves required, like Stripe: a key with no price to
        // sell it against hands model usage to everybody.
        let assistant = config
            .hosts_assistant()
            .then(|| {
                crate::assistant::Assistant::new(
                    config.assistant_key.clone().unwrap_or_default(),
                    config.assistant_model.clone(),
                )
            })
            .transpose()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "the assistant is misconfigured; it will not be hosted");
                None
            })
            .map(Arc::new);

        Self {
            store,
            hub: Hub::new(),
            config: Arc::new(config),
            mailer,
            stripe,
            limiter: crate::ratelimit::RateLimiter::new(),
            assistant,
        }
    }

    /// Resolves a bearer token to the user it belongs to.
    pub async fn authenticate(&self, headers: &HeaderMap) -> ServerResult<UserProfile> {
        let header = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ServerError::Unauthenticated)?;

        let token = auth::bearer_token(header).ok_or(ServerError::Unauthenticated)?;

        self.store
            .session_user(&auth::hash_token(token))
            .await?
            .ok_or(ServerError::Unauthenticated)
    }

    /// Creates the bootstrap administrator, if one is configured and the
    /// server has no users yet.
    ///
    /// Only on an empty server: re-running with the variables still set
    /// must not reset anybody's password or resurrect a deleted account.
    pub async fn bootstrap_admin(&self) -> ServerResult<Option<UserProfile>> {
        let (Some(email), Some(password)) = (&self.config.admin_email, &self.config.admin_password)
        else {
            return Ok(None);
        };

        if self.store.user_count().await? > 0 {
            tracing::info!("users already exist; skipping administrator bootstrap");
            return Ok(None);
        }

        let email = auth::validate_email(email)?;
        let profile = self
            .store
            .create_user(NewUser {
                email: email.clone(),
                password_hash: auth::hash_password(password)?,
                display_name: "Administrator".to_string(),
                is_server_admin: true,
            })
            .await?;

        tracing::info!(%email, "created the bootstrap administrator");
        Ok(Some(profile))
    }
}

/// An authenticated caller, extracted from the `Authorization` header.
///
/// Taking this as a handler argument is what makes a route authenticated.
/// A route that forgets it simply does not compile against the store,
/// because every store method needs an [`Actor`].
#[derive(Debug, Clone)]
pub struct AuthenticatedUser(pub UserProfile);

impl AuthenticatedUser {
    /// The actor to pass to the store.
    pub fn actor(&self) -> Actor {
        Actor {
            user_id: self.0.id.clone(),
            is_server_admin: self.0.is_server_admin,
        }
    }
}

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        state.authenticate(&parts.headers).await.map(Self)
    }
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    use tower_http::limit::RequestBodyLimitLayer;
    use tower_http::trace::TraceLayer;

    let limit = state.config.max_body_bytes;

    Router::new()
        // Unauthenticated.
        .route("/health", get(routes::health))
        // The two pages a person sees in a browser, reached from the
        // links in a verification or reset e-mail.
        .route("/auth/verify", get(crate::pages::verify))
        .route("/invite", get(crate::pages::invite))
        .route(
            "/auth/reset",
            get(crate::pages::reset_form).post(crate::pages::reset_submit),
        )
        // Authenticated.
        .route("/v1/auth/me", get(routes::me))
        .route("/v1/auth/logout", post(routes::logout))
        .route(
            "/v1/auth/verify/confirm",
            post(routes::confirm_verification),
        )
        .route(
            "/v1/auth/password/confirm",
            post(routes::confirm_password_reset),
        )
        .route(
            "/v1/workspaces",
            get(routes::list_workspaces).post(routes::create_workspace),
        )
        .route("/v1/workspaces/{id}/head", get(routes::head))
        .route(
            "/v1/workspaces/{id}/changes",
            get(routes::pull).post(routes::push),
        )
        .route("/v1/workspaces/{id}/export", get(routes::export))
        .route("/v1/workspaces/{id}/members", get(routes::list_members))
        .route(
            "/v1/workspaces/{id}/invitations",
            get(routes::list_invitations).post(routes::create_invitation),
        )
        .route(
            "/v1/workspaces/{id}/invitations/{invitation_id}",
            delete(routes::revoke_invitation),
        )
        .route("/v1/workspaces/{id}/billing", get(routes::billing_status))
        .route(
            "/v1/workspaces/{id}/billing/checkout",
            post(routes::billing_checkout),
        )
        .route(
            "/v1/workspaces/{id}/billing/portal",
            post(routes::billing_portal),
        )
        .route("/v1/assistant/usage", get(routes::assistant_usage))
        .route("/v1/assistant/messages", post(routes::assistant_messages))
        .route("/v1/billing/webhook", post(routes::billing_webhook))
        // Where Stripe returns the browser. Named in every checkout
        // session, so they have to exist.
        // Registration from a browser, for anyone who has not installed
        // Relay yet. The page says so itself when this server is
        // invite-only or closed.
        .route("/billing/done", get(crate::pages::billing_done))
        .route("/billing/cancelled", get(crate::pages::billing_cancelled))
        .route("/v1/invitations/{token}", get(routes::invitation_offer))
        .route(
            "/v1/invitations/{token}/accept",
            post(routes::accept_invitation),
        )
        .route(
            "/v1/workspaces/{id}/members/{user_id}",
            put(routes::put_member),
        )
        .route(
            "/v1/workspaces/{id}/members/{user_id}",
            delete(routes::delete_member),
        )
        .route("/v1/workspaces/{id}/ws", get(routes::websocket))
        // Throttled. Each group has its own allowance, so grinding
        // passwords cannot also stop anybody signing up. Safe methods
        // are never counted — the GET of /signup is a page view.
        .merge(
            Router::new()
                .route("/v1/auth/register", post(routes::register))
                .route(
                    "/signup",
                    get(crate::pages::signup_form).post(crate::pages::signup_submit),
                )
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    crate::ratelimit::signup,
                ))
                .with_state(state.clone()),
        )
        .merge(
            Router::new()
                .route("/v1/auth/login", post(routes::login))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    crate::ratelimit::sign_in,
                ))
                .with_state(state.clone()),
        )
        .merge(
            Router::new()
                .route(
                    "/v1/auth/verify/request",
                    post(routes::request_verification),
                )
                .route(
                    "/v1/auth/password/reset",
                    post(routes::request_password_reset),
                )
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    crate::ratelimit::sends_mail,
                ))
                .with_state(state.clone()),
        )
        .layer(RequestBodyLimitLayer::new(limit))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Registers a new account, subject to the configured signup mode.
pub async fn register_user(
    state: &AppState,
    email: &str,
    password: &str,
    display_name: &str,
    invitation: Option<&str>,
) -> ServerResult<UserProfile> {
    // Checked before anything is created: an invitation decides both
    // whether registration is allowed under `invite` mode and which
    // address the account must use.
    let offer = match invitation {
        None => None,
        Some(token) => Some(
            state
                .store
                .invitation_offer(&auth::hash_token(token), Timestamp::now().as_millis())
                .await?
                .ok_or_else(|| {
                    ServerError::BadRequest(
                        "that invitation has expired or has already been used".into(),
                    )
                })?,
        ),
    };

    match state.config.signup_mode {
        SignupMode::Open => {}
        SignupMode::Off => return Err(ServerError::Forbidden),
        SignupMode::Invite => {
            if offer.is_none() {
                return Err(ServerError::Forbidden);
            }
        }
    }

    // The account must use the address the invitation was sent to, or an
    // invitation to one person is a registration ticket for whoever it
    // gets forwarded to.
    if let Some(offer) = &offer
        && !offer.email.eq_ignore_ascii_case(email.trim())
    {
        return Err(ServerError::BadRequest(format!(
            "that invitation was sent to {}; register with that address",
            offer.email
        )));
    }

    let email = auth::validate_email(email)?;
    let hash = auth::hash_password(password)?;

    let profile = state
        .store
        .create_user(NewUser {
            email,
            password_hash: hash,
            display_name: display_name.trim().to_string(),
            is_server_admin: false,
        })
        .await?;

    // Joining is what the invitation was for, so a failure here is not
    // something to shrug off: the account would exist but sit outside
    // the workspace, with the token already spent.
    if let Some(token) = invitation {
        state
            .store
            .accept_invitation(
                &profile.id,
                &profile.email,
                &auth::hash_token(token),
                Timestamp::now().as_millis(),
            )
            .await?;
    }

    // Best effort: the account exists either way, and a user who never
    // receives the mail can ask for another.
    if let Err(error) = issue_email_token(
        state,
        &profile,
        crate::store::EmailTokenPurpose::VerifyEmail,
    )
    .await
    {
        tracing::error!(%error, user = %profile.id, "could not send the verification e-mail");
    }

    Ok(profile)
}

/// Verifies credentials and opens a session.
pub async fn sign_in(
    state: &AppState,
    email: &str,
    password: &str,
) -> ServerResult<(String, Timestamp, UserProfile)> {
    let found = state.store.find_user_by_email(email).await?;

    let Some(stored) = found else {
        // Spend comparable effort before failing, so response timing does
        // not reveal which addresses are registered.
        auth::verify_dummy(password);
        return Err(ServerError::Unauthenticated);
    };

    if !auth::verify_password(password, &stored.password_hash) {
        return Err(ServerError::Unauthenticated);
    }

    // Checked after the password, so this never tells someone without
    // the password whether an address is registered. Administrators are
    // exempt: the bootstrap admin is created from the environment and
    // has no inbox to check.
    if state.config.require_verified_email
        && stored.profile.email_verified_at.is_none()
        && !stored.profile.is_server_admin
    {
        return Err(ServerError::EmailNotVerified);
    }

    let session = auth::new_session_token()?;
    let expires_at =
        Timestamp(Timestamp::now().as_millis() + state.config.session_ttl.as_millis() as i64);

    state
        .store
        .create_session(&stored.profile.id, &session.hash, expires_at.as_millis())
        .await?;

    Ok((session.token, expires_at, stored.profile))
}

/// The shape of a health check response.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// Always `"ok"` when the database answered.
    pub status: &'static str,
    /// Which storage backend is in use.
    pub backend: &'static str,
    /// The protocol version this server speaks.
    pub protocol: &'static str,
    /// The build's version.
    pub version: &'static str,
}

/// Reports whether the server and its database are usable.
pub async fn health_check(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let body = Health {
        status: if state.store.ping().await.is_ok() {
            "ok"
        } else {
            "degraded"
        },
        backend: state.store.backend(),
        protocol: relay_sync::PROTOCOL_VERSION,
        version: env!("CARGO_PKG_VERSION"),
    };

    let code = if body.status == "ok" {
        StatusCode::OK
    } else {
        // A load balancer must be able to tell, without parsing a body.
        StatusCode::SERVICE_UNAVAILABLE
    };

    (code, Json(body))
}

// --- e-mail verification and password reset ------------------------------

/// How soon a user may ask for another e-mail of the same kind.
///
/// Without this, anyone who knows an address can use this server to post
/// mail at it, which is both a nuisance to the recipient and a fast way
/// to get a sending domain blocked.
const RESEND_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Issues a token, stores its hash and e-mails the link.
///
/// Returns quietly whether or not anything was sent: the caller must not
/// be able to tell an unregistered address from a registered one.
async fn issue_email_token(
    state: &AppState,
    user: &UserProfile,
    purpose: crate::store::EmailTokenPurpose,
) -> ServerResult<()> {
    use crate::store::EmailTokenPurpose;

    let now = Timestamp::now().as_millis();

    if let Some(last) = state.store.latest_email_token_at(&user.id, purpose).await?
        && now - last < RESEND_COOLDOWN.as_millis() as i64
    {
        // Already sent one a moment ago. Succeed silently rather than
        // reporting a rate limit, which would itself confirm the address.
        tracing::debug!(user = %user.id, "suppressed a duplicate e-mail within the cooldown");
        return Ok(());
    }

    let token = auth::new_session_token()?;
    let expires_at = now + purpose.lifetime().as_millis() as i64;

    state
        .store
        .create_email_token(&user.id, purpose, &token.hash, expires_at)
        .await?;

    let base = state.config.public_url.trim_end_matches('/');
    let message = match purpose {
        EmailTokenPurpose::VerifyEmail => crate::mail::Email {
            to: user.email.clone(),
            subject: "Confirm your Relay address".to_string(),
            body: format!(
                "Someone registered this address with the Relay server at {base}.\n\n\
                 Confirm it by opening:\n\n  {base}/auth/verify?token={}\n\n\
                 The link works once and expires in 24 hours. If this was not \
                 you, ignore this message — nothing happens until the link is \
                 opened.\n",
                token.token
            ),
        },
        EmailTokenPurpose::ResetPassword => crate::mail::Email {
            to: user.email.clone(),
            subject: "Reset your Relay password".to_string(),
            body: format!(
                "Someone asked to reset the password for this address on the \
                 Relay server at {base}.\n\n\
                 Set a new one by opening:\n\n  {base}/auth/reset?token={}\n\n\
                 The link works once and expires in an hour. If this was not \
                 you, ignore this message — your current password still \
                 works and nothing has changed.\n",
                token.token
            ),
        },
    };

    // A provider outage must not fail the surrounding request: the token
    // is already stored, and the user can ask again.
    if let Err(error) = state.mailer.send(&message).await {
        tracing::error!(%error, user = %user.id, "could not send a transactional e-mail");
    }

    Ok(())
}

/// Sends a fresh verification link, if the address is registered.
pub async fn request_verification(state: &AppState, email: &str) -> ServerResult<()> {
    use crate::store::EmailTokenPurpose;

    let Some(stored) = state.store.find_user_by_email(email).await? else {
        return Ok(());
    };
    if stored.profile.email_verified_at.is_some() {
        return Ok(());
    }

    issue_email_token(state, &stored.profile, EmailTokenPurpose::VerifyEmail).await
}

/// Redeems a verification token.
pub async fn confirm_verification(state: &AppState, token: &str) -> ServerResult<()> {
    use crate::store::EmailTokenPurpose;

    let now = Timestamp::now().as_millis();
    let user_id = state
        .store
        .consume_email_token(
            EmailTokenPurpose::VerifyEmail,
            &auth::hash_token(token),
            now,
        )
        .await?
        .ok_or_else(|| {
            ServerError::BadRequest("that link has expired or has already been used".into())
        })?;

    state.store.mark_email_verified(&user_id, now).await
}

/// Sends a password reset link, if the address is registered.
pub async fn request_password_reset(state: &AppState, email: &str) -> ServerResult<()> {
    use crate::store::EmailTokenPurpose;

    let Some(stored) = state.store.find_user_by_email(email).await? else {
        // Deliberately indistinguishable from success. Whether an address
        // has an account here is not something a stranger gets to ask.
        return Ok(());
    };

    issue_email_token(state, &stored.profile, EmailTokenPurpose::ResetPassword).await
}

/// Redeems a reset token and sets a new password.
///
/// Every existing session is dropped. Someone asking for a reset may be
/// doing it *because* an attacker is signed in; leaving that session
/// alive would make the reset pointless.
pub async fn confirm_password_reset(
    state: &AppState,
    token: &str,
    new_password: &str,
) -> ServerResult<()> {
    use crate::store::EmailTokenPurpose;

    // Checked before the token is spent, so a user who fumbles the
    // password rule can try again with the same link.
    let hash = auth::hash_password(new_password)?;

    let now = Timestamp::now().as_millis();
    let user_id = state
        .store
        .consume_email_token(
            EmailTokenPurpose::ResetPassword,
            &auth::hash_token(token),
            now,
        )
        .await?
        .ok_or_else(|| {
            ServerError::BadRequest("that link has expired or has already been used".into())
        })?;

    state.store.set_password_hash(&user_id, &hash).await?;

    // Any other outstanding reset link is now stale.
    state
        .store
        .clear_email_tokens(&user_id, EmailTokenPurpose::ResetPassword)
        .await?;

    let dropped = state.store.delete_sessions_for_user(&user_id).await?;
    tracing::info!(user = %user_id, dropped, "password reset; signed out every device");

    Ok(())
}

// --- invitations ----------------------------------------------------------

/// How long an invitation stays open.
const INVITATION_LIFETIME: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Invites an address to a workspace and e-mails the link.
pub async fn invite_to_workspace(
    state: &AppState,
    actor: &crate::store::Actor,
    workspace_id: &str,
    email: &str,
    role: relay_sync::Role,
) -> ServerResult<crate::store::Invitation> {
    let email = auth::validate_email(email)?;
    let token = auth::new_session_token()?;
    let expires_at = Timestamp::now().as_millis() + INVITATION_LIFETIME.as_millis() as i64;

    let invitation = state
        .store
        .create_invitation(actor, workspace_id, &email, role, &token.hash, expires_at)
        .await?;

    let base = state.config.public_url.trim_end_matches('/');
    let message = crate::mail::Email {
        to: email.clone(),
        subject: "You have been invited to a Relay workspace".to_string(),
        body: format!(
            "You have been invited to collaborate on a Relay workspace at \
             {base}, as {article} {role}.\n\n\
             Accept by opening:\n\n  {base}/invite?token={}\n\n\
             The link works once and expires in seven days. If you were not \
             expecting this, ignore it — nothing happens until the link is \
             opened, and it only works for {email}.\n",
            token.token,
            article = match role {
                relay_sync::Role::Admin | relay_sync::Role::Editor => "an",
                relay_sync::Role::Viewer => "a",
            },
            role = role.as_str(),
        ),
    };

    if let Err(error) = state.mailer.send(&message).await {
        tracing::error!(%error, "could not send an invitation");
    }

    Ok(invitation)
}

/// Accepts an invitation for an already signed-in user.
pub async fn accept_invitation(
    state: &AppState,
    user: &UserProfile,
    token: &str,
) -> ServerResult<relay_sync::Membership> {
    state
        .store
        .accept_invitation(
            &user.id,
            &user.email,
            &auth::hash_token(token),
            Timestamp::now().as_millis(),
        )
        .await
}
