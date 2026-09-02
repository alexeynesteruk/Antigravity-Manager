# Fix Opus 4.6 Call Error & UserToken Display Optimization

## Problem

The account pool mixes Pro and Ultra accounts. Pro doesn't have Opus 4.6 access, Ultra does.

The old rotation logic picked accounts by quota level, regardless of subscription tier. When a user called Opus 4.6, the system could pick a Pro account and error out immediately.

## Changes

### 1. Ultra-First Scheduling

When calling Opus 4.6/4.5, sort by subscription tier first:

```
Ultra > Pro > Free
```

Within the same tier, sort by quota as before. Other models still use the old logic, quota first.

Matching rule: if the model name contains `claude-opus-4-6`, `claude-opus-4-5`, or `opus`, Ultra-first scheduling applies.

### 2. UserToken Edit Data Not Showing

When clicking edit on a Token, the IP restriction and curfew time showed as empty.

Problem:
- The frontend passed `undefined`, but Rust needed `null`
- Reads used `||`, which swallowed `0` and empty strings; changed to `??`

### 3. Custom Expiration Time

Token creation now has a Custom option, letting you pick a date/time down to the hour.

## Files

```
src-tauri/src/proxy/token_manager.rs      # sorting logic
src-tauri/src/proxy/tests/mod.rs          # test module
src-tauri/src/proxy/tests/ultra_priority_tests.rs  # Ultra-first tests
src-tauri/src/commands/user_token.rs      # custom expiration parameter
src-tauri/src/modules/user_token_db.rs    # database
src/pages/UserToken.tsx                   # frontend
```

## Verification

1. Call Opus 4.6, check the logs to confirm it goes through an Ultra account
2. Create a Token with IP restriction and curfew set, then confirm the data displays correctly when editing
