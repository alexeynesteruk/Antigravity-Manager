# Stability and Search Feature: Test Example Guide

To verify the recent fixes for the API 400 error and the "search file error", you can run the following instructions in the Claude CLI (Claude Code) for a live test.

## 1. Verify Search Tool Self-Healing (Grep/Glob Fix)

For the previous "Error searching files" issue, these instructions will trigger `Grep` and `Glob` tool calls and verify that parameter mapping is correct.

### Test Instruction Examples
*   **Instruction A**: `Search the current directory for Rust files containing "fn handle_messages".`
    *   *Verification point*: check whether the proxy correctly maps `query` to `pattern` and injects the default `path: "."`.
*   **Instruction B**: `List all .rs files under the src-tauri directory.`
    *   *Verification point*: verify that the `Glob` tool name is correctly recognized and that path filtering logic works normally.

---

## 2. Verify Protocol Ordering and Signature Stability (Thinking/Signature Fix)

For the previous `Found 'text'` and `Invalid signature` 400 errors.

### Test Instruction Examples
*   **Instruction A (reasoning + search)**: `Analyze the core logic in this project that handles cloud requests, summarize it in call order, and give Grep search evidence for the key lines of code.`
    *   *Verification point*: verify that block order is correct in the "thinking -> tool call -> result -> continued thinking" loop.
*   **Instruction B (history replay retry)**: switch models frequently in a long conversation and observe whether the system silently fixes the signature and retries on a 400 error.

---

## Appendix: Deep Error Reference and Fix Table

| Error Category | Specific Error Signature (Error Detail) | Fix/Handling Logic Applied by the Proxy |
| :--- | :--- | :--- |
| **Message stream ordering violation** | `If an assistant message contains any thinking blocks... Found 'text'.` | **Fixed**: `streaming.rs` no longer allows a thinking block to be illegally appended after a text block. |
| **Thinking signature mismatch** | `Invalid signature in thinking block` | **Fixed**: the original name is preserved with priority to protect Google backend signature validation. |
| **Thinking signature missing** | `Function call is missing a thought_signature` | **Fixed**: the `skip_thought_signature_validator` placeholder is auto-injected. |
| **Invalid cache marker** | `thinking.cache_control: Extra inputs are not permitted` | **Fixed**: the `cache_control` marker is globally stripped from historical messages. |
| **Plan Mode error**| `EnterPlanMode tool call: InputValidationError: Extra inputs are not permitted` | **Fixed**: `streaming.rs` forcibly clears tool arguments to comply with the official no-argument protocol. |
| **Consecutive User messages**| `Consecutive user messages are not allowed` | **Fixed**: `merge_consecutive_messages` automatically merges adjacent same-role messages. |

---

## 3. Verify Claude Code Plan Mode and Role Alternation (Issue #813)

For protocol errors caused by Plan Mode switching.

### A. Verify Plan Mode Activation (UI State)
*   **Instruction**: `Enter Plan Mode and investigate the directory structure of src-tauri.`
*   **Expected result**:
    *   A blue **`plan mode on`** badge should appear immediately at the bottom left of the terminal.
    *   The log should show `[Streaming] Tool Call: 'EnterPlanMode' Args: {}`.

### B. Verify Role Alternation Self-Healing (Consecutive Messages)
*   **Instruction**: `While in Plan Mode, help me analyze the logic of proxy/mappers/claude/request.rs, then exit Plan Mode and give a brief summary.`
*   **Expected result**:
    *   Switching modes (e.g. from Plan to Code) does not trigger a 400 error due to "two consecutive User messages".
    *   The log will show the merge action from `merge_consecutive_messages`.

---

## 4. QuotaData Field Logic Breakdown

The "Account Management" list on the settings page shows a progress bar whose data comes from `QuotaData`. The system checks account quota before each request and automatically rotates accounts once a threshold is hit.

---

## Debugging Tips
```bash
RUST_LOG=debug npm run tauri dev
```
Search the logs for `[Claude-Request]` and pay attention to the ordering of message roles.

---

## 5. Verify Thinking Signature Persistence and Restart Fault Tolerance (Proxy Restart Test)

This test simulates the main proxy service logic: verifying whether historical messages carrying an old signature cause a 400 error after the proxy restarts (losing the in-memory signature cache). This is the most effective way to reproduce `Invalid signature`.

### Test Flow
1.  **Generate Thinking (Step 1)**:
    *   **Instruction**: `Analyze in detail the code structure of proxy/mappers/claude/request.rs, especially how it handles the Thinking Block. Please show your thinking process.`
    *   *State*: the Claude CLI will receive a response containing Thinking and a Signature.

2.  **Simulate an Environment Change (Step 2)**:
    *   **Action**: **keep the current Claude CLI session open**.
    *   **Action**: fully restart Antigravity (or `npm run tauri dev`) in another terminal.
    *   *Rationale*: restarting clears the "signature allowlist" in the proxy's memory, meaning the signature issued in Step 1 is now "unknown/untrusted" to the proxy.

3.  **Trigger History Replay (Step 3)**:
    *   **Instruction**: `Based on the analysis above, summarize the core logic of signature validation.`
    *   *Rationale*: the CLI will send the Thinking Block + Signature from Step 1 as history to the restarted proxy.

### Expected Result (Verifying the Fix)
*   **If it fails**: an `Invalid signature in thinking block` error (because the proxy cannot verify the signature and passes it straight through to Google, which rejects it).
*   **If it passes (current version)**: the proxy detects that the signature is not in its memory cache and **automatically triggers downgrade logic** (stripping the Thinking Block or sending it as plain text); the conversation continues normally with no error.

---

## 6. Verify Dynamic Thinking Stripping

This test verifies that the system can automatically strip useless historical Thinking Blocks under **high context pressure** or **signature invalidation** scenarios, resolving both "Prompt is too long" and "Invalid signature" errors.

### Prerequisites
*   Enable debug logging: `RUST_LOG=debug npm run tauri dev`
*   Make sure you are using a model that supports Thinking (e.g. `claude-3-7-sonnet` or a mapped `gemini-2.0-flash-thinking-exp`)

### Verification Scenario A: Simulate High Context Pressure (Simulate High Load)

This scenario verifies whether the system automatically cleans up old Thinking as conversation history approaches the token limit.

1.  **Build a long conversation**:
    *   **Method 1 (auto-generate)**: run `docs/generate_long_payload.sh` to generate a 2MB test file.
        ```bash
        chmod +x docs/generate_long_payload.sh
        ./docs/generate_long_payload.sh
        cat docs/long_context_payload.txt | pbcopy
        ```
        Then paste the clipboard content to Claude multiple times until you notice significant delay or receive a context warning.

    *   **Method 2 (Deep Thinking induction - sustained pressure)**:
        The following prompts are designed to induce extremely long chains of reasoning in the model. They can be sent in rotation:

        > **Round 1 (History)**: "Please analyze the history of computing from the abacus to quantum computers. For every major milestone (at least 20), perform a deep 'thinking' block simulating the thought process of the inventors. Detailed thinking is required. Aim for maximum output tokens."

        > **Round 2 (Math/Logic)**: "Prove the Riemann Hypothesis. Just kidding. But please perform a deep, step-by-step derivation of the Navier-Stokes existence and smoothness problem's core challenges. Explore 10 different mathematical approaches, evaluating the pros and cons of each in extreme detail."

        > **Round 3 (System Architecture)**: "Design a distributed system capable of handling 100 billion requests per second. Detail the consensus execution flow (Paxos/Raft) for a single transaction across 5000 nodes. Simulate the network partition handling logic in your 'thinking' process for at least 50 failure scenarios."

        > **Round 4 (Literature)**: "Write a recursive story where the protagonist is a recursive function. The story must nest at least 20 levels deep, and for each level, you must 'think' about the symbolic meaning of that recursion depth before writing the narrative part."

2.  **Observe the logs**:
    *   Search the terminal for `[ContextManager]`.
    *   **Expected log**:
        ```
        [INFO] [ContextManager] Context pressure: 95.0% (1900000 / 2000000), Strategy: Aggressive => Purifying history
        [DEBUG] History purified successfully
        ```

3.  **Verify the result**:
    *   The request is sent to Gemini successfully, with no "Prompt is too long" error.
    *   The HTTP response header includes `X-Context-Purified: true`.
    *   Transparent to the Claude CLI user (history is still shown locally in the CLI, but was purified server-side).

### Verification Scenario B: Signature Invalidation Immunity (Signature Immunity via Stripping)

This scenario verifies that even without triggering the retry logic, proactive stripping under high load also incidentally resolves the signature problem.

1.  **Generate signed Thinking**:
    *   **Instruction**: `Think about Rust's ownership model and write 500 words.`

2.  **Restart the Proxy and inject a fake load (optional)**:
    *   Restart the proxy (clearing the signature cache).
    *   Continue the conversation. At this point, Thinking carrying the old signature will be sent to the proxy.

3.  **Expected result**:
    *   If context pressure is high enough to trigger Stripping, or a signature error triggers RetriedWithoutThinking, the system will strip the Thinking Block.
    *   **Key point**: once the Thinking Block is stripped, the `thought_signature` disappears with it.
    *   Gemini receives plain-text history, so it will **never** report Invalid Signature.

---

## 7. OpenCode (Claude Code CLI) Multi-Protocol Integration Test

**Antigravity now fully supports OpenCode's multi-protocol integration**, thoroughly resolving compatibility issues such as `AI_TypeValidationError`. You can choose any of the following integration methods as needed.

### Endpoint Configuration Table

| Protocol Type | Base URL (Antigravity) | Corresponding OpenCode Provider | Notes |
| :--- | :--- | :--- | :--- |
| **Anthropic (native)** | `http://localhost:8045/v1` | `anthropic` | **Recommended**. Supports Thinking, tool calls, and Artifacts preview. |
| **OpenAI (standard)** | `http://localhost:8045/v1` | `openai` | Supports general OpenAI client logic. |
| **OA-Compatible** | `http://localhost:8045/v1` | `openai-compatible` | Suitable for scenarios that require forcing a non-standard model name. |
| **Google Gemini** | `http://localhost:8045/v1` | `gemini` | Uses the Gemini protocol directly and supports native Google SDK features. |

### A. Method 1: Native Anthropic Protocol (Recommended)

This method gives the best native Claude experience, with Thinking signature protection and Beta feature support.

1.  **Configuration**:
    ```bash
    # Set the Base URL (note: OpenCode's anthropic provider sometimes needs the full path)
    export ANTHROPIC_BASE_URL="http://localhost:8045/v1"
    # Set the API Key (Antigravity's key)
    export ANTHROPIC_API_KEY="sk-antigravity-key"
    ```

2.  **Test instruction**:
    ```bash
    claude "Use the chain of thought (Thinking) to analyze the Cargo.toml dependency structure in the current directory."
    ```

3.  **Verification points**:
    *   **Thinking**: can you see the blue thinking block output?
    *   **Signature**: check the Antigravity log; it should show `Cached signature to session ... [FIFO: true]`.
    *   **No errors**: no `Invalid signature` error occurs throughout.

### B. Method 2: OpenAI Protocol (including Compatible)

Suitable for users accustomed to the OpenAI ecosystem, or who need specific model mapping.

1.  **Configuration**:
    ```bash
    # Set the Base URL
    export OPENAI_BASE_URL="http://localhost:8045/v1"
    export OPENAI_API_KEY="sk-antigravity-key"
    ```

2.  **Start OpenCode**:
    ```bash
    claude --provider openai --model gemini-2.0-flash
    # Or use compatible mode
    claude --provider openai-compatible --model gemini-2.0-flash
    ```

3.  **Verification points**:
    *   **JSON error**: try deliberately disconnecting the network or using an invalid key; OpenCode should show a friendly JSON error message (e.g. `{"error": {"message": "..."}}`) instead of crashing.
    *   **Non-streaming compatibility**: some OpenCode tool calls may use non-streaming requests; verify that JSON responses parse correctly.

### C. Method 3: Native Google Gemini Protocol

Newly added support in Antigravity v4.1.4.

1.  **Configuration**:
    ```bash
    export GEMINI_API_KEY="sk-antigravity-key"
    # If OpenCode supports GEMINI_BASE_URL (usually requires a reverse proxy tool like cloudflared, or config changes):
    export GEMINI_BASE_URL="http://localhost:8045/v1"
    ```

2.  **Verification points**:
    *   **Adapter detection**: the Antigravity log should show `[Gemini] Client Adapter detected`.
    *   **Let It Crash**: when a 403/404 error occurs, the response should return immediately instead of leaving OpenCode hanging while it retries.

### D. Common Troubleshooting

*   **Q: Getting `AI_TypeValidationError`?**
    *   **A**: Please make sure you upgrade Antigravity to v4.1.2+. The error format returned by older versions (plain text) cannot pass OpenCode's Zod validation.

*   **Q: The Thinking block shows as `[Redacted]` or disappears entirely?**
    *   **A**: This is expected behavior. To protect Google's signature from being broken, Antigravity may proactively strip the thinking block under certain conditions (such as high context pressure or signature validation failure). As long as the conversation can continue, it means the "Dynamic Stripping" mechanism is working.

---

## 8. Multi-Round Continuous Conversation Stress Test (Continuous Conversation Stress Test)

This test aims to verify **Signed Session Stability** under high-frequency, multi-round interaction. Perform the following steps **consecutively** within a single OpenCode session, without restarting or clearing the context.

### Scenario: Rust Project Refactoring in Practice

#### Round 1: Deep Code Review (Initial Analysis)
*   **Instruction**:
    ```bash
    claude "Please review src-tauri/src/proxy/handlers/claude.rs in detail. Focus on the handle_messages function and analyze how it handles Beta Header injection. Use chain of thought to list your analysis steps."
    ```
*   **Verification points**:
    *   You must see the analysis of the logic that injects headers via `ClientAdapter`.
    *   The response includes a complete Thinking Block.

#### Round 2: Simulate a Change Proposal (Refactoring Proposal)
*   **Instruction**:
    ```bash
    claude "Based on your analysis, if I wanted to add a new adapter named 'CherryStudio', which files would need to change? Give a concrete implementation plan without modifying any files directly."
    ```
*   **Verification points**:
    *   Claude must accurately reference the context from Round 1 (proving Session ID propagation works correctly).
    *   The Thinking signature is not lost (an `Invalid signature` error would indicate the signature cache became invalid).

#### Round 3: High-Frequency Concurrency Test (Concurrent Simulation)
*   **Background**: in this round, we simulate rapid, consecutive follow-up questions to test the robustness of the FIFO signature queue.
*   **Instruction (execute 3 times in quick succession)**:
    ```bash
    # Quickly enter the following short instructions to simulate a user firing off rapid follow-ups
    claude "In the plan just given, does StreamingState need to change?"
    claude "What about the ClientAdapter trait?"
    claude "Does Cargo.toml need a new dependency?"
    ```
*   **Verification points**:
    *   **Out-of-order tolerance**: even if responses arrive in an order different from the requests, the client should not crash.
    *   **Queue depth**: the Antigravity log should show the Signature Cache updating normally, with no earlier signature invalidated due to an overwrite.

#### Round 4: Long Text Generation (Output Token Limit)
*   **Instruction**:
    ```bash
    claude "Please write a thorough developer document (in Markdown) for the ClientAdapter trait, including detailed comments for every method and best-practice example code for three different scenarios. Aim for 2000+ words."
    ```
*   **Verification points**:
    *   Verify that the SSE stream remains stable under a large output volume.
    *   Watch the logs for whether `ContextManager`'s proactive purification (Purify) is triggered, and whether the signature is safely stripped.

---

## 6. Intelligent Context Compression Level Verification (Compression Level Test)

For the newly introduced `Intelligent Context Compression Level` (Disabled / Low / Medium / High), you can use the following steps to verify the noise-reduction behavior at each level.

### A. Basic Test Data Preparation

In a client (such as Cline or Cherry Studio), start a new session. We will prepare, as the first-round request, a piece of text mixing **redundant filler speech** with **typical repeated build logs**:

```text
Hello, could you please help me build this package? Actually, basically, I think probably it is failed.

Progress: 10%
Progress: 20%
Progress: 30%
Progress: 40%
Progress: 50%
Error: compilation failed at src/main.rs:25
```

After the model replies, immediately follow up with a **second round of questions** (e.g. `How to fix this error?`); at this point the first-round message becomes "historical message", triggering the corresponding static cleaning rules.

---

### B. Verify Low Level (Log Noise Reduction Only)

1.  **Setup**: in the settings panel, set `Intelligent Context Compression Level` to **`Low - Log Noise Reduction`**.
2.  **Test**: perform the two rounds of conversation described above.
3.  **Log and Effect Verification**:
    *   In the `npm run tauri:debug` console, observe the payload as the second-round request is sent:
        *   **RTK log de-noising works**: `Progress: 10% ... 50%` in the first-round message is automatically cleaned and collapsed into a placeholder like `[Collapsed 3 similar lines]`.
        *   **Error message safely preserved**: the `Error: compilation failed...` message at the bottom is fully preserved.
        *   **Filler speech left as-is**: the opening filler text `Hello, could you please...` in the first-round message remains fully intact in the payload, unshortened.

---

### C. Verify Medium Level (Log Noise Reduction + Filler-Speech Cleaning)

1.  **Setup**: in the settings panel, set `Intelligent Context Compression Level` to **`Medium - Log + Filler Speech`**.
2.  **Test**: start a fresh session and perform the two rounds of conversation described above.
3.  **Log and Effect Verification**:
    *   Observe the payload in the console as the second-round request is sent:
        *   **RTK log de-noising works**: repeated progress lines continue to be collapsed successfully.
        *   **Caveman-style filler cleaning works**: the opening text in the historical message is heavily cleaned; meaningless filler phrases such as `Hello, could you please...` and `Actually, basically, I think probably...` are removed entirely, converted into a bare-bones, caveman-style minimal register.
        *   **Code isolation preserved**: all code snippets and stack paths (such as `src/main.rs:25`) remain untouched.

---

### D. Verify High Level (Log + Filler Speech + Dynamic Anti-Overflow)

1.  **Setup**: in the settings panel, set `Intelligent Context Compression Level` to **`High - Dynamic Anti-Overflow`**.
2.  **Test**: send a fairly long piece of project code, or one containing tens of thousands of tokens, to push up context usage.
3.  **Log and Effect Verification**:
    *   **Dynamic three-tier pressure adaptation**: in the console you will see `ContextManager` estimating the current context pressure ratio in real time (e.g. `Context pressure: 45.2%`).
    *   **Tiered trigger logs**:
        *   Exceeding the L1 threshold: triggers `[Layer-1] Tool trimming`, trimming old tool packages.
        *   Exceeding the L2 threshold: triggers `[Layer-2] Thinking compression`, compressing historical thinking while replaying and protecting the signature.
        *   Exceeding the L3 threshold: triggers `[Layer-3] Fork+Summary`, the ultimate reset; the console will show the session being reopened and the summary being generated, and the payload of the first summary message will show the injected `cache_control: {"type": "ephemeral"}` marker (in subsequent chats, you will observe the Prompt Cache hit rate spike dramatically in the client, giving very fast responses).
