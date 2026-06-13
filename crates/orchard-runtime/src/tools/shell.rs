//! The `shell` tool pack + the gating logic. `allow_shell` is `never`/`ask`/
//! `always`; `always` auto-downgrades to `ask` when the agent ingests external
//! content (unless `i_understand_injection_risk`). `ask` denies in unattended
//! runs. Output is sentinel-wrapped (external).

use crate::error::ToolError;
use crate::tools::NativeTool;
use crate::traits::Tool;
#[cfg(feature = "native")]
use serde_json::json;
use serde_json::Value as Json;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "native")]
const STREAM_MAX: usize = 24 * 1024;

/// Gate a shell invocation. Returns an error message if denied.
pub fn gate(allow_shell: &str, interactive: bool) -> Result<(), String> {
    match allow_shell {
        "always" => Ok(()),
        "ask" => {
            if interactive {
                Ok(()) // an interactive host trusts the operator
            } else {
                Err("shell requires interactive confirmation but this run is unattended".into())
            }
        }
        _ => Err("shell is disabled by policy (allow_shell: never)".into()),
    }
}

/// Execute a shell command, returning `{exit_code, stdout, stderr}` (capped).
#[cfg(feature = "native")]
pub fn execute(
    command: &str,
    base_dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<Json, ToolError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(base_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::new(format!("shell spawn failed: {e}")))?;
    // simple timeout via polling
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // reap, else the killed child lingers as a zombie
                    return Err(ToolError::new(format!(
                        "shell command timed out after {timeout_secs}s"
                    )));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(ToolError::new(format!("shell wait failed: {e}"))),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| ToolError::new(e.to_string()))?;
    let cap = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        if s.len() > STREAM_MAX {
            // cut on a UTF-8 char boundary so we never panic mid-codepoint
            let mut end = STREAM_MAX;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...[truncated]", &s[..end])
        } else {
            s.into_owned()
        }
    };
    Ok(json!({
        "exit_code": out.status.code().unwrap_or(-1),
        "stdout": cap(&out.stdout),
        "stderr": cap(&out.stderr),
    }))
}

#[cfg(not(feature = "native"))]
pub fn execute(
    _command: &str,
    _base_dir: &std::path::Path,
    _timeout: u64,
) -> Result<Json, ToolError> {
    Err(ToolError::new("shell is unavailable on this target"))
}

/// The `shell` pack: a gated `run_command` tool.
pub fn shell_pack(allow_shell: String, interactive: bool, base_dir: PathBuf) -> Vec<Arc<dyn Tool>> {
    let tool = NativeTool::builder("run_command")
        .description("Run a shell command (policy-gated). Returns {exit_code, stdout, stderr}.")
        .param("command", "string", true)
        .external(true)
        .handler(move |args| {
            let allow = allow_shell.clone();
            let base = base_dir.clone();
            async move {
                if let Err(msg) = gate(&allow, interactive) {
                    return Err(ToolError::new(msg));
                }
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                #[cfg(feature = "native")]
                {
                    tokio::task::spawn_blocking(move || execute(&command, &base, 60))
                        .await
                        .map_err(|e| ToolError::new(e.to_string()))?
                }
                #[cfg(not(feature = "native"))]
                {
                    let _ = (&command, &base);
                    Err(ToolError::new("shell is unavailable on this target"))
                }
            }
        });
    vec![tool]
}
