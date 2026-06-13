//! Exercise the C ABI surface end-to-end from Rust (load → session → message →
//! free), proving the extern "C" functions run an agent.

use orchard_ffi::*;
use std::ffi::{CStr, CString};

#[test]
fn ffi_loads_and_runs_an_agent() {
    let src = CString::new(
        "agent A { model { provider: mock, name: \"echo\" } on message(text: str) -> str { return gen \"Hi {text}\" } }",
    )
    .unwrap();
    let file = CString::new("<ffi>").unwrap();
    let base = CString::new(".").unwrap();
    let text = CString::new("there").unwrap();

    unsafe {
        let mut err: *mut std::ffi::c_char = std::ptr::null_mut();
        let agent = orch_agent_load(src.as_ptr(), file.as_ptr(), &mut err);
        assert!(
            !agent.is_null(),
            "load failed: {:?}",
            if err.is_null() {
                "".into()
            } else {
                CStr::from_ptr(err).to_string_lossy()
            }
        );

        let session = orch_session_new(agent, base.as_ptr(), &mut err);
        assert!(!session.is_null(), "session build failed");

        let reply_ptr = orch_session_message(session, text.as_ptr());
        let reply = CStr::from_ptr(reply_ptr).to_string_lossy().into_owned();
        assert!(reply.contains("Hi there"), "reply: {reply}");

        orch_string_free(reply_ptr);
        orch_session_free(session);
        orch_agent_free(agent);
    }
}

#[test]
fn ffi_check_reports_errors() {
    let src = CString::new("agent A { model { provider: bogus, name: \"m\" } }").unwrap();
    let file = CString::new("<ffi>").unwrap();
    unsafe {
        let out = orch_check(src.as_ptr(), file.as_ptr());
        let s = CStr::from_ptr(out).to_string_lossy().into_owned();
        assert!(s.contains("is not one of"), "diagnostics: {s}");
        orch_string_free(out);
    }
}
