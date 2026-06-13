//! Orchard 3.0 IR + lowering (`orch compile`).
//!
//! The IR is a `serde_json::Value` tree (mirroring v2's dict IR). Serialization
//! is byte-stable: `serde_json`'s default `Map` is sorted (`BTreeMap`-backed)
//! and the pretty printer uses 2-space indent and raw UTF-8 — matching v2's
//! `json.dumps(sort_keys=True, indent=2, ensure_ascii=False)`.

pub mod lower;
pub mod manifest;

pub use lower::{lower_program, ORCHARD_VERSION};
pub use manifest::build_manifest;

use orchard_syntax::{parse_source, Diagnostic};
use orchard_types::check;
use serde_json::Value;

/// Check then lower source to the IR. Returns error diagnostics (parse or check)
/// instead of IR when the program is invalid.
pub fn compile_source(source: &str, filename: &str) -> Result<Value, Vec<Diagnostic>> {
    let program = match parse_source(source, filename) {
        Ok(p) => p,
        Err(e) => return Err(vec![e.diagnostic]),
    };
    let diags = check(&program);
    let errors: Vec<Diagnostic> = diags.into_iter().filter(|d| d.is_error()).collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(lower_program(&program))
}

/// Serialize the IR to byte-stable JSON (no trailing newline).
pub fn dumps(ir: &Value) -> String {
    serde_json::to_string_pretty(ir).expect("IR serializes")
}

/// Parse compiled IR JSON back into a Value.
pub fn from_ir(ir_json: &str) -> Result<Value, String> {
    serde_json::from_str(ir_json).map_err(|e| e.to_string())
}
