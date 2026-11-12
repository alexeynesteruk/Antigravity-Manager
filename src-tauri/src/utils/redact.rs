//! Credential redaction at diagnostic boundaries. Never use this on live requests.
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::borrow::Cow;

static ASSIGNMENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
    r#"(?ix)\b(?P<key>access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|proxy-authorization|(?:x-)?api[_-]?key|x-goog-api-key|client[_-]?secret|admin[_-]?password|password|secret|tunnel[_-]?token|token|cookie|set-cookie|code|key)\b(?:\\?["'])?[ \t]*[:=][ \t]*"#
).unwrap()
});
static USERINFO: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\b((?:https?|socks[45]h?)://)[^/\s?#]+@").unwrap());
static BARE_USERINFO: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[^/\s:@]+:[^/\s?#]+@([a-zA-Z0-9.\-]+|\[[0-9a-fA-F:]+\])").unwrap());
static AUTH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)\b(bearer|basic)[ \t]+[^\s"'\\,;}\]]+"#).unwrap());
static GOOGLE_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(?:ya29\.[A-Za-z0-9._~\-]+|1//[A-Za-z0-9._~\-]+|GOCSPX-[A-Za-z0-9_\-]+|AIza[A-Za-z0-9_\-]{20,})").unwrap()
});
static ESCAPED_IDENTIFIER: Lazy<Regex> = Lazy::new(|| Regex::new(r"\\+u([0-9a-fA-F]{4})").unwrap());

fn wrapped_value_end(value: &str) -> Option<usize> {
    let opening = value.find(['(', '[', '{'])?;
    if opening > 64
        || !value[..opening]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_:<> ".contains(&c))
    {
        return None;
    }
    let mut stack = Vec::new();
    let mut quoted = None;
    let mut escaped = false;
    for (offset, byte) in value.as_bytes()[opening..].iter().copied().enumerate() {
        if let Some(delimiter) = quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quoted = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quoted = Some(byte),
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'{' => stack.push(b'}'),
            b')' | b']' | b'}' => {
                if stack.pop() != Some(byte) {
                    return Some(value.len());
                }
                if stack.is_empty() {
                    return Some(opening + offset + 1);
                }
            }
            b'\r' | b'\n' if stack.is_empty() => return Some(opening + offset),
            _ => {}
        }
        if stack.len() > 64 {
            return Some(value.len());
        }
    }
    Some(value.len())
}

pub fn is_secret_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('-', "_");
    let compact = name.replace('_', "");
    if matches!(
        compact.as_str(),
        "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "clientsecret"
            | "adminpassword"
            | "tunneltoken"
            | "xauthtoken"
            | "xaccesstoken"
    ) || compact.ends_with("apikey")
    {
        return true;
    }
    matches!(
        name.as_str(),
        "authorization"
            | "proxy_authorization"
            | "cookie"
            | "set_cookie"
            | "password"
            | "admin_password"
            | "secret"
            | "client_secret"
            | "api_key"
            | "apikey"
            | "key"
            | "token"
    ) || name.ends_with("_api_key")
        || name.ends_with("_apikey")
        || name.ends_with("_access_token")
        || name.ends_with("_refresh_token")
        || name.ends_with("_id_token")
        || name.ends_with("_auth_token")
        || name.ends_with("_token")
        || matches!(
            name.as_str(),
            "access_token" | "refresh_token" | "id_token" | "tunnel_token"
        )
}

pub fn field(name: &str, value: &str) -> String {
    if is_secret_field(name) && !value.is_empty() {
        "<redacted>".into()
    } else {
        text(value).into_owned()
    }
}

/// Redacts quoted JSON, header/assignment values, URL userinfo and known token forms.
/// Escaped quoted diagnostics are conservatively removed through the line end.
pub fn text(input: &str) -> Cow<'_, str> {
    // Serialized tool outputs can escape the credential's property name itself.
    // Decode only ASCII identifier characters, never separators or newlines.
    let normalized = ESCAPED_IDENTIFIER.replace_all(input, |captures: &regex::Captures<'_>| {
        let character = u32::from_str_radix(&captures[1], 16)
            .ok()
            .and_then(char::from_u32);
        match character.filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
            Some(character) => character.to_string(),
            None => captures[0].to_string(),
        }
    });
    let redacted = redact_normalized(&normalized);
    if normalized.as_ref() == input && redacted.as_ref() == input {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(redacted.into_owned())
    }
}

fn redact_normalized(input: &str) -> Cow<'_, str> {
    let mut output = String::new();
    let mut copied = 0;
    for captures in ASSIGNMENT.captures_iter(input) {
        let matched = captures.get(0).unwrap();
        if matched.start() < copied {
            continue;
        }
        let rest = &input[matched.end()..];
        if rest.is_empty() {
            continue;
        }
        let key = captures.name("key").unwrap().as_str();
        let mut quoted = false;
        let consumed = if rest.starts_with("\\\"") || rest.starts_with("\\'") {
            rest.find(['\r', '\n']).unwrap_or(rest.len())
        } else if let Some(end) = wrapped_value_end(rest) {
            end
        } else if matches!(rest.as_bytes()[0], b'"' | b'\'') {
            quoted = true;
            let delimiter = rest.as_bytes()[0];
            let mut end = 1;
            while end < rest.len() {
                match rest.as_bytes()[end] {
                    b'\\' => end = (end + 2).min(rest.len()),
                    value if value == delimiter => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            end
        } else if matches!(
            key.to_ascii_lowercase().as_str(),
            "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
        ) {
            rest.find(['\r', '\n']).unwrap_or(rest.len())
        } else {
            rest.find(|c: char| {
                c.is_whitespace() || matches!(c, ',' | ';' | '}' | ']' | '"' | '\'' | '&' | '#')
            })
            .unwrap_or(rest.len())
        };
        if consumed == 0 {
            continue;
        }
        if key.eq_ignore_ascii_case("code")
            && operational_code(rest[..consumed].trim_matches(['\'', '"']))
        {
            continue;
        }
        output.push_str(&input[copied..matched.end()]);
        output.push_str(if quoted {
            "\"<redacted>\""
        } else {
            "<redacted>"
        });
        copied = matched.end() + consumed;
    }
    let assigned: Cow<'_, str> = if copied == 0 {
        Cow::Borrowed(input)
    } else {
        output.push_str(&input[copied..]);
        Cow::Owned(output)
    };
    let urls = USERINFO.replace_all(&assigned, "$1<redacted>@");
    let authorities = BARE_USERINFO.replace_all(&urls, "<redacted>@$1");
    let auth = AUTH.replace_all(&authorities, "$1 <redacted>");
    let tokens = GOOGLE_TOKEN.replace_all(&auth, "<redacted>");
    if assigned.as_ref() == tokens.as_ref() {
        assigned
    } else {
        Cow::Owned(tokens.into_owned())
    }
}

fn operational_code(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase() || byte == b'_')
}

/// Preserve JSON structure while removing credential-bearing fields and string fragments.
pub fn json(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    let sensitive_code = key.eq_ignore_ascii_case("code")
                        && value.as_str().is_some_and(|value| !operational_code(value));
                    let clean = if (is_secret_field(key) || sensitive_code) && !value.is_null() {
                        Value::String("<redacted>".into())
                    } else {
                        json(value)
                    };
                    (key.clone(), clean)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(json).collect()),
        Value::String(value) => Value::String(text(value).into_owned()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hides_json_header_query_and_proxy_credentials() {
        for source in [
            r#"{"access_token":"private-access","refresh_token":"private-refresh","client_secret":"private-client"}"#,
            "Authorization: Bearer private-auth\nCookie: sid=private-cookie; other=private-session",
            "https://alice:private-password@proxy.test:8080/path?key=private-key&alt=sse",
            "alice:private-bare-password@proxy.test:8080",
            "http://alice:private@private-more@proxy.test:8080",
            r#"{\"client_secret\":\"private-escaped\",\"status\":403}"#,
            "refresh_token='private with spaces' status=403",
            "failed: ya29.private-access or 1//private-refresh or GOCSPX-private-client",
            r#"password=Some("private-option") other=value"#,
            r#"Token { id_token: Some("private-jwt"), other: 42 }"#,
            r#"{\"refresh\u005ftoken\":\"private-unicode-property\"}"#,
        ] {
            let clean = text(source);
            assert!(
                !clean.contains("private"),
                "credential survived redaction: {clean}"
            );
        }
    }
    #[test]
    fn preserves_operational_fields_and_valid_json() {
        let source = serde_json::json!({"session_id":"conversation-123", "status":403,
            "error":{"code":403,"refresh_token":"private","message":"Validation required"}, "input_tokens":42,
            "accessToken":"private","refreshToken":"private","clientSecret":"private"});
        let clean = json(&source);
        assert_eq!(clean["session_id"], source["session_id"]);
        assert_eq!(clean["input_tokens"], 42);
        assert_eq!(clean["error"]["message"], "Validation required");
        assert_eq!(clean["error"]["code"], 403);
        assert_eq!(
            text("code=403 reason=PERMISSION_DENIED"),
            "code=403 reason=PERMISSION_DENIED"
        );
        assert_eq!(
            text(r#"{"code":"PERMISSION_DENIED"}"#),
            r#"{"code":"PERMISSION_DENIED"}"#
        );
        assert!(!clean.to_string().contains("private"));
        let plain = text(r#"{"refresh_token":"private","status":403}"#);
        assert_eq!(
            serde_json::from_str::<Value>(&plain).unwrap()["status"],
            403
        );
    }
    #[test]
    fn handles_unicode_and_long_secrets_without_crossing_lines() {
        let source = format!("refresh_token=秘密{}\nnext=保持", "a".repeat(16_384));
        let clean = text(&source);
        assert_eq!(clean, "refresh_token=<redacted>\nnext=保持");
        assert_eq!(text("ordinary model progress"), "ordinary model progress");
    }
}
