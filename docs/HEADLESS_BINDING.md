# Listener addresses and headless startup

`proxy.bind_address` optionally selects an exact numeric IPv4/IPv6 listener address. `localhost` normalizes to `127.0.0.1`. Bracketed IPv6 is accepted in configuration and `--host`; the stored value is unbracketed. DNS names, URL strings, ports embedded in addresses, and IPv6 zone identifiers are rejected. Invalid addresses never fall back to a wildcard.

```json
{
  "proxy": {
    "bind_address": "::1"
  }
}
```

This example shows the field to add to an existing configuration, not a complete replacement configuration. The proxy settings panel exposes the same optional address beside the LAN toggle. Clearing the address restores the toggle; changing the toggle clears the exact address. Restart the application or headless server process after changing listener settings: stopping/restarting the logical proxy preserves its administration listener.

Headless precedence is `--host` > `ABV_BIND_ADDRESS` > truthy `ABV_BIND_LOCAL_ONLY` > saved exact address. An installation with no explicit address retains the existing headless wildcard default. The legacy wildcard values `0.0.0.0` and `::` retain the existing dual-stack behavior with IPv4 fallback; specific IPs, including `::1`, are not expanded to wildcards.

```powershell
antigravity-server --host ::1 --port 8045
# Or, for headless startup without a CLI override:
$env:ABV_BIND_ADDRESS = "::1"
antigravity-server
```

An explicit address is persisted through the normal configuration path, so subsequent proxy starts keep it. URLs shown in status, Python examples, CLI synchronization, and tunnel origins follow the configured address; IPv6 URLs use brackets. Authentication in `auto` mode uses the actual live listener's exposure while a saved address change waits for restart. Headless authentication defaults remain in effect.

Internal warmup endpoints continue to require a direct loopback peer and the process credential. IPv6 loopback and wildcard listeners support these checks. Binding exclusively to a specific non-loopback interface makes internal warmup unavailable; this setting does not relax that boundary. A named Cloudflare tunnel's origin remains controlled in Cloudflare; update that origin separately when changing listener addresses.

Headless startup now propagates malformed configuration errors before database startup or override persistence, preserving the original file for repair. A missing or whitespace-only configuration retains the existing initialization behavior. Missing CLI values, invalid ports/hosts, and unknown server options produce an error instead of silently starting with defaults.
