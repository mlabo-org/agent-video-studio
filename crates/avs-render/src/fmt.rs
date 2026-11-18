//! Number and command formatting shared by every module that writes ffmpeg text.

use avs_core::Time;

/// Exact decimal seconds from integer microseconds, trailing zeros trimmed
/// (`1500000` becomes `1.5`). Never goes through float `Display`.
pub fn fmt_time(time: Time) -> String {
    let micros = time.micros();
    let sign = if micros < 0 { "-" } else { "" };
    let abs = micros.unsigned_abs();
    let (whole, fraction) = (abs / 1_000_000, abs % 1_000_000);
    if fraction == 0 {
        return format!("{sign}{whole}");
    }
    let fraction = format!("{fraction:06}");
    format!("{sign}{whole}.{}", fraction.trim_end_matches('0'))
}

/// A filter-expression number: at most six decimals, no exponent, no trailing zeros.
pub fn fmt_num(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" || text == "-0" { "0".to_string() } else { text.to_string() }
}

/// The command line as one copy-pasteable string.
pub fn shell_join(parts: &[String]) -> String {
    parts.iter().map(|part| shell_quote(part)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(part: &str) -> String {
    let plain = !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain { part.to_string() } else { format!("'{}'", part.replace('\'', "'\\''")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_is_exact() {
        assert_eq!(fmt_time(Time::from_micros(1_500_000)), "1.5");
        assert_eq!(fmt_time(Time::from_micros(2_000_000)), "2");
        assert_eq!(fmt_time(Time::from_micros(33_333)), "0.033333");
        assert_eq!(fmt_time(Time::from_micros(-250_000)), "-0.25");
    }

    #[test]
    fn numbers_trim() {
        assert_eq!(fmt_num(0.5), "0.5");
        assert_eq!(fmt_num(2.0), "2");
        assert_eq!(fmt_num(-0.0000001), "0");
        assert_eq!(fmt_num(f64::NAN), "0");
    }
}
