# 🌳 Orchard

**A programming language for building autonomous AI agents.** You write an agent
as a single `.orch` file, its model, memory, tools, persona, safety policy, and
behavior, and you run it with one command. The language and runtime are written
in Rust, and the runtime is a library you can embed in Rust, C, Python, or the
browser through WebAssembly.

Orchard comes out of the research at **Artemis Labs**. We kept running into the
same wall: building a real agent meant gluing together SDKs, prompt strings,
retry loops, JSON parsing, and a pile of safety checks, all in a general-purpose
language that understood none of it. Orchard makes the agent the unit of the
language instead. Model calls, tool use, memory, typed generation, concurrency,
and budgets are first-class, so the things you reason about when you build an
agent are the things the language actually knows about.

Current release: **3.1.0**. Docs and tutorials live at
**[orchardproject.dev](https://orchardproject.dev)**.

```rust
// triage.orch: deterministic glue around an autonomous span
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
Done. 6 × 7 = 42, saved to memory as 'last_answer'. Ran fully offline.
```

## Install

macOS and Linux, via the install script:

```console
$ curl -fsSL https://raw.githubusercontent.com/Artemis-Inc/Orchard/main/scripts/install.sh | sh
```

Homebrew:

```console
$ brew install artemis-inc/orchard/orchard
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/Artemis-Inc/Orchard/main/scripts/install.ps1 | iex
```

With Cargo, or Docker if you would rather not install anything:

```console
$ cargo install --git https://github.com/Artemis-Inc/Orchard orch-cli
$ docker run --rm ghcr.io/artemis-inc/orchard:latest --version
```

Every method installs the same `orch` binary. Full notes, including the macOS
`.pkg` and Windows `.msi` installers and checksums, are at
[orchardproject.dev/install](https://orchardproject.dev/install). Confirm it
worked with `orch --version`, then scaffold your first agent with `orch new`.

## How it works

Every interaction with a model is one of two verbs.

- **`gen`, where you orchestrate.** A single model call that returns a value.
  `gen as T` constrains and validates the reply into a typed `enum`, `type`, or
  `list` using a JSON schema plus validate-and-retry, so you get data back, not a
  string you have to parse and hope about. The call sees persona context only, so
  it stays controlled and close to reproducible.
- **`delegate`, where the model orchestrates.** You hand a goal to the full
  perceive, think, act loop. The model picks tools, calls your skills, reads
  results, and iterates until it is done. Wrap it in a `budget` to cap spend and
  steps.

The pattern that defines Orchard is deterministic glue (`for`, `if`, `match`,
`retry`, `budget`) wrapped around autonomous spans and typed model calls. You
decide what is fixed and what is left to the model, in one place, in one
language. Concurrency is first-class too: `spawn`, `await`, and `parallel` run
delegate spans at the same time with thread-safe policy, state, and storage.

## What you can give an agent

Capabilities are granted in the file, one line each, and gated by policy.

- **Tools.** Built-in packs for math (`use calculator`), files (`use files`),
  the shell (`use shell`), the web (`use web`), HTTP APIs (`use http`), time
  (`use time`), and a real headless browser (`use browser`). You can also declare
  your own tools inline, expose a skill as a tool, or connect a Model Context
  Protocol server.
- **A browser, for real.** `use browser` drives a headless Chrome over the
  DevTools Protocol. The agent can open a page, read it after JavaScript runs,
  click, type, run JavaScript against the DOM, and save a screenshot.
- **Memory.** Durable facts and conversation history, with semantic recall, kept
  in an embedded store so it survives across runs.
- **Persona and policy.** Tone and instructions on one side, hard limits on the
  other: spend caps, step and tool-call ceilings, an allowlist of domains, and a
  shell mode that defaults to off and can require confirmation per command.

Safety is built in, not bolted on. Budgets and spend caps, secret redaction,
untrusted-content sentinels around anything an agent reads from the outside, an
egress guard that blocks private addresses and strips credentials across hosts,
and filesystem containment all hold without you wiring them up.

## The agent terminal

Run an agent and you get a live terminal session, not a black box. `orch run`
opens with the agent and its model, streams the model's answer token by token as
it is written, and shows the full pipeline as the agent works: each model call,
each tool call and its result, token counts, and the autonomous loop's progress.
Type `/` to see the agent's skills and tools. When you pipe it, it falls back to
clean line output for scripts and CI.

```console
$ orch run examples/3.0/atlas.orch          # a capable agent: files, shell, web, browser, math
$ orch run examples/3.0/scout.orch          # a web researcher that drives the browser
```

## The `orch` CLI

| Command | Purpose |
|---|---|
| `orch run a.orch` · `-t "task"` · `--skill name k=v` | open the agent terminal · run one task · call a skill |
| `orch check a.orch` | static analysis: parse, types, and lints, with no execution |
| `orch compile a.orch -o ir.json` | lower an agent to the JSON IR |
| `orch fmt a.orch [--check]` | canonical formatting |
| `orch new myagent` | scaffold a starter `.orch` that runs offline |
| `orch info a.orch` · `orch doctor` | inspect an agent · check your environment |

## Build from source

```console
$ cargo build --workspace      # the core, the CLI, and the C FFI
$ cargo test --workspace       # the full offline test suite (no keys, no network)
$ cargo run -p orch-cli -- run examples/3.0/demo-offline.orch -t demo
```

The Python and WebAssembly bindings build with their own toolchains (`maturin`
and `wasm-pack`). See the embedding guide linked below.

## Learn

- **[orchardproject.dev/docs](https://orchardproject.dev/docs)** is the home for
  everything below.
- **[Learn Orchard](https://orchardproject.dev/docs/learn)** is a guided tutorial
  that starts from your first agent and works up to tools, typed generation,
  memory, delegation, and concurrency.
- **[Language reference](https://orchardproject.dev/docs/reference)** documents
  every construct.
- **[CLI reference](https://orchardproject.dev/docs/cli)** covers every command
  and flag.
- **[Embedding guide](https://orchardproject.dev/docs/embedding)** shows how to
  run Orchard from Rust, C, Python, and WebAssembly.
- **[examples/3.0/](examples/3.0/)** holds runnable agents. Start with
  [`demo-offline.orch`](examples/3.0/demo-offline.orch), which needs no API key
  and no network.

## License

MIT. See [LICENSE](LICENSE).

Built at [Artemis Labs](https://github.com/Artemis-Inc).
