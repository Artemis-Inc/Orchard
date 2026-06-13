/* Orchard 3.0 — C ABI. Link against liborchard_ffi (cdylib or staticlib). */
#ifndef ORCHARD_H
#define ORCHARD_H

typedef struct OrchAgent OrchAgent;
typedef struct OrchSession OrchSession;

const char *orch_version(void);
void orch_string_free(char *s);
char *orch_check(const char *source, const char *filename);
OrchAgent *orch_agent_load(const char *source, const char *filename, char **err_out);
void orch_agent_free(OrchAgent *agent);
OrchSession *orch_session_new(const OrchAgent *agent, const char *base_dir, char **err_out);
void orch_session_free(OrchSession *session);
char *orch_session_message(const OrchSession *session, const char *text);
char *orch_session_task(const OrchSession *session, const char *text);

#endif
