```
Codex /v1/responses JSON                          Gemini v1internal JSON
═════════════════════════════                      ═════════════════════════

outer body:                                       outer body:
┌─ model                 ────────────────────────→ resolve_model_route() ──→ model
├─ instructions (string) ─┐                       ┌─ request:
│                         ├─ sanitize ────────────→│  ├─ systemInstruction ← ╗
│                         │ + Antigravity identity │  │   {role:"user",      ║
│                         │ + global system prompt │  │    parts:[{text}…]}  ║ ~17.5K tokens
│                         └────────────────────────│  │                      ║ stable prefix
│                                                  │  ├─ tools ────── ╗     ║
├─ tools (Codex schema)  ── flatten ── sort ──────→│  │   {functionDeclarations:[…]} ║
│   {type,function,…}     + clean + uppercase      │  │                 ║     ║
│                                                  │  ├─ toolConfig     ║     ║
│                                                  │  ├─ generationConfig ← context params
│                                                  │  ├─ sessionId ← FNV-1a(account_id)
│                                                  │  └─ contents ← ═══════════════╝
│                                                  │      ↕ (convert by role)
├─ input[]                                         │
│  ├─ {type:"message", role, content}             │    {role: user/model,
│  │   └─ text / input_image ─────────────────────→│     parts:[{text/inlineData}]}
│  │                                                │
│  ├─ {type:"function_call", name, arguments, id} │    {role: model,
│  │   └─ name → shell/apply_patch/… ────────────→│     parts:[{functionCall:{name,args,id}}]}
│  │                                                │
│  ├─ {type:"function_call_output", call_id, output}│  {role: user,
│  │   └─ output → {result} ──────────────────────→│     parts:[{functionResponse:{name,response,id}}]}
│  │                                                │
│  └─ {type:"local_shell_call" / "web_search_call"}→│  same as above, special name mapping
│
├─ temperature           ─────────────────────────→ generationConfig.temperature
├─ max_tokens            ─────────────────────────→ generationConfig.maxOutputTokens
├─ top_p                 ─────────────────────────→ generationConfig.topP
├─ thinking              ─────────────────────────→ generationConfig.thinkingConfig {thinkingBudget}
├─ stream                ── controlled by handler ──→ streamGenerateContent / generateContent
│
└─ prompt_cache_key (Codex)── unused
                                                    
                                                    outer body (cont.):
                                                    ├─ project
                                                    ├─ userAgent: "antigravity"  
                                                    └─ requestId ← [tail]
```

**Key path in three lines:**

| Flow                                                          | Transform                       |
| :----------------------------------------------------------- | :----------------------------- |
| **Codex `instructions`** → developer message → `sanitize()` scrubs dynamic values → adds Antigravity identity → `systemInstruction.parts[].text` | Core of the stable prefix (~17.5K tokens) |
| **Codex `input[]`** → mapped item by item: `message`→`contents[role]`, `function_call`→`functionCall`, `function_call_output`→`functionResponse` | Dynamic content (~1.1M tokens) |
| **Codex `tools[]`** → `flatten` flattens the namespace → `sort` sorts by name → clean schema → `tools[].functionDeclarations` | Part of the stable prefix (~5K tokens) |