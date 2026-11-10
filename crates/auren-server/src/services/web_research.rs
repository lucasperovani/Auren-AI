//! Web research: search, fetch, and read public pages.
//!
//! Design goals:
//!
//! - **No API key required.** Search uses DuckDuckGo's HTML endpoint, which is
//!   publicly reachable and needs no credentials. If a keyed provider is
//!   configured later it can be layered on top without changing the tools.
//! - **Text, not markup.** Pages are converted to readable text (scripts,
//!   styles, and markup removed) so the model spends its context on content.
//! - **SSRF-safe, including redirects.** Every hop is validated, so a public
//!   URL cannot bounce into the cloud metadata service or a private network.
//! - **No polling loops.** A fetch is bounded by time, size, and redirect
//!   count; nothing retries in a hot loop.

use serde_json::{json, Value};

use crate::{error::ApiError, state::AppState};

/// Hard cap on downloaded bytes. Retrieved content is untrusted input.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Cap on the text handed to the model, in characters.
const MAX_TEXT_CHARS: usize = 40_000;
/// A request may not take longer than this, redirects included.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Maximum redirect hops. Each hop is re-validated.
const MAX_REDIRECTS: usize = 5;

/// A search hit.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Build an HTTP client that validates every redirect target.
///
/// `reqwest` follows redirects by default with no re-validation, so a public
/// URL replying `302 -> http://169.254.169.254/` would be followed. The policy
/// here re-runs the SSRF check on each hop and caps the chain length.
fn safe_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.stop();
            }
            let target = attempt.url();
            if validate_public_url(target.as_str()).is_err() {
                // Never follow a redirect into an internal destination.
                return attempt.stop();
            }
            attempt.follow()
        }))
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Auren-AI/0.1 (+https://github.com/auren-ai)")
        .build()
        .map_err(|_| ApiError::ServiceUnavailable)
}

/// Reject non-http(s) URLs and private, loopback, or link-local destinations.
///
/// This runs for the requested URL and for every redirect hop.
pub fn validate_public_url(url: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(url).map_err(|_| ApiError::Unprocessable("invalid url".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::Unprocessable("url must be http(s)".into()));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host.is_empty() {
        return Err(ApiError::Unprocessable("url has no host".into()));
    }
    let lowered = host.to_ascii_lowercase();
    if lowered == "localhost"
        || lowered.ends_with(".localhost")
        || lowered.ends_with(".internal")
        || lowered.ends_with(".local")
        || lowered.ends_with(".home.arpa")
    {
        return Err(ApiError::Forbidden("access to internal hosts is not allowed".into()));
    }
    // Metadata endpoints are reachable by name as well as by address.
    if lowered == "metadata.google.internal" || lowered == "metadata" {
        return Err(ApiError::Forbidden("access to cloud metadata is not allowed".into()));
    }

    // IPv6 literals arrive bracketed (`[::1]`); strip them before parsing.
    let ip_candidate = lowered.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = ip_candidate.parse::<std::net::IpAddr>() {
        let internal = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_unspecified()
                    || v4.is_private()
                    || v4.is_link_local()
                    // Carrier-grade NAT range, also not a public destination.
                    || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // Unique local addresses (fc00::/7).
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    // Link-local (fe80::/10).
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // IPv4-mapped addresses (::ffff:127.0.0.1) reach IPv4.
                    || v6.to_ipv4_mapped().is_some_and(|v4| {
                        v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                    })
            }
        };
        if internal {
            return Err(ApiError::Forbidden("access to internal addresses is not allowed".into()));
        }
    }
    Ok(())
}

/// Search the web. Uses DuckDuckGo's HTML endpoint, which requires no API key.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<SearchResult>, ApiError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ApiError::Unprocessable("query is required".into()));
    }
    let limit = limit.clamp(1, 20).min(MAX_SEARCH_RESULTS);
    let _ = state;

    let client = safe_client()?;
    let response = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| {
            tracing::debug!(error = %e, "web search request failed");
            ApiError::ServiceUnavailable
        })?;

    if !response.status().is_success() {
        return Err(ApiError::ServiceUnavailable);
    }
    let html = response.text().await.unwrap_or_default();
    Ok(parse_duckduckgo_results(&html, limit))
}

/// Upper bound on results regardless of what the caller asks for.
const MAX_SEARCH_RESULTS: usize = 20;

/// Extract results from DuckDuckGo's HTML page.
///
/// The page is parsed with light string handling instead of an HTML parser:
/// the markup around results is stable (`result__a` / `result__snippet`), and
/// adding a full parser for one endpoint is not worth the dependency.
fn parse_duckduckgo_results(html: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Results are list items carrying `result__body`; each holds one link and
    // one snippet.
    for chunk in html.split("result__body").skip(1) {
        if results.len() >= limit {
            break;
        }
        let Some(anchor) = extract_anchor(chunk) else { continue };
        let title = strip_tags(&anchor.text);
        if title.is_empty() {
            continue;
        }
        let snippet = chunk
            .find("result__snippet")
            .map(|idx| strip_tags(&first_text_block(&chunk[idx..])))
            .unwrap_or_default();

        results.push(SearchResult {
            title,
            url: normalize_ddg_url(&anchor.href),
            snippet,
        });
    }
    results
}

struct Anchor {
    href: String,
    text: String,
}

/// Pull the first `<a ...href="...">text</a>` out of a chunk.
fn extract_anchor(chunk: &str) -> Option<Anchor> {
    let start = chunk.find("<a")?;
    let tag_end = chunk[start..].find('>')? + start;
    let tag = &chunk[start..tag_end];
    if !tag.contains("result__a") {
        return None;
    }
    let href = extract_attr(tag, "href")?;
    let text_start = tag_end + 1;
    let text_end = chunk[text_start..].find("</a>")? + text_start;
    Some(Anchor { href, text: chunk[text_start..text_end].to_string() })
}

/// Read an attribute value from a tag body.
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = tag.find(&pattern)? + pattern.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// Text content of the element that follows a marker, used for snippets.
///
/// The marker (`result__snippet`) appears inside the element's opening tag, so
/// the tag itself must be skipped: taking the raw chunk kept the class name.
fn first_text_block(chunk: &str) -> String {
    match chunk.find('>') {
        Some(idx) => chunk[idx + 1..].to_string(),
        None => String::new(),
    }
}

/// DuckDuckGo wraps outbound links in a redirect (`/l/?uddg=<encoded>`).
/// Unwrap it so consumers get the real destination.
fn normalize_ddg_url(href: &str) -> String {
    let html_unescaped = href.replace("&amp;", "&");
    if let Some(rest) = html_unescaped.strip_prefix("//duckduckgo.com/l/?") {
        return extract_query_param(rest, "uddg").unwrap_or_else(|| html_unescaped.clone());
    }
    if let Some(rest) = html_unescaped.strip_prefix("/l/?") {
        return extract_query_param(rest, "uddg").unwrap_or_else(|| html_unescaped.clone());
    }
    html_unescaped
}

/// Percent-decode a query parameter without pulling in a form crate.
fn extract_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Fetch a public page and return its readable text.
pub async fn fetch_page(state: &AppState, url: &str) -> Result<Value, ApiError> {
    let _ = state;
    validate_public_url(url)?;

    let client = safe_client()?;
    let response = client.get(url).send().await.map_err(|e| {
        tracing::debug!(error = %e, "page fetch failed");
        ApiError::ServiceUnavailable
    })?;

    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    // Read at most MAX_BODY_BYTES so a huge or endless response cannot exhaust
    // memory.
    let bytes = read_capped(response).await;
    let raw = String::from_utf8_lossy(&bytes).to_string();

    let is_html = content_type.contains("html") || raw.trim_start().starts_with('<');
    let (title, text) = if is_html {
        let title = extract_title(&raw).unwrap_or_default();
        (title, html_to_text(&raw))
    } else {
        (String::new(), raw)
    };

    let truncated = text.chars().count() > MAX_TEXT_CHARS;
    let text: String = text.chars().take(MAX_TEXT_CHARS).collect();

    Ok(json!({
        "url": url,
        "final_url": final_url,
        "status": status,
        "content_type": content_type,
        "title": title,
        "text": text,
        "truncated": truncated,
    }))
}

/// Read a response body up to the byte cap.
async fn read_capped(response: reqwest::Response) -> Vec<u8> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        let remaining = MAX_BODY_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    buffer
}

/// Page `<title>`, for display in results.
fn extract_title(html: &str) -> Option<String> {
    let start = html.to_ascii_lowercase().find("<title")?;
    let open_end = html[start..].find('>')? + start + 1;
    let close = html[open_end..].to_ascii_lowercase().find("</title>")? + open_end;
    let title = html[open_end..close].trim();
    if title.is_empty() {
        None
    } else {
        Some(decode_entities(title))
    }
}

/// Convert an HTML document to readable text.
///
/// Removes `script`, `style`, `noscript`, `svg`, and `head` blocks, turns
/// block-level tags into line breaks, and collapses runs of whitespace. The
/// result keeps the page's meaning while dropping what the model cannot use.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    let bytes = html.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find the end of the tag to decide whether to skip a whole block.
            let Some(rel_end) = html[i..].find('>') else { break };
            let tag_end = i + rel_end;
            let tag = &html[i + 1..tag_end];
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();

            if !tag.starts_with('/') && is_skipped_element(&name) {
                // Skip to the matching close tag; a missing close tag skips the
                // rest, which is the safe choice for untrusted input.
                let close = format!("</{name}");
                match html[tag_end..].to_ascii_lowercase().find(&close) {
                    Some(offset) => {
                        let after = tag_end + offset;
                        // Continue after the closing tag's own '>'.
                        i = html[after..].find('>').map(|e| after + e + 1).unwrap_or(bytes.len());
                    }
                    None => break,
                }
                continue;
            }

            if is_block_element(&name) {
                push_newline(&mut out);
            }
            i = tag_end + 1;
            continue;
        }

        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }

    let decoded = decode_entities(&out);
    collapse_whitespace(&decoded)
}

/// Elements whose content is never useful as text.
fn is_skipped_element(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "svg" | "iframe" | "head" | "template" | "canvas"
    )
}

/// Elements that should start a new line when converted to text.
fn is_block_element(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "aside"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "form"
            | "figure"
            | "figcaption"
            | "hr"
    )
}

fn push_newline(out: &mut String) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Decode the HTML entities that appear in page text.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let Some(semi) = after[..after.len().min(10)].find(';') else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "rsquo" => Some('\u{2019}'),
            "lsquo" => Some('\u{2018}'),
            "ldquo" => Some('\u{201c}'),
            "rdquo" => Some('\u{201d}'),
            _ => {
                // Numeric entities: `&#8212;` and `&#x2014;`.
                if let Some(num) = entity.strip_prefix('#') {
                    let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse whitespace while preserving paragraph breaks.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    let mut newlines = 0;

    for ch in text.chars() {
        if ch == '\n' {
            newlines += 1;
            pending_space = false;
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if newlines > 0 {
            // At most one blank line between blocks.
            if !out.is_empty() {
                out.push('\n');
                if newlines > 1 {
                    out.push('\n');
                }
            }
            newlines = 0;
        } else if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out.trim().to_string()
}

/// Strip every tag from a fragment, for search snippets.
fn strip_tags(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    collapse_whitespace(&decode_entities(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_internal_destinations_but_allows_public_urls() {
        assert!(validate_public_url("https://example.com/page").is_ok());
        assert!(validate_public_url("http://93.184.216.34/").is_ok());

        // Loopback, private, link-local, and metadata endpoints are refused.
        for blocked in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://foo.localhost/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/",
            "http://[::1]/",
            "http://100.64.0.1/",
            "http://service.internal/",
        ] {
            assert!(validate_public_url(blocked).is_err(), "{blocked} must be blocked");
        }

        // Non-http schemes and malformed input are rejected.
        assert!(validate_public_url("file:///etc/passwd").is_err());
        assert!(validate_public_url("ftp://example.com").is_err());
        assert!(validate_public_url("not a url").is_err());
    }

    #[test]
    fn converts_html_to_readable_text() {
        let html = r##"
            <html><head><title>Ignored</title><style>.a{color:red}</style></head>
            <body>
              <script>console.log('ignore me')</script>
              <h1>Hello &amp; welcome</h1>
              <p>First paragraph.</p>
              <p>Second with <a href="#">a link</a>.</p>
            </body></html>
        "##;
        let text = html_to_text(html);
        assert!(text.contains("Hello & welcome"), "entities decoded: {text}");
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second with a link."));
        // Script and style content never reaches the model.
        assert!(!text.contains("console.log"), "script removed: {text}");
        assert!(!text.contains("color:red"), "style removed: {text}");
    }

    #[test]
    fn unclosed_script_does_not_leak_into_text() {
        let text = html_to_text("<p>Visible</p><script>secret();");
        assert!(text.contains("Visible"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn decodes_numeric_and_named_entities() {
        assert_eq!(decode_entities("&#8212;"), "—");
        assert_eq!(decode_entities("&#x2014;"), "—");
        assert_eq!(decode_entities("a&nbsp;b"), "a b");
        assert_eq!(decode_entities("5 &lt; 6"), "5 < 6");
        // An unknown entity is preserved rather than dropped.
        assert_eq!(decode_entities("&bogus;"), "&bogus;");
    }

    #[test]
    fn extracts_page_title() {
        assert_eq!(extract_title("<html><head><title> My Page </title></head>").as_deref(), Some("My Page"));
        assert!(extract_title("<html><body>no title</body></html>").is_none());
    }

    #[test]
    fn parses_duckduckgo_results() {
        // Shape mirrors the live page: anchor with result__a plus a snippet.
        let html = r#"
          <div class="result result__body">
            <h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&amp;rut=abc">Example Docs</a></h2>
            <a class="result__snippet">Official documentation for the thing.</a>
          </div>
          <div class="result result__body">
            <h2><a rel="nofollow" class="result__a" href="https://other.org/page">Other Page</a></h2>
            <a class="result__snippet">A second <b>result</b>.</a>
          </div>
        "#;
        let results = parse_duckduckgo_results(html, 10);
        assert_eq!(results.len(), 2, "both results parsed");
        assert_eq!(results[0].title, "Example Docs");
        // The DuckDuckGo redirect wrapper is unwrapped to the real URL.
        assert_eq!(results[0].url, "https://example.com/docs");
        assert!(results[0].snippet.contains("Official documentation"));
        assert_eq!(results[1].url, "https://other.org/page");
        assert_eq!(results[1].snippet, "A second result.", "tags stripped from snippets");

        // The limit is honored.
        assert_eq!(parse_duckduckgo_results(html, 1).len(), 1);
    }

    #[test]
    fn percent_decoding_handles_plus_and_escapes() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%E2%80%94"), "—");
        // A malformed escape is left as-is instead of panicking.
        assert_eq!(percent_decode("100%"), "100%");
    }
}
