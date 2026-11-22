//! Small, dependency-free helpers for interpreting HTTP responses.
//!
//! Everything here is pure string handling so it can be unit-tested without a
//! network or an async runtime.

/// Decode `%XX` escapes. Invalid escapes are kept literally.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Make an arbitrary string safe to use as a single file name.
///
/// Returns an empty string when nothing usable is left, so callers can fall
/// back to another source for the name.
pub fn sanitize_filename(input: &str) -> String {
    let last = input
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .unwrap_or("");
    let mut cleaned = String::with_capacity(last.len());
    for ch in last.chars() {
        if ch.is_control() {
            continue;
        }
        // Bidi overrides / invisible formatting characters.
        if matches!(
            ch,
            '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        ) {
            continue;
        }
        match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => cleaned.push('_'),
            c => cleaned.push(c),
        }
    }
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        return String::new();
    }
    if cleaned.chars().count() <= 200 {
        return cleaned;
    }
    // Keep the extension when truncating.
    let ext = match cleaned.rfind('.') {
        Some(idx) if cleaned.len() - idx <= 12 => cleaned[idx..].to_string(),
        _ => String::new(),
    };
    let keep = 200usize.saturating_sub(ext.chars().count()).max(1);
    let stem: String = cleaned.chars().take(keep).collect();
    format!("{}{}", stem.trim_end_matches([' ', '.']), ext)
}

/// File name taken from the URL path (query and fragment ignored).
/// Returns `download.bin` when the path has no usable last segment.
pub fn filename_from_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or("");
    let without_query = without_fragment.split('?').next().unwrap_or("");
    let after_scheme = match without_query.find("://") {
        Some(idx) => &without_query[idx + 3..],
        None => without_query,
    };
    let path = match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => "",
    };
    let last = path.rsplit('/').next().unwrap_or("");
    let name = sanitize_filename(&percent_decode(last));
    if name.is_empty() {
        "download.bin".to_string()
    } else {
        name
    }
}

/// Parse a `Content-Disposition` header and return a sanitized file name.
/// Supports `filename="x"`, `filename=x` and RFC 5987 `filename*=UTF-8''x`.
pub fn content_disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in split_outside_quotes(value, ';') {
        let part = part.trim();
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let raw = raw.trim();
        if key == "filename*" {
            // charset'language'percent-encoded-value
            let encoded = raw.splitn(3, '\'').nth(2).unwrap_or(raw);
            extended = Some(percent_decode(encoded.trim_matches('"')));
        } else if key == "filename" {
            plain = Some(unquote(raw));
        }
    }
    let name = sanitize_filename(&extended.or(plain)?);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn split_outside_quotes(value: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut start = 0;
    for (idx, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            c if c == sep && !in_quotes => {
                parts.push(&value[start..idx]);
                start = idx + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

fn unquote(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(ch);
            }
        }
        out
    } else {
        raw.to_string()
    }
}

/// True for `text/html` and `application/xhtml+xml` (parameters ignored).
pub fn is_html_content_type(content_type: &str) -> bool {
    let base = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    base == "text/html" || base == "application/xhtml+xml"
}

/// Extension commonly used for a media content type.
pub fn extension_for_content_type(content_type: &str) -> Option<&'static str> {
    let base = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match base.as_str() {
        "video/mp4" | "application/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" | "application/ogg" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" => "wav",
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" => "m3u8",
        "application/dash+xml" => "mpd",
        _ => return None,
    })
}

/// Add an extension derived from the content type when the name has none.
pub fn ensure_extension(name: &str, content_type: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return name.to_string();
    }
    if name.rsplit_once('.').is_some() {
        return name.to_string();
    }
    match extension_for_content_type(content_type) {
        Some(ext) => format!("{name}.{ext}"),
        None => name.to_string(),
    }
}

/// True when the file name has an extension that is not a web page extension,
/// e.g. `gta3.rar`. Names without an extension return false.
pub fn has_non_html_extension(filename: &str) -> bool {
    let Some((_, ext)) = filename.rsplit_once('.') else {
        return false;
    };
    let ext = ext.trim().to_ascii_lowercase();
    if ext.is_empty() {
        return false;
    }
    !matches!(
        ext.as_str(),
        "html" | "htm" | "xhtml" | "shtml" | "php" | "asp" | "aspx" | "jsp" | "cgi"
    )
}

/// Whether a file name suggested by the browser is specific enough to trust.
///
/// Chrome's downloads API reports placeholders such as `download` (Google
/// Drive links end in `/download`) when it only saw a web page. Saving under
/// those names produces extension-less files that cannot be opened, so the
/// server's `Content-Disposition` name should win instead.
pub fn is_usable_filename_hint(filename: &str) -> bool {
    let name = filename.trim();
    if name.is_empty() {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "download" | "download.bin" | "file" | "file.bin" | "unknown" | "blob"
    ) {
        return false;
    }
    let ext = lower.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    !matches!(ext, "html" | "htm" | "xhtml" | "shtml")
}

/// Host part of an http(s) URL, lowercased.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = authority.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Hosts whose HTML responses are Google Drive warning/sign-in pages rather
/// than the requested file.
pub fn is_google_drive_host(url: &str) -> bool {
    matches!(
        host_of(url).as_deref(),
        Some("drive.google.com") | Some("drive.usercontent.google.com") | Some("docs.google.com")
    )
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Read an attribute (`name="v"` or `name='v'`) from a single tag's text.
fn tag_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(found) = lower[search_from..].find(name) {
        let start = search_from + found;
        let after = start + name.len();
        let boundary_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace();
        let rest = &tag[after..];
        let rest_trim = rest.trim_start();
        if boundary_ok && rest_trim.starts_with('=') {
            let value = rest_trim[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &value[1..];
                let end = inner.find(quote)?;
                return Some(decode_html_entities(&inner[..end]));
            }
            let end = value
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(value.len());
            return Some(decode_html_entities(&value[..end]));
        }
        search_from = after;
    }
    None
}

/// Build the real download URL from Google Drive's "can't scan this file for
/// viruses" confirmation page. The result may be relative to the page URL.
pub fn google_drive_confirm_url(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();

    // Current page: <form id="download-form" action="..."> with hidden inputs.
    if let Some(id_pos) = lower.find("id=\"download-form\"") {
        let form_start = lower[..id_pos].rfind("<form")?;
        let form_tag_end = form_start + lower[form_start..].find('>')?;
        let form_tag = &html[form_start..=form_tag_end];
        let action = tag_attr(form_tag, "action")?;
        let form_end = lower[form_tag_end..]
            .find("</form")
            .map(|i| form_tag_end + i)
            .unwrap_or(html.len());
        let body = &html[form_tag_end..form_end];
        let body_lower = &lower[form_tag_end..form_end];

        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut cursor = 0;
        while let Some(rel) = body_lower[cursor..].find("<input") {
            let tag_start = cursor + rel;
            let tag_end = tag_start + body_lower[tag_start..].find('>')?;
            let tag = &body[tag_start..=tag_end];
            cursor = tag_end + 1;
            let is_hidden = tag_attr(tag, "type")
                .map(|t| t.eq_ignore_ascii_case("hidden"))
                .unwrap_or(false);
            if !is_hidden {
                continue;
            }
            if let Some(name) = tag_attr(tag, "name") {
                let value = tag_attr(tag, "value").unwrap_or_default();
                pairs.push((name, value));
            }
        }
        if pairs.is_empty() {
            return None;
        }
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode_component(k), percent_encode_component(v)))
            .collect::<Vec<_>>()
            .join("&");
        let sep = if action.contains('?') { '&' } else { '?' };
        return Some(format!("{action}{sep}{query}"));
    }

    // Older page: a plain link to /uc?export=download&confirm=...
    let marker = "href=\"/uc?export=download";
    if let Some(pos) = lower.find(marker) {
        let value_start = pos + "href=\"".len();
        let end = html[value_start..].find('"')? + value_start;
        return Some(decode_html_entities(&html[value_start..end]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_edges() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%E2%9C%93"), "✓");
        assert_eq!(percent_decode("x%4"), "x%4");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\win\\sys32"), "sys32");
        assert_eq!(sanitize_filename("bad\u{0}name\n.zip"), "badname.zip");
        assert_eq!(sanitize_filename(".."), "");
        assert_eq!(sanitize_filename("a:b*c?.rar"), "a_b_c_.rar");
        assert_eq!(sanitize_filename("  "), "");
        let long = format!("{}.rar", "a".repeat(400));
        let out = sanitize_filename(&long);
        assert!(out.chars().count() <= 200);
        assert!(out.ends_with(".rar"));
    }

    #[test]
    fn url_filenames() {
        assert_eq!(filename_from_url("https://example.com/a/b.iso?token=x"), "b.iso");
        assert_eq!(filename_from_url("https://example.com/"), "download.bin");
        assert_eq!(filename_from_url("https://example.com"), "download.bin");
        assert_eq!(filename_from_url("https://example.com/a%20b.zip#frag"), "a b.zip");
        assert_eq!(
            filename_from_url("https://drive.usercontent.google.com/download?id=1&export=download"),
            "download"
        );
        assert_eq!(filename_from_url("https://example.com/..%2F..%2Fx.zip"), "x.zip");
    }

    #[test]
    fn content_disposition() {
        assert_eq!(
            content_disposition_filename("attachment; filename=\"gta3.rar\"").as_deref(),
            Some("gta3.rar")
        );
        assert_eq!(
            content_disposition_filename("attachment; filename=gta3.zip").as_deref(),
            Some("gta3.zip")
        );
        // RFC 5987 form wins over the plain fallback.
        assert_eq!(
            content_disposition_filename(
                "attachment; filename=\"fallback.bin\"; filename*=UTF-8''G%C3%BCzel%20dosya.rar"
            )
            .as_deref(),
            Some("Güzel dosya.rar")
        );
        // Semicolon inside quotes does not split.
        assert_eq!(
            content_disposition_filename("attachment; filename=\"a;b.zip\"").as_deref(),
            Some("a;b.zip")
        );
        // Traversal is neutralised.
        assert_eq!(
            content_disposition_filename("attachment; filename=\"../../evil.sh\"").as_deref(),
            Some("evil.sh")
        );
        assert_eq!(content_disposition_filename("inline"), None);
        assert_eq!(content_disposition_filename("attachment; filename=\"\""), None);
    }

    #[test]
    fn html_detection() {
        assert!(is_html_content_type("text/html; charset=utf-8"));
        assert!(is_html_content_type("TEXT/HTML"));
        assert!(!is_html_content_type("application/x-rar-compressed"));
        assert!(has_non_html_extension("gta3.rar"));
        assert!(has_non_html_extension("a.b.zip"));
        assert!(!has_non_html_extension("index.html"));
        assert!(!has_non_html_extension("download"));
        assert!(!has_non_html_extension("page.php"));
        assert_eq!(extension_for_content_type("video/mp4; charset=binary"), Some("mp4"));
        assert_eq!(extension_for_content_type("audio/mpeg"), Some("mp3"));
        assert_eq!(ensure_extension("videoplayback", "video/mp4"), "videoplayback.mp4");
        assert_eq!(ensure_extension("song.mp3", "video/mp4"), "song.mp3");
    }

    #[test]
    fn filename_hints() {
        // Real names from Content-Disposition are trusted.
        assert!(is_usable_filename_hint("gta3.rar"));
        assert!(is_usable_filename_hint("Güzel dosya.zip"));
        assert!(is_usable_filename_hint("README"));
        // Browser placeholders and saved web pages are not.
        assert!(!is_usable_filename_hint("download"));
        assert!(!is_usable_filename_hint("Download"));
        assert!(!is_usable_filename_hint("download.bin"));
        assert!(!is_usable_filename_hint("file"));
        assert!(!is_usable_filename_hint("index.html"));
        assert!(!is_usable_filename_hint("page.HTM"));
        assert!(!is_usable_filename_hint("  "));
    }

    #[test]
    fn drive_hosts() {
        assert!(is_google_drive_host("https://drive.google.com/uc?id=1"));
        assert!(is_google_drive_host("https://drive.usercontent.google.com/download?id=1"));
        assert!(is_google_drive_host("https://DRIVE.google.com:443/x"));
        assert!(!is_google_drive_host("https://example.com/drive.google.com"));
        assert!(!is_google_drive_host("https://drive.google.com.evil.test/x"));
        assert!(!is_google_drive_host("not a url"));
    }

    #[test]
    fn drive_confirm_form() {
        let html = r#"<html><body><p>Google Drive can't scan this file for viruses.</p>
<form id="download-form" action="https://drive.usercontent.google.com/download" method="get">
<input type="submit" id="uc-download-link" class="jfk-button" value="Download anyway"/>
<input type="hidden" name="id" value="1AbC_dEf">
<input type="hidden" name="export" value="download">
<input type="hidden" name="authuser" value="0">
<input type="hidden" name="confirm" value="t">
<input type="hidden" name="uuid" value="1234-abcd">
<input type="hidden" name="at" value="AB&amp;C=">
</form></body></html>"#;
        let url = google_drive_confirm_url(html).unwrap();
        assert_eq!(
            url,
            "https://drive.usercontent.google.com/download?id=1AbC_dEf&export=download&authuser=0&confirm=t&uuid=1234-abcd&at=AB%26C%3D"
        );
    }

    #[test]
    fn drive_confirm_old_link() {
        let html = r#"<a id="uc-download-link" href="/uc?export=download&amp;id=XYZ&amp;confirm=AbCd">Download anyway</a>"#;
        assert_eq!(
            google_drive_confirm_url(html).unwrap(),
            "/uc?export=download&id=XYZ&confirm=AbCd"
        );
    }

    #[test]
    fn drive_confirm_absent() {
        assert_eq!(google_drive_confirm_url("<html>Sign in</html>"), None);
        assert_eq!(
            google_drive_confirm_url("<form id=\"download-form\" action=\"/x\"></form>"),
            None
        );
    }
}
