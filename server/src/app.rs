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
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("backend", &self.store.backend())
            .finish()
    }
}

impl AppState {
    /// Builds the state.
    pub fn new(store: Arc<dyn SyncStore>, config: Config) -> Self {
        Self {
            store,
            hub: Hub::new(),
            config: Arc::new(config),
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

    state
        .store
        .create_user(NewUser {
            email,
            password_hash: hash,
            display_name: display_name.trim().to_string(),
            is_server_admin: false,
        })
        .await
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
