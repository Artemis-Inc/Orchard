# Changelog

## 3.1.0 — The agent terminal

### The agent terminal (`orch run`)
- `orch run` is now a live agent terminal. An entrance banner shows the agent,
  its model, and the working directory; typing `/` opens a palette of the
  agent's skills and tools; and the whole pipeline streams as the agent works
  (model calls, tool calls and their results, `emit` output, token counts, an
  animated spinner). On a real terminal it runs a rich raw-mode UI; when piped it
  falls back to clean line streaming. `-t` and `--skill` are unchanged.
- New runtime event stream: `RuntimeBuilder::on_event` observes the live pipeline
  via `AgentEvent` (model start/end with usage, tool start/end, `emit`, task
  complete). The same events power the TUI and can drive any host UI.

### Token-level streaming
- The model's answer now streams token by token into the terminal as it is
  generated, so the thinking/answer text appears live instead of in one block.
- Implemented end to end: a streaming path through the `HttpClient` and
  `Provider` traits (`request_stream` / `chat_stream`), real Server-Sent-Events
  parsing for Anthropic and OpenAI-compatible providers (text deltas, streamed
  tool calls, and usage), streaming through provider fallback chains, and a
  chunked offline path for the `mock` provider. Non-streaming clients keep
  working unchanged via safe defaults.

### Browser control
- New `browser` tool pack: grant it with one line, `use browser`, and the
  autonomous loop can drive a real headless Chrome over the Chrome DevTools
  Protocol — `browser_open`, `browser_read` (rendered text, after JavaScript),
  `browser_click`, `browser_type`, `browser_eval`, and `browser_screenshot`
  (saves a real PNG). It launches Chrome with an ephemeral profile, speaks CDP
  directly over a WebSocket, and tears the browser down when the run ends. Page
  content is treated as untrusted (sentinel-wrapped). Set `$ORCH_CHROME` to
  point at a specific Chrome/Chromium binary.

### Examples & scaffolding
- `orch new` scaffolds a capable, commented starter agent that grants tools and
  runs offline. Added `examples/3.0/atlas.orch` (a flagship agent granted files,
  shell, web, HTTP, browser, and math, with skills and a safety policy) and
  `examples/3.0/scout.orch` (a web-research agent that drives the browser).

## 3.0.0 — A Rust rewrite

Orchard 3.0 is a from-scratch Rust reimplementation of the Orchard agent
language and runtime (v2 was Python). It preserves the v2 language and behavior
(every v2 example agent runs with identical observable output, offline) and adds
concurrency, embeddability, and a pure-Rust stack.

### Language & front end
- Hand-written lexer (ASI, string interpolation, durations/money, nested block
  comments), recursive-descent parser, gradual type checker, and a stable
  diffable JSON IR — verified structurally identical to v2's IR.
- `orch check` catches v2's error classes with byte-identical caret diagnostics.
- Canonical formatter (`orch fmt`): idempotent, AST-preserving, comment-preserving.

### Runtime
- Tree-walking async interpreter over the IR (control flow via a `Flow` enum).
- The two verbs: `gen` / `gen as T` (validate-and-retry coercion, `GenError`)
  and `delegate` (the autonomous tool loop, shared decrementing budget,
  `max_delegate_depth`, skill exposure as `skill_<name>`).
- **Concurrency: `spawn` / `await` / `parallel`** — real, with thread-safe
  policy/state/store and deterministic ordering. (New in v3; v2 reserved it.)
- Transactional typed `state` (commit at turn end, rollback on uncaught error);
  free-form `facts` memory.
- Tool packs (calculator/files/time/memory), custom declarative tools, and
  **native host tools** (the embedding superpower).
- Providers: Anthropic, OpenAI-compatible (openai/groq/together/openrouter),
  Ollama, and `mock` (offline scripted replay + schema synthesis); fallback
  chains + retry/backoff.
- Durable, pure-Rust store (`redb`) + an in-memory store, behind a `Store` trait.
- Safety: budgets/spend caps (+ $5 unattended cap), secret taint + `«…»`
  redaction, untrusted-content sentinels, an egress guard (private-IP block,
  per-hop recheck, cross-host cred stripping), files realpath containment, and a
  per-(tool,args) circuit breaker.
- Triggers: `on start/message/schedule/file` + `orch serve`.

### Embeddable everywhere
- A core library + a thin `orch` CLI + a **C ABI** + **Python** (PyO3) +
  **WebAssembly** (wasm-bindgen), all thin adapters over one facade API.
- Pure-Rust, no bundled C (rustls, redb).

### Clean break from v2
- No 1.0 YAML dialect, no migrator. Pragma `#!orchard 3.0` (accepts `2.0`).
- Default models bumped to the latest Claude set; pricing refreshed.

### Tools, memory & scheduling (completed)
- `http` / `web` / `shell` tool packs over the egress-guarded client, plus the
  in-body `http.METHOD(...)` and `shell(...)` builtins (same egress/gating as the
  delegate loop). `web` ships `fetch_page` (HTML→text) and keyless `web_search`.
- MCP client: launches an MCP server subprocess, JSON-RPC 2.0 over stdio,
  exposes tools namespaced `ns_<tool>`, gated by `allow_mcp`.
- Bit-exact keyword (TF-IDF cosine) scorer + semantic recall wired into the
  delegate loop's context (sentinel-wrapped), with the conversation window.
- Full 5-field cron matcher in `orch serve` (ranges/steps/lists, `0`/`7` Sunday,
  Vixie DOM/DOW OR rule) with UTC minute-tick + catch-up, alongside `every` and
  file-watch triggers.

### P20 — adversarial review & release
- Independent multi-dimension adversarial review (safety/sandbox, concurrency,
  interpreter parity, new code). All confirmed bug/data-integrity findings fixed
  with regression tests; see `docs/reviews/P20-adversarial-review.md`. Highlights:
  IPv4-mapped-IPv6 SSRF block, host-boundary secret redaction, floored modulo +
  negative indexing + Python-style float text (v2 parity), order-independent
  budget frames + atomic delegate-depth, atomic redb id allocation, MCP string-id
  + fast-fail, UTF-8-safe shell output capping.
- Release packaging (`scripts/package-release.sh`): `orch` CLI, C-FFI static +
  dynamic lib + `orchard.h` header, PyO3 wheel (maturin), and a WASM artifact.
  All four targets build and pass their smoke test (incl. an end-to-end C driver).
