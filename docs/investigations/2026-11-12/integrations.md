# Reviewed integrations on main

The six strongest candidate groups from the [review](report.md) are integrated directly into local `main`, as requested. The starting commit is `1e6e90b6`; `backup/pre-reviewed-integrations-20261112` retains that baseline and its unpublished work. No push, release, or deployment was performed.

## Functional commits

| Commit | Result | Source |
| --- | --- | --- |
| `ed3ba09e` | Preserve supplied skill instructions verbatim. | [Upstream fb987bde](https://github.com/lbjlaq/Antigravity-Manager/commit/fb987bde479a3a4d179747533d1d3033852d246e), fixing issue 3914. |
| `7fff7a42` | Resolve dynamic model tiers, use Claude's independent custom-budget mode, and preserve explicit thought-output suppression through final wrapping. | Selected model/pipeline fixes from [f4a83aca](https://github.com/lbjlaq/Antigravity-Manager/commit/f4a83aca1701d232d53a35375e3c36e140f68214). |
| `554d8970` | Reject unresolved or aliased migration destinations and recursive self-copy while preserving migration persistence/recovery ordering. | SanHsien [40176234](https://github.com/SanHsien/Antigravity-Manager/commit/40176234f4df542df830c9aa3bb624fb9666c98d) and [dee4b84c](https://github.com/SanHsien/Antigravity-Manager/commit/dee4b84c2727f6a6215053c8fa248fd636948889). |
| `5cd7389f` | Integrate sticky/QoS preservation, bounded transport/upload recovery, probe claims, versioned leases, and cooldown acquisition budgets as one coherent request-recovery change. | Upstream PRs [3892](https://github.com/lbjlaq/Antigravity-Manager/pull/3892), [3893](https://github.com/lbjlaq/Antigravity-Manager/pull/3893), [3895](https://github.com/lbjlaq/Antigravity-Manager/pull/3895), [3899](https://github.com/lbjlaq/Antigravity-Manager/pull/3899), [3906](https://github.com/lbjlaq/Antigravity-Manager/pull/3906); DNAlec [PR 3](https://github.com/DNAlec/Antigravity-Manager/pull/3), [42c9a34a](https://github.com/DNAlec/Antigravity-Manager/commit/42c9a34ab95d5a50169d91b8e0533314a311da14), [381736c5](https://github.com/DNAlec/Antigravity-Manager/commit/381736c50cb20f47dd6e16cd1eaf1e786534a4dd), [47a07f52](https://github.com/DNAlec/Antigravity-Manager/commit/47a07f52808835e68120871a85d36b8991735b47). |

Contributor authorship is retained through the original author or verified human coauthor trailers. Release notes, manifests, broad prompt rewriting, optional QoS configuration, and raw-usage diagnostics were excluded.

## Behavior and compatibility

Thinking disable respects the catalog's supported channel. Models requiring reasoning can suppress returned thoughts while retaining their valid level/budget configuration; the change does not promise zero reasoning for those models. Dynamic `-1` is distinct from explicit zero, duplicate field aliases are removed, and output-limit reconciliation still runs. Official model budgets remain authoritative; Claude's independent custom mode applies to the existing fallback for uncatalogued models. Local Windows manifest handling is retained.

Request recovery uses versioned ownership across all four protocol handlers. Late success/failure cannot overwrite a newer sticky binding; reset/removal invalidates probe claims. Probes share the account client/proxy, require parsed generated text, and use one eight-second deadline with endpoint fallback. Authorized physical model metadata takes precedence over quota-bucket aliases.

Intentional cooldowns no longer consume the 15-second acquisition work allowance, while the original request deadline remains fixed. The existing five-second ceiling for unbound all-pool cooldown recovery is retained. Upload failures receive at most one eligible retry on the same account and do not replay across accounts. Other recovery decisions follow scheduling mode and replay/global-budget limits. See [the upload policy](../../UPLOAD_TIMEOUT_POLICY.md) for the complete matrix.

The fallback first-byte cap is available through all configuration surfaces:

- Config field: `proxy.upload_timeouts.fallback_first_byte_cap_secs`.
- Environment: `ANTIGRAVITY_TIMEOUT_FALLBACK_FIRST_BYTE_CAP_SECS`.
- GUI: Timeout Settings, with authenticated effective-value readback also available to the web console.

The default is 45 seconds; zero disables this additional cap while keeping shared deadlines. Configuration values normalize to 0 through 600. Existing serialized fields survive GUI saves.

## Validation

On Windows, 153 distinct focused Rust tests passed, including four integration tests using production policy functions. TypeScript checking with `tsc --noEmit` passed. Coverage includes skills preservation, final wrapped thinking output, dynamic models, directory identity and pointer failure, lease concurrency/ABA/removal, probe claims and SSE semantics, streaming budget context, terminal policies, and two actual sixteen-second bound-account cooldowns. The local drive/UNC alias test ran without its conditional skip. Diff whitespace checks passed.

The work received parallel static review. No full suite, release pre-flight, authenticated Google API/inference request, or macOS/Linux runtime check was performed. Rust compilation retains existing warnings; this is not a claim of a warning-free release gate.

Successful session bindings retain their previous lifetime policy; the new account-epoch tombstones have no fixed global retention cap. Vacant cancelled selections are cleaned incrementally without scanning every bound session. Epoch checks protect leased health observations, including a rejected binding commit that retains its original proof. PerformanceFirst health observations have no lease and check current account existence only.

## Remaining review findings

This integration batch does not fix the separate findings involving disabled user-token admission, server-local media reads, headless host binding/configuration replacement, batch deletion ordering, browser-owned pedal restoration, or stream-progress/persistence deadlines. Those remain separate functional work described in the [review](report.md).

Each functional commit can be reverted as a unit. Reverting restores its previous behavior and associated defect; the baseline backup is a recovery reference, not an instruction to discard later local work.
