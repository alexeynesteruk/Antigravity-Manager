#[cfg(test)]
mod tests {
    use crate::proxy::mappers::claude::models::{
        ClaudeRequest, ContentBlock, Message, MessageContent, ThinkingConfig,
    };
    use crate::proxy::mappers::claude::request::transform_claude_request_in;
    use crate::proxy::mappers::claude::thinking_utils::{
        analyze_conversation_state, close_tool_loop_for_thinking,
    };
    use serde_json::json;

    // ==================================================================================
    // Scenario 1: first Thinking request (P0-2 Fix)
    // Verifies a first Thinking request is allowed through when there is no signature history (Permissive Mode)
    // ==================================================================================
    #[test]
    fn test_first_thinking_request_permissive_mode() {
        // 1. Construct a brand-new request (no message history)
        let req = ClaudeRequest {
            model: "claude-3-7-sonnet-20250219".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: MessageContent::String("Hello, please think.".to_string()),
            }],
            system: None,
            tools: None, // no tool calls
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: Some(ThinkingConfig {
                type_: "enabled".to_string(),
                budget_tokens: Some(1024),
                effort: None,
            }),
            metadata: None,
            output_config: None,
            size: None,
            quality: None,
        };

        // 2. Execute the transform
        // If the fix is effective, this should succeed and thinkingConfig should be preserved
        let result =
            transform_claude_request_in(&req, "test-project", false, None, "test_session", None);
        assert!(result.is_ok(), "First thinking request should be allowed");

        let body = result.unwrap();
        let request = &body["request"];

        // Verify thinkingConfig exists (i.e. thinking mode was not disabled)
        let has_thinking_config = request
            .get("generationConfig")
            .and_then(|g| g.get("thinkingConfig"))
            .is_some();

        assert!(
            has_thinking_config,
            "Thinking config should be preserved for first request without tool calls"
        );
    }

    // ==================================================================================
    // Scenario 2: tool loop recovery (P1-4 Fix)
    // Verifies that a synthetic message is auto-injected to close the loop when a missing Thinking block in history causes a deadlock
    // ==================================================================================
    #[test]
    fn test_tool_loop_recovery() {
        // 1. Construct a "Broken Tool Loop" scenario
        // Assistant (ToolUse) -> User (ToolResult)
        // but the Assistant message is missing a Thinking block (simulating it being stripped)
        let mut messages = vec![
            Message {
                role: "user".to_string(),
                content: MessageContent::String("Check weather".to_string()),
            },
            Message {
                role: "assistant".to_string(),
                content: MessageContent::Array(vec![
                    // Only ToolUse, no Thinking (broken state)
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "get_weather".to_string(),
                        input: json!({"location": "Beijing"}),
                        signature: None,
                        cache_control: None,
                    },
                ]),
            },
            Message {
                role: "user".to_string(),
                content: MessageContent::Array(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: json!("Sunny"),
                    is_error: None,
                }]),
            },
        ];

        // 2. Analyze the current state
        let state = analyze_conversation_state(&messages);
        assert!(state.in_tool_loop, "Should detect tool loop");

        // 3. Execute the recovery logic
        close_tool_loop_for_thinking(&mut messages);

        // 4. Verify synthetic messages were injected
        assert_eq!(
            messages.len(),
            5,
            "Should have injected 2 synthetic messages"
        );

        // Verify the second-to-last message is the Assistant's "Completed" message
        let injected_assistant = &messages[3];
        assert_eq!(injected_assistant.role, "assistant");

        // Verify the last message is the User's "Proceed" message
        let injected_user = &messages[4];
        assert_eq!(injected_user.role, "user");

        // This way the current state is no longer "in_tool_loop" (the last message is User Text), and the model can start new Thinking
        let new_state = analyze_conversation_state(&messages);
        assert!(!new_state.in_tool_loop, "Tool loop should be broken/closed");
    }

    // ==================================================================================
    // Scenario 3: cross-model compatibility (P1-5 Fix) - simulated
    // Since is_model_compatible in request.rs is private, we verify its effect via integration tests
    // ==================================================================================
    /*
       Note: since is_model_compatible and the caching logic are deeply integrated into transform_claude_request_in,
       and depend on the global SignatureCache singleton, a unit test can hardly simulate the state of "a stale
       cached signature after switching models". This is mainly tested by verifying the side effect of "the
       incompatible signature is discarded" (i.e. the thoughtSignature field being dropped from the message).
       But because SignatureCache is global, we can't easily pre-seed its state in a test.
       So this scenario mostly relies on manual testing per the Verification Guide.
       Alternatively, we could test some helper publicly exposed by request.rs (if one existed), but none does yet.
    */
}
