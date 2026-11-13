# Multiple Antigravity accounts on one machine — online findings

Researched November 12, 2026. No verified method was found that guarantees authenticated multi-account use is invisible or uncorrelatable to Google. Local credential isolation, service eligibility and private detection rules are different questions.

## What the current sources support

An October 19 answer on Google's developer forum describes manually switching between two independently paid, non-shared Pro subscriptions on the same computer as permitted. The question excludes automated switching and family/discount arrangements. The reply gives official sign-out/sign-in and the older IDE's Reset Onboarding command as switching paths. This is a forum answer addressing that stated case, not a guarantee about arbitrary account-pooling tools or enforcement outcomes. [Forum discussion](https://discuss.ai.google.dev/t/google-antigravity-using-two-separate-paid-google-ai-pro-accounts/190436)

The current product FAQ explicitly says consumer plans do not support automated account rotation or wrapping headless `agy` inside third-party agents/applications. It directs third-party Gemini integrations toward Enterprise or an AI Studio API key. Direct CLI scripting and CI are described separately. [Current FAQ](https://antigravity.google/docs/faq#can-i-use-antigravity-cli-headless-mode-in-third-party-coding-agents-or-automate-account-switching)

The additional terms separately address third-party software using Antigravity OAuth and possible restrictions. They also state that telemetry opt-out does not stop data use needed to provide and protect the service. Enterprise arrangements have separate applicable terms. [Additional terms](https://antigravity.google/terms)

## Practical local isolation

1. For the forum's independently paid account scenario, finish active work and switch through the official client. Current CLI authentication documentation describes `/logout` and subsequent Google sign-in; logout clears saved authentication profiles/local caches, so avoid doing it while work is running. [Authenticate](https://antigravity.google/docs/install/)
2. Separate Chrome profiles keep browser cookies/settings separate and reduce accidentally authorizing the wrong Google account. They do not isolate the native Antigravity credential store. [Chrome profiles](https://support.google.com/chrome/answer/2364824?hl=en)
3. Current Antigravity documentation describes shared authentication across local App/CLI/IDE surfaces using the OS keyring. A different working directory or browser profile therefore is not an independent native account session. [Authenticate](https://antigravity.google/docs/install/)
4. On Windows, separate OS users provide separate local user data and credential sets; this is an isolation option to evaluate for genuinely independent sessions. Microsoft documents user accounts under Settings → Accounts → Other users and scopes Credential Manager reads to the caller's logon credential set. This review did not test simultaneous Antigravity instances in separate Windows sessions. [Windows users](https://support.microsoft.com/en-us/windows/security/identity-signin/manage-user-accounts-in-windows), [Credential Manager API](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreada)

None of these local boundaries makes OAuth account identities or network connections disappear from Google. Whether Google links two accounts to the same physical device, and how it decides to enforce a policy, was not established.

## Community tools and what their existence proves

The examined `ismaelsoilet/antigravity-account-switcher` project documents manual local profile/token switching and explicitly distinguishes it from parallel instance isolation and quota pooling. It coordinates several token files, databases and keyrings because local surfaces can disagree. That is useful architecture evidence about credential conflicts, not evidence of server-side invisibility. [Project documentation](https://github.com/ismaelsoilet/antigravity-account-switcher)

Other projects advertise automatic quota-aware rotation, including `hrmasss/antigravity-swap`. Their advertised functionality does not establish Google's approval, private detection behavior, or freedom from restrictions. This review did not install those tools or submit their traffic. [Project documentation](https://github.com/hrmasss/antigravity-swap)

Older advice about `--user-data-dir`/`--profile` should be checked against the exact product/version: a March/April forum discussion addresses local instance folders, whereas current documentation describes shared native authentication. Neither is an undetectability claim. [Older profile discussion](https://discuss.ai.google.dev/t/how-do-user-data-dir-and-profile-work/137054), [current authentication documentation](https://antigravity.google/docs/install/)

For this gateway, shared default/per-proxy HTTP clients and fallback-to-default routing are additional reasons not to promise account unlinkability or strict per-account egress isolation. Concrete bugs discovered while investigating that path are recorded in [the deep review](deep-review.md).
