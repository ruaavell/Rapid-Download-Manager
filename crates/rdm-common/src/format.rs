/// Human readable byte size. Uses binary units but familiar labels.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn format_speed(bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        "0 B/s".into()
    } else {
        format!("{}/s", format_bytes(bytes_per_second))
    }
}

pub fn format_eta(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "-".into();
    };
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = minutes / 60;
    let minutes = minutes % 60;
    if hours < 24 {
        return format!("{hours}h {minutes:02}m");
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

pub fn format_percent(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => {
            format!(
                "{:.1}%",
                (downloaded as f64 / total as f64 * 100.0).min(100.0)
            )
        }
        _ => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn formats_eta() {
        assert_eq!(format_eta(None), "-");
        assert_eq!(format_eta(Some(12)), "12s");
        assert_eq!(format_eta(Some(65)), "1m 05s");
        assert_eq!(format_eta(Some(3660)), "1h 01m");
    }
}
