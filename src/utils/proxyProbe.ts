export type ProbeProtocol = 'openai' | 'responses' | 'anthropic' | 'gemini';

/** The reply "pong" is what proves a model works; see classifyReply. */
export const PING_PROMPT = 'Reply with exactly: pong';

interface ProbeProtocolSpec {
    path: (model: string) => string;
    template: (model: string) => unknown;
    headers: (apiKey: string) => Record<string, string>;
}

export const PROBE_PROTOCOLS: Record<ProbeProtocol, ProbeProtocolSpec> = {
    responses: {
        path: () => '/v1/responses',
        template: (model) => ({ model, input: PING_PROMPT, stream: false, store: false }),
        headers: (apiKey) => ({ Authorization: `Bearer ${apiKey}` }),
    },
    openai: {
        path: () => '/v1/chat/completions',
        template: (model) => ({ model, messages: [{ role: 'user', content: PING_PROMPT }], stream: false }),
        headers: (apiKey) => ({ Authorization: `Bearer ${apiKey}` }),
    },
    anthropic: {
        path: () => '/v1/messages',
        template: (model) => ({ model, messages: [{ role: 'user', content: PING_PROMPT }], stream: false }),
        headers: (apiKey) => ({ 'x-api-key': apiKey, 'anthropic-version': '2023-06-01' }),
    },
    gemini: {
        path: (model) => `/v1beta/models/${encodeURIComponent(model)}:generateContent`,
        template: () => ({ contents: [{ role: 'user', parts: [{ text: PING_PROMPT }] }] }),
        headers: (apiKey) => ({ 'x-goog-api-key': apiKey }),
    },
};

export interface ProbeResult {
    status: number;
    ttftMs: number | null;
    totalMs: number;
    account: string | null;
    mappedModel: string | null;
    headers: [string, string][];
    body: string;
}

/** Thrown when the request body is not valid JSON; no request is sent. */
export class ProbeInputError extends Error {}

function post(baseUrl: string, apiKey: string, protocol: ProbeProtocol, model: string, body: unknown, signal?: AbortSignal) {
    const spec = PROBE_PROTOCOLS[protocol];
    return fetch(baseUrl.replace(/\/+$/, '') + spec.path(model), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...spec.headers(apiKey) },
        body: JSON.stringify(body),
        signal,
    });
}

export async function sendProbe(
    baseUrl: string, apiKey: string, protocol: ProbeProtocol, model: string, bodyText: string, signal?: AbortSignal,
    limits: { timeoutMs?: number; maxResponseBytes?: number } = {},
): Promise<ProbeResult> {
    if (signal?.aborted) throw signal.reason ?? new DOMException('Probe cancelled.', 'AbortError');
    if (bodyText.length > 1024 * 1024) throw new ProbeInputError('Request exceeds the 1 MiB probe limit.');
    let body: unknown;
    try {
        body = JSON.parse(bodyText);
    } catch (e) {
        throw new ProbeInputError(e instanceof Error ? e.message : String(e));
    }
    const controller = new AbortController();
    const forwardAbort = () => controller.abort(signal?.reason);
    if (signal?.aborted) forwardAbort();
    else signal?.addEventListener('abort', forwardAbort, { once: true });
    const timer = setTimeout(() => controller.abort(new DOMException('Probe timed out.', 'TimeoutError')), limits.timeoutMs ?? 120_000);
    const start = performance.now();
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let cancel: (() => void) | undefined;
    const aborted = new Promise<never>((_, reject) => {
        cancel = () => {
            void reader?.cancel().catch(() => {});
            reject(controller.signal.reason ?? new DOMException('Probe cancelled.', 'AbortError'));
        };
        if (controller.signal.aborted) cancel();
        else controller.signal.addEventListener('abort', cancel, { once: true });
    });
    try {
        return await Promise.race([aborted, (async () => {
    const res = await post(baseUrl, apiKey, protocol, model, body, controller.signal);
    let ttftMs: number | null = null;
    let text = '';
    let bytes = 0;
    let scanned = 0;
    const streaming = res.headers.get('content-type')?.includes('text/event-stream');
    if (res.body) {
        reader = res.body.getReader();
        const decoder = new TextDecoder();
        for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            bytes += value.byteLength;
            if (bytes > (limits.maxResponseBytes ?? 1024 * 1024)) throw new Error('Response exceeds the probe limit.');
            text += decoder.decode(value, { stream: true });
            if (streaming && ttftMs === null) {
                const end = text.lastIndexOf('\n') + 1;
                for (const line of text.slice(scanned, end).split('\n')) {
                    if (!line.startsWith('data:')) continue;
                    try {
                        if (hasOutput(JSON.parse(line.slice(5).trim()))) ttftMs = Math.round(performance.now() - start);
                    } catch { /* Comments, keepalives and incomplete events are not tokens. */ }
                }
                scanned = end;
            }
        }
        text += decoder.decode();
    }
    if (!streaming) {
        try { if (hasOutput(JSON.parse(text))) ttftMs = Math.round(performance.now() - start); } catch { /* Non-JSON error body. */ }
    }
    return {
        status: res.status,
        ttftMs,
        totalMs: Math.round(performance.now() - start),
        account: res.headers.get('X-Account-Email'),
        mappedModel: res.headers.get('X-Mapped-Model'),
        headers: [...res.headers.entries()],
        body: text,
    };
        })()]);
    } finally {
        clearTimeout(timer);
        signal?.removeEventListener('abort', forwardAbort);
        if (cancel) controller.signal.removeEventListener('abort', cancel);
        void reader?.cancel().catch(() => {});
    }
}

function hasOutput(value: any): boolean {
    const nonempty = (v: unknown) => typeof v === 'string' && v.length > 0;
    const delta = value?.choices?.[0]?.delta ?? value?.delta;
    return nonempty(delta?.content) || nonempty(delta?.text) || nonempty(delta?.thinking)
        || nonempty(delta?.partial_json) || Boolean(delta?.tool_calls?.length)
        || (typeof value?.type === 'string' && /(?:output_text|reasoning_text|function_call_arguments)\.delta$/.test(value.type) && nonempty(value.delta))
        || nonempty(value?.choices?.[0]?.message?.content)
        || Boolean(value?.content?.some?.((p: any) => nonempty(p?.text)))
        || Boolean(value?.output?.some?.((o: any) => o?.content?.some?.((p: any) => nonempty(p?.text))))
        || Boolean(value?.candidates?.[0]?.content?.parts?.some?.((p: any) => nonempty(p?.text) || p?.functionCall || p?.inlineData));
}

export type PingState = 'ok' | 'fail' | 'limited' | 'skipped';

export interface PingResult {
    state: PingState;
    status: number;
    ms: number;
    message: string;
    /** Model id reported in the reply body, when present (may differ from the requested one). */
    servedModel: string | null;
    /** The full exchange, shown in the probe when the result is clicked. */
    probe: ProbeResult | null;
}

export type PingVerdict = Pick<PingResult, 'state' | 'message' | 'servedModel'>;

function replyText(protocol: ProbeProtocol, json: any): string | null {
    const join = (parts: unknown) => Array.isArray(parts)
        ? parts.map((p: any) => (typeof p?.text === 'string' ? p.text : '')).join('')
        : null;
    if (protocol === 'openai') {
        const content = json?.choices?.[0]?.message?.content;
        return typeof content === 'string' ? content : null;
    }
    if (protocol === 'anthropic') return join(json?.content);
    if (protocol === 'responses') return join(json?.output?.flatMap?.((o: any) => o?.content ?? []));
    return join(json?.candidates?.[0]?.content?.parts);
}

/**
 * Judge a non-streaming reply. Retired models still answer 200 with a "no longer available,
 * switch to …" text, so only the requested "pong" counts as working; any other reply becomes
 * the failure message. Returns null for bodies that are not JSON (e.g. a raw SSE stream).
 */
export function classifyReply(protocol: ProbeProtocol, status: number, body: string): PingVerdict | null {
    let json: any;
    try { json = JSON.parse(body); } catch { return null; }
    const content = replyText(protocol, json);
    const served = json?.model ?? json?.modelVersion;
    const servedModel = typeof served === 'string' && served ? served : null;
    if (status >= 200 && status < 300 && content && /\bpong\b/i.test(content)) {
        return { state: 'ok', message: '', servedModel };
    }
    const upstream = json?.error?.message;
    const message = typeof upstream === 'string' && upstream ? upstream
        : content?.trim() ? content.trim().slice(0, 300)
        : body.slice(0, 300);
    return { state: status === 429 ? 'limited' : 'fail', message, servedModel };
}

export async function pingModel(baseUrl: string, apiKey: string, model: string, signal?: AbortSignal): Promise<PingResult> {
    if (/-image/i.test(model)) return { state: 'skipped', status: 0, ms: 0, message: '', servedModel: null, probe: null };
    const start = performance.now();
    let probe: ProbeResult;
    try {
        // No max_tokens: thinking models would otherwise spend the budget and return empty content.
        probe = await sendProbe(baseUrl, apiKey, 'openai', model, JSON.stringify(PROBE_PROTOCOLS.openai.template(model)), signal);
    } catch (e) {
        if (signal?.aborted) throw e;
        const message = e instanceof Error ? e.message : String(e);
        return { state: 'fail', status: 0, ms: Math.round(performance.now() - start), message, servedModel: null, probe: null };
    }
    const verdict = classifyReply('openai', probe.status, probe.body)
        ?? { state: probe.status === 429 ? 'limited' : 'fail', message: probe.body.slice(0, 300), servedModel: null };
    return { ...verdict, status: probe.status, ms: probe.totalMs, probe };
}
