# Account safety and request budgets

Open **API Proxy → Service settings → Account safety and request budgets** to see the current stop state, outbound requests used today, and effective limits. Device credentials are separate User Tokens; caller-supplied device names or IP headers do not create a new budget identity. The master gateway key and unauthenticated requests share a single incoming-request budget. Internal warmup requests use a system identity and share the global outbound budget.

These are local operator limits, not provider-approved thresholds, token billing estimates, or a guarantee against account restrictions.

## Emergency stop

By default, a proven `ACCOUNT_RESTRICTED` response or an explicit authentication challenge stops gateway dispatch globally. Individual accounts retain their separate manual recovery holds. Ordinary provider 429 responses, transient transport failures, model permissions, and an unproven 403 do not activate this stop.

The stop persists in `gateway_safety.db` in the application data directory and survives normal restarts. The in-process stop is raised before durable account metadata work. If persistence fails, the running process continues to deny dispatch; repair storage before restarting, since a failed disk write cannot guarantee persistence across a restart.

Review the provider's restriction or verification request in **Accounts**, then select the acknowledgement checkbox and **Acknowledge and resume**. A stale acknowledgement cannot clear a newer event. Resume does not recover individual accounts, reset usage counters, or revive queued work admitted before the stop. Requests already dispatched cannot be recalled. Changing settings, refreshing credentials, clearing provider cooldowns, or restarting does not acknowledge a persisted stop.

The stop blocks automatic subscription activity: proactive OAuth refreshes, startup and scheduled quota sweeps, automatic warmups, QoS probes, generation retries, image requests and continuations. Account management and explicit OAuth/recovery operations remain available. A successful manual account recovery does not itself acknowledge the global stop.

Frontend periodic refreshes, MiniView timers and post-switch quota reads use dedicated automatic quota entrypoints, rather than borrowing the manual recovery command. Background jobs retain their captured stop generation through asynchronous workers. Failed durable-stop writes keep a bounded process-local latch even after the last request drops its permit.

## Request accounting

Incoming request budgets apply after authentication across Chat Completions, Responses, Claude, Gemini, image and warmup routes. Every Responses WebSocket turn is admitted separately. Separate authenticated User Token IDs receive separate incoming budgets. The global budget counts each attempted `generateContent` or `streamGenerateContent` HTTP dispatch, including endpoint retries, image fan-out, continuation requests, background summaries and QoS probes. Attempts are charged before dispatch and are not refunded for a failed connection. OAuth, model discovery and quota reads do not consume the model-attempt budget.

Daily counters use UTC days. Minute limits use a rolling 60-second window. Both are stored transactionally, including rate windows, so restarting does not reset them. Concurrent callers cannot spend the same remaining request. Clock rollback does not reset counters. Only the current UTC day's client counters and recent rate buckets are retained; at most 10,000 client scopes may be recorded in one day.

Local budget rejections return HTTP **429** with `Retry-After`, an `error.type` of `gateway_request_limit`, and `REQUEST_RATE_LIMIT` or `DAILY_REQUEST_BUDGET`. They stop the current request without banning, cooling down, or rotating provider accounts. If a continuation hits a limit after streaming headers were sent, its protocol error event carries the limit and retry delay; it does not synthesize successful completion.

## Configuration and headless operation

The **Moderate preset** selects 30 incoming requests per device per minute, 60 global outbound model attempts per minute, 2,000 global outbound attempts per UTC day, and no additional per-device daily cap. New configurations otherwise default to 10 incoming requests per device per minute, 20 global outbound model attempts per minute and 500 outbound attempts per UTC day. A zero numeric limit disables that limit while counters continue tracking usage.

These fields live under `proxy.account_safety` in the application configuration:

```json
{
  "halt_on_account_restriction": true,
  "halt_on_authentication_challenge": true,
  "client_requests_per_minute": 30,
  "client_requests_per_day": 0,
  "upstream_requests_per_minute": 60,
  "upstream_requests_per_day": 2000
}
```

Environment overrides take precedence over saved settings:

| Field | Environment variable |
| --- | --- |
| `halt_on_account_restriction` | `ANTIGRAVITY_HALT_ON_ACCOUNT_RESTRICTION` |
| `halt_on_authentication_challenge` | `ANTIGRAVITY_HALT_ON_AUTHENTICATION_CHALLENGE` |
| `client_requests_per_minute` | `ANTIGRAVITY_CLIENT_REQUESTS_PER_MINUTE` |
| `client_requests_per_day` | `ANTIGRAVITY_CLIENT_REQUESTS_PER_DAY` |
| `upstream_requests_per_minute` | `ANTIGRAVITY_UPSTREAM_REQUESTS_PER_MINUTE` |
| `upstream_requests_per_day` | `ANTIGRAVITY_UPSTREAM_REQUESTS_PER_DAY` |

Boolean overrides accept `true` / `false` or `1` / `0`. Invalid overrides retain the saved value. Setting either stop policy to false affects future observations; it never clears an existing stop.

The existing administrator-authenticated HTTP API supports the same controls without a desktop:

| Method and path | Purpose |
| --- | --- |
| `GET /api/proxy/account-safety` | Read status, generation, effective configuration and today's outbound count. |
| `POST /api/proxy/account-safety/halt` | Persist an operator emergency stop. |
| `POST /api/proxy/account-safety/resume` | Explicitly acknowledge the current generation. |

Automatic quota clients use `POST /api/accounts/refresh-automatic` or `GET /api/accounts/{accountId}/quota/automatic`. These reject a stop before loading credentials or contacting the provider. Explicit manual quota controls retain their separate administrator endpoints.

Resume requires JSON containing the exact current generation and acknowledgement, for example `{"generation":7,"acknowledged":true}`. A newer stop produces HTTP 409; read status and review the new event before acknowledging again. Device credentials cannot administer these endpoints. Health and management routes stay available while dispatch is stopped.

No code can recover Google's private enforcement rules. Account verification and appeals must be completed with the provider. Stream inspection is bounded; a provider error field hidden beyond the bounded inspection prefix may remain unclassified.

## Offline credential migration

Close every desktop and headless instance using this data directory before running the compatible updated executable:

```text
antigravity-server credentials-migrate
```

`antigravity-tools credentials-migrate` performs the same operation. It protects configuration secrets, indexed legacy account credentials and device keys in the OS credential store, and prints only a migration count. It does not start a listener, inspect client login state, switch accounts, refresh OAuth tokens or contact the provider. A native-store failure is explicit; completed earlier steps remain protected and the command can be retried after repairing access.

Keep an encrypted pre-migration backup and the corresponding compatible rollback build or repository reference. Older executables cannot read migrated native-store references or hashed device-key records; replacing the executable alone is not a rollback. Native references depend on the original OS user and credential store. Existing legacy archives and backups need separate protection. The plaintext `export` command is not a migration or rollback fallback.
