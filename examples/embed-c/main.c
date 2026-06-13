/* A minimal C host: load an Orchard agent and drive one turn, fully offline. */
#include "orchard.h"
#include <stdio.h>

int main(void) {
    const char *src =
        "agent Greeter {\n"
        "    model { provider: mock, name: \"echo\" }\n"
        "    on message(text: str) -> str { return gen \"Hello, {text}!\" }\n"
        "}\n";

    printf("orchard %s\n", orch_version());

    char *err = NULL;
    OrchAgent *agent = orch_agent_load(src, "greeter.orch", &err);
    if (!agent) {
        fprintf(stderr, "load failed:\n%s\n", err ? err : "(unknown)");
        orch_string_free(err);
        return 1;
    }

    OrchSession *session = orch_session_new(agent, ".", &err);
    if (!session) {
        fprintf(stderr, "session failed: %s\n", err ? err : "(unknown)");
        return 1;
    }

    char *reply = orch_session_message(session, "world");
    printf("%s\n", reply);

    orch_string_free(reply);
    orch_session_free(session);
    orch_agent_free(agent);
    return 0;
}
