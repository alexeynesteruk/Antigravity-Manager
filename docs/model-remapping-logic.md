# Model Remapping Logic (Current Implementation)

Last updated: 2026-03-02

This document describes the current model remapping chain in the proxy (including the adjusted behavior for Gemini 3/3.1 Pro).

## 1) Overall Flow

Regardless of whether the OpenAI protocol or the native Gemini protocol is used, the requested model goes through two stages of processing:

1. Static route resolution (global rules):
   - Runs once, before account selection.
   - Code: `resolve_model_route` in `src-tauri/src/proxy/common/model_mapping.rs`.
2. Dynamic account-aware rewrite (conditional fallback):
   - Runs after an account has been selected.
   - Code: `resolve_dynamic_model_for_account` in `src-tauri/src/proxy/token_manager.rs`.

Entry points that use this flow:
- `src-tauri/src/proxy/handlers/openai.rs`
- `src-tauri/src/proxy/handlers/gemini.rs`

## 2) Static Route Priority

`resolve_model_route(original_model, custom_mapping)` resolves in priority order, from highest to lowest:

1. Official dynamic deprecation forwarding rules:
   - `DYNAMIC_MODEL_FORWARDING_RULES`
2. User-defined exact mapping:
   - `custom_mapping[original_model]`
3. User-defined wildcard mapping:
   - compared by "number of non-`*` characters"; the more specific one wins
4. Built-in system default mapping:
   - `map_claude_model_to_gemini`

If none of these match, the model name is passed through unchanged.

## 3) Current Built-in Gemini Pro Mapping Strategy

The current strategy is: concrete model IDs are passed through directly; only generic aliases get normalized.

Concrete IDs (no forced cross-version rewrite):
- `gemini-3-pro-high -> gemini-3-pro-high`
- `gemini-3-pro-low -> gemini-3-pro-low`
- `gemini-3-pro-preview -> gemini-3-pro-preview`
- `gemini-3.1-pro-high -> gemini-3.1-pro-high`
- `gemini-3.1-pro-low -> gemini-3.1-pro-low`
- `gemini-3.1-pro-preview -> gemini-3.1-pro-preview`

Generic aliases (still mapped to the preview entry):
- `gemini-3-pro -> gemini-3-pro-preview`
- `gemini-3.1-pro -> gemini-3.1-pro-preview`

Code location:
- `src-tauri/src/proxy/common/model_mapping.rs`

## 4) Dynamic Account-Aware Rewrite (Triggered Only When Needed)

Once an account is selected, the system reads the models available in that account's local quota to determine whether the current model is usable.

Behavior:

1. Read the account JSON: `quota.models[*].name`.
2. Build a fallback candidate list only for the Gemini 3/3.1 Pro family.
3. Candidate order:
   - try the current model first
   - then try other compatible models in the same family, in a preset order
4. Select the first model that exists in the account's available set.
5. If none match, keep the current model unchanged.

Key points:
- If the requested model is itself available, no remapping happens.
- Remapping only happens when the requested model is unavailable and a compatible candidate exists.

Code location:
- `src-tauri/src/proxy/token_manager.rs`
  - `get_available_models_from_json`
  - `build_dynamic_model_candidates`
  - `resolve_dynamic_model_for_account`

## 5) Log Observation Points

You can tell whether each step was triggered from the logs:

- Static mapping log:
  - `[Router] System default mapping: <original> -> <mapped>`
- Dynamic rewrite log:
  - `[Dynamic-Model-Rewrite] account=<id> <from> -> <to>`

If `Dynamic-Model-Rewrite` does not appear for a given request, it means that account used the current model directly.

## 6) Examples

Example A (no rewrite):
- Request: `gemini-3-pro-high`
- Account's available models include: `gemini-3-pro-high`
- Final upstream model: `gemini-3-pro-high`

Example B (fallback rewrite happens):
- Request: `gemini-3-pro-high`
- Unavailable on the account: `gemini-3-pro-high`
- Available on the account: `gemini-3.1-pro-high`
- Final upstream model: `gemini-3.1-pro-high`

Example C (generic alias):
- Request: `gemini-3-pro`
- Mapped in the static stage first to: `gemini-3-pro-preview`
- The dynamic stage then decides whether to continue falling back, based on the account's available models.

## 7) Design Goals

This design satisfies three goals at once:
- Concrete models preserve the user's original intent as the top priority.
- Generic aliases retain historical compatibility.
- When account capabilities are inconsistent, dynamic fallback improves availability.
