//! The two pages a person actually sees in a browser.
//!
//! Everything else Relay serves is JSON for the desktop client. These
//! exist because a verification e-mail contains a link, and a link has
//! to lead somewhere.
//!
//! No external stylesheet, font or script. Partly because a page this
//! small does not need one, and partly because the reset page has a
//! token in its URL: any external request from it would carry that
//! token to a third party in the `Referer` header.

use axum::Form;
use axum::extract::{Query, State};
use axum::response::Html;

use crate::app::AppState;

/// The token carried in an e-mailed link.
#[derive(Debug, serde::Deserialize)]
pub struct LinkToken {
    /// The token as it appeared in the link.
    #[serde(default)]
    pub token: String,
}

/// A submitted new password.
#[derive(Debug, serde::Deserialize)]
pub struct ResetForm {
    /// The token, carried through the form.
    #[serde(default)]
    pub token: String,
    /// The new password.
    #[serde(default)]
    pub password: String,
    /// The same password again.
    #[serde(default)]
    pub confirm: String,
}

/// `GET /auth/verify?token=…`
///
/// Confirms on sight. A mail scanner that follows the link does consume
/// the token, but a scanner inside the recipient's own mail path having
/// received the message is exactly what verification tests, so the
/// outcome is right even when the click is not a human's.
pub async fn verify(State(state): State<AppState>, Query(query): Query<LinkToken>) -> Html<String> {
    match crate::app::confirm_verification(&state, &query.token).await {
        Ok(()) => Html(page(
            "Address confirmed",
            "You can close this tab and go back to Relay.",
            None,
        )),
        // Deliberately one message for expired, spent and forged: the
        // honest answer is the same, and distinguishing them would tell
        // a stranger which tokens once existed.
        Err(_) => Html(page(
            "That link no longer works",
            "It has already been used, or it expired. Ask Relay to send \
             another and use the newest message.",
            None,
        )),
    }
}

/// `GET /invite?token=…`
///
/// Describes the invitation and nothing else. Accepting needs an
/// account, and signing in happens in Relay rather than here: a page
/// that asked for a password would be indistinguishable from a phishing
/// page sent to the same address.
pub async fn invite(State(state): State<AppState>, Query(query): Query<LinkToken>) -> Html<String> {
    let offer = state
        .store
        .invitation_offer(
            &crate::auth::hash_token(&query.token),
            relay_models::Timestamp::now().as_millis(),
        )
        .await;

    match offer {
        Ok(Some(offer)) => Html(page(
            "You have been invited",
            "",
            Some(&format!(
                "<p>You have been invited to the workspace \
                 <strong>{}</strong> as {}, at <strong>{}</strong>.</p>\
                 <p>Open Relay, add this server under <em>Sync</em>, and sign \
                 in — or create an account with that address. The invitation \
                 is applied when you do.</p>\
                 <p class=\"muted\">Relay never asks for your password on a \
                 web page. If a page claiming to be Relay does, it is not us.</p>",
                escape(&offer.workspace_name),
                escape(offer.role.as_str()),
                escape(&offer.email),
            )),
        )),
        Ok(None) => Html(page(
            "That invitation no longer works",
            "It has been accepted already, or it expired. Ask whoever invited \
             you to send another.",
            None,
        )),
        Err(_) => Html(page(
            "Something went wrong",
            "The invitation could not be read. Try the link again.",
            None,
        )),
    }
}

/// `GET /auth/reset?token=…`
///
/// Shows a form and changes nothing. A GET that reset a password would
/// be triggered by every link scanner that touched the message.
pub async fn reset_form(Query(query): Query<LinkToken>) -> Html<String> {
    Html(page(
        "Choose a new password",
        "",
        Some(&form_body(&query.token, None)),
    ))
}

/// `POST /auth/reset`
pub async fn reset_submit(
    State(state): State<AppState>,
    Form(form): Form<ResetForm>,
) -> Html<String> {
    if form.password != form.confirm {
        return Html(page(
            "Choose a new password",
            "",
            Some(&form_body(&form.token, Some("Those two did not match."))),
        ));
    }

    match crate::app::confirm_password_reset(&state, &form.token, &form.password).await {
        Ok(()) => Html(page(
            "Password changed",
            "Every device has been signed out. Sign in again with the new \
             password.",
            None,
        )),
        Err(crate::error::ServerError::BadRequest(message)) => {
            Html(page("That link no longer works", &message, None))
        }
        Err(_) => Html(page(
            "Something went wrong",
            "The password was not changed. Try the link again.",
            None,
        )),
    }
}

/// The reset form, with an optional complaint above it.
fn form_body(token: &str, problem: Option<&str>) -> String {
    let notice = problem
        .map(|p| format!("<p class=\"problem\">{}</p>", escape(p)))
        .unwrap_or_default();

    format!(
        "{notice}\
         <form method=\"post\" action=\"/auth/reset\">\
           <input type=\"hidden\" name=\"token\" value=\"{}\">\
           <label for=\"password\">New password</label>\
           <input id=\"password\" name=\"password\" type=\"password\" \
                  autocomplete=\"new-password\" required minlength=\"12\" autofocus>\
           <label for=\"confirm\">Again, to be sure</label>\
           <input id=\"confirm\" name=\"confirm\" type=\"password\" \
                  autocomplete=\"new-password\" required minlength=\"12\">\
           <button type=\"submit\">Change it</button>\
         </form>\
         <p class=\"muted\">Twelve characters or more. A phrase you can \
          remember beats a short tangle you cannot.</p>",
        escape(token)
    )
}

/// A submitted signup.
#[derive(Debug, serde::Deserialize)]
pub struct SignupForm {
    /// The address to register.
    #[serde(default)]
    pub email: String,
    /// What to call them. Optional; the address is used otherwise.
    #[serde(default)]
    pub name: String,
    /// The chosen password.
    #[serde(default)]
    pub password: String,
    /// The same password again.
    #[serde(default)]
    pub confirm: String,
}

/// `GET /signup`
///
/// Registration from a browser, for people who have not installed
/// anything yet. The desktop application can register on its own, but
/// that is no use to someone reading about Relay on a web page and
/// wanting an account before they commit to a download.
pub async fn signup_form(State(state): State<AppState>) -> Html<String> {
    Html(match state.config.signup_mode {
        crate::config::SignupMode::Open => page(
            "Create an account",
            "This gives you somewhere to keep your workspace so it follows you \
             between machines. Relay itself works without one.",
            Some(&signup_body(None, "", "")),
        ),
        crate::config::SignupMode::Invite => page(
            "This server is invitation only",
            "Ask whoever runs it to send you an invitation. The link in it \
             will let you set a password and join.",
            None,
        ),
        crate::config::SignupMode::Off => page(
            "This server is not taking new accounts",
            "Registration is closed here. Relay works perfectly well without \
             an account, and you can always run a server of your own.",
            None,
        ),
    })
}

/// `POST /signup`
pub async fn signup_submit(
    State(state): State<AppState>,
    Form(form): Form<SignupForm>,
) -> Html<String> {
    let again = |problem: &str| {
        Html(page(
            "Create an account",
            "",
            Some(&signup_body(Some(problem), &form.email, &form.name)),
        ))
    };

    if form.password != form.confirm {
        return again("Those two passwords did not match.");
    }

    let display_name = if form.name.trim().is_empty() {
        form.email.split('@').next().unwrap_or("User").to_string()
    } else {
        form.name.clone()
    };

    match crate::app::register_user(&state, &form.email, &form.password, &display_name, None).await
    {
        Ok(_) => Html(page(
            "Account created",
            "",
            Some(&format!(
                "<p>Now install Relay, open <b>Sync</b>, and give it this address:</p>\
                 <p class=\"address\">{}</p>\
                 <p class=\"muted\">Sign in there with the address and password you \
                  just chose. Everything you make stays on your own machine; the \
                  server holds a copy so it reaches your other ones.</p>\
                 <p class=\"muted\">If this server asks you to confirm your address \
                  first, the message is on its way.</p>",
                escape(state.config.public_url.trim_end_matches('/'))
            )),
        )),
        // Its own wording is the useful one — "that address is already
        // registered", "password is too short" — and none of it
        // describes the deployment.
        Err(crate::error::ServerError::Conflict(message))
        | Err(crate::error::ServerError::BadRequest(message)) => again(&message),
        Err(crate::error::ServerError::Forbidden) => Html(page(
            "This server is not taking new accounts",
            "Registration was closed while you were filling that in.",
            None,
        )),
        Err(_) => again("Something went wrong creating the account. Try again."),
    }
}

/// The signup form, with an optional complaint above it.
fn signup_body(problem: Option<&str>, email: &str, name: &str) -> String {
    let notice = problem
        .map(|p| format!("<p class=\"problem\">{}</p>", escape(p)))
        .unwrap_or_default();

    format!(
        "{notice}\
         <form method=\"post\" action=\"/signup\">\
           <label for=\"email\">E-mail address</label>\
           <input id=\"email\" name=\"email\" type=\"email\" value=\"{}\" \
                  autocomplete=\"email\" required autofocus>\
           <label for=\"name\">Your name <span class=\"opt\">(optional)</span></label>\
           <input id=\"name\" name=\"name\" type=\"text\" value=\"{}\" \
                  autocomplete=\"name\">\
           <label for=\"password\">Password</label>\
           <input id=\"password\" name=\"password\" type=\"password\" \
                  autocomplete=\"new-password\" required minlength=\"12\">\
           <label for=\"confirm\">Again, to be sure</label>\
           <input id=\"confirm\" name=\"confirm\" type=\"password\" \
                  autocomplete=\"new-password\" required minlength=\"12\">\
           <button type=\"submit\">Create the account</button>\
         </form>\
         <p class=\"muted\">Twelve characters or more. A phrase you can remember \
          beats a short tangle you cannot.</p>",
        escape(email),
        escape(name)
    )
}

/// `GET /billing/done`
///
/// Where Stripe sends the browser after a successful checkout. It
/// exists because the checkout session names it as `success_url`: a
/// person who has just paid and lands on a 404 has no way of knowing
/// whether the money went anywhere.
///
/// It deliberately claims nothing about the subscription. The
/// authority on that is the webhook, which may not have arrived yet,
/// and this page has no session to look anything up with.
pub async fn billing_done() -> Html<String> {
    Html(page(
        "Thank you",
        "Your subscription is being set up. Go back to Relay — the seats \
         will appear in the Sync dialog within a few moments.",
        None,
    ))
}

/// `GET /billing/cancelled`
///
/// The `cancel_url`. Reached by pressing back out of Stripe's page,
/// which is not an error and should not read like one.
pub async fn billing_cancelled() -> Html<String> {
    Html(page(
        "Nothing was charged",
        "You left before paying, so no subscription was started. You can \
         close this tab; Relay is unchanged.",
        None,
    ))
}

/// Wraps content in the one page template.
fn page(heading: &str, detail: &str, body: Option<&str>) -> String {
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", escape(detail))
    };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <!-- Keeps the token out of any outbound Referer. -->\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <title>{heading} · Relay</title><style>\
         :root{{color-scheme:light dark;--bg:#f8fafc;--fg:#0f172a;--muted:#64748b;\
         --line:#e2e8f0;--accent:#2563eb;--danger:#b91c1c}}\
         @media(prefers-color-scheme:dark){{:root{{--bg:#0b1120;--fg:#e2e8f0;\
         --muted:#94a3b8;--line:#1e293b;--accent:#60a5fa;--danger:#f87171}}}}\
         *{{box-sizing:border-box}}\
         body{{font-family:system-ui,-apple-system,Segoe UI,sans-serif;\
         background:var(--bg);color:var(--fg);margin:0;min-height:100vh;\
         display:grid;place-items:center;padding:1.5rem;line-height:1.55}}\
         main{{width:100%;max-width:26rem}}\
         h1{{font-size:1.25rem;margin:0 0 .5rem}}\
         p{{color:var(--muted);margin:0 0 1rem;font-size:.925rem}}\
         .problem{{color:var(--danger)}}\
         .muted{{font-size:.825rem;margin-top:1rem}}\
         label{{display:block;font-size:.8rem;font-weight:600;margin:.875rem 0 .3rem}}\
         input{{width:100%;padding:.55rem .7rem;border:1px solid var(--line);\
         border-radius:.375rem;background:var(--bg);color:var(--fg);font-size:.925rem}}\
         input:focus{{outline:2px solid var(--accent);outline-offset:1px}}\
         button{{margin-top:1.25rem;width:100%;padding:.6rem;border:0;\
         border-radius:.375rem;background:var(--accent);color:#fff;\
         font-size:.925rem;font-weight:600;cursor:pointer}}\
         .mark{{font-weight:700;letter-spacing:-.02em;color:var(--muted);\
         font-size:.8rem;margin-bottom:1.25rem}}\
         .opt{{font-weight:400;color:var(--muted)}}\
         .muted{{font-size:.825rem}}\
         .address{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;\
         font-size:.9rem;background:var(--line);color:var(--fg);padding:.6rem .7rem;\
         border-radius:.375rem;word-break:break-all;margin:.5rem 0 1rem}}\
         </style></head><body><main>\
         <div class=\"mark\">RELAY</div><h1>{}</h1>{detail}{}</main></body></html>",
        escape(heading),
        body.unwrap_or("")
    )
}

/// Escapes text for HTML. Everything interpolated above goes through it.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_cannot_break_out_of_the_form_field() {
        // The token comes straight from a URL anyone can craft.
        let hostile = "\"><script>alert(1)</script>";
        let html = form_body(hostile, None);

        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&quot;&gt;"));
    }

    #[test]
    fn a_complaint_is_escaped_too() {
        let html = form_body("t", Some("<b>no</b>"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn the_page_asks_browsers_not_to_leak_the_token() {
        // The reset URL carries a token; a Referer header on any
        // outbound request would hand it to a third party.
        let html = page("Choose a new password", "", Some(&form_body("tok", None)));
        assert!(html.contains("name=\"referrer\" content=\"no-referrer\""));
    }

    #[test]
    fn the_form_posts_rather_than_gets() {
        // A GET that reset a password would fire on every link scanner
        // that touched the message.
        let html = form_body("tok", None);
        assert!(html.contains("method=\"post\""));
        assert!(!html.contains("method=\"get\""));
    }
}
