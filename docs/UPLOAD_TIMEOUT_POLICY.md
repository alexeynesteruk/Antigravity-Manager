# Upload timeouts, endpoint fallback, and account retries

All four adapters execute the shared decisions in `pipeline::terminal` and
`pipeline::rotation`. Transport errors do not establish an account quota failure.
This policy integrates upstream PRs #3892, #3893, #3895, #3899, and #3906.

## Upload progress and deadlines

Streaming uploads report bytes consumed by the HTTP transport. These observations
measure local delivery to that transport, not acknowledgement from Google's server.

- A sustained absence of progress ends an upload with `UploadStalled`.
- `stall_secs` is independent of the estimated upload budget. The effective stall
  allowance is at least `connect_secs`, so initial connection work has time to finish.
- The size/bandwidth formula is a soft observation threshold. Crossing it while
  progressing emits `upload_budget_extended`; it does not interrupt the upload.
- Passive throughput samples can extend estimates for slower connections. Faster
  samples never shorten the allowance based on configured minimum bandwidth.
  Failed/stalled transfers with usable progress also contribute samples.
- The existing safe per-call limit and the fixed request deadline still bound work.
  The outer call uses the original request deadline, including time spent acquiring
  accounts, retrying, and falling back; retries do not restart that deadline.

Fixed-length uploads do not expose byte progress. A transport failure in that path
is classified conservatively as `Upload`, rather than assuming delivery completed.

## Terminal decisions

The following table describes shared terminal planning for text requests. Every
retry also needs remaining request budget and replay capacity.

| Failure | CacheFirst | Balance | PerformanceFirst |
| --- | --- | --- | --- |
| `Upload` / `UploadStalled` | Retry same account once, then stop | Retry same account once, then stop | Retry same account once, then stop |
| `TransportReset` after tracked upload | Retry same account once, then stop | Retry same account once, then rotate | Rotate |
| `FirstByte` timeout | Retry same account once, then stop | Rotate | Rotate |
| `GlobalBudget` / `ReplayCap` | Stop | Stop | Stop |

Upload failures never trigger cross-account replay or sticky unbinding. CacheFirst
preserves text bindings for all of these terminals. Other modes may unbind after a
first-byte timeout, exhausted global budget, or transport reset that reaches rotation.

Requests marked as having no reusable text cache do not get an in-place replay.
They can rotate where the plan allows it, but upload failures still stop without
cross-account replay. CacheFirst preserves the existing text-session binding.

Timeouts return 504, transport resets return 502, and the replay cap returns 503.
None of these terminal failures penalizes account quota. Account/auth/model and
upstream HTTP errors continue through their separate shared classification paths.

A correctable User-Agent gate does not write a 900-second model breaker or rotate
accounts. The existing diagnostic is returned so the configuration can be repaired.

## Endpoint selection

Regular requests retain primary-endpoint preference, including recovery attempts
before Sandbox. Content uploads of at least 512 KiB may prefer healthy Sandbox over
a cooling primary endpoint. Unhealthy endpoints remain available as fallback.

Only body bytes determine `large_body_threshold_mb` admission; a high token estimate
alone does not impose a single-upload endpoint limit. `large_body_endpoint_attempts`
is clamped to the available endpoint count when planning, without discarding the
configured value. HTTP response fallback retains its existing classification.

## Configuration and headless parity

Existing upload and stream settings retain their serialized names. The deprecated
`header_wait_secs` field is preserved across frontend saves and remains unused in
first-byte calculations.

`proxy.upload_timeouts.fallback_first_byte_cap_secs` controls the fallback endpoint's
first-byte cap: default **45 seconds**, range **0 to 600**, with **0 disabling only this
additional cap**. Media-adjusted first-byte limits and the fixed global deadline
still apply. It is available in Timeout Settings, the configuration file, and
`ANTIGRAVITY_TIMEOUT_FALLBACK_FIRST_BYTE_CAP_SECS` for headless/CLI use.

Other existing environment overrides remain supported, including
`ANTIGRAVITY_TIMEOUT_CONNECT_SECS`, `ANTIGRAVITY_TIMEOUT_UPLOAD_MIN_MBPS`,
`ANTIGRAVITY_TIMEOUT_UPLOAD_EXTRA_WAIT_RATIO`, `ANTIGRAVITY_TIMEOUT_UPLOAD_STALL_SECS`,
`ANTIGRAVITY_TIMEOUT_LARGE_BODY_MB`, and
`ANTIGRAVITY_TIMEOUT_LARGE_BODY_ENDPOINT_ATTEMPTS`.

The authenticated management endpoint `GET /api/system/timeouts/effective-upload`
and desktop command `get_effective_timeout_config` expose normalized settings,
valid environment overrides, representative upload estimates, and replay limits.
The contextual panel refreshes after saving. Connection-timeout changes still
require rebuilding the HTTP client, as described by that setting.

## Verification and rollback

Focused checks cover terminal classification/planning, one in-place retry per
account, preserved streaming QoS context, byte-based payload thresholds, configurable
fallback caps, serialization, endpoint preference, and passive slow-link sampling.
The integration test `simulate_upstream_rotation` exercises production policy
functions without contacting Google or starting a mock service.

Live acceptance should compare slow progressing uploads versus true stalls,
concurrent account requests, Sandbox fallback, cancellation, and all four protocols.
These paths are not established by compilation or the policy tests alone.

Revert the transport/upload functional change as a unit to restore the previous
policy. No database migration is introduced; older configurations receive the
fallback cap's default, and the optional field can be removed during rollback.
