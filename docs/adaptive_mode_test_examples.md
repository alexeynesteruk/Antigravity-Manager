# Claude 4.6 Adaptive Thinking Mode: Test Example Guide

To verify the integration of Claude 4.6 Adaptive Thinking mode, in particular whether the `effort` parameter takes effect and whether the token limit is adjusted automatically, refer to the following test scenarios.

## 1. Verify Adaptive Mode Activation and Effort Control

This test verifies whether the system correctly passes through the `thinking: { type: "adaptive", effort: "..." }` parameter, and observes the resulting differences in model behavior.

### Prerequisites
*   Make sure you are using a model ID that supports Adaptive Thinking (e.g. `claude-opus-4-6-thinking` or a mapped ID).
*   In settings, set the "Thinking Budget" mode to **"Adaptive"**.

### Test Instruction Examples

#### Scenario A: Low Effort
*   **Configuration**: set Effort to `Low`.
*   **Instruction**: `Write a Hello World program that compiles with Rust.`
*   **Expected result**:
    *   The Thinking block should be fairly short, since the model judges this a simple task that doesn't need deep reasoning.
    *   Response is relatively fast.

#### Scenario B: High Effort
*   **Configuration**: set Effort to `High`.
*   **Instruction**: `Analyze in detail how Rust's async/await state machine generation works, and compare it with Go's Goroutine scheduling model. Use chain of thought to derive the difference in memory overhead between the two in depth.`
*   **Expected result**:
    *   The Thinking block should be very long and detailed (possibly exceeding 5k tokens).
    *   The model will attempt an in-depth comparative analysis and reasoning process.
    *   **Key verification point**: check the Antigravity log to confirm that `generationConfig` includes `thinkingConfig: { type: "adaptive", effort: "high" }`.

---

## 2. Verify Adaptive State Persistence Across Multiple Turns

Verify whether Adaptive mode keeps working across multiple conversation turns, and whether the token limit (128k) works correctly.

### Scenario: Iterative Complex Algorithm Design

#### Round 1: Initial Design
*   **Instruction**:
    ```bash
    claude "Design a distributed, high-concurrency flash-sale system. Consider core issues such as cache consistency, oversell prevention, and anti-abuse interfaces. Use High Effort for deep thinking."
    ```
*   **Verification points**:
    *   A design document is generated that includes an architecture diagram and detailed logic.
    *   The thinking process records a detailed trade-off between different approaches (e.g. Redis Lua vs. a database pessimistic lock).
    *   Verify in the response headers or logs that `maxOutputTokens` was raised to **128,000** (or higher) to accommodate the long output.

#### Round 2: Challenging the Design (simulated user feedback)
*   **Instruction**:
    ```bash
    claude "In your design, how would you guarantee strong consistency of inventory data if the Redis cluster experiences split-brain? Please rethink and revise the design."
    ```
*   **Verification points**:
    *   The Thinking block keeps up its depth of reasoning, analyzing the applicability of Redlock or other consistency algorithms.
    *   **Signature validation**: confirm that the Thinking Block signature validates correctly across multiple turns (no `Invalid signature` error).

#### Round 3: Code Implementation
*   **Instruction**:
    ```bash
    claude "Please provide the Rust code implementation for the core inventory deduction logic."
    ```
*   **Verification points**:
    *   The generated code matches the previously designed approach.
    *   Under high context pressure, check whether the system automatically triggers Thinking stripping (if dynamic stripping is configured), or whether it can continue generating normally while carrying the full history.

---

## 3. Verify Automatic Switching Between Budget Mode and Adaptive Mode

This test verifies whether the backend correctly converts parameters when the user switches between "Fixed Budget" and "Adaptive" mode.

### Test Flow
1.  **Set to Fixed Budget**: in settings, choose "Custom" and set the Budget to `16384`.
    *   Send the request.
    *   *Verify*: the backend request should only include `thinkingConfig: { budget: 16384 }`, and **should not include** `effort`.

2.  **Switch to Adaptive**: in settings, choose "Adaptive" and set Effort to `Medium`.
    *   Send the request.
    *   *Verify*: the backend request should only include `thinkingConfig: { type: "adaptive", effort: "medium" }`, and **should not include** `budget`.

---

## 4. Debugging Tips

When running the tests above, it's recommended to enable debug logging to observe how the parameters are passed through:

```bash
RUST_LOG=debug npm run tauri dev
```

Search the logs for these keywords:
*   `[Claude-Request]`: view the converted request body.
*   `thinkingConfig`: confirm the config was injected correctly.
*   `maxOutputTokens`: confirm the token limit was adjusted correctly.
