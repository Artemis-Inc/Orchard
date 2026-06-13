//! C ABI for Orchard 3.0. A thin `extern "C"` adapter over the `orchard`
//! facade: load an agent, build a session, drive a turn, free handles.
//!
//! All `unsafe` is confined to this boundary (pointer ↔ reference conversions
//! and C-string marshaling) and documented. Strings returned to C are
//! heap-allocated and must be freed with [`orch_string_free`].

use orchard::{Agent, Runtime, Session};
use std::ffi::{c_char, CStr, CString};

/// Opaque handle to a loaded [`Agent`].
pub struct OrchAgent {
    agent: Agent,
}

/// Opaque handle to a live [`Session`] plus the runtime that drives it.
pub struct OrchSession {
    session: Session,
    rt: tokio::runtime::Runtime,
}

/// SAFETY: `s` must be a valid NUL-terminated C string (or null → "").
unsafe fn cstr(s: *const c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    CStr::from_ptr(s).to_string_lossy().into_owned()
}

fn to_c_string(s: String) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

/// The Orchard version (static C string; do not free).
#[no_mangle]
pub extern "C" fn orch_version() -> *const c_char {
    concat!("3.0", "\0").as_ptr() as *const c_char
}

/// Free a string returned by this library.
///
/// # Safety
/// `s` must have been returned by an `orch_*` function and not freed already.
#[no_mangle]
pub unsafe extern "C" fn orch_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Static analysis. Returns a newly-allocated C string with rendered
/// diagnostics (empty if clean). Free with [`orch_string_free`].
///
/// # Safety
/// `source` and `filename` must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn orch_check(source: *const c_char, filename: *const c_char) -> *mut c_char {
    let src = cstr(source);
    let file = cstr(filename);
    let diags = Agent::check(&src, &file);
    let rendered: Vec<String> = diags.iter().map(|d| d.render(&src)).collect();
    to_c_string(rendered.join("\n\n"))
}

/// Load + check + lower an agent. Returns null on error; the rendered
/// diagnostics are written to `*err_out` (free with [`orch_string_free`]).
///
/// # Safety
/// `source`/`filename` must be valid C strings; `err_out` may be null.
#[no_mangle]
pub unsafe extern "C" fn orch_agent_load(
    source: *const c_char,
    filename: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut OrchAgent {
    let src = cstr(source);
    let file = cstr(filename);
    match Agent::load(&src, &file) {
        Ok(agent) => Box::into_raw(Box::new(OrchAgent { agent })),
        Err(orchard::Error::Diagnostics(d)) => {
            if !err_out.is_null() {
                let rendered: Vec<String> = d.iter().map(|x| x.render(&src)).collect();
                *err_out = to_c_string(rendered.join("\n\n"));
            }
            std::ptr::null_mut()
        }
        Err(e) => {
            if !err_out.is_null() {
                *err_out = to_c_string(e.to_string());
            }
            std::ptr::null_mut()
        }
    }
}

/// Free an agent handle.
///
/// # Safety
/// `agent` must be a handle from [`orch_agent_load`], not freed already.
#[no_mangle]
pub unsafe extern "C" fn orch_agent_free(agent: *mut OrchAgent) {
    if !agent.is_null() {
        drop(Box::from_raw(agent));
    }
}

/// Build a session from an agent (default provider/store; `base_dir` resolves
/// relative paths). Returns null on error → `*err_out`.
///
/// # Safety
/// `agent` must be a valid handle; `base_dir` a valid C string; `err_out` may be null.
#[no_mangle]
pub unsafe extern "C" fn orch_session_new(
    agent: *const OrchAgent,
    base_dir: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut OrchSession {
    if agent.is_null() {
        return std::ptr::null_mut();
    }
    let agent = (*agent).agent.clone();
    let dir = cstr(base_dir);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            if !err_out.is_null() {
                *err_out = to_c_string(format!("runtime: {e}"));
            }
            return std::ptr::null_mut();
        }
    };
    let builder = Runtime::builder(agent).base_dir(if dir.is_empty() { ".".into() } else { dir });
    match builder.build() {
        Ok(session) => Box::into_raw(Box::new(OrchSession { session, rt })),
        Err(e) => {
            if !err_out.is_null() {
                *err_out = to_c_string(e.to_string());
            }
            std::ptr::null_mut()
        }
    }
}

/// Free a session handle.
///
/// # Safety
/// `session` must be a handle from [`orch_session_new`], not freed already.
#[no_mangle]
pub unsafe extern "C" fn orch_session_free(session: *mut OrchSession) {
    if !session.is_null() {
        drop(Box::from_raw(session));
    }
}

/// Drive one `on message` turn. Returns the reply as a newly-allocated C string
/// (free with [`orch_string_free`]); on error the message is `"error: ..."`.
///
/// # Safety
/// `session` must be a valid handle; `text` a valid C string.
#[no_mangle]
pub unsafe extern "C" fn orch_session_message(
    session: *const OrchSession,
    text: *const c_char,
) -> *mut c_char {
    if session.is_null() {
        return to_c_string("error: null session".into());
    }
    let s = &*session;
    let msg = cstr(text);
    let result = s.rt.block_on(s.session.message(&msg));
    match result {
        Ok(reply) => to_c_string(reply),
        Err(e) => to_c_string(format!("error: {e}")),
    }
}

/// A one-shot task (alias for a message turn).
///
/// # Safety
/// As [`orch_session_message`].
#[no_mangle]
pub unsafe extern "C" fn orch_session_task(
    session: *const OrchSession,
    text: *const c_char,
) -> *mut c_char {
    orch_session_message(session, text)
}
