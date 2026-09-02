// 429 retry strategy
// Duration parsing

use once_cell::sync::Lazy;
use regex::Regex;

static DURATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)([\d.]+)\s*(milliseconds?|ms|seconds?|secs?|s|minutes?|mins?|m|hours?|hrs?|h)")
        .unwrap()
});

static RE_TEXT_DELAY_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)quota will reset after ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)quota will reset in ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)retry after ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)reset after ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)try again in ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)backoff for ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)(?:^|[\s(])wait\s+([^,;\]\n)]+)").unwrap(),
    ]
});

static RE_LEGACY_DELAY_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)quota will reset after ([^.,;\]\n]+)").unwrap(),
        Regex::new(r"(?i)retry after ([^.,;\]\n]+)").unwrap(),
        Regex::new(r#"(?i)quotaResetDelay["'=:\s]+([^\s,"}\]]+)"#).unwrap(),
    ]
});

static RETRY_HINT_KEYS: Lazy<std::collections::HashSet<&'static str>> = Lazy::new(|| {
    [
        "retryafter",
        "retry_after",
        "retrydelay",
        "retry_delay",
        "quotaresetdelay",
        "quota_reset_delay",
        "backofflimit",
        "backoff_limit",
    ]
    .iter()
    .cloned()
    .collect()
});

/// Parse a duration string (e.g., "1.5s", "200ms", "1h16m0.667s")
pub fn parse_duration_ms(duration_str: &str) -> Option<u64> {
    let mut total_ms: f64 = 0.0;
    let mut matched = false;

    for cap in DURATION_RE.captures_iter(duration_str) {
        matched = true;
        let value: f64 = cap[1].parse().ok()?;
        let unit = cap[2].to_ascii_lowercase();

        match unit.as_str() {
            unit if unit == "ms" || unit.starts_with("millisecond") => total_ms += value,
            unit if unit == "s" || unit.starts_with("sec") || unit.starts_with("second") => {
                total_ms += value * 1000.0
            }
            unit if unit == "m" || unit.starts_with("min") || unit.starts_with("minute") => {
                total_ms += value * 60.0 * 1000.0
            }
            unit if unit == "h" || unit.starts_with("hr") || unit.starts_with("hour") => {
                total_ms += value * 60.0 * 60.0 * 1000.0
            }
            _ => {}
        }
    }

    if !matched {
        return None;
    }

    Some(total_ms.round() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDelaySource {
    Structured,
    ResponseText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedRetryDelay {
    pub raw_ms: u64,
    pub source: RetryDelaySource,
}

impl ParsedRetryDelay {
    pub fn actual_wait_ms(self) -> u64 {
        let buffer_ms = match self.source {
            RetryDelaySource::Structured => 200,
            RetryDelaySource::ResponseText => 1000,
        };
        self.raw_ms.saturating_add(buffer_ms)
    }
}

/// Extract the raw retry delay from Retry-After or a 429 error (deep recursive parsing)
pub fn parse_retry_delay(error_text: &str, retry_after: Option<&str>) -> Option<u64> {
    parse_retry_delay_with_source(error_text, retry_after).map(|delay| delay.raw_ms)
}

pub fn parse_retry_delay_with_source(
    error_text: &str,
    retry_after: Option<&str>,
) -> Option<ParsedRetryDelay> {
    // 1. Retry-After delta-seconds; also accepts the existing duration-string sources
    if let Some(value) = retry_after.map(str::trim).filter(|value| !value.is_empty()) {
        if let Ok(seconds) = value.parse::<u64>() {
            return seconds.checked_mul(1000).map(|raw_ms| ParsedRetryDelay {
                raw_ms,
                source: RetryDelaySource::Structured,
            });
        }
        if let Some(delay) = parse_duration_ms(value) {
            return Some(ParsedRetryDelay {
                raw_ms: delay,
                source: RetryDelaySource::Structured,
            });
        }
    }

    // 2. Prefer structured JSON fields, so a retryDelay inside JSON is not treated as response text.
    if let Ok(json) = serde_json::from_str(error_text) {
        if let Some(raw_ms) = extract_structured_delay_recursive(&json, 0, false) {
            return Some(ParsedRetryDelay {
                raw_ms,
                source: RetryDelaySource::Structured,
            });
        }
    }

    // 3. Extract a natural-language duration from the response text only.
    for re in RE_TEXT_DELAY_PATTERNS.iter() {
        if let Some(cap) = re.captures(error_text) {
            if let Some(delay) = parse_duration_ms(&cap[1]) {
                return Some(ParsedRetryDelay {
                    raw_ms: delay,
                    source: RetryDelaySource::ResponseText,
                });
            }
        }
    }
    None
}

/// Preserve the pre-state-machine delay parsing used by the Claude handler.
pub(crate) fn parse_legacy_retry_delay(error_text: &str) -> Option<u64> {
    for re in RE_LEGACY_DELAY_PATTERNS.iter() {
        if let Some(cap) = re.captures(error_text) {
            if let Some(delay) = parse_duration_ms(&cap[1]) {
                return Some(delay);
            }
        }
    }

    let delay = if let Ok(json) = serde_json::from_str(error_text) {
        extract_structured_delay_recursive(&json, 0, true)
    } else {
        None
    };

    delay.map(|delay_ms| delay_ms.saturating_add(1500))
}

/// Recursively extract a structured delay
fn extract_structured_delay_recursive(
    value: &serde_json::Value,
    depth: usize,
    parse_unkeyed_strings: bool,
) -> Option<u64> {
    if depth > 8 {
        return None;
    }

    match value {
        serde_json::Value::Object(map) => {
            // Check whether this object is itself a Duration object (seconds/nanos)
            if let Some(d) = parse_structured_duration_object(value) {
                return Some(d);
            }

            // Recursively scan child fields
            for (key, val) in map {
                // Fuzzy key match (lowercased, separators stripped)
                let normalized_key = key.to_lowercase().replace('-', "").replace('_', "");
                if RETRY_HINT_KEYS.contains(normalized_key.as_str()) {
                    // On a hint-key hit, try parsing its content directly
                    if let Some(d) = parse_structured_duration_value(val) {
                        return Some(d);
                    }
                }
                // Continue searching deeper
                if let Some(d) =
                    extract_structured_delay_recursive(val, depth + 1, parse_unkeyed_strings)
                {
                    return Some(d);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for val in arr {
                if let Some(d) =
                    extract_structured_delay_recursive(val, depth + 1, parse_unkeyed_strings)
                {
                    return Some(d);
                }
            }
        }
        serde_json::Value::String(s) if parse_unkeyed_strings => return parse_duration_ms(s),
        serde_json::Value::String(_) => {}
        _ => {}
    }
    None
}

/// Parse a strongly typed Duration object (Google format: {seconds: 1, nanos: 0})
fn parse_structured_duration_object(value: &serde_json::Value) -> Option<u64> {
    let obj = value.as_object()?;
    let seconds = obj
        .get("seconds")
        .or_else(|| obj.get("Seconds"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let nanos = obj
        .get("nanos")
        .or_else(|| obj.get("Nanos"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if seconds > 0.0 || nanos > 0.0 {
        let total_ms = (seconds * 1000.0) + (nanos / 1_000_000.0);
        return Some(total_ms.round() as u64);
    }
    None
}

/// Parse any Value that may carry duration information
fn parse_structured_duration_value(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::String(s) => parse_duration_ms(s),
        serde_json::Value::Number(n) => n.as_f64().map(|f| (f * 1000.0).round() as u64),
        serde_json::Value::Object(_) => parse_structured_duration_object(value),
        _ => None,
    }
}

/// [NEW] Decide whether to perform a grace retry (retry in place)
/// When the reset time reported by a 429 is within 5s, retrying in place beats rotating accounts.
pub fn should_grace_retry(duration_ms: u64) -> bool {
    duration_ms > 0 && duration_ms <= 5000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_ms() {
        assert_eq!(parse_duration_ms("1.5s"), Some(1500));
        assert_eq!(parse_duration_ms("200ms"), Some(200));
        assert_eq!(parse_duration_ms("1h16m0.667s"), Some(4560667));
        assert_eq!(parse_duration_ms("58h24m53s"), Some(210_293_000));
        assert_eq!(parse_duration_ms("72h"), Some(259_200_000));
        assert_eq!(parse_duration_ms("invalid"), None);
    }

    #[test]
    fn task_parses_google_retry_info_and_retry_after() {
        let retry_info_1s = r#"{
            "error": {
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "1s"
                }]
            }
        }"#;
        let retry_info_3s = r#"{
            "error": {
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.RetryInfo",
                    "retryDelay": "3s"
                }]
            }
        }"#;

        let structured_1s = parse_retry_delay_with_source(retry_info_1s, None).unwrap();
        assert_eq!(structured_1s.raw_ms, 1000);
        assert_eq!(structured_1s.source, RetryDelaySource::Structured);
        assert_eq!(structured_1s.actual_wait_ms(), 1200);

        let structured_3s = parse_retry_delay_with_source(retry_info_3s, None).unwrap();
        assert_eq!(structured_3s.actual_wait_ms(), 3200);

        let quota_reset = parse_retry_delay_with_source(
            r#"{"error":{"details":[{"metadata":{"quotaResetDelay":"5s"}}]}}"#,
            None,
        )
        .unwrap();
        assert_eq!(quota_reset.actual_wait_ms(), 5200);
        assert!(should_grace_retry(quota_reset.raw_ms));

        let retry_after = parse_retry_delay_with_source("", Some("3")).unwrap();
        assert_eq!(retry_after.actual_wait_ms(), 3200);

        let response_text = parse_retry_delay_with_source("quota reset after 3s", None).unwrap();
        assert_eq!(response_text.source, RetryDelaySource::ResponseText);
        assert_eq!(response_text.actual_wait_ms(), 4000);
    }
}
