# 503 Error (Service Unavailable) Fix Verification Guide

This guide provides test verification examples for the recently reported 503 errors (Issue #1794 and backend capacity limits).

## 1. Verify Automatic Fallback After a Project ID Fetch Failure (Issue #1794)

**Scenario**:
Some accounts (especially Free accounts or restricted accounts) get an error when calling the official endpoint to fetch a project ID: `Account is not eligible for the official cloudaicompanionProject`. Before the fix, the system would simply skip that account, leading to a 503 in the end; after the fix, the system automatically falls back to a generic Project ID (`bamboo-precept-lgxtn`).

### A. Basic Connectivity Test With `curl`
Use an API Key corresponding to an account that previously returned 503 (or go through the proxy directly):

```bash
curl http://localhost:8045/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-antigravity-key" \
  -d '{
    "model": "gemini-2.0-flash",
    "messages": [
      {"role": "user", "content": "Hello, please confirm your working status."}
    ],
    "stream": false
  }'
```

### B. Watch the Server-Side Logs (npm run tauri dev)
**Expected behavior**:
When the system detects a permission issue, the following **Warn** message appears in the logs, but the request **should not error with 503**; it should continue executing instead:

```text
WARN Failed to fetch project_id for user@example.com, using fallback: Account is not eligible for official cloudaicompanionProject
DEBUG [TokenManager] Using project_id: bamboo-precept-lgxtn for request
```

---

## 2. Verify That Quota Protection Prevents 503s

**Scenario**:
When an account's quota is exhausted, or the backend returns 503 due to high load, the system should correctly detect this and try rotating to another account rather than passing the 503 straight through to the client.

### Test Instruction (Claude CLI)
```bash
claude "Where's the bug in this code? [attach a long code snippet]"
```

**Verification points**:
- If the current account returns 503, the log should show `[RetryStrategy] Status 503 detected, rotating account...`.
- The system should automatically try the next available account until it gets a successful response or exhausts its retries.

---

## 3. Distinguishing a "Code Bug" From a "Backend Capacity Limit" (Opus 4.6)

**Scenario**:
Since the `claude-opus-4-6-thinking` model is currently experimental, the Google backend frequently returns `No capacity available` (503).

### Test Instruction
```bash
claude --model claude-opus-4-6-thinking "Perform a deep reasoning task comparing the async memory models of Rust and C++."
```

**Expected result analysis**:
1. **If it returns 503 with a message containing "No capacity available"**:
   - This is a **Google backend capacity limit**, not a bug in this software.
   - The proxy will automatically try other accounts via the retry strategy, but if every account hits the capacity limit, the 503 is eventually passed through.
   - **Recommendation**: switch to `gemini-2.0-flash-thinking-exp` or `claude-3-7-sonnet` for testing during this peak-load period.

2. **If it returns successfully**:
   - This means the backend currently has sufficient capacity.

---

## Debugging Tips

If you want to force-simulate a Project ID failure scenario for code-level verification, you can temporarily modify the simulation logic in `src-tauri/src/proxy/token_manager.rs`. In most cases, though, you can confirm the fix is working simply by watching the logs for the string `using fallback: ...`.
