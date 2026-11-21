use crate::http::{parse_content_range, parse_unsatisfied_range, status_error};
use rdm_common::{url::content_disposition_filename, RdmError, Result};
use reqwest::{
    header::{HeaderMap, HeaderValue, RANGE},
    Client, StatusCode,
};

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub final_url: String,
    pub total: Option<u64>,
    pub supports_ranges: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
}

/// Probe the server with a one-byte range request.
///
/// Range support is only accepted when the server actually answers 206 or 416
/// with a valid `Content-Range`. A 200 response means "no ranges", even if the
/// server advertises `Accept-Ranges: bytes`.
pub async fn probe_server(client: &Client, url: &str, headers: &HeaderMap) -> Result<ProbeResult> {
    let mut request = client.get(url).headers(headers.clone());
    request = request.header(RANGE, HeaderValue::from_static("bytes=0-0"));
    let response = request
        .send()
        .await
        .map_err(|err| RdmError::network(format!("probe request failed: {err}")))?;
    let status = response.status();
    let final_url = response.url().to_string();
    let response_headers = response.headers().clone();
    let etag = header_string(&response_headers, reqwest::header::ETAG);
    let last_modified = header_string(&response_headers, reqwest::header::LAST_MODIFIED);
    let mime_type = header_string(&response_headers, reqwest::header::CONTENT_TYPE)
        .map(|value| value.split(';').next().unwrap_or(&value).trim().to_string());
    let filename = header_string(&response_headers, reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| content_disposition_filename(&value));

    match status {
        StatusCode::PARTIAL_CONTENT => {
            let content_range = header_string(&response_headers, reqwest::header::CONTENT_RANGE)
                .ok_or_else(|| RdmError::InvalidContentRange("missing Content-Range".into()))?;
            let range = parse_content_range(&content_range)?;
            if range.start != 0 {
                return Err(RdmError::InvalidContentRange(format!(
                    "expected range to start at 0, got {}",
                    range.start
                )));
            }
            Ok(ProbeResult {
                final_url,
                total: range.total,
                supports_ranges: true,
                etag,
                last_modified,
                mime_type,
                filename,
            })
        }
        StatusCode::OK => Ok(ProbeResult {
            final_url,
            total: response.content_length(),
            supports_ranges: false,
            etag,
            last_modified,
            mime_type,
            filename,
        }),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            let total = header_string(&response_headers, reqwest::header::CONTENT_RANGE)
                .map(|value| parse_unsatisfied_range(&value))
                .transpose()?
                .flatten();
            Ok(ProbeResult {
                final_url,
                total,
                supports_ranges: true,
                etag,
                last_modified,
                mime_type,
                filename,
            })
        }
        other => Err(status_error(other)),
    }
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
