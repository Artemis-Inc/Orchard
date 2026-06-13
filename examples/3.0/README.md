# Orchard 2.0 examples

Complete, runnable agents — each a single `.orch` file in the real Orchard 2.0
language (not YAML). Statically check any of them without running:

```console
$ orch check examples/2.0/assistant.orch
```

**Start here.** [`demo-offline.orch`](demo-offline.orch) needs no API key at all
— the `mock` provider replays a scripted model while every tool call and memory
write happens for real:

```console
$ orch run examples/2.0/demo-offline.orch -t "demo"
```

Then read [`assistant.orch`](assistant.orch), the fully-commented tour of a
typical agent.

| Example | Start? | Requires | Teaches |
|---|---|---|---|
| [`demo-offline.orch`](demo-offline.orch) (+ [`demo-script.yaml`](demo-script.yaml)) | **1st** | nothing (offline) | The whole agent loop with zero keys: the `mock` provider, real tool execution, real fact memory, a `delegate` loop calling a tool, `remember`, and an exposed `skill_note` |
| [`assistant.orch`](assistant.orch) | **2nd** | Anthropic *or* OpenAI key | Every section, heavily commented: `model` with a `fallback` chain, `persona`, conversation+facts `memory`, tool packs, two `skill`s, `on message { delegate }`, and the `gen` vs `delegate` distinction |
| [`ollama-local.orch`](ollama-local.orch) | keyless | [Ollama](https://ollama.com) running | A free, fully local agent — `provider: ollama`, no key, nothing leaves your machine |
| [`triage.orch`](triage.orch) | | Anthropic key | The flagship pattern: `enum` + `type`, `gen as Ticket` (constrained generation), exhaustive `match` routing, a `tool`, an exposed `skill`, and a budgeted `on message` |
| [`pipeline.orch`](pipeline.orch) | | Anthropic key | The agent-native control-flow showcase: `gen as T`, `retry…until`, `match`, `for`/`if`, and `budget`-scoped `delegate` — deterministic glue around model calls |
| [`researcher.orch`](researcher.orch) | | Anthropic key | The keyless `web` pack, semantic `memory` with the offline keyword scorer, inline `knowledge`, a `survey` skill that runs each angle as its own budgeted `delegate … with { tools }` span |
| [`coder.orch`](coder.orch) | | Anthropic key | The shell-gating model (`policy.allow_shell: ask`), a declared `shell` `tool` with quoted params, a pure in-process `tool`, and the `files` pack with a custom `root` |
| [`mcp-notes.orch`](mcp-notes.orch) | | Anthropic key + Node.js | Connecting an MCP server with one `use mcp(...) as notes` line, and the `allow_mcp` confirmation gate |

A sensible reading order: **demo-offline → assistant → ollama-local → triage →
pipeline → researcher → coder → mcp-notes**. Each file's header comment includes
the exact command to run it.

## The one idea to take away

Every model interaction is one of two verbs, and the language makes you choose:

- **`gen`** — *you* orchestrate. One model call (optionally type-constrained
  with `gen as T`) that returns a value. No autonomous tool use.
- **`delegate`** — *the model* orchestrates. Hand it a goal and the full
  perceive→think→act tool loop runs until it produces an answer.

`triage.orch` and `pipeline.orch` show the move that defines Orchard: ordinary
`for`/`if`/`match`/`retry`/`budget` wrapped around `gen`/`delegate` — you write
the orchestration, the model fills the typed holes.

## Notes

- `claude-sonnet-4-6` / `gpt-5.2` / `qwen3` are the model names used throughout;
  swap in whatever your provider offers — the checker validates the *provider*,
  not the model string.
- Running an agent creates a `<Name>.orchmem` SQLite store next to the file.
  Inspect it with `orch memory <file>`; start fresh with `orch run --fresh`.
- These files are canonically formatted (`orch fmt`). Run `orch fmt --check
  examples/2.0/*.orch` to confirm.
