# Pro Model 1.5/2.5 Pro Automatic Alignment and Routing Test (v4.2.4)

## Test Objective

Verify the correctness, stability, and cost optimization effect of the three-layer progressive context compression feature.

## Prerequisites

1. **Start the application**:
   ```bash
   cd /Users/lbjlaq/Desktop/xin
   npm run tauri dev
   ```

2. **Enable debug logging**:
   ```bash
   export RUST_LOG=debug
   ```

3. **Prepare test accounts**:
   - At least 1 Google account (for the Gemini API)
   - Make sure the account has enough quota

## Test Scenarios

### Scenario 1: Layer 1 Tool Message Trimming (60% pressure)

**Goal**: verify the intelligent tool message trimming feature

**Steps**:
1. Use the Claude Code CLI or Cherry Studio
2. Start a task that requires multiple tool calls (e.g. code search, file reading)
3. Keep the conversation going until it triggers 60% context pressure

**Expected result**:
- The log shows `[Layer-1] Tool trimming triggered`
- The most recent 5 rounds of tool interaction are kept
- Older tool messages are removed
- **No 400 error**
- **Normal response speed**

**Verification command**:
```bash
# View the logs
tail -f ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep "Layer-1"
```

---

### Scenario 2: Layer 2 Thinking Compression (75% pressure)

**Goal**: verify Thinking content compression + signature preservation

**Steps**:
1. Use a Claude 4.5 Opus/Sonnet Thinking model
2. Start a complex reasoning task (e.g. code refactoring, algorithm design)
3. Keep the conversation going until it triggers 75% context pressure

**Expected result**:
- The log shows `[Layer-2] Thinking compression triggered`
- The Thinking block text is compressed to "..."
- **The `signature` field is fully preserved**
- The 4 most recent messages are not compressed
- **No 400 signature error**

**Verification command**:
```bash
# Check signature preservation
tail -f ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep -E "(Layer-2|signature)"
```

---

### Scenario 3: Layer 3 Session Fork + XML Summary (90% pressure)

**Goal**: verify XML summary generation and session Fork

**Steps**:
1. Have an extremely long conversation with any model
2. Keep the conversation going until it triggers 90% context pressure

**Expected result**:
- The log shows `[Layer-3] Critical context pressure`
- Calls `gemini-2.5-flash-lite` to generate the XML summary
- Creates a new message sequence: `[User: XML summary] + [Assistant: acknowledgment] + [User's latest message]`
- **Compression ratio 86-97%**
- **No Prompt Cache breakage**
- **Signature chain intact**

**Verification command**:
```bash
# Check Layer 3 trigger and summary generation
tail -f ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep -E "(Layer-3|XML summary|Fork)"
```

---

### Scenario 4: Progressive Trigger Test

**Goal**: verify the progressive trigger mechanism across the three compression layers

**Steps**:
1. Start from an empty conversation
2. Keep the conversation going and observe the order in which compression layers are triggered

**Expected result**:
- Trigger order: Layer 1 (60%) -> Layer 2 (75%) -> Layer 3 (90%)
- Token usage is re-estimated after each compression
- The logs clearly record the trigger and effect of each layer

**Verification command**:
```bash
# View triggers across all layers
tail -f ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep -E "Layer-[123]"
```

---

### Scenario 5: Error Handling Test

**Goal**: verify the fault tolerance mechanism when Layer 3 fails

**Steps**:
1. Temporarily disable the Gemini account or the network
2. Trigger Layer 3 compression

**Expected result**:
- Layer 3 failure returns a `BAD_REQUEST` error
- The error message is friendly: `Context too long and automatic compression failed`
- The user is prompted to use `/compact` or switch models

**Verification command**:
```bash
# Check error handling
tail -f ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep -E "(Layer-3.*failed|BAD_REQUEST)"
```

---

## Performance Verification

### Token Cost Savings

**Test method**:
1. Record the Token usage before compression (extracted from the logs)
2. Record the Token usage after compression
3. Calculate the savings ratio

**Expected result**:
- Layer 1: 60-90% savings
- Layer 2: 70-95% savings
- Layer 3: 86-97% savings

### Response Speed

**Test method**:
1. Measure response time using the `time` command
2. Compare response speed before and after compression

**Expected result**:
- Layer 1/2: no noticeable change in response speed
- Layer 3: the first summary generation may add 2-5 seconds; subsequent requests are normal

---

## Compatibility Testing

### Client Compatibility

Test the following clients:
- Claude Code CLI
- Cherry Studio
- Cursor
- Python OpenAI SDK
- Kilo Code

### Model Compatibility

Test the following models:
- Gemini 3 Flash
- Gemini 3 Pro High
- Claude 4.5 Sonnet
- Claude 4.5 Opus Thinking

---

## Regression Testing

### Signature Chain Integrity

**Verification points**:
- The signature is not lost after Layer 2 compression
- The signature is correctly restored after a Layer 3 Fork
- No 400 signature error

### Tool Call Chain

**Verification points**:
- Tool calls still work correctly after compression
- Tool results are passed through correctly
- No tool call interruption

---

## Log Analysis

### Key Log Patterns

```bash
# Layer 1 trigger
grep "Layer-1.*Tool trimming" antigravity.log

# Layer 2 trigger
grep "Layer-2.*Thinking compression" antigravity.log

# Layer 3 trigger
grep "Layer-3.*Fork successful" antigravity.log

# Token savings statistics
grep "Compression result.*saved" antigravity.log
```

---

## Test Report Template

```markdown
## Test Results

### Scenario 1: Layer 1 Tool Message Trimming
- [ ] Triggered successfully
- [ ] Most recent 5 rounds kept
- [ ] No 400 error
- [ ] Normal response speed

### Scenario 2: Layer 2 Thinking Compression
- [ ] Triggered successfully
- [ ] Signature fully preserved
- [ ] No signature error
- [ ] Compression ratio meets target

### Scenario 3: Layer 3 Session Fork
- [ ] Triggered successfully
- [ ] XML summary generated
- [ ] Compression ratio 86-97%
- [ ] No Cache breakage

### Scenario 4: Progressive Trigger
- [ ] Correct order (1->2->3)
- [ ] Token re-estimation
- [ ] Logs are clear

### Scenario 5: Error Handling
- [ ] Friendly message on failure
- [ ] No crash
- [ ] Clear recommendation

### Performance Verification
- Token savings: ____%
- Response speed: normal/slow (___ms)

### Compatibility
- Claude Code: pass/fail
- Cherry Studio: pass/fail
- Cursor: pass/fail
- Python SDK: pass/fail

### Regression Testing
- Signature chain intact: pass/fail
- Tool calls work normally: pass/fail

## Issue Log

(Record issues found during testing)

## Conclusion

(Overall assessment and recommendations)
```

---

## Quick Test Script

```bash
#!/bin/bash
# Quick test for the three compression layers

echo "=== Testing Layer 1 (tool trimming) ==="
# Use Claude Code to run multiple file searches
claude "Search the project for all .rs files, then read the contents of 5 of them"

echo "=== Testing Layer 2 (Thinking compression) ==="
# Use a Thinking model for complex reasoning
claude --model claude-opus-4-5-thinking "Analyze the performance bottleneck in this code in detail and propose an optimization plan"

echo "=== Testing Layer 3 (session Fork) ==="
# An extremely long conversation triggers Fork
for i in {1..20}; do
  claude "Continue the previous topic, please provide more detail (round $i)"
done

echo "=== Viewing logs ==="
tail -100 ~/Library/Application\ Support/com.antigravity.tools/logs/antigravity.log | grep -E "Layer-[123]"
```

---

## Notes

1. **Test environment**: make sure to test in a clean environment, avoiding interference from other factors
2. **Log level**: `RUST_LOG=debug` must be set to see detailed logs
3. **Account quota**: make sure the account has enough quota before testing
4. **Data backup**: back up important data before testing
5. **Version check**: confirm you are running v4.2.4

---

## Troubleshooting

### Issue 1: Layer 1 not triggering
- Check whether the conversation has reached 60% pressure
- Check whether the Token estimate is accurate

### Issue 2: Layer 2 signature lost
- Check the `compress_thinking_preserve_signature` function
- Verify the signature extraction logic

### Issue 3: Layer 3 summary failure
- Check whether the Gemini account is available
- Verify the `call_gemini_sync` function
- Check the upstream API error

### Issue 4: 400 error
- Check whether the signature chain is intact
- Verify the tool call parameters
- Check the upstream API response

---

## Contact

If you have questions, please open an Issue on GitHub:
https://github.com/lbjlaq/Antigravity-Manager/issues
