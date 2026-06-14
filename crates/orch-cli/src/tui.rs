//! The `orch run` agent terminal: an entrance banner, a live pipeline that
//! streams the agent's model calls, tool calls, and output as it works, and an
//! interactive chat. On a real terminal it runs a rich raw-mode UI; when piped
//! (CI, scripts) it falls back to a clean streaming line mode. Both render the
//! same `AgentEvent` stream from the runtime.

use orchard::{AgentEvent, ModelKind, Session};
use std::io::{IsTerminal, Write};
use tokio::sync::mpsc::UnboundedReceiver;

pub mod rich;

/// Static info shown on the entrance banner.
pub struct Meta {
    pub agent: String,
    pub description: String,
    pub provider: String,
    pub model: String,
    pub cwd: String,
    pub version: String,
    /// Callable skills (name, description) exposed for the slash menu.
    pub skills: Vec<(String, String)>,
    /// Tool names available to the agent.
    pub tools: Vec<String>,
}

const LEAF: &str = "\
   ▗▄▖
 ▗▟███▖
▗█████▛
▝▜███▛
  ▝▀▘";

pub(crate) const G: (u8, u8, u8) = (98, 171, 46); // leaf
pub(crate) const G_LIGHT: (u8, u8, u8) = (139, 198, 63);
pub(crate) const G_DEEP: (u8, u8, u8) = (44, 110, 22);

/// Entry point. Picks the rich TUI on a tty, else the streaming line mode.
pub async fn run(session: Session, rx: UnboundedReceiver<AgentEvent>, meta: Meta) {
    if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        if let Err(e) = rich::run(session, rx, meta).await {
            eprintln!("orch: terminal error: {e}");
        }
    } else {
        run_plain(session, rx, meta).await;
    }
}

// ---------------------------------------------------------------------------
// Banner (works in any ANSI terminal)
// ---------------------------------------------------------------------------

pub(crate) fn rgb(s: &str, (r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{r};{g};{b}m{s}\x1b[0m")
}
pub(crate) fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}
pub(crate) fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[22m")
}

pub fn banner(meta: &Meta) -> String {
    let leaf_lines: Vec<&str> = LEAF.lines().collect();
    // a soft top-to-bottom green gradient on the leaf
    let grad = [G_LIGHT, G_LIGHT, G, G_DEEP, G_DEEP];
    let title = format!(
        "{} {}",
        bold(&meta.agent),
        dim(&format!("· Orchard {}", meta.version))
    );
    let model_line = dim(&format!("{}:{}", meta.provider, meta.model));
    let cwd_line = dim(&meta.cwd);
    let info = [title, model_line, cwd_line];
    let mut out = String::from("\n");
    for i in 0..leaf_lines.len().max(info.len()) {
        let leaf = leaf_lines.get(i).copied().unwrap_or("");
        let color = grad.get(i).copied().unwrap_or(G_DEEP);
        let leaf_col = rgb(&format!("{leaf:<8}"), color);
        let text = info.get(i).cloned().unwrap_or_default();
        out.push_str(&format!("  {leaf_col} {text}\n"));
    }
    if !meta.description.is_empty() {
        out.push_str(&format!("\n  {}\n", dim(&meta.description)));
    }
    out
}

/// One-line hint shown under the banner / above the prompt.
pub fn hint(meta: &Meta) -> String {
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut bits = vec!["/ for skills & tools".to_string()];
    if !meta.skills.is_empty() {
        bits.push(plural(meta.skills.len(), "skill"));
    }
    if !meta.tools.is_empty() {
        bits.push(plural(meta.tools.len(), "tool"));
    }
    bits.push("ctrl-c to exit".to_string());
    dim(&bits.join(" · "))
}

// ---------------------------------------------------------------------------
// Event rendering (shared vocabulary, plain-text form)
// ---------------------------------------------------------------------------

fn truncate(s: &str, n: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= n {
        s
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn brief_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    let vs = match v {
                        serde_json::Value::String(s) => truncate(s, 48),
                        other => truncate(&other.to_string(), 48),
                    };
                    format!("{k}: {vs}")
                })
                .collect();
            truncate(&parts.join(", "), 80)
        }
        serde_json::Value::Null => String::new(),
        other => truncate(&other.to_string(), 80),
    }
}

/// Render one event as a committed line for the streaming (plain) mode.
pub(crate) fn line_for(ev: &AgentEvent) -> Option<String> {
    match ev {
        AgentEvent::ModelStart { kind, .. } => match kind {
            ModelKind::Delegate => Some(rgb("✻", G).to_string() + " " + &dim("thinking")),
            ModelKind::Gen => None,
        },
        AgentEvent::ModelEnd {
            input_tokens,
            output_tokens,
            tool_calls,
            ..
        } => {
            let usage = dim(&format!("↑{input_tokens} ↓{output_tokens} tok"));
            if *tool_calls > 0 {
                Some(format!(
                    "  {} {}",
                    dim(&format!("planning {tool_calls} action(s)")),
                    usage
                ))
            } else {
                Some(format!("  {usage}"))
            }
        }
        AgentEvent::ToolStart { name, args, .. } => Some(format!(
            "{} {}{}",
            rgb("▸", G_LIGHT),
            bold(name),
            dim(&format!("({})", brief_args(args)))
        )),
        AgentEvent::ToolEnd {
            name, ok, output, ms, ..
        } => {
            let mark = if *ok { rgb("✔", G) } else { "✗".to_string() };
            let preview = truncate(output, 72);
            Some(format!(
                "  {mark} {} {}\n    {}",
                dim(name),
                dim(&format!("{ms}ms")),
                dim(&preview)
            ))
        }
        AgentEvent::Emit { text } => Some(format!("  {} {}", rgb("·", G_LIGHT), text)),
        AgentEvent::TaskComplete { .. } => None,
        AgentEvent::Notice { text, .. } => Some(format!("  {} {}", dim("!"), dim(text))),
    }
}

// ---------------------------------------------------------------------------
// Plain streaming mode (piped stdin / no tty)
// ---------------------------------------------------------------------------

async fn run_plain(session: Session, mut rx: UnboundedReceiver<AgentEvent>, meta: Meta) {
    print!("{}", banner(&meta));
    println!("  {}\n", hint(&meta));
    let _ = std::io::stdout().flush();

    let stdin = std::io::stdin();
    loop {
        print!("{} ", rgb("›", G));
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        if stdin.read_line(&mut input).unwrap_or(0) == 0 {
            println!();
            break;
        }
        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if input == "/exit" || input == "/quit" {
            break;
        }

        let fut = session.message(&input);
        tokio::pin!(fut);
        let reply = loop {
            tokio::select! {
                biased;
                Some(ev) = rx.recv() => {
                    if let Some(line) = line_for(&ev) { println!("{line}"); let _ = std::io::stdout().flush(); }
                }
                res = &mut fut => {
                    while let Ok(ev) = rx.try_recv() {
                        if let Some(line) = line_for(&ev) { println!("{line}"); }
                    }
                    break res;
                }
            }
        };
        match reply {
            Ok(text) => println!("\n{}\n", text),
            Err(e) => eprintln!("{} {e}\n", "orch:"),
        }
    }
}
