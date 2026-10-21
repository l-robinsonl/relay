//! The loopback listener that catches an OAuth redirect.
//!
//! The authorization code grant sends the user to a browser, which comes
//! back to a `redirect_uri`. For a desktop application that URI is a
//! loopback address, as RFC 8252 §7.3 recommends: a custom scheme can be
//! claimed by any other program on the machine, whereas a port on
//! 127.0.0.1 can only be bound by one process at a time.
//!
//! The listener accepts exactly one request and shuts down, so nothing
//! is left open after a sign-in.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::error::EngineError;

/// What came back from the authorization server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectResult {
    /// The authorization code, when the user approved.
    pub code: String,
    /// The state parameter, to be checked against the one sent.
    pub state: String,
}

/// A listener waiting for one redirect.
pub struct RedirectListener {
    listener: TcpListener,
    port: u16,
}

impl std::fmt::Debug for RedirectListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedirectListener")
            .field("port", &self.port)
            .finish()
    }
}

impl RedirectListener {
    /// Binds a loopback port, letting the OS choose one unless given.
    ///
    /// A port of zero asks for any free port, which avoids colliding
    /// with whatever else the developer is running. Some authorization
    /// servers insist on a registered redirect URI with a fixed port, so
    /// one can be named.
    pub async fn bind(port: u16) -> Result<Self, EngineError> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
            EngineError::OAuth(format!(
                "could not listen on 127.0.0.1:{port} for the OAuth redirect: {e}"
            ))
        })?;

        let port = listener
            .local_addr()
            .map_err(|e| EngineError::OAuth(e.to_string()))?
            .port();

        Ok(Self { listener, port })
    }

    /// The port actually bound.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to send to the authorization server.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Waits for the browser to come back.
    ///
    /// `expected_state` is compared against what arrives. A mismatch is
    /// refused: without that check, anyone able to make the user's
    /// browser hit this port could inject an authorization code of their
    /// choosing and have Relay exchange it.
    pub async fn wait(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<RedirectResult, EngineError> {
        let accept = async {
            loop {
                let (stream, _) = self.listener.accept().await.map_err(|e| {
                    EngineError::OAuth(format!("the redirect listener failed: {e}"))
                })?;

                match handle(stream, expected_state).await {
                    // A browser may request /favicon.ico alongside the
                    // callback; keep listening rather than giving up.
                    Ok(None) => continue,
                    Ok(Some(result)) => return Ok(result),
                    Err(error) => return Err(error),
                }
            }
        };

        tokio::time::timeout(timeout, accept).await.map_err(|_| {
            EngineError::OAuth(format!(
                "no response from the browser within {} seconds",
                timeout.as_secs()
            ))
        })?
    }
}

/// Reads one request and answers it. `None` means "not the callback".
async fn handle(
    stream: tokio::net::TcpStream,
    expected_state: &str,
) -> Result<Option<RedirectResult>, EngineError> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();

    reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| EngineError::OAuth(format!("could not read the redirect: {e}")))?;

    // "GET /callback?code=…&state=… HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");

    // A relative target needs a base before it can be parsed.
    let url = url::Url::parse("http://127.0.0.1")
        .and_then(|base| base.join(target))
        .map_err(|e| EngineError::OAuth(format!("the redirect was malformed: {e}")))?;

    if !url.path().starts_with("/callback") {
        // Something else — a favicon request, usually.
        let _ = respond(reader.into_inner(), 404, "Not found", "").await;
        return Ok(None);
    }

    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

    if let Some(error) = query.get("error") {
        let description = query
            .get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();

        let _ = respond(
            reader.into_inner(),
            400,
            "Authorization failed",
            &format!("{error}{description}"),
        )
        .await;

        return Err(EngineError::OAuth(format!(
            "the authorization server refused: {error}{description}"
        )));
    }

    let state = query.get("state").cloned().unwrap_or_default();

    // Compared before the code is touched. A forged redirect carrying
    // somebody else's code must not be exchanged.
    if state != expected_state {
        let _ = respond(
            reader.into_inner(),
            400,
            "Authorization failed",
            "The state parameter did not match. This sign-in was not started by Relay.",
        )
        .await;

        return Err(EngineError::OAuth(
            "the redirect carried the wrong state — it did not come from the sign-in \
             Relay started, so it was refused"
                .into(),
        ));
    }

    let Some(code) = query.get("code").filter(|c| !c.is_empty()).cloned() else {
        let _ = respond(
            reader.into_inner(),
            400,
            "Authorization failed",
            "No authorization code was returned.",
        )
        .await;

        return Err(EngineError::OAuth(
            "the redirect carried no authorization code".into(),
        ));
    };

    let _ = respond(
        reader.into_inner(),
        200,
        "Signed in",
        "You can close this tab and return to Relay.",
    )
    .await;

    Ok(Some(RedirectResult { code, state }))
}

/// Writes a small HTML page back to the browser.
async fn respond(
    mut stream: tokio::net::TcpStream,
    status: u16,
    heading: &str,
    detail: &str,
) -> Result<(), std::io::Error> {
    // Escaped: `detail` can contain an error description from the
    // authorization server, which is attacker-influenced text.
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Relay</title>\
         <style>body{{font-family:system-ui,sans-serif;background:#111827;color:#e5e7eb;\
         display:grid;place-items:center;height:100vh;margin:0}}\
         div{{text-align:center;max-width:32rem;padding:2rem}}\
         h1{{font-size:1.1rem;font-weight:600;margin:0 0 .5rem}}\
         p{{color:#9ca3af;font-size:.9rem;margin:0}}</style></head>\
         <body><div><h1>{}</h1><p>{}</p></div></body></html>",
        escape(heading),
        escape(detail),
    );

    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn visit(port: u16, target: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();

        let mut body = Vec::new();
        use tokio::io::AsyncReadExt;
        let _ = stream.read_to_end(&mut body).await;
        String::from_utf8_lossy(&body).into_owned()
    }

    #[tokio::test]
    async fn a_callback_with_the_right_state_yields_its_code() {
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();
        assert!(listener.redirect_uri().contains("127.0.0.1"));

        let waiting =
            tokio::spawn(async move { listener.wait("the-state", Duration::from_secs(5)).await });

        let page = visit(port, "/callback?code=the-code&state=the-state").await;
        assert!(page.contains("200 OK"));
        assert!(page.contains("close this tab"));

        let result = waiting.await.unwrap().unwrap();
        assert_eq!(result.code, "the-code");
        assert_eq!(result.state, "the-state");
    }

    #[tokio::test]
    async fn a_mismatched_state_is_refused() {
        // Without this check anyone who can make the browser hit this
        // port could have Relay exchange a code of their choosing.
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();

        let waiting =
            tokio::spawn(async move { listener.wait("expected", Duration::from_secs(5)).await });

        let page = visit(port, "/callback?code=injected&state=forged").await;
        assert!(page.contains("400"));

        let error = waiting.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("wrong state"), "{error}");
    }

    #[tokio::test]
    async fn an_authorization_error_is_reported_with_its_description() {
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();

        let waiting = tokio::spawn(async move { listener.wait("s", Duration::from_secs(5)).await });

        visit(
            port,
            "/callback?error=access_denied&error_description=User%20said%20no&state=s",
        )
        .await;

        let error = waiting.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("access_denied"), "{error}");
        assert!(error.contains("User said no"), "{error}");
    }

    #[tokio::test]
    async fn a_callback_with_no_code_is_refused() {
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();

        let waiting = tokio::spawn(async move { listener.wait("s", Duration::from_secs(5)).await });

        visit(port, "/callback?state=s").await;

        let error = waiting.await.unwrap().unwrap_err().to_string();
        assert!(error.contains("no authorization code"), "{error}");
    }

    #[tokio::test]
    async fn an_unrelated_request_does_not_end_the_wait() {
        // Browsers ask for /favicon.ico; giving up on it would make
        // sign-in fail intermittently and inexplicably.
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();

        let waiting = tokio::spawn(async move { listener.wait("s", Duration::from_secs(5)).await });

        let page = visit(port, "/favicon.ico").await;
        assert!(page.contains("404"));

        let result = visit(port, "/callback?code=c&state=s").await;
        assert!(result.contains("200 OK"));

        assert_eq!(waiting.await.unwrap().unwrap().code, "c");
    }

    #[tokio::test]
    async fn waiting_gives_up_rather_than_hanging_forever() {
        let listener = RedirectListener::bind(0).await.unwrap();
        let error = listener
            .wait("s", Duration::from_millis(200))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("no response from the browser"), "{error}");
    }

    #[tokio::test]
    async fn an_error_description_cannot_inject_markup() {
        // error_description comes from the authorization server and is
        // rendered into a page the user's browser displays.
        let listener = RedirectListener::bind(0).await.unwrap();
        let port = listener.port();

        let waiting = tokio::spawn(async move { listener.wait("s", Duration::from_secs(5)).await });

        let page = visit(
            port,
            "/callback?error=bad&error_description=%3Cscript%3Ealert(1)%3C/script%3E&state=s",
        )
        .await;

        assert!(!page.contains("<script>"), "markup was injected: {page}");
        assert!(page.contains("&lt;script&gt;"));

        let _ = waiting.await;
    }

    #[tokio::test]
    async fn a_named_port_is_honoured_for_servers_that_require_one() {
        let listener = RedirectListener::bind(0).await.unwrap();
        let chosen = listener.port();
        drop(listener);

        let again = RedirectListener::bind(chosen).await.unwrap();
        assert_eq!(again.port(), chosen);
        assert_eq!(
            again.redirect_uri(),
            format!("http://127.0.0.1:{chosen}/callback")
        );
    }
}
