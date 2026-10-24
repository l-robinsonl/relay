//! Server-sent events.
//!
//! A GET whose body never ends: the server writes events as they
//! happen and the connection stays open. Everything else about the
//! request — URL, headers, authentication, TLS, proxy — is the same
//! as any other, which is why this takes a built request rather than
//! a URL.
//!
//! The parser is the interesting part. Events arrive as lines and a
//! blank line ends one, so a chunk boundary can land anywhere,
//! including mid-line, and a reader that assumed otherwise would
//! drop or split events under load — exactly when it matters.

use relay_models::ServerEvent;

/// Accumulates bytes and yields whole events.
///
/// Fed arbitrary chunks, including ones that split a line in half.
#[derive(Debug, Default)]
pub struct EventParser {
    /// Bytes seen but not yet forming a complete event.
    buffer: String,
    /// The event being built.
    current: Draft,
}

#[derive(Debug, Default)]
struct Draft {
    event: Option<String>,
    data: Vec<String>,
    id: Option<String>,
    retry_ms: Option<u64>,
    /// Whether any field at all has been seen. A blank line with
    /// nothing before it is a keep-alive, not an empty event.
    touched: bool,
}

impl EventParser {
    /// A fresh parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk, returning every event it completed.
    pub fn push(&mut self, chunk: &str) -> Vec<ServerEvent> {
        self.buffer.push_str(chunk);
        let mut out = Vec::new();

        // Only consume up to the last line break: whatever follows is
        // a partial line and must wait for the next chunk.
        while let Some(index) = self.buffer.find('\n') {
            let line = self.buffer[..index].trim_end_matches('\r').to_string();
            self.buffer.drain(..=index);

            if line.is_empty() {
                if let Some(event) = self.current.finish() {
                    out.push(event);
                }
                continue;
            }

            // A line starting with a colon is a comment, which is how
            // servers keep a connection alive through a proxy.
            if line.starts_with(':') {
                continue;
            }

            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                // A bare field name with no colon is a field with an
                // empty value.
                None => (line.as_str(), ""),
            };

            self.current.touched = true;
            match field {
                "event" => self.current.event = Some(value.to_string()),
                "data" => self.current.data.push(value.to_string()),
                "id" => self.current.id = Some(value.to_string()),
                "retry" => self.current.retry_ms = value.parse().ok(),
                // Unknown fields are ignored by the specification,
                // which is what lets servers add their own.
                _ => {}
            }
        }

        out
    }

    /// Whatever was half-built when the stream ended.
    ///
    /// A server that closes without a trailing blank line has still
    /// sent that event, and dropping it would lose the last thing
    /// said — often the interesting one.
    pub fn finish(&mut self) -> Option<ServerEvent> {
        if !self.buffer.is_empty() {
            let rest = std::mem::take(&mut self.buffer);
            self.push(&format!("{rest}\n"));
        }
        self.current.finish()
    }
}

impl Draft {
    fn finish(&mut self) -> Option<ServerEvent> {
        if !self.touched {
            return None;
        }
        let drafted = std::mem::take(self);
        Some(ServerEvent {
            event: drafted.event.unwrap_or_else(|| "message".to_string()),
            data: drafted.data.join("\n"),
            id: drafted.id,
            retry_ms: drafted.retry_ms,
        })
    }
}

/// Opens a server-sent events stream and reads it until it ends.
///
/// Takes the same `HttpRequestSpec` an ordinary request does, so the
/// URL, headers, authentication, TLS settings and proxy all come
/// along: an events endpoint behind a token is the normal case, not
/// the exception.
///
/// `on_event` is called for each event as it arrives. Returning
/// `false` closes the connection — that is how the interface stops a
/// stream that by design never ends.
pub async fn stream<F>(
    pool: &crate::client::ClientPool,
    spec: &relay_models::HttpRequestSpec,
    mut on_event: F,
) -> Result<(), crate::error::EngineError>
where
    F: FnMut(relay_models::ServerEvent) -> bool + Send,
{
    use futures_util::StreamExt;

    let mut url = crate::build::resolve_url(spec)?;
    crate::build::apply_query_auth(&spec.auth, &mut url);
    let mut headers = crate::build::build_headers(spec)?;
    // Unless the request says otherwise. Somebody testing what a
    // server does with a different Accept should be able to.
    headers
        .entry(reqwest::header::ACCEPT)
        .or_insert_with(|| reqwest::header::HeaderValue::from_static("text/event-stream"));

    let client = pool.get(&spec.settings, url.host_str())?;
    let response = client
        .get(url)
        .headers(headers)
        // No timeout: a stream that has said nothing for a while is
        // working as designed, and the ordinary request timeout would
        // cut it off.
        .timeout(std::time::Duration::from_secs(60 * 60 * 24))
        .send()
        .await
        .map_err(crate::error::EngineError::from)?;

    let status = response.status();
    if !status.is_success() {
        return Err(crate::error::EngineError::Stream(format!(
            "the endpoint answered {status}"
        )));
    }

    let mut parser = EventParser::new();
    let mut body = response.bytes_stream();

    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(crate::error::EngineError::from)?;
        // Lossy: a stream is text by definition, and a malformed byte
        // should not end a connection that is otherwise fine.
        let text = String::from_utf8_lossy(&chunk);

        for event in parser.push(&text) {
            if !on_event(event) {
                return Ok(());
            }
        }
    }

    if let Some(last) = parser.finish() {
        on_event(last);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_event_comes_out_on_the_blank_line() {
        let mut parser = EventParser::new();

        assert!(parser.push("data: hello\n").is_empty(), "emitted too early");
        let events = parser.push("\n");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
        // Omitted names default to `message`.
        assert_eq!(events[0].event, "message");
    }

    #[test]
    fn a_chunk_may_split_a_line_anywhere() {
        // The failure this parser exists to avoid. Chunk boundaries
        // follow the network, not the protocol.
        let mut parser = EventParser::new();

        assert!(parser.push("ev").is_empty());
        assert!(parser.push("ent: pri").is_empty());
        assert!(parser.push("ce\ndata: 4").is_empty());
        let events = parser.push("2\n\n");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "price");
        assert_eq!(events[0].data, "42");
    }

    #[test]
    fn several_events_in_one_chunk_all_arrive() {
        let mut parser = EventParser::new();
        let events = parser.push("data: one\n\ndata: two\n\ndata: three\n\n");

        let payloads: Vec<&str> = events.iter().map(|e| e.data.as_str()).collect();
        assert_eq!(payloads, ["one", "two", "three"]);
    }

    #[test]
    fn repeated_data_lines_join_with_newlines() {
        let mut parser = EventParser::new();
        let events = parser.push("data: {\ndata:   \"a\": 1\ndata: }\n\n");

        assert_eq!(events[0].data, "{\n  \"a\": 1\n}");
    }

    #[test]
    fn comments_are_keepalives_and_not_events() {
        // How a server holds a connection open through a proxy that
        // would otherwise time it out.
        let mut parser = EventParser::new();

        assert!(parser.push(": keep alive\n\n").is_empty());
        assert!(parser.push("\n\n\n").is_empty(), "blank lines made events");
    }

    #[test]
    fn id_and_retry_are_read() {
        let mut parser = EventParser::new();
        let events = parser.push("id: 7\nretry: 5000\ndata: x\n\n");

        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].retry_ms, Some(5000));
    }

    #[test]
    fn carriage_returns_do_not_end_up_in_the_data() {
        // Plenty of servers send CRLF.
        let mut parser = EventParser::new();
        let events = parser.push("data: hello\r\n\r\n");

        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn a_stream_ending_without_a_blank_line_still_yields_its_last_event() {
        let mut parser = EventParser::new();
        assert!(parser.push("data: last words").is_empty());

        let final_event = parser.finish().expect("the last event was dropped");
        assert_eq!(final_event.data, "last words");
    }

    #[test]
    fn finishing_an_empty_stream_yields_nothing() {
        assert!(EventParser::new().finish().is_none());
    }
}
