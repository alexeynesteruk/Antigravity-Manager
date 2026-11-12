# What Google can observe from this proxy

Reviewed November 12, 2026, against local `1e6e90b6a613e26cb2b4b06a6ebd29588fa92b85`.

Google can associate forwarded requests with the authorizing Google account and observe their connection characteristics, submitted content, identifiers, and timing. The proxy does not make those requests anonymous. Its implementation also produces recognizable protocol and identity patterns. Whether Google uses any particular pattern to classify this project, and which thresholds trigger enforcement, is not public.

## Signals confirmed in the code

| Signal | Local evidence | What this establishes |
| --- | --- | --- |
| Account authorization | [upstream/client.rs](../../../src-tauri/src/proxy/upstream/client.rs), line 1407, sends an OAuth Bearer token. [oauth.rs](../../../src-tauri/src/modules/oauth.rs), lines 445 and 589, handles token exchange/refresh. | Google can associate requests with the token's authorized account. Header normalization does not remove that association. |
| OAuth application and requested permissions | `modules/oauth.rs:399` requests OpenID, Cloud Platform, profile/email, `cclog`, and experiments/configuration scopes. Authorization supplies client identity and redirect information. | Google receives the application identity, requested permissions, and authorization/refresh activity. Requesting a logging-related scope alone does not prove logging data is uploaded. |
| Network source | `proxy/upstream/client.rs:799/813` selects direct or configured upstream-proxy clients; request dispatch is around line 1623. | Google's edge sees the connection's egress IP. This may be the server or an upstream proxy, rather than the original workstation. |
| TLS and HTTP behavior | `proxy/upstream/client.rs:781` selects Chrome123 emulation. [utils/http.rs](../../../src-tauri/src/utils/http.rs), lines 99 and 144, builds other clients used for OAuth/project operations. | Google can observe connection behavior. Establishing an actual mismatch from an official client requires measurement; selecting an emulation profile does not establish indistinguishability. |
| User-Agent and platform claims | [hub_version.rs](../../../src-tauri/src/modules/hub_version.rs), lines 93–102, fixes Hub platform fields to Windows/amd64; `upstream/client.rs:1414` sends the resulting UA. `constants.rs:198` constructs an OAuth UA containing the literal placeholder `vscode/1.X.X`. | The requests make specific version/platform claims. Fixed platform claims on other operating systems and placeholder versions are possible distinguishing features. This workspace runs Windows, so a Windows claim alone is not evidence of a platform mismatch here. |
| Project and model | [gemini/wrapper.rs](../../../src-tauri/src/proxy/mappers/gemini/wrapper.rs), line 722, includes `project`, `model`, body `userAgent`, and request ID. [project_resolver.rs](../../../src-tauri/src/proxy/project_resolver.rs), line 30, performs discovery with IDE metadata. | Google receives project/model information. Omitting inference's quota-project header does not hide the project already included in JSON. |
| Session and request identifiers | [common/session.rs](../../../src-tauri/src/proxy/common/session.rs), lines 61–73, derives upstream sessions from account/conversation/generation. [common_utils.rs](../../../src-tauri/src/proxy/mappers/common_utils.rs), line 2129, constructs request IDs. | Stable derived identifiers permit correlation even when the original internal identifier is hashed. |
| Prompts, history, media, tools, and tool results | `gemini/wrapper.rs` constructs the request body; `upstream/client.rs:1506` serializes it and lines 1646–1652 send it. | Google receives the content retained in the transformed request. It terminates TLS and processes the payload. Protocol conversion does not conceal surviving client-specific instructions, tool names, or schemas. |
| Request behavior | `upstream/client.rs:498–501` selects endpoints; retry dispatch is around line 1623. Scheduling/stickiness lives in `token_manager.rs`; optional warmup runs through `modules/scheduler.rs:121`. | Google observes request timing, sizes, endpoints, token identities, retries, and concurrent activity. Its use of these observations in private classifiers is unknown. |

[Google's OAuth documentation](https://developers.google.com/identity/protocols/oauth2) explains the connection between registered clients, granted scopes, access tokens, and authorized API requests. A token can identify the account and registration without proving which executable is currently holding it. Detecting this specific proxy requires additional evidence or inference.

## Distinctive implementation patterns

The request-ID helper emits `agent/<16 hex>/<timestamp>/<8 hex>/<step>` for ordinary nonempty sessions. The sample in that function's comment uses full UUID-shaped conversation and trajectory components. This is a concrete difference from the repository's reference sample, not an independently measured description of all official traffic.

The inference client combines a selected Chrome123 transport profile with an Antigravity Hub application UA, while OAuth/discovery use other client builders. The UA builder always advertises Windows/amd64. These combinations and the OAuth version placeholder give Google potential classifier features. Their existence does not prove that Google checks them or that changing one would prevent classification.

Submitted content can provide another signal. A generic gateway may forward instructions and tool vocabularies originating from several agent clients. The code also creates its own system reminders and alignment structures. Google receives whichever of these survive conversion. The inference that their combinations could distinguish gateway traffic is technically plausible; no private detection rule was established.

Large bursts, repeated context on several accounts, synchronized refreshes, or sustained automated requests are also observable in principle. They can occur for legitimate reasons as well. This review did not measure the user's traffic or establish a behavioral signature for their account.

## Machine IDs and telemetry are separate paths

The audited inference path explicitly filters `x-machine-id`, `x-vscode-sessionid`, `x-session-id`, client-name/version headers, and `x-jeikcode*` from forwarded extra headers (`upstream/client.rs:1440–1447`). No raw workstation machine ID was established as transmitted by that path. Body-level session/request identifiers still remain.

The desktop account-switch feature separately writes identifiers such as `telemetry.machineId`, `telemetry.devDeviceId`, and `storage.serviceMachineId` into the official IDE's local storage: [device.rs](../../../src-tauri/src/modules/device.rs), lines 226–258. What the official IDE subsequently sends is governed by its own behavior and settings; the gateway's header filtering does not control it.

Local monitor/debug captures are another distinct path. `upstream/client.rs:1480` records redacted headers locally, and [debug_logger.rs](../../../src-tauri/src/proxy/debug_logger.rs), line 115, writes filesystem logs. Those audited operations do not establish that the log files themselves are uploaded to Google. The underlying inference payload is sent independently.

## What Google publishes

Google's current Antigravity additional terms explicitly address third-party software accessing the service with Antigravity OAuth and describe possible suspension or termination. They also state that disabling telemetry still permits data use to provide the service and maintain safety. Access through enterprise/Cloud arrangements can have different applicable terms. These statements establish policy context, not a disclosed detection algorithm. [Google Antigravity Additional Terms](https://antigravity.google/terms)

Google's privacy policy describes collecting application/device identifiers and versions, IP addresses, request timing, activity, and other information, and using information to address abuse and security risks. This establishes published categories of collection and use; it does not prove that this Rust proxy sends every category mentioned in that policy. [Google Privacy Policy](https://policies.google.com/privacy)

Google's API terms describe monitoring API usage and prohibit masking client identity. They do not publish a classifier for this repository. [Google APIs Terms](https://developers.google.com/terms)

For custom integrations, assess documented Google API access and the terms applicable to that service rather than assuming an Antigravity OAuth session is interchangeable with a supported API integration. Consumer Antigravity and Cloud products should not be assumed to have identical retention, monitoring, or account policies.

## Interpreting an actual failure

A 429 or 403 alone does not establish proxy detection. Quota, capacity, validation requirements, region/model eligibility, malformed payloads, and enforcement can produce overlapping symptoms. Distinguishing them requires the structured error reason, affected endpoint/model, timestamps, token-refresh outcome, and the account's status in the official service. Keep credentials and private prompts out of any shared diagnostic sample.

This was static code review and public-document research. No user credentials, packet captures, authenticated Google probes, or account-specific enforcement evidence were accessed. The review establishes visibility and possible distinguishing signals, not that Google has classified this user's activity or which signal it used.
