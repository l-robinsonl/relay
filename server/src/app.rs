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
        Self {
            store,
            hub: Hub::new(),
            config: Arc::new(config),
            mailer,
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
        .route("/v1/auth/register", post(routes::register))
        .route("/v1/auth/login", post(routes::login))
        // Authenticated.
        .route("/v1/auth/me", get(routes::me))
        .route("/v1/auth/logout", post(routes::logout))
        .route(
            "/v1/auth/verify/request",
            post(routes::request_verification),
        )
        .route(
            "/v1/auth/verify/confirm",
            post(routes::confirm_verification),
        )
        .route(
            "/v1/auth/password/reset",
            post(routes::request_password_reset),
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
            "/v1/workspaces/{id}/members/{user_id}",
            put(routes::put_member),
        )
        .route(
            "/v1/workspaces/{id}/members/{user_id}",
            delete(routes::delete_member),
        )
        .route("/v1/workspaces/{id}/ws", get(routes::websocket))
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
) -> ServerResult<UserProfile> {
    match state.config.signup_mode {
        SignupMode::Open => {}
        SignupMode::Off => {
            return Err(ServerError::Forbidden);
        }
        SignupMode::Invite => {
            // Invitations are not issued yet, so invite-only means closed
            // rather than silently open. Failing shut is the safe default.
            return Err(ServerError::Forbidden);
        }
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
