# Changelog

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
