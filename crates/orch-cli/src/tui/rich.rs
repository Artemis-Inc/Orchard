//! The rich raw-mode terminal UI for `orch run` on a real tty: the entrance
//! banner, an inline input line with a `/` command palette of the agent's skills
//! and tools, and a live pipeline (animated spinner + streamed step lines) while
//! the agent works.

use super::{banner, bold, dim, hint, line_for, rgb, Meta, G, G_DEEP, G_LIGHT};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    terminal::{self, Clear, ClearType},
};
use orchard::{AgentEvent, Session};
use std::io::{stdout, Write};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Restores the terminal on the way out (including on panic).
struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

fn w(s: &str) {
    let mut o = stdout();
    let _ = write!(o, "{s}");
    let _ = o.flush();
}
/// Write a logical line, translating `\n` to raw-mode `\r\n`.
fn wln(s: &str) {
    let mut o = stdout();
    let _ = write!(o, "{}\r\n", s.replace('\n', "\r\n"));
    let _ = o.flush();
}
fn clear_line() {
    let _ = queue!(stdout(), cursor::MoveToColumn(0), Clear(ClearType::CurrentLine));
    let _ = stdout().flush();
}

/// What the user asked for at the prompt.
enum Outcome {
    Message(String),
    Skill(String),
    Clear,
    Help,
    Tools,
    Exit,
}

/// One palette row.
struct Item {
    label: String,
    hint: String,
    kind: ItemKind,
}
enum ItemKind {
    Builtin(&'static str),
    Skill(String),
    Tool(String),
}

fn palette_items(meta: &Meta, filter: &str) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let builtins = [
        ("help", "show keys and what this agent can do", "help"),
        ("tools", "list the tools the agent can use", "tools"),
        ("clear", "clear the screen", "clear"),
        ("exit", "leave the session", "exit"),
    ];
    for (name, desc, id) in builtins {
        items.push(Item {
            label: format!("/{name}"),
            hint: desc.to_string(),
            kind: ItemKind::Builtin(id),
        });
    }
    for (name, desc) in &meta.skills {
        items.push(Item {
            label: format!("/{name}"),
            hint: if desc.is_empty() {
                "run this skill".to_string()
            } else {
                desc.clone()
            },
            kind: ItemKind::Skill(name.clone()),
        });
    }
    for t in &meta.tools {
        items.push(Item {
            label: format!("/{t}"),
            hint: "ask the agent to use this tool".to_string(),
            kind: ItemKind::Tool(t.clone()),
        });
    }
    let f = filter.to_lowercase();
    items
        .into_iter()
        .filter(|i| f.is_empty() || i.label.to_lowercase().contains(&f))
        .take(8)
        .collect()
}

/// Read one prompt. Returns None to exit (ctrl-c / ctrl-d on empty).
fn read_input(meta: &Meta) -> std::io::Result<Option<Outcome>> {
    let mut buf = String::new();
    let mut sel = 0usize;

    loop {
        // Recompute the palette when the line is a slash command.
        let palette = if buf.starts_with('/') {
            palette_items(meta, &buf[1..])
        } else {
            Vec::new()
        };
        if sel >= palette.len() {
            sel = palette.len().saturating_sub(1);
        }

        // Redraw: clear from the prompt line down, print prompt + buffer, then
        // the palette below, then return the cursor to the end of the input.
        let mut o = stdout();
        let _ = queue!(o, cursor::MoveToColumn(0), Clear(ClearType::FromCursorDown));
        let _ = write!(o, "{} {}", rgb("›", G), buf);
        let rows = palette.len() as u16;
        if rows > 0 {
            for (i, it) in palette.iter().enumerate() {
                let line = if i == sel {
                    format!(
                        "  {} {}  {}",
                        rgb("▸", G),
                        bold(&it.label),
                        dim(&it.hint)
                    )
                } else {
                    format!("    {}  {}", it.label, dim(&it.hint))
                };
                let _ = write!(o, "\r\n{line}");
            }
            // move back up to the input line, cursor after the buffer
            let _ = queue!(o, cursor::MoveUp(rows));
        }
        let col = 2 + buf.chars().count() as u16;
        let _ = queue!(o, cursor::MoveToColumn(col));
        let _ = o.flush();
        let _ = rows;

        // Read a key.
        let ev = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            _ => continue,
        };
        let KeyEvent {
            code, modifiers, ..
        } = ev;
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        // Clear the palette area before acting so output starts clean.
        let finish = |buf: &str| {
            let mut o = stdout();
            let _ = queue!(o, cursor::MoveToColumn(0), Clear(ClearType::FromCursorDown));
            let _ = o.flush();
            buf.to_string()
        };

        match code {
            KeyCode::Char('c') if ctrl => {
                let _ = finish(&buf);
                wln(&dim("bye"));
                return Ok(None);
            }
            KeyCode::Char('d') if ctrl && buf.is_empty() => {
                let _ = finish(&buf);
                return Ok(None);
            }
            KeyCode::Esc => {
                buf.clear();
                sel = 0;
            }
            KeyCode::Up if !palette.is_empty() => {
                sel = sel.saturating_sub(1);
            }
            KeyCode::Down if !palette.is_empty() => {
                sel = (sel + 1).min(palette.len().saturating_sub(1));
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char('\r') | KeyCode::Char('\n') | KeyCode::Enter => {
                if !palette.is_empty() {
                    let it = &palette[sel];
                    let _ = finish(&buf);
                    return Ok(Some(match &it.kind {
                        ItemKind::Builtin("help") => Outcome::Help,
                        ItemKind::Builtin("tools") => Outcome::Tools,
                        ItemKind::Builtin("clear") => Outcome::Clear,
                        ItemKind::Builtin("exit") => Outcome::Exit,
                        ItemKind::Builtin(_) => Outcome::Help,
                        ItemKind::Skill(n) => Outcome::Skill(n.clone()),
                        ItemKind::Tool(t) => Outcome::Message(format!(
                            "Use the {t} tool to help with: "
                        )),
                    }));
                }
                let text = buf.trim().to_string();
                let _ = finish(&buf);
                if text.is_empty() {
                    wln("");
                    return Ok(Some(Outcome::Message(String::new()))); // no-op handled by caller
                }
                return Ok(Some(Outcome::Message(text)));
            }
            KeyCode::Char(c) => {
                buf.push(c);
                sel = 0;
            }
            _ => {}
        }
    }
}

/// Drive one agent turn, streaming the live pipeline. `fut` resolves to the
/// final reply text.
async fn stream<F>(fut: F, rx: &mut UnboundedReceiver<AgentEvent>) -> Result<String, orchard::Error>
where
    F: std::future::Future<Output = Result<String, orchard::Error>>,
{
    tokio::pin!(fut);
    let t0 = Instant::now();
    let mut frame = 0usize;
    let mut in_tok = 0i64;
    let mut out_tok = 0i64;
    let mut tools = 0u32;
    let mut activity = String::from("Thinking");
    let mut spinning = false;

    macro_rules! commit {
        ($line:expr) => {{
            if spinning {
                clear_line();
                spinning = false;
            }
            wln(&$line);
        }};
    }
    let draw_spinner = |frame: usize, activity: &str, in_tok: i64, out_tok: i64, tools: u32| {
        let f = FRAMES[frame % FRAMES.len()];
        let meta = dim(&format!(
            "({}s · ↑{} ↓{} · {} tool{})",
            t0.elapsed().as_secs(),
            in_tok,
            out_tok,
            tools,
            if tools == 1 { "" } else { "s" }
        ));
        clear_line();
        w(&format!("{} {} {meta}", rgb(f, G_LIGHT), activity));
    };

    loop {
        tokio::select! {
            biased;
            maybe = rx.recv() => {
                let Some(ev) = maybe else { continue };
                match &ev {
                    AgentEvent::ModelStart { .. } => { activity = "Thinking".into(); }
                    AgentEvent::ModelEnd { input_tokens, output_tokens, .. } => {
                        in_tok += *input_tokens; out_tok += *output_tokens;
                    }
                    AgentEvent::ToolStart { name, .. } => {
                        tools += 1;
                        activity = format!("Running {name}");
                        if let Some(l) = line_for(&ev) { commit!(l); }
                    }
                    AgentEvent::ToolEnd { .. } => {
                        activity = "Thinking".into();
                        if let Some(l) = line_for(&ev) { commit!(l); }
                    }
                    AgentEvent::Emit { .. } => {
                        if let Some(l) = line_for(&ev) { commit!(l); }
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(90)) => {
                frame += 1;
                draw_spinner(frame, &activity, in_tok, out_tok, tools);
                spinning = true;
                // allow ctrl-c to interrupt a long turn
                if event::poll(Duration::ZERO).unwrap_or(false) {
                    if let Ok(Event::Key(k)) = event::read() {
                        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                            if spinning { clear_line(); }
                            wln(&dim("interrupted"));
                            return Ok(String::new());
                        }
                    }
                }
            }
            res = &mut fut => {
                if spinning { clear_line(); }
                while let Ok(ev) = rx.try_recv() {
                    if let Some(l) = line_for(&ev) { wln(&l); }
                }
                return res;
            }
        }
    }
}

fn print_reply(r: Result<String, orchard::Error>) {
    match r {
        Ok(text) if text.trim().is_empty() => {}
        Ok(text) => {
            wln("");
            for l in text.lines() {
                wln(&format!("  {l}"));
            }
            wln("");
        }
        Err(e) => {
            wln(&rgb(&format!("  error: {e}"), G_DEEP));
            wln("");
        }
    }
}

fn print_help(meta: &Meta) {
    wln("");
    wln(&bold("  keys"));
    wln(&format!("    {}  open the skill & tool palette", rgb("/", G)));
    wln(&format!("    {}  send your message", dim("enter")));
    wln(&format!("    {}  interrupt a running turn / exit", dim("ctrl-c")));
    if !meta.skills.is_empty() {
        wln("");
        wln(&bold("  skills"));
        for (n, d) in &meta.skills {
            wln(&format!("    {}  {}", rgb(&format!("/{n}"), G), dim(d)));
        }
    }
    wln("");
}

fn print_tools(meta: &Meta) {
    wln("");
    if meta.tools.is_empty() {
        wln(&dim("  this agent has no tools granted"));
    } else {
        wln(&bold("  tools the agent can call"));
        for t in &meta.tools {
            wln(&format!("    {} {}", rgb("•", G), t));
        }
    }
    wln("");
}

pub async fn run(
    session: Session,
    mut rx: UnboundedReceiver<AgentEvent>,
    meta: Meta,
) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    execute!(stdout(), cursor::Show)?;

    for l in banner(&meta).lines() {
        wln(l);
    }
    wln(&format!("  {}", hint(&meta)));
    wln("");

    loop {
        match read_input(&meta)? {
            None | Some(Outcome::Exit) => break,
            Some(Outcome::Clear) => {
                execute!(stdout(), Clear(ClearType::All), cursor::MoveTo(0, 0))?;
                for l in banner(&meta).lines() {
                    wln(l);
                }
                wln(&format!("  {}", hint(&meta)));
                wln("");
            }
            Some(Outcome::Help) => print_help(&meta),
            Some(Outcome::Tools) => print_tools(&meta),
            Some(Outcome::Message(text)) => {
                if text.is_empty() {
                    continue;
                }
                wln(&format!("{} {}", rgb("›", G), text));
                let r = stream(session.message(&text), &mut rx).await;
                print_reply(r);
            }
            Some(Outcome::Skill(name)) => {
                wln(&format!("{} {}", rgb("/", G), bold(&name)));
                let fut = async {
                    session
                        .skill(&name, serde_json::json!({}))
                        .await
                        .map(|v| v.to_text())
                };
                let r = stream(fut, &mut rx).await;
                print_reply(r);
            }
        }
    }
    Ok(())
}
