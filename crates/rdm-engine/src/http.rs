use rand::Rng;
use rdm_common::{HeaderEntry, RdmError, Result};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, COOKIE, REFERER},
    StatusCode,
};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

/// Parse a `Content-Range: bytes start-end/total` header.
pub fn parse_content_range(value: &str) -> Result<ContentRange> {
    let value = value.trim();
    let Some(rest) = value.strip_prefix("bytes ") else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let Some((range, total)) = rest.split_once('/') else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let Some((start, end)) = range.split_once('-') else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let start: u64 = start
        .trim()
        .parse()
        .map_err(|_| RdmError::InvalidContentRange(value.to_string()))?;
    let end: u64 = end
        .trim()
        .parse()
        .map_err(|_| RdmError::InvalidContentRange(value.to_string()))?;
    if end < start {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    }
    let total = total.trim();
    let total = if total == "*" {
        None
    } else {
        Some(
            total
                .parse()
                .map_err(|_| RdmError::InvalidContentRange(value.to_string()))?,
        )
    };
    if let Some(total) = total {
        if end >= total {
            return Err(RdmError::InvalidContentRange(format!(
                "range end {end} is not below total {total}"
            )));
        }
    }
    Ok(ContentRange { start, end, total })
}

/// Parse `Content-Range: bytes */total` from a 416 response.
pub fn parse_unsatisfied_range(value: &str) -> Result<Option<u64>> {
    let value = value.trim();
    let Some(rest) = value.strip_prefix("bytes ") else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let Some(total) = rest.strip_prefix('*') else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let Some(total) = total.strip_prefix('/') else {
        return Err(RdmError::InvalidContentRange(value.to_string()));
    };
    let total = total.trim();
    if total == "*" {
        return Ok(None);
    }
    total
        .parse()
        .map(Some)
        .map_err(|_| RdmError::InvalidContentRange(value.to_string()))
}

/// Build the final header map for a request. Sensitive headers are supplied
/// separately from the in-memory secret store and never persisted.
pub fn build_headers(
    stored: &[HeaderEntry],
    sensitive: &[HeaderEntry],
    referrer: Option<&str>,
    cookies: Option<&str>,
) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for header in stored.iter().chain(sensitive.iter()) {
        let name = HeaderName::from_bytes(header.name.trim().as_bytes())
            .map_err(|_| RdmError::InvalidHeader(header.name.clone()))?;
        let value = HeaderValue::from_str(&header.value)
            .map_err(|_| RdmError::InvalidHeader(header.name.clone()))?;
        map.insert(name, value);
    }
    if let Some(referrer) = referrer {
        let value = HeaderValue::from_str(referrer)
            .map_err(|_| RdmError::InvalidHeader("Referer".into()))?;
        map.insert(REFERER, value);
    }
    if let Some(cookies) = cookies {
        if cookies.contains(['\r', '\n', '\0']) {
            return Err(RdmError::InvalidHeader("Cookie".into()));
        }
        let value =
            HeaderValue::from_str(cookies).map_err(|_| RdmError::InvalidHeader("Cookie".into()))?;
        map.insert(COOKIE, value);
    }
    Ok(map)
}

pub fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "proxy-authorization" | "x-api-key" | "x-auth-token"
    )
}

pub fn status_error(status: StatusCode) -> RdmError {
    let message = match status.as_u16() {
        401 => "authentication required",
        403 => "the server refused access",
        404 => "the file was not found",
        405 => "the server does not allow this request",
        408 => "the server timed out the request",
        416 => "the requested byte range is not satisfiable",
        429 => "the server is rate limiting requests",
        500..=599 => "the server reported an internal error",
        _ => "unexpected HTTP status",
    };
    RdmError::Http {
        status: status.as_u16(),
        message: message.into(),
    }
}

pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(3600)));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::Utc::now();
    let delta = date.with_timezone(&chrono::Utc) - now;
    if delta.num_seconds() > 0 {
        Some(Duration::from_secs((delta.num_seconds() as u64).min(3600)))
    } else {
        Some(Duration::from_secs(1))
    }
}

pub fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 400u64.saturating_mul(1u64 << attempt.min(6));
    let capped = base_ms.min(30_000);
    let jitter = rand::thread_rng().gen_range(0..=capped / 4 + 1);
    Duration::from_millis(capped + jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_content_ranges() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000").unwrap(),
            ContentRange {
                start: 0,
                end: 99,
                total: Some(1000)
            }
        );
        assert_eq!(
            parse_content_range("bytes 10-19/*").unwrap(),
            ContentRange {
                start: 10,
                end: 19,
                total: None
            }
        );
    }

    #[test]
    fn rejects_invalid_content_ranges() {
        assert!(parse_content_range("bytes 100-50/1000").is_err());
        assert!(parse_content_range("bytes 0-99").is_err());
        assert!(parse_content_range("bytes 0-1000/1000").is_err());
        assert!(parse_content_range("items 0-1/2").is_err());
    }

    #[test]
    fn parses_unsatisfied_ranges() {
        assert_eq!(parse_unsatisfied_range("bytes */1234").unwrap(), Some(1234));
        assert_eq!(parse_unsatisfied_range("bytes */*").unwrap(), None);
    }

    #[test]
    fn builds_and_rejects_headers() {
        let headers = build_headers(
            &[HeaderEntry {
                name: "X-Test".into(),
                value: "ok".into(),
            }],
            &[],
            Some("https://example.com/page"),
            Some("a=b"),
        )
        .unwrap();
        assert_eq!(headers.get("x-test").unwrap(), "ok");
        assert!(headers.get(REFERER).is_some());
        assert!(headers.get(COOKIE).is_some());
        assert!(build_headers(&[], &[], None, Some("a\r\nb")).is_err());
    }
}
