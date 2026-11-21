use crate::{RdmError, Result, MAX_HEADERS, MAX_HEADER_VALUE_LEN, MAX_URL_LEN};
use std::path::{Path, PathBuf};
use url::Url;

/// Parse and validate a user or browser supplied URL.
pub fn parse_http_url(value: &str) -> Result<Url> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RdmError::InvalidUrl("URL is empty".into()));
    }
    if value.len() > MAX_URL_LEN {
        return Err(RdmError::InvalidUrl("URL is too long".into()));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(RdmError::InvalidUrl(
            "URL contains control characters".into(),
        ));
    }
    let parsed = Url::parse(value)?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(RdmError::UnsupportedScheme(other.into())),
    }
    if parsed.host_str().unwrap_or_default().is_empty() {
        return Err(RdmError::InvalidUrl("URL has no host".into()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(RdmError::EmbeddedCredentials);
    }
    Ok(parsed)
}

pub fn validate_http_url(value: &str) -> Result<()> {
    parse_http_url(value).map(|_| ())
}

/// Validate an optional referrer. Empty values are treated as absent.
pub fn validate_referrer(value: Option<&str>) -> Result<Option<String>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            parse_http_url(value)?;
            Ok(Some(value.to_string()))
        }
        None => Ok(None),
    }
}

/// Extract a safe filename from the URL path. Query strings are ignored.
pub fn filename_from_url(value: &str) -> String {
    let Ok(parsed) = Url::parse(value) else {
        return "download.bin".into();
    };
    let last = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or_default();
    let decoded = percent_decode(last);
    sanitize_filename(&decoded)
}

/// Parse `Content-Disposition` and return a sanitized filename.
///
/// Supports both `filename=` and RFC 5987 `filename*=UTF-8''...`.
pub fn content_disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in split_semicolon_outside_quotes(value) {
        let part = part.trim();
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let raw = raw.trim();
        if key == "filename*" {
            extended = Some(decode_rfc5987(raw));
        } else if key == "filename" {
            plain = Some(unquote(raw).to_string());
        }
    }
    let candidate = extended.or(plain)?;
    let sanitized = sanitize_filename(&candidate);
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

/// Make an arbitrary string safe to use as a single filename component.
///
/// This never returns a path. Directory separators are removed by taking the
/// final component, control/bidi characters are dropped, and characters that
/// are problematic on common filesystems are replaced.
pub fn sanitize_filename(input: &str) -> String {
    let mut name = input
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .unwrap_or("")
        .to_string();

    // Strip URL fragments/queries that sometimes end up in filenames.
    if let Some(index) = name.find(['?', '#']) {
        name.truncate(index);
    }

    let mut cleaned = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_control() {
            continue;
        }
        // Bidi overrides and other invisible formatting characters.
        if matches!(
            ch,
            '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        ) {
            continue;
        }
        match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => cleaned.push('_'),
            c if c.is_whitespace() => cleaned.push(' '),
            c => cleaned.push(c),
        }
    }

    let mut cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    while cleaned.contains("  ") {
        cleaned = cleaned.replace("  ", " ");
    }
    if cleaned.is_empty() {
        return "download.bin".into();
    }

    // Windows device names are rejected even on Linux to keep shared folders sane.
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        cleaned = format!("_{cleaned}");
    }

    truncate_filename(&cleaned, 200)
}

fn truncate_filename(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    let keep = max_chars.saturating_sub(extension.chars().count()).max(1);
    let mut stem: String = name.chars().take(keep).collect();
    stem = stem.trim_end_matches([' ', '.']).to_string();
    format!("{stem}{extension}")
}

/// Resolve a user-selected destination into a final absolute file path.
pub fn resolve_destination(destination: &str, filename: &str) -> PathBuf {
    let expanded = expand_tilde(destination.trim());
    let looks_like_file = expanded
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.contains('.'))
        .unwrap_or(false)
        && !expanded.is_dir();
    if looks_like_file {
        expanded
    } else {
        expanded.join(sanitize_filename(filename))
    }
}

/// If `path` already exists, add ` (n)` before the extension.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let candidate = match extension {
            Some(ext) => parent.join(format!("{stem} ({index}).{ext}")),
            None => parent.join(format!("{stem} ({index})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem}-{}", crate::time::now_unix()))
}

/// Validate and normalize a SHA-256 checksum (hex, optional `sha256:` prefix).
pub fn normalize_sha256(value: &str) -> Result<String> {
    let value = value.trim();
    let value = value
        .strip_prefix("sha256:")
        .or_else(|| value.strip_prefix("SHA256:"))
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RdmError::InvalidFilename(
            "checksum must be a 64-character SHA-256 hex string".into(),
        ));
    }
    Ok(value)
}

/// Validate a custom request header before it reaches reqwest.
pub fn validate_header(name: &str, value: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.len() > 256 {
        return Err(RdmError::InvalidHeader("invalid header name".into()));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(RdmError::InvalidHeader(format!(
            "header name '{name}' contains invalid characters"
        )));
    }
    if value.len() > MAX_HEADER_VALUE_LEN {
        return Err(RdmError::InvalidHeader(format!(
            "header '{name}' is too large"
        )));
    }
    if value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        return Err(RdmError::InvalidHeader(format!(
            "header '{name}' contains a newline"
        )));
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "upgrade"
            | "te"
            | "trailer"
            | "expect"
            | "range"
            | "if-range"
            | "proxy-connection"
    ) || lower.starts_with("proxy-")
    {
        return Err(RdmError::InvalidHeader(format!(
            "header '{name}' is managed by the download engine"
        )));
    }
    Ok(())
}

pub fn validate_headers(headers: &[crate::types::HeaderEntry]) -> Result<()> {
    if headers.len() > MAX_HEADERS {
        return Err(RdmError::InvalidHeader(format!(
            "at most {MAX_HEADERS} custom headers are allowed"
        )));
    }
    for header in headers {
        validate_header(&header.name, &header.value)?;
    }
    Ok(())
}

/// Remove obvious secrets from a URL before logging it.
pub fn redact_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "<invalid-url>".into();
    };
    let sensitive = [
        "token",
        "key",
        "sig",
        "signature",
        "auth",
        "password",
        "session",
        "expires",
        "expiry",
    ];
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            if sensitive.iter().any(|needle| lower.contains(needle)) {
                (key.into_owned(), "REDACTED".to_string())
            } else {
                (key.into_owned(), value.into_owned())
            }
        })
        .collect();
    if !pairs.is_empty() {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

pub fn expand_tilde(value: &str) -> PathBuf {
    if value == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn percent_decode(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"')
}

fn split_semicolon_outside_quotes(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            ';' if !quoted => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

fn decode_rfc5987(value: &str) -> String {
    let value = unquote(value);
    // charset'language'percent-encoded
    let mut pieces = value.splitn(3, '\'');
    let charset = pieces.next().unwrap_or("utf-8");
    let _language = pieces.next();
    let encoded = pieces.next().unwrap_or(value);
    let decoded = percent_decode(encoded);
    let _ = charset;
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_schemes() {
        assert!(validate_http_url("file:///etc/passwd").is_err());
        assert!(validate_http_url("ftp://example.com/file").is_err());
        assert!(validate_http_url("javascript:alert(1)").is_err());
        assert!(validate_http_url("https://example.com/file").is_ok());
    }

    #[test]
    fn rejects_embedded_credentials() {
        assert!(matches!(
            parse_http_url("https://user:secret@example.com/file"),
            Err(RdmError::EmbeddedCredentials)
        ));
    }

    #[test]
    fn sanitizes_traversal_and_control_characters() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\windows\\system32"), "system32");
        assert_eq!(sanitize_filename("bad\u{0}name\n.zip"), "badname.zip");
        assert_eq!(sanitize_filename(".."), "download.bin");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert!(!sanitize_filename("a/b").contains('/'));
    }

    #[test]
    fn extracts_filename_from_url() {
        assert_eq!(
            filename_from_url("https://example.com/a/b.iso?token=x"),
            "b.iso"
        );
        assert_eq!(filename_from_url("https://example.com/"), "download.bin");
        assert_eq!(
            filename_from_url("https://example.com/My%20File.zip"),
            "My File.zip"
        );
    }

    #[test]
    fn parses_content_disposition() {
        assert_eq!(
            content_disposition_filename("attachment; filename=\"report.pdf\""),
            Some("report.pdf".into())
        );
        assert_eq!(
            content_disposition_filename("attachment; filename*=UTF-8''na%C3%AFve%20file.zip"),
            Some("naïve file.zip".into())
        );
        assert_eq!(
            content_disposition_filename("attachment; filename=\"../../evil.sh\""),
            Some("evil.sh".into())
        );
    }

    #[test]
    fn validates_headers() {
        assert!(validate_header("Authorization", "Bearer token").is_ok());
        assert!(validate_header("X-Test", "value").is_ok());
        assert!(validate_header("Host", "evil").is_err());
        assert!(validate_header("X-Test", "a\r\nInjected: yes").is_err());
        assert!(validate_header("Bad Name", "x").is_err());
    }

    #[test]
    fn normalizes_checksums() {
        let value = "sha256:".to_string() + &"ab".repeat(32);
        assert_eq!(normalize_sha256(&value).unwrap(), "ab".repeat(32));
        assert!(normalize_sha256("not-a-checksum").is_err());
    }

    #[test]
    fn redacts_sensitive_query_parameters() {
        let redacted = redact_url("https://example.com/file?token=abc&x=1");
        assert!(!redacted.contains("abc"));
        assert!(redacted.contains("REDACTED"));
        assert!(redacted.contains("x=1"));
    }
}
