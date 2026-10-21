//! Reading response bodies without letting them exhaust memory.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures_util::StreamExt;
use relay_models::{KeyValue, ResponseBody, ResponseCookie, Timestamp};
use tokio::io::AsyncWriteExt;

use crate::error::EngineError;

/// Bodies at or below this size stay in memory and travel inline to the
/// webview. Above it they are written to a temporary file and referenced.
///
/// 8 MiB comfortably covers the JSON payloads an API client sees all day,
/// while keeping a stray large download from being copied into the webview,
/// base64-encoded, and held by both processes at once.
pub const INLINE_BODY_LIMIT: u64 = 8 * 1024 * 1024;

/// The default hard ceiling on a response body.
///
/// Past this the body is discarded and the response reported as truncated,
/// rather than filling the user's disk. Raiseable per request.
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;

/// How many leading bytes of a spooled body are kept for preview.
const PREVIEW_BYTES: usize = 64 * 1024;

/// Where a streamed body should be written if it grows too large to inline.
pub struct SpoolTarget {
    /// Directory for temporary body files.
    pub dir: std::path::PathBuf,
    /// A unique stem for this execution.
    pub stem: String,
}

/// Reads a response body, spilling to disk past [`INLINE_BODY_LIMIT`].
///
/// `on_progress` is called as bytes arrive so the UI can show a live counter;
/// the body is never accumulated beyond the inline limit in memory.
pub async fn read_body<F>(
    response: reqwest::Response,
    max_bytes: u64,
    spool: Option<&SpoolTarget>,
    content_type: Option<&str>,
    mut on_progress: F,
) -> Result<(ResponseBody, u64), EngineError>
where
    F: FnMut(u64),
{
    let mut stream = response.bytes_stream();
    let mut buffered: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut spill: Option<(tokio::fs::File, std::path::PathBuf)> = None;

    while let Some(chunk) = stream.next().await {
        let chunk: Bytes = chunk.map_err(EngineError::Transport)?;
        total += chunk.len() as u64;

        if total > max_bytes {
            // Stop reading immediately; the connection is dropped with the
            // stream, so the remote stops sending too.
            if let Some((mut file, path)) = spill.take() {
                let _ = file.shutdown().await;
                let _ = tokio::fs::remove_file(&path).await;
            }
            return Ok((
                ResponseBody::Truncated {
                    size: total,
                    reason: format!(
                        "response exceeded the {max_bytes} byte limit and was discarded"
                    ),
                },
                total,
            ));
        }

        if let Some((file, _)) = spill.as_mut() {
            file.write_all(&chunk).await?;
        } else if let Some(target) = spool.filter(|_| total > INLINE_BODY_LIMIT) {
            // Crossed the inline threshold: move what we have to disk and
            // keep streaming there.
            tokio::fs::create_dir_all(&target.dir).await?;
            let path = target.dir.join(format!("{}.body", target.stem));
            let mut file = tokio::fs::File::create(&path).await?;
            file.write_all(&buffered).await?;
            file.write_all(&chunk).await?;
            buffered.truncate(PREVIEW_BYTES);
            spill = Some((file, path));
        } else {
            buffered.extend_from_slice(&chunk);
        }

        on_progress(total);
    }

    if let Some((mut file, path)) = spill {
        file.flush().await?;
        file.shutdown().await?;
        let preview = decode_text(&buffered, content_type)
            .unwrap_or_else(|| format!("<{total} bytes of binary data>"));
        return Ok((
            ResponseBody::File {
                path: path.to_string_lossy().into_owned(),
                size: total,
                preview,
            },
            total,
        ));
    }

    if buffered.is_empty() {
        return Ok((ResponseBody::Empty, 0));
    }

    let body = match decode_text(&buffered, content_type) {
        Some(text) => ResponseBody::Text { text },
        None => ResponseBody::Binary {
            base64: BASE64.encode(&buffered),
        },
    };

    Ok((body, total))
}

/// Decodes bytes as text when they plausibly are text.
///
/// The charset from `Content-Type` is honoured; otherwise UTF-8 is tried and,
/// failing that, the bytes are treated as binary. Guessing an encoding and
/// silently producing mojibake would be worse than showing a hex view.
pub fn decode_text(bytes: &[u8], content_type: Option<&str>) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }

    let charset = content_type
        .and_then(|ct| {
            ct.split(';')
                .skip(1)
                .map(str::trim)
                .find_map(|p| p.strip_prefix("charset="))
        })
        .map(|c| c.trim_matches('"').to_ascii_lowercase());

    if let Some(label) = charset.as_deref()
        && let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes())
    {
        let (text, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return Some(text.into_owned());
        }
    }

    // No usable charset: accept it as text only if it is valid UTF-8 and does
    // not look like a binary blob.
    match std::str::from_utf8(bytes) {
        Ok(text) if !looks_binary(bytes) => Some(text.to_owned()),
        _ => {
            if is_textual_content_type(content_type) {
                let (text, _, _) = encoding_rs::UTF_8.decode(bytes);
                Some(text.into_owned())
            } else {
                None
            }
        }
    }
}

/// True when the bytes contain NUL, which no sane text payload does.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

/// True when the content type announces text, JSON, XML or similar.
pub fn is_textual_content_type(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("x-www-form-urlencoded")
        || ct.contains("graphql")
        || ct.contains("csv")
        || ct.contains("yaml")
}

/// Converts response headers into ordered key/value pairs.
pub fn collect_headers(headers: &reqwest::header::HeaderMap) -> Vec<KeyValue> {
    headers
        .iter()
        .map(|(name, value)| {
            KeyValue::new(
                name.as_str(),
                value.to_str().unwrap_or("<non-UTF-8 header value>"),
            )
        })
        .collect()
}

/// Parses `Set-Cookie` headers into structured cookies.
pub fn collect_cookies(headers: &reqwest::header::HeaderMap) -> Vec<ResponseCookie> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(parse_set_cookie)
        .collect()
}

fn parse_set_cookie(raw: &str) -> ResponseCookie {
    let mut parts = raw.split(';');
    let (name, value) = parts
        .next()
        .and_then(|p| p.split_once('='))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .unwrap_or_else(|| (raw.trim().to_owned(), String::new()));

    let mut cookie = ResponseCookie {
        name,
        value,
        domain: None,
        path: None,
        expires: None,
        http_only: false,
        secure: false,
        same_site: None,
    };

    for attr in parts {
        let attr = attr.trim();
        let (key, val) = match attr.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_owned()),
            None => (attr.to_ascii_lowercase(), String::new()),
        };
        match key.as_str() {
            "domain" => cookie.domain = Some(val),
            "path" => cookie.path = Some(val),
            "httponly" => cookie.http_only = true,
            "secure" => cookie.secure = true,
            "samesite" => cookie.same_site = Some(val),
            "max-age" => {
                if let Ok(secs) = val.parse::<i64>() {
                    cookie.expires = Some(Timestamp(Timestamp::now().as_millis() + secs * 1000));
                }
            }
            _ => {}
        }
    }

    cookie
}

/// The essential media type, with parameters stripped.
pub fn essence(content_type: Option<&str>) -> Option<String> {
    content_type.map(|ct| {
        ct.split(';')
            .next()
            .unwrap_or(ct)
            .trim()
            .to_ascii_lowercase()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_text_is_decoded() {
        assert_eq!(
            decode_text("hello ✓".as_bytes(), Some("text/plain")).as_deref(),
            Some("hello ✓")
        );
    }

    #[test]
    fn a_declared_charset_is_honoured() {
        // 0xE9 is 'é' in latin-1 but invalid UTF-8.
        let bytes = [0x63, 0x61, 0x66, 0xE9];
        let decoded = decode_text(&bytes, Some("text/plain; charset=iso-8859-1"));
        assert_eq!(decoded.as_deref(), Some("café"));
    }

    #[test]
    fn binary_payloads_are_not_decoded_as_text() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x00, 0x1A];
        assert!(decode_text(&png, Some("image/png")).is_none());
    }

    #[test]
    fn json_is_treated_as_text_even_with_odd_bytes() {
        // A JSON content type is trusted enough to decode lossily rather than
        // show the user a hex dump of their API response.
        let bytes = [b'{', 0xFF, b'}'];
        assert!(decode_text(&bytes, Some("application/json")).is_some());
    }

    #[test]
    fn empty_bodies_decode_to_empty_strings() {
        assert_eq!(decode_text(&[], None).as_deref(), Some(""));
    }

    #[test]
    fn textual_content_types_are_recognised() {
        for ct in [
            "text/html",
            "application/json",
            "application/vnd.api+json",
            "application/xml",
            "text/csv",
        ] {
            assert!(is_textual_content_type(Some(ct)), "{ct}");
        }
        for ct in ["image/png", "application/octet-stream", "video/mp4"] {
            assert!(!is_textual_content_type(Some(ct)), "{ct}");
        }
    }

    #[test]
    fn essence_strips_parameters() {
        assert_eq!(
            essence(Some("application/json; charset=utf-8")).as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn set_cookie_attributes_are_parsed() {
        let c = parse_set_cookie(
            "sid=abc123; Domain=example.com; Path=/; HttpOnly; Secure; SameSite=Lax",
        );
        assert_eq!(c.name, "sid");
        assert_eq!(c.value, "abc123");
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.path.as_deref(), Some("/"));
        assert!(c.http_only);
        assert!(c.secure);
        assert_eq!(c.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn a_cookie_without_attributes_still_parses() {
        let c = parse_set_cookie("token=xyz");
        assert_eq!(c.name, "token");
        assert_eq!(c.value, "xyz");
        assert!(!c.secure);
    }
}
