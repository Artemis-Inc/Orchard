//! The `orch` CLI. A thin layer over the `orchard` facade.

use clap::{Parser, Subcommand};
use orchard::{Agent, Runtime, Severity};
use std::path::Path;
use std::process::ExitCode;

mod tui;

#[derive(Parser)]
#[command(name = "orch", version = orchard::VERSION, about = "Orchard 3.0 — a language for autonomous AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Static analysis: parse, types, lints (no execution).
    Check { file: String },
    /// Lower to the JSON IR.
    Compile {
        file: String,
        #[arg(short = 'o', long)]
        out: Option<String>,
    },
    /// Run the agent: chat, a one-shot task (-t), or a named skill (--skill).
    Run {
        file: String,
        #[arg(short = 't', long)]
        task: Option<String>,
        #[arg(long, num_args = 1.., value_name = "NAME k=v...")]
        skill: Option<Vec<String>>,
    },
    /// Canonical formatting (the one true style).
    Fmt {
        file: String,
        #[arg(long)]
        check: bool,
    },
    /// Scaffold a starter agent.
    New { name: String },
    /// Show what an agent can do.
    Info { file: String },
    /// Inspect the persistent store.
    Memory { file: String },
    /// Step-by-step log of the last run.
    Trace { file: String },
    /// Environment self-check.
    Doctor,
    /// Run schedule/file triggers (long-lived).
    Serve { file: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check { file }) => cmd_check(&file),
        Some(Command::Compile { file, out }) => cmd_compile(&file, out.as_deref()),
        Some(Command::Run { file, task, skill }) => cmd_run(&file, task, skill).await,
        Some(Command::Fmt { file, check }) => cmd_fmt(&file, check),
        Some(Command::New { name }) => cmd_new(&name),
        Some(Command::Info { file }) => cmd_info(&file),
        Some(Command::Memory { file }) => cmd_memory(&file),
        Some(Command::Trace { file }) => cmd_trace(&file),
        Some(Command::Doctor) => cmd_doctor(),
        Some(Command::Serve { file }) => cmd_serve(&file).await,
        None => {
            println!("orch {} — run `orch --help`", orchard::VERSION);
            ExitCode::SUCCESS
        }
    }
}

fn read(file: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(file).map_err(|e| {
        eprintln!("orch: cannot read {file}: {e}");
        ExitCode::from(1)
    })
}

fn render_diagnostics(diags: &[orchard::Diagnostic], source: &str) {
    for d in diags {
        eprintln!("{}\n", d.render(source));
    }
}

fn cmd_check(file: &str) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let diags = Agent::check(&src, file);
    render_diagnostics(&diags, &src);
    if diags.iter().any(|d| d.severity == Severity::Error) {
        ExitCode::from(2)
    } else {
        println!("✓ {file} is a valid Orchard 3.0 agent");
        ExitCode::SUCCESS
    }
}

fn cmd_compile(file: &str, out: Option<&str>) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match Agent::compile(&src, file) {
        Ok(ir) => {
            if let Some(path) = out {
                if let Err(e) = std::fs::write(path, format!("{ir}\n")) {
                    eprintln!("orch: cannot write {path}: {e}");
                    return ExitCode::from(1);
                }
            } else {
                println!("{ir}");
            }
            ExitCode::SUCCESS
        }
        Err(orchard::Error::Diagnostics(d)) => {
            render_diagnostics(&d, &src);
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("orch: {e}");
            ExitCode::from(1)
        }
    }
}

async fn cmd_run(file: &str, task: Option<String>, skill: Option<Vec<String>>) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let agent = match Agent::load(&src, file) {
        Ok(a) => a,
        Err(orchard::Error::Diagnostics(d)) => {
            render_diagnostics(&d, &src);
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("orch: {e}");
            return ExitCode::from(1);
        }
    };
    let base_dir = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut builder = Runtime::builder(agent.clone()).base_dir(base_dir.clone());
    // Durable store next to the file (redb), so state/memory persist across runs.
    if let Ok(store) = orchard_runtime::RedbStore::open(store_path(&agent, file, &base_dir)) {
        builder = builder.store(std::sync::Arc::new(store));
    }
    // Live pipeline events for the interactive TUI.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<orchard::AgentEvent>();
    builder = builder.on_event(move |ev| {
        let _ = tx.send(ev);
    });
    let session = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("orch: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = session.start().await {
        eprintln!("orch: on start failed: {e}");
        return ExitCode::from(1);
    }
    if let Some(parts) = skill {
        drop(rx); // non-interactive: no live pipeline UI
        let name = parts.first().cloned().unwrap_or_default();
        let mut args = serde_json::Map::new();
        for kv in parts.iter().skip(1) {
            if let Some((k, v)) = kv.split_once('=') {
                args.insert(k.to_string(), coerce_arg(v));
            }
        }
        return match session.skill(&name, serde_json::Value::Object(args)).await {
            Ok(v) => {
                println!("{}", v.to_text());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("orch: {e}");
                ExitCode::from(1)
            }
        };
    }
    if let Some(t) = task {
        drop(rx); // non-interactive: no live pipeline UI
        if !session.has_handler("message") {
            eprintln!("orch: this agent has no 'on message' handler");
            return ExitCode::from(1);
        }
        return match session.task(&t).await {
            Ok(reply) => {
                println!("{reply}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("orch: {e}");
                ExitCode::from(1)
            }
        };
    }
    // Interactive agent terminal (rich TUI on a tty, streaming lines when piped).
    let manifest = agent.manifest();
    let meta = tui::Meta {
        agent: {
            let n = agent.name();
            if n.is_empty() {
                "agent".to_string()
            } else {
                n.to_string()
            }
        },
        description: manifest
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        provider: manifest["model"]["provider"]
            .as_str()
            .unwrap_or("mock")
            .to_string(),
        model: manifest["model"]["name"].as_str().unwrap_or("").to_string(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        version: orchard::VERSION.to_string(),
        skills: skills_of(agent.ir()),
        tools: session.tool_names(),
    };
    tui::run(session, rx, meta).await;
    ExitCode::SUCCESS
}

/// Callable skills `(name, description)` for the slash palette, from the IR.
fn skills_of(ir: &serde_json::Value) -> Vec<(String, String)> {
    ir["agents"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a0| a0["skills"].as_array())
        .map(|skills| {
            skills
                .iter()
                .filter_map(|s| {
                    let name = s.get("name").and_then(|v| v.as_str())?;
                    let desc = s
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((name.to_string(), desc))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn coerce_arg(v: &str) -> serde_json::Value {
    if let Ok(i) = v.parse::<i64>() {
        serde_json::json!(i)
    } else if let Ok(f) = v.parse::<f64>() {
        serde_json::json!(f)
    } else if v == "true" || v == "false" {
        serde_json::json!(v == "true")
    } else {
        serde_json::json!(v)
    }
}

fn cmd_fmt(file: &str, check: bool) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match orchard_syntax::format_source(&src, file) {
        Ok(formatted) => {
            if check {
                if formatted == src {
                    ExitCode::SUCCESS
                } else {
                    eprintln!("orch: {file} is not formatted");
                    ExitCode::from(1)
                }
            } else if let Err(e) = std::fs::write(file, formatted) {
                eprintln!("orch: cannot write {file}: {e}");
                ExitCode::from(1)
            } else {
                println!("formatted {file}");
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("{}", e.diagnostic.render(&src));
            ExitCode::from(2)
        }
    }
}

const STARTER: &str = r#"#!orchard 3.0

// A capable starter agent. It runs offline as-is (the `mock` provider needs no
// key); swap the model block for a real one to make it think for real:
//
//     model { provider: anthropic, name: "claude-opus-4-8" }
//
// Run it:   orch run __NAME_LOWER__.orch
// Inside the chat, type `/` to see this agent's skills and tools.
agent __NAME__ {
    model { provider: mock, name: "echo" }

    persona {
        tone: "warm, direct, concrete"
        instructions: """
            Be concise. Use your tools to get real answers instead of guessing.
            Say in one line what you are about to do before you run a command.
        """
    }

    memory {
        conversation { enabled: true, window: 40 }
        facts: true
    }

    // Capabilities. Each `use` grants the agent a set of tools it can call on
    // its own during `delegate`. Uncomment to grant more power; the `policy`
    // block below is the safety envelope.
    use calculator              // exact arithmetic
    use time                    // the current date and time
    // use files { root: "." }  // read, write, and list files under a folder
    // use web                  // fetch and read web pages
    // use http                 // call HTTP APIs
    // use shell                // run shell commands (full computer access)

    // A skill is a model-using procedure you can call from the chat with
    // `/summarize`, or that the agent can call on its own.
    skill summarize(text: str) -> str {
        return gen "Summarize this in three short bullet points:\n{text}"
    }

    policy {
        // Shell starts OFF. Set `ask` to confirm each command interactively, or
        // `always` for trusted inputs. `max_spend` caps a real-model run.
        allow_shell: never
        max_steps: 25
        max_spend: $1.00
    }

    on message(text: str) -> str {
        // `delegate` hands the goal to the autonomous loop over the tools and
        // skills above. Watch the pipeline stream in the terminal as it works.
        return delegate text
    }
}
"#;

fn cmd_new(name: &str) -> ExitCode {
    let camel = to_upper_camel(name);
    let path = format!("{name}.orch");
    let template = STARTER
        .replace("__NAME_LOWER__", name)
        .replace("__NAME__", &camel);
    if Path::new(&path).exists() {
        eprintln!("orch: {path} already exists");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::write(&path, template) {
        eprintln!("orch: cannot write {path}: {e}");
        return ExitCode::from(1);
    }
    println!("created {path}");
    ExitCode::SUCCESS
}

fn to_upper_camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in name.chars() {
        if c == '-' || c == '_' || c == ' ' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    if out
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(true)
    {
        format!("Agent{out}")
    } else {
        out
    }
}

fn cmd_info(file: &str) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let agent = match Agent::load(&src, file) {
        Ok(a) => a,
        Err(orchard::Error::Diagnostics(d)) => {
            render_diagnostics(&d, &src);
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("orch: {e}");
            return ExitCode::from(1);
        }
    };
    let m = agent.manifest();
    println!("agent: {}", agent.name());
    println!(
        "model: {}:{}",
        m["model"]["provider"].as_str().unwrap_or("?"),
        m["model"]["name"].as_str().unwrap_or("?")
    );
    let ir = agent.ir();
    if let Some(a0) = ir["agents"].as_array().and_then(|a| a.first()) {
        let skills: Vec<&str> = a0["skills"]
            .as_array()
            .map(|s| s.iter().filter_map(|x| x["name"].as_str()).collect())
            .unwrap_or_default();
        if !skills.is_empty() {
            println!("skills: {}", skills.join(", "));
        }
        let handlers: Vec<&str> = a0["handlers"]
            .as_array()
            .map(|h| h.iter().filter_map(|x| x["kind"].as_str()).collect())
            .unwrap_or_default();
        if !handlers.is_empty() {
            println!("handlers: {}", handlers.join(", "));
        }
    }
    ExitCode::SUCCESS
}

/// The durable store path: `memory.store` (resolved vs `base_dir`) or
/// `<agent-name-lowercased>.orchmem` next to the file.
fn store_path(agent: &Agent, _file: &str, base_dir: &Path) -> std::path::PathBuf {
    if let Some(s) = agent.manifest()["memory"]
        .get("store")
        .and_then(|v| v.as_str())
    {
        return base_dir.join(s);
    }
    let name = agent.name().to_lowercase();
    let name = if name.is_empty() {
        "agent".to_string()
    } else {
        name
    };
    base_dir.join(format!("{name}.orchmem"))
}

fn open_store(file: &str) -> Result<orchard_runtime::RedbStore, ExitCode> {
    let src = read(file)?;
    let agent = Agent::load(&src, file).map_err(|_| ExitCode::from(2))?;
    let base_dir = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    orchard_runtime::RedbStore::open(store_path(&agent, file, &base_dir)).map_err(|e| {
        eprintln!("orch: cannot open store: {e}");
        ExitCode::from(1)
    })
}

fn cmd_memory(file: &str) -> ExitCode {
    use orchard_runtime::Store;
    let store = match open_store(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    println!("messages: {}", store.message_count());
    let facts = store.all_facts();
    println!("facts: {}", facts.len());
    for (k, v) in &facts {
        println!("  {k} = {v}");
    }
    println!("chunks: {}", store.chunk_count());
    let state = store.get_all_state();
    if !state.is_empty() {
        println!("state:");
        for (k, v) in &state {
            println!("  {k} = {v}");
        }
    }
    ExitCode::SUCCESS
}

fn cmd_trace(file: &str) -> ExitCode {
    use orchard_runtime::Store;
    let store = match open_store(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match store.last_run_id() {
        Some(run) => {
            println!("last run: {run}");
            for ev in store.run_trace(&run) {
                println!("  {} {}", ev["kind"].as_str().unwrap_or("?"), ev["payload"]);
            }
            ExitCode::SUCCESS
        }
        None => {
            println!("no runs recorded yet");
            ExitCode::SUCCESS
        }
    }
}

async fn cmd_serve(file: &str) -> ExitCode {
    let src = match read(file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let agent = match Agent::load(&src, file) {
        Ok(a) => a,
        Err(_) => return ExitCode::from(2),
    };
    let base_dir = Path::new(file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut builder = Runtime::builder(agent.clone())
        .base_dir(base_dir.clone())
        .unattended(true);
    if let Ok(store) = orchard_runtime::RedbStore::open(store_path(&agent, file, &base_dir)) {
        builder = builder.store(std::sync::Arc::new(store));
    }
    let session = match builder.build() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("orch: {e}");
            return ExitCode::from(1);
        }
    };
    let _ = session.start().await;
    let spec = session.schedule_spec();
    let every = spec.as_ref().and_then(|(kind, v)| {
        if kind == "every" {
            parse_duration_secs(v)
        } else {
            None
        }
    });
    let cron = match spec.as_ref() {
        Some((kind, v)) if kind == "cron" => {
            if let Err(e) = orchard_runtime::cron_validate(v) {
                eprintln!("orch: invalid cron schedule '{v}': {e}");
                return ExitCode::from(1);
            }
            Some(v.clone())
        }
        _ => None,
    };
    let watch = session.watch_dir().map(|d| base_dir.join(d));
    if every.is_none() && cron.is_none() && watch.is_none() {
        eprintln!("orch: nothing to serve (no schedule/file handlers)");
        return ExitCode::from(1);
    }
    println!("serving {file} … (ctrl-c to stop)");
    let mut next_every =
        std::time::Instant::now() + std::time::Duration::from_secs(every.unwrap_or(3600));
    // last UTC minute index we have already evaluated for cron (None = prime on
    // first tick so we don't replay history).
    let mut last_cron_minute: Option<i64> = None;
    let mut seen: std::collections::HashMap<std::path::PathBuf, std::time::SystemTime> =
        std::collections::HashMap::new();
    let mut primed = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Some(secs) = every {
            if std::time::Instant::now() >= next_every {
                if let Ok(reply) = session.schedule().await {
                    let t = reply.to_text();
                    if !t.is_empty() {
                        println!("[schedule] {t}");
                    }
                }
                next_every = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            }
        }
        if let Some(expr) = &cron {
            let now_min = unix_now() / 60;
            match last_cron_minute {
                None => last_cron_minute = Some(now_min),
                Some(last) if now_min > last => {
                    // catch up over any minutes missed while a handler ran,
                    // capped so a long pause can't fire thousands of times.
                    let start = (last + 1).max(now_min - 1440);
                    for m in start..=now_min {
                        if orchard_runtime::cron_matches(expr, m * 60).unwrap_or(false) {
                            if let Ok(reply) = session.schedule().await {
                                let t = reply.to_text();
                                if !t.is_empty() {
                                    println!("[schedule] {t}");
                                }
                            }
                        }
                    }
                    last_cron_minute = Some(now_min);
                }
                _ => {}
            }
        }
        if let Some(dir) = &watch {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut current = std::collections::HashMap::new();
                for e in entries.flatten() {
                    if let Ok(meta) = e.metadata() {
                        if let Ok(m) = meta.modified() {
                            current.insert(e.path(), m);
                        }
                    }
                }
                if primed {
                    for (p, m) in &current {
                        if seen.get(p) != Some(m) {
                            if let Ok(reply) = session.file(&p.to_string_lossy()).await {
                                let t = reply.to_text();
                                if !t.is_empty() {
                                    println!("[file {}] {t}", p.display());
                                }
                            }
                        }
                    }
                }
                seen = current;
                primed = true;
            }
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    // milliseconds round up to whole seconds; the serve loop ticks at ~2 Hz so
    // sub-second schedules are clamped to 1s below.
    if let Some(num) = s.strip_suffix("ms") {
        let n: u64 = num.trim().parse().ok()?;
        return Some(n.div_ceil(1000).max(1));
    }
    for (suf, mult) in [("s", 1u64), ("m", 60), ("h", 3600), ("d", 86400)] {
        if let Some(num) = s.strip_suffix(suf) {
            let n: u64 = num.trim().parse().ok()?;
            // saturate rather than panic on absurd values
            return Some(n.saturating_mul(mult).max(1));
        }
    }
    s.parse::<u64>().ok().map(|n| n.max(1))
}

fn cmd_doctor() -> ExitCode {
    println!("orch {} — environment check", orchard::VERSION);
    for (provider, var) in [
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("openai", "OPENAI_API_KEY"),
        ("groq", "GROQ_API_KEY"),
    ] {
        let present = std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false);
        println!(
            "  {provider:<10} {}",
            if present { "key present" } else { "no key" }
        );
    }
    println!("  mock       always available (offline)");
    ExitCode::SUCCESS
}
