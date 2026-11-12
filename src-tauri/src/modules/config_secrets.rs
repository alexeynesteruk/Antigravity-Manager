//! Native-store references for gateway, tunnel and proxy credentials.
use super::credential_store::{self, VaultRef};
use serde_json::{Map, Value};
use std::path::Path;

const SCOPE: &str = "config";
const OWNER: &str = "app_config";
const POINTERS: &[&str] = &[
    "/proxy/api_key",
    "/proxy/admin_password",
    "/cloudflared/token",
];

fn protected_url(raw: &str) -> Result<Option<Value>, String> {
    if !raw.contains('@') && !raw.contains('?') {
        return Ok(None);
    }
    let normalized = crate::proxy::config::normalize_proxy_url(raw);
    let mut url =
        rquest::Url::parse(&normalized).map_err(|_| "Invalid credential-bearing proxy URL")?;
    if url.username().is_empty() && url.password().is_none() && url.query().is_none() {
        return Ok(None);
    }
    url.set_username("")
        .map_err(|_| "Invalid proxy URL username")?;
    url.set_password(None)
        .map_err(|_| "Invalid proxy URL password")?;
    url.set_query(None);
    Ok(Some(
        serde_json::json!({"original":raw,"visible":url.as_str()}),
    ))
}

fn hide_url(field: &mut Value) -> Result<Option<Value>, String> {
    let Some(raw) = field.as_str() else {
        return Ok(None);
    };
    let protected = protected_url(raw)?;
    if let Some(record) = &protected {
        *field = record["visible"].clone();
    }
    Ok(protected)
}

fn restore_url(field: &mut Value, record: &Value) -> Result<(), String> {
    let original = record
        .get("original")
        .and_then(Value::as_str)
        .ok_or("Invalid protected proxy URL")?;
    let visible = record
        .get("visible")
        .and_then(Value::as_str)
        .ok_or("Invalid protected proxy URL")?;
    // A new host/address in the public configuration is an explicit override.
    if field.as_str() == Some(visible) {
        *field = Value::String(original.into());
    }
    Ok(())
}

fn reference(document: &Value) -> Result<Option<VaultRef>, String> {
    document
        .get("credential_store")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| "Invalid configuration credential-store reference".to_string())
        })
        .transpose()
}

pub(super) fn has_unprotected_fields(document: &Value) -> bool {
    POINTERS.iter().any(|pointer| {
        document
            .pointer(pointer)
            .is_some_and(|value| !value.is_null())
    }) || document
        .pointer("/proxy/upstream_proxy/url")
        .and_then(Value::as_str)
        .is_some_and(|url| protected_url(url).map_or(true, |record| record.is_some()))
        || document
            .pointer("/proxy/proxy_pool/proxies")
            .and_then(Value::as_array)
            .is_some_and(|proxies| {
                proxies.iter().any(|proxy| {
                    proxy.get("auth").is_some_and(|auth| !auth.is_null())
                        || proxy.get("url").and_then(Value::as_str).is_some_and(|url| {
                            protected_url(url).map_or(true, |record| record.is_some())
                        })
                })
            })
}

pub(super) fn hydrate(document: &mut Value) -> Result<(), String> {
    let Some(reference) = reference(document)? else {
        return Ok(());
    };
    let secrets = credential_store::load_json(SCOPE, OWNER, &reference)?;
    let values = secrets
        .get("fields")
        .and_then(Value::as_object)
        .ok_or("Invalid protected configuration credentials")?;
    for (pointer, value) in values {
        if !POINTERS.contains(&pointer.as_str()) {
            return Err("Unexpected protected configuration field".into());
        }
        let field = document
            .pointer_mut(pointer)
            .ok_or("Missing protected configuration field")?;
        // A manually supplied non-null value is an explicit config-file override.
        if field.is_null() {
            *field = value.clone();
        }
    }
    if let Some(url) = secrets.get("upstream_url").filter(|value| !value.is_null()) {
        let field = document
            .pointer_mut("/proxy/upstream_proxy/url")
            .ok_or("Missing proxy URL configuration")?;
        restore_url(field, url)?;
    }
    let records = secrets
        .get("proxies")
        .and_then(Value::as_object)
        .ok_or("Invalid protected proxy credentials")?;
    if let Some(proxies) = document
        .pointer_mut("/proxy/proxy_pool/proxies")
        .and_then(Value::as_array_mut)
    {
        let mut ids = std::collections::HashSet::new();
        for proxy in proxies {
            let id = proxy
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Missing proxy identity")?
                .to_string();
            if !ids.insert(id.clone()) {
                return Err("Duplicate proxy identity".into());
            }
            if let Some(record) = records.get(&id) {
                if let Some(auth) = record.get("auth") {
                    if proxy.get("auth").map_or(true, Value::is_null) {
                        proxy["auth"] = auth.clone();
                    }
                }
                if let Some(url) = record.get("url") {
                    restore_url(proxy.get_mut("url").ok_or("Missing proxy URL")?, url)?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn persist(path: &Path, mut document: Value) -> Result<(), String> {
    let mut secrets = Map::new();
    for pointer in POINTERS {
        if let Some(value) = document.pointer_mut(pointer) {
            secrets.insert((*pointer).into(), value.take());
        }
    }
    let mut proxy_credentials = Map::new();
    if let Some(proxies) = document
        .pointer_mut("/proxy/proxy_pool/proxies")
        .and_then(Value::as_array_mut)
    {
        let mut ids = std::collections::HashSet::new();
        for proxy in proxies {
            let id = proxy
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Missing proxy identity")?
                .to_string();
            if !ids.insert(id.clone()) {
                return Err("Duplicate proxy identity".into());
            }
            let mut record = Map::new();
            if let Some(auth) = proxy.get_mut("auth").filter(|auth| !auth.is_null()) {
                record.insert("auth".into(), auth.take());
            }
            if let Some(url) = proxy.get_mut("url") {
                if let Some(hidden) = hide_url(url)? {
                    record.insert("url".into(), hidden);
                }
            }
            if !record.is_empty() {
                proxy_credentials.insert(id, Value::Object(record));
            }
        }
    }
    let upstream = match document.pointer_mut("/proxy/upstream_proxy/url") {
        Some(field) => hide_url(field)?,
        None => None,
    };
    let secrets =
        serde_json::json!({"fields":secrets,"proxies":proxy_credentials,"upstream_url":upstream});
    let old_reference = match std::fs::read(path) {
        // Explicit saves receive a fully validated config and retain the existing
        // ability to initialize an empty file or replace a malformed document.
        // Unknown retired references are left intact rather than guessed/deleted.
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(old) => reference(&old)?,
            Err(_) => None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Failed to read configuration metadata: {error}")),
    };
    let reused = old_reference.as_ref().is_some_and(|reference| {
        credential_store::load_json(SCOPE, OWNER, reference).is_ok_and(|old| old == secrets)
    });
    let next = if reused {
        old_reference.clone().unwrap()
    } else {
        credential_store::store_json(SCOPE, OWNER, &secrets)?
    };
    document["credential_store"] = serde_json::to_value(&next)
        .map_err(|_| "Failed to encode configuration credential reference")?;
    let saved = serde_json::to_vec_pretty(&document)
        .map_err(|_| "Failed to encode protected configuration".to_string())
        .and_then(|bytes| crate::utils::fs::write_private_atomic(path, &bytes));
    if let Err(error) = saved {
        if !reused {
            let _ = credential_store::delete(SCOPE, &next);
        }
        return Err(error);
    }
    if !reused {
        if let Some(old) = old_reference {
            if let Err(error) = credential_store::delete(SCOPE, &old) {
                tracing::warn!(
                    "Unable to remove retired configuration credential generation: {error}"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_secrets_roundtrip_without_plaintext_and_reuse_unchanged_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui_config.json");
        let config = serde_json::json!({"proxy":{"port":8045,"api_key":"private-api",
            "admin_password":"private-admin","upstream_proxy":{"enabled":true,"url":"http://u:private-proxy@proxy.test"},
            "proxy_pool":{"enabled":true,"strategy":"round_robin","proxies":[{"id":"p1","url":"http://u:private-url@pool.test","auth":{"password":"private-pool"}}]}},
            "cloudflared":{"token":"private-tunnel"}});
        persist(&path, config.clone()).unwrap();
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("private"));
        let mut restored: Value = serde_json::from_str(&bytes).unwrap();
        let initial = restored["credential_store"].clone();
        assert_eq!(restored["proxy"]["proxy_pool"]["enabled"], true);
        assert_eq!(restored["proxy"]["proxy_pool"]["strategy"], "round_robin");
        hydrate(&mut restored).unwrap();
        restored.as_object_mut().unwrap().remove("credential_store");
        assert_eq!(restored, config);
        persist(&path, restored).unwrap();
        let same: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(same["credential_store"], initial);
        let mut unavailable = same.clone();
        credential_store::delete(SCOPE, &reference(&same).unwrap().unwrap()).unwrap();
        assert!(hydrate(&mut unavailable).is_err());
    }
    #[test]
    fn explicit_file_overrides_survive_hydration_and_native_credential_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui_config.json");
        persist(
            &path,
            serde_json::json!({"proxy":{"api_key":"old-private","port":8045}}),
        )
        .unwrap();
        let mut edited: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edited["proxy"]["api_key"] = serde_json::json!("replacement-private");
        edited["proxy"]["port"] = serde_json::json!(9000);
        hydrate(&mut edited).unwrap();
        assert_eq!(edited["proxy"]["api_key"], "replacement-private");
        persist(&path, edited).unwrap();
        let mut loaded: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        hydrate(&mut loaded).unwrap();
        assert_eq!(loaded["proxy"]["api_key"], "replacement-private");
        assert_eq!(loaded["proxy"]["port"], 9000);
    }

    #[test]
    fn proxy_credentials_follow_identity_across_reordering_and_public_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui_config.json");
        let original = serde_json::json!({"proxy":{"proxy_pool":{"enabled":true,"proxies":[
            {"id":"a","url":"http://alice:private-a@one.test:8080","auth":{"password":"first"}},
            {"id":"b","url":"http://two.test:8080","auth":{"password":"second"}}]}}});
        persist(&path, original).unwrap();
        let mut edited: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edited["proxy"]["proxy_pool"]["proxies"]
            .as_array_mut()
            .unwrap()
            .reverse();
        edited["proxy"]["proxy_pool"]["enabled"] = Value::Bool(false);
        hydrate(&mut edited).unwrap();
        assert_eq!(
            edited["proxy"]["proxy_pool"]["proxies"][0]["auth"]["password"],
            "second"
        );
        assert_eq!(
            edited["proxy"]["proxy_pool"]["proxies"][1]["url"],
            "http://alice:private-a@one.test:8080"
        );
        assert_eq!(edited["proxy"]["proxy_pool"]["enabled"], false);
    }
}
