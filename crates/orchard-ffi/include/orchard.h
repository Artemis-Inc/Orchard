/* Orchard 3.0 — C ABI.
 *
 * A thin C interface over the Orchard agent runtime. Link against
 * liborchard_ffi (static or dynamic). All strings returned by `orch_*`
 * functions are heap-allocated and must be released with `orch_string_free`.
 *
 * Threading: an `OrchSession` owns its own async runtime and is not internally
 * synchronized — drive one session from one thread at a time.
 */
#ifndef ORCHARD_H
#define ORCHARD_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handles. */
typedef struct OrchAgent OrchAgent;
typedef struct OrchSession OrchSession;

/* The Orchard version as a static C string. Do NOT free. */
const char *orch_version(void);

/* Free a string previously returned by an `orch_*` function. Safe on NULL. */
void orch_string_free(char *s);

/* Static analysis. Returns rendered diagnostics (empty string if clean).
 * Caller frees the result with `orch_string_free`. */
char *orch_check(const char *source, const char *filename);

/* Load + check + lower an agent from source. Returns NULL on error; if
 * `err_out` is non-NULL it receives a rendered diagnostics string (which the
 * caller must free). The returned handle is freed with `orch_agent_free`. */
OrchAgent *orch_agent_load(const char *source, const char *filename,
                           char **err_out);

/* Free an agent handle. Safe on NULL. */
void orch_agent_free(OrchAgent *agent);

/* Build a session from an agent. `base_dir` resolves relative paths (NULL or
 * "" → "."). Returns NULL on error; `err_out` (if non-NULL) receives the error
 * message to free. The session is freed with `orch_session_free`. */
OrchSession *orch_session_new(const OrchAgent *agent, const char *base_dir,
                              char **err_out);

/* Free a session handle. Safe on NULL. */
void orch_session_free(OrchSession *session);

/* Drive one `on message` turn. Returns the reply as a heap string to free; on
 * error the string is "error: ...". */
char *orch_session_message(const OrchSession *session, const char *text);

/* A one-shot task (alias for a message turn). */
char *orch_session_task(const OrchSession *session, const char *text);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* ORCHARD_H */
