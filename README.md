# 🌳 Orchard 3.0

**A programming language and embeddable runtime for autonomous AI agents,
written in Rust.**

One `.orch` file defines a whole agent — its model, memory, tools, persona,
policy, and behavior. One command runs it. The runtime is a library you can
embed from Rust, C, Python, or the browser (WebAssembly).

```rust
// triage.orch — deterministic glue around an autonomous span
agent Triage {
    model { provider: anthropic, name: "claude-opus-4-8" }
    use web
    memory { facts: true }

    enum Severity { low, medium, high, critical }
    type Ticket { title: str, severity: Severity, summary: str }

    skill triage(report: str) -> Ticket {
        let t = gen as Ticket "Extract a ticket from:\n{report}"   // typed, validated
        match t.severity {                                          // exhaustive
            critical => { remember "oncall" = t.title }
            _        => {}
        }
        return t
    }

    on message(text: str) -> str {
        return budget(spend: $0.10, steps: 6) { delegate text }     // budget-capped loop
    }
}
```

```console
$ orch check triage.orch
✓ triage.orch is a valid Orchard 3.0 agent

$ orch run examples/3.0/demo-offline.orch -t demo      # no API key, no network
Done — 6 × 7 = 42, saved to memory as 'last_answer'. Ran fully offline.
```

## Two verbs

Every model interaction is one of two first-class verbs:

- **`gen` — you orchestrate.** One model call returning a value. `gen as T`
  constrains and validates the reply into a typed `enum`/`type`/`list` (JSON
  schema + validate-and-retry). Persona-only context; controlled and
  near-reproducible.
- **`delegate` — the model orchestrates.** Hand a goal to the full
  perceive→think→act tool loop; the model picks tools, calls your skills, and
  iterates — wrappable in a `budget`.

Deterministic glue (`for`/`if`/`match`/`retry`/`budget`) around autonomous spans
and typed model calls is the move that defines Orchard. Plus first-class
**concurrency** — `spawn` / `await` / `parallel` run delegate spans in parallel.

## Why Rust

- **A real language.** Own lexer, parser, gradual type checker, and a diffable
  JSON IR. `orch check` catches errors before any model call; `orch fmt` is the
  one true style; `orch compile` emits the IR.
- **Embeddable everywhere.** A core library + a thin `orch` CLI + a **C ABI** +
  **Python** (PyO3) + **WebAssembly** (wasm-bindgen) — every binding is a thin
  adapter over one facade API. See [`docs/embedding.md`](docs/embedding.md).
- **Pure Rust, no bundled C.** rustls (not OpenSSL), redb/in-memory (not
  SQLite). Storage, providers, HTTP, and the async executor are traits with
  pure-Rust defaults, so native, WASM, C, and Python all work.
- **Safety enforced by the runtime, not the model.** Hard caps on
  steps/tool-calls/spend; tighten-only `budget` scopes; secret taint-tracking +
  `«…»` redaction at every sink; untrusted-content sentinels; an egress guard
  (blocks loopback/private IPs, re-checks each redirect hop, strips creds
  cross-host); files-pack realpath containment.

## The `orch` CLI

| Command | Purpose |
|---|---|
| `orch run a.orch` / `-t "task"` / `--skill name k=v` | chat / one-shot / a skill |
| `orch check a.orch` | static analysis — parse, types, lints (no execution) |
| `orch compile a.orch -o ir.json` | lower to the JSON IR |
| `orch fmt a.orch [--check]` | canonical formatting |
| `orch new myagent` | scaffold a starter `.orch` |
| `orch info a.orch` · `orch doctor` | inspect an agent · environment check |

## Build

```console
$ cargo build --workspace      # the core, CLI, and C-FFI
$ cargo test --workspace       # the full offline test suite (no keys, no network)
$ cargo run -p orch-cli -- run examples/3.0/demo-offline.orch -t demo
```

The Python and WASM bindings build with their own toolchains (`maturin`,
`wasm-pack`) — see [`docs/embedding.md`](docs/embedding.md).

## Learn

- **[SPEC3.md](SPEC3.md)** — the language + embedding spec.
- **[docs/guide.md](docs/guide.md)** — a progressive tutorial.
- **[docs/language-reference.md](docs/language-reference.md)** — every construct.
- **[docs/embedding.md](docs/embedding.md)** — embed from Rust/C/Python/WASM.
- **[docs/parity-report.md](docs/parity-report.md)** — how v3 reproduces v2.
- **[examples/3.0/](examples/3.0/)** — runnable agents; start with
  [`demo-offline.orch`](examples/3.0/demo-offline.orch).

MIT licensed.
