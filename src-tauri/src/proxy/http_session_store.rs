// HTTP session history store
// Provides previous_response_id chained history support for /v1/responses POST
// This way multi-turn conversations work even when the client uses HTTP instead of WebSocket

use crate::proxy::handlers::openai::get_cached_tool_call;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const SESSION_TTL_SECS: u64 = 3600; // Expires after 1 hour

#[derive(Debug, Clone)]
pub struct HttpSessionEntry {
    /// Conversation history: instructions + all input items (including historical response output)
    pub input_items: Vec<Value>,
    /// System instructions
    pub instructions: String,
    /// Model name
    pub model: String,
    /// Last access time (used for TTL eviction)
    pub last_accessed: Instant,
}

#[derive(Debug)]
struct SessionNode {
    parent: Option<Arc<SessionNode>>,
    input_delta: Vec<Value>,
    response_output: Vec<Value>,
    instructions: String,
    model: String,
}

#[derive(Debug, Clone)]
pub struct SessionParent(Arc<SessionNode>);

struct StoredSession {
    node: Arc<SessionNode>,
    last_accessed: Instant,
}

struct HttpSessionStore {
    sessions: HashMap<String, StoredSession>,
}

impl HttpSessionStore {
    fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    fn get(&mut self, response_id: &str) -> Option<(HttpSessionEntry, SessionParent)> {
        let stored = self.sessions.get_mut(response_id)?;
        stored.last_accessed = Instant::now();
        let node = stored.node.clone();
        Some((
            HttpSessionEntry {
                input_items: materialize_history(&node),
                instructions: node.instructions.clone(),
                model: node.model.clone(),
                last_accessed: stored.last_accessed,
            },
            SessionParent(node),
        ))
    }

    fn insert(&mut self, response_id: String, entry: HttpSessionEntry) {
        self.insert_delta(
            response_id,
            None,
            entry.input_items,
            Vec::new(),
            entry.instructions,
            entry.model,
        );
    }

    fn insert_delta(
        &mut self,
        response_id: String,
        parent: Option<SessionParent>,
        input_delta: Vec<Value>,
        response_output: Vec<Value>,
        instructions: String,
        model: String,
    ) {
        self.sessions.insert(
            response_id,
            StoredSession {
                node: Arc::new(SessionNode {
                    parent: parent.map(|parent| parent.0),
                    input_delta,
                    response_output,
                    instructions,
                    model,
                }),
                last_accessed: Instant::now(),
            },
        );
        // Also evict expired sessions (lazy cleanup)
        self.evict_expired();
    }

    fn evict_expired(&mut self) {
        let ttl = Duration::from_secs(SESSION_TTL_SECS);
        self.sessions
            .retain(|_, stored| stored.last_accessed.elapsed() < ttl);
    }
}

fn materialize_history(node: &Arc<SessionNode>) -> Vec<Value> {
    let mut chain = Vec::new();
    let mut current = Some(node.clone());
    while let Some(node) = current {
        chain.push(node.clone());
        current = node.parent.clone();
    }

    let capacity = chain
        .iter()
        .map(|node| node.input_delta.len() + node.response_output.len())
        .sum();
    let mut history = Vec::with_capacity(capacity);
    for node in chain.into_iter().rev() {
        history.extend(node.input_delta.iter().cloned());
        history.extend(node.response_output.iter().cloned());
    }
    history
}

static STORE: OnceLock<Mutex<HttpSessionStore>> = OnceLock::new();

fn store() -> &'static Mutex<HttpSessionStore> {
    STORE.get_or_init(|| Mutex::new(HttpSessionStore::new()))
}

/// Look up the historical session by previous_response_id
pub async fn get_session(previous_response_id: &str) -> Option<HttpSessionEntry> {
    store()
        .lock()
        .await
        .get(previous_response_id)
        .map(|(entry, _)| entry)
}

pub async fn get_session_with_parent(
    previous_response_id: &str,
) -> Option<(HttpSessionEntry, SessionParent)> {
    store().lock().await.get(previous_response_id)
}

/// Save the new session state (keyed by response_id)
pub async fn save_session(response_id: String, entry: HttpSessionEntry) {
    store().lock().await.insert(response_id, entry);
}

/// Save this round's Responses delta; the parent's strong Arc reference ensures branches share ancestors.
pub async fn save_session_delta(
    response_id: String,
    parent: Option<SessionParent>,
    input_delta: Vec<Value>,
    response_output: Vec<Value>,
    instructions: String,
    model: String,
) {
    store().lock().await.insert_delta(
        response_id,
        parent,
        input_delta,
        response_output,
        instructions,
        model,
    );
}

pub struct PreparedSessionInput {
    pub merged: Vec<Value>,
    pub delta: Vec<Value>,
    pub reset_parent: bool,
}

/// Merge request history, extracting only the newly added items when the client replays the full history.
pub fn prepare_session_input(
    history: Vec<Value>,
    new_input: Vec<Value>,
    tool_call_cache: &HashMap<String, Value>,
) -> PreparedSessionInput {
    let reset_parent = new_input.iter().any(|item| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some("compaction") | Some("compaction_summary")
        )
    });
    let exact_replay = !history.is_empty() && new_input.starts_with(&history);
    let replayed_through = if reset_parent || exact_replay {
        None
    } else {
        let history_ids: std::collections::HashSet<&str> = history
            .iter()
            .filter_map(|item| item.get("id").and_then(Value::as_str))
            .filter(|id| !id.is_empty())
            .collect();
        new_input.iter().rposition(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| history_ids.contains(id))
        })
    };
    // Helper: check whether two input items are semantically equivalent (ignoring volatile
    // fields such as id).
    let items_semantically_equal = |a: &Value, b: &Value| -> bool {
        let role_a = a.get("role").and_then(Value::as_str);
        let role_b = b.get("role").and_then(Value::as_str);
        let type_a = a.get("type").and_then(Value::as_str);
        let type_b = b.get("type").and_then(Value::as_str);
        let content_a = a.get("content").or_else(|| a.get("text"));
        let content_b = b.get("content").or_else(|| b.get("text"));
        role_a == role_b && type_a == type_b && content_a == content_b
    };

    // Semantic prefix match: does new_input start with history, ignoring volatile fields?
    let semantic_prefix_match = !history.is_empty()
        && new_input.len() >= history.len()
        && history
            .iter()
            .zip(new_input.iter())
            .all(|(h, n)| items_semantically_equal(h, n));

    // Semantic suffix find: locate the last history item inside new_input by content.
    let semantic_suffix_idx = if !history.is_empty()
        && !reset_parent
        && !exact_replay
        && replayed_through.is_none()
        && !semantic_prefix_match
    {
        let last_h = &history[history.len() - 1];
        new_input
            .iter()
            .rposition(|n| items_semantically_equal(last_h, n))
    } else {
        None
    };

    let (delta_source, use_new_input_as_merged) = if reset_parent || history.is_empty() {
        (new_input.clone(), false)
    } else if exact_replay {
        (new_input[history.len()..].to_vec(), false)
    } else if semantic_prefix_match {
        (new_input[history.len()..].to_vec(), false)
    } else if let Some(index) = replayed_through {
        (new_input[index + 1..].to_vec(), false)
    } else if let Some(index) = semantic_suffix_idx {
        (new_input[index + 1..].to_vec(), false)
    } else if new_input.len() >= history.len() {
        // [FIX #3382] Fallback protection: the client sent a full conversation history but
        // formatting/id differences meant no boundary could be identified. Appending all of
        // new_input onto history here would double (or quadruple, across repeated turns) the
        // stored history. Instead treat new_input as the authoritative current history and
        // extract only its last element as the delta.
        tracing::warn!(
            "[Session] Match failed but new_input (len: {}) >= history (len: {}). Preventing history duplication.",
            new_input.len(),
            history.len()
        );
        let delta_slice = if new_input.is_empty() {
            Vec::new()
        } else {
            vec![new_input.last().unwrap().clone()]
        };
        (delta_slice, true)
    } else {
        (new_input.clone(), false)
    };

    let delta = merge_history_with_new_input(Vec::new(), &[], delta_source, tool_call_cache);
    let merged = if reset_parent || history.is_empty() {
        delta.clone()
    } else if use_new_input_as_merged {
        merge_history_with_new_input(Vec::new(), &[], new_input, tool_call_cache)
    } else {
        merge_history_with_new_input(history, &[], delta.clone(), tool_call_cache)
    };

    PreparedSessionInput {
        merged,
        delta,
        reset_parent,
    }
}

/// Convert the previous round's response output items into input items and append them to history
/// Also append the new user input items
/// Returns the merged input items
pub fn merge_history_with_new_input(
    mut history: Vec<Value>,
    response_output: &[Value],
    new_input: Vec<Value>,
    tool_call_cache: &HashMap<String, Value>,
) -> Vec<Value> {
    // Detect whether the new input contains compaction / compaction_summary; if so, the client is sending a compacted brand-new full history
    let has_compaction = new_input.iter().any(|item| {
        let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        t == "compaction" || t == "compaction_summary"
    });

    if has_compaction {
        tracing::info!(
            "[Session] Compaction detected in new input. Overwriting stale history (new items: {})",
            new_input.len()
        );
        // Filter out the compaction item itself
        let mut filtered = Vec::new();
        for item in new_input {
            let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if t == "compaction" || t == "compaction_summary" {
                continue;
            }
            filtered.push(item);
        }
        repair_tool_calls(&mut filtered, tool_call_cache);
        return dedupe_input_items(filtered);
    }

    // Append the previous round's response output (assistant messages, tool calls, etc.)
    for item in response_output {
        history.push(item.clone());
    }

    // Append the new input items
    for item in new_input {
        let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "compaction" || t == "compaction_summary" {
            continue;
        }
        history.push(item);
    }

    // Repair tool calls (ensure each function_call_output has a matching function_call before it)
    repair_tool_calls(&mut history, tool_call_cache);

    // Deduplicate
    dedupe_input_items(history)
}

fn repair_tool_calls(items: &mut Vec<Value>, tool_call_cache: &HashMap<String, Value>) {
    let mut call_present = std::collections::HashSet::new();
    for item in items.iter() {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call" || item_type == "custom_tool_call" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                call_present.insert(call_id.to_string());
            }
        }
    }

    let mut new_items = Vec::new();
    let mut inserted = std::collections::HashSet::new();
    for item in items.drain(..) {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call_output" || item_type == "custom_tool_call_output" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                if !call_id.is_empty()
                    && !call_present.contains(call_id)
                    && !inserted.contains(call_id)
                {
                    if let Some(cached_call) = tool_call_cache
                        .get(call_id)
                        .cloned()
                        .or_else(|| get_cached_tool_call(call_id))
                    {
                        new_items.push(cached_call.clone());
                        inserted.insert(call_id.to_string());
                    }
                }
            }
        }
        new_items.push(item);
    }
    *items = new_items;
}

fn dedupe_input_items(items: Vec<Value>) -> Vec<Value> {
    use std::collections::{HashMap, HashSet};
    let mut referenced_call_ids = HashSet::new();
    for item in &items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "function_call_output" || item_type == "custom_tool_call_output" {
            if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                if !call_id.is_empty() {
                    referenced_call_ids.insert(call_id.to_string());
                }
            }
        }
    }

    let mut keep_map: HashMap<String, usize> = HashMap::new();
    for (idx, item) in items.iter().enumerate() {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if item_id.is_empty() {
            continue;
        }
        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let is_referenced = !call_id.is_empty() && referenced_call_ids.contains(call_id);
        if let Some(&existing_idx) = keep_map.get(item_id) {
            let existing_call_id = items[existing_idx]
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let existing_referenced =
                !existing_call_id.is_empty() && referenced_call_ids.contains(existing_call_id);
            if is_referenced || !existing_referenced {
                keep_map.insert(item_id.to_string(), idx);
            }
        } else {
            keep_map.insert(item_id.to_string(), idx);
        }
    }

    let mut keep_indices = std::collections::HashSet::new();
    for (_, idx) in keep_map {
        keep_indices.insert(idx);
    }

    let mut filtered = Vec::new();
    for (idx, item) in items.into_iter().enumerate() {
        let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if !item_id.is_empty() && !keep_indices.contains(&idx) {
            continue;
        }
        filtered.push(item);
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(text: &str) -> HttpSessionEntry {
        HttpSessionEntry {
            input_items: vec![json!({
                "id": format!("msg-{text}"),
                "type": "message",
                "role": "user",
                "content": text
            })],
            instructions: "be concise".to_string(),
            model: "gemini-3.7-flash-high".to_string(),
            last_accessed: Instant::now(),
        }
    }

    #[test]
    fn session_chain_stores_delta_and_materializes_history() {
        let mut store = HttpSessionStore::new();
        store.insert("resp-1".to_string(), entry("first"));
        let (root, parent) = store.get("resp-1").expect("root");
        let mut replay = root.input_items.clone();
        replay.push(json!({"id": "msg-second", "content": "second"}));
        let prepared = prepare_session_input(root.input_items, replay, &HashMap::new());
        assert_eq!(prepared.delta.len(), 1);
        assert_eq!(prepared.merged.len(), 2);
        store.insert_delta(
            "resp-2".to_string(),
            Some(parent),
            prepared.delta,
            vec![json!({"id": "out-second", "content": "answer"})],
            "be concise".to_string(),
            "gemini-3.7-flash-high".to_string(),
        );

        let (previous, _) = store.get("resp-2").expect("child");
        assert_eq!(previous.input_items[0]["content"], "first");
        assert_eq!(previous.input_items.len(), 3);
        assert_eq!(store.sessions["resp-2"].node.input_delta.len(), 1);
        assert_eq!(store.sessions["resp-2"].node.response_output.len(), 1);
    }

    #[test]
    fn old_response_id_branches_share_parent() {
        let mut store = HttpSessionStore::new();
        store.insert("resp-root".to_string(), entry("root"));
        let (_, parent_a) = store.get("resp-root").expect("parent a");
        let (_, parent_b) = store.get("resp-root").expect("parent b");
        assert!(Arc::ptr_eq(&parent_a.0, &parent_b.0));

        store.insert_delta(
            "resp-a".to_string(),
            Some(parent_a),
            vec![json!({"content": "branch a"})],
            Vec::new(),
            String::new(),
            String::new(),
        );
        store.insert_delta(
            "resp-b".to_string(),
            Some(parent_b),
            vec![json!({"content": "branch b"})],
            Vec::new(),
            String::new(),
            String::new(),
        );

        let parent_a = store.sessions["resp-a"].node.parent.as_ref().unwrap();
        let parent_b = store.sessions["resp-b"].node.parent.as_ref().unwrap();
        assert!(Arc::ptr_eq(parent_a, parent_b));
    }

    #[test]
    fn prepare_session_input_prevents_duplication_on_full_history_replay_without_ids() {
        // [FIX #3382 regression test] Client resends full history with no "id" field: history
        // has 2 messages, new_input has the same 2 plus 1 new one.
        let history = vec![
            json!({"role": "user", "type": "message", "content": "hello"}),
            json!({"role": "assistant", "type": "message", "content": "hi there"}),
        ];
        let new_input = vec![
            json!({"role": "user", "type": "message", "content": "hello"}),
            json!({"role": "assistant", "type": "message", "content": "hi there"}),
            json!({"role": "user", "type": "message", "content": "next question"}),
        ];

        let prepared = prepare_session_input(history, new_input, &HashMap::new());
        // Delta should be the 1 new message, not all 3.
        assert_eq!(prepared.delta.len(), 1);
        assert_eq!(prepared.delta[0]["content"], "next question");
        // Merged history should be 3 messages, not 5 (2 + 3).
        assert_eq!(prepared.merged.len(), 3);
    }

    #[test]
    fn prepare_session_input_fallback_avoids_duplication_when_unmatched() {
        // [FIX #3382 regression test] Client sends a same-length but unrelated history (no
        // matching prefix, suffix, or replayed-through id) - the fallback must not append it
        // onto the stored history and double it.
        let history = vec![
            json!({"role": "user", "type": "message", "content": "msg 1"}),
            json!({"role": "assistant", "type": "message", "content": "msg 2"}),
        ];
        let new_input = vec![
            json!({"role": "user", "type": "message", "content": "different 1"}),
            json!({"role": "assistant", "type": "message", "content": "different 2"}),
        ];

        let prepared = prepare_session_input(history, new_input, &HashMap::new());
        assert_eq!(prepared.merged.len(), 2);
    }
}
