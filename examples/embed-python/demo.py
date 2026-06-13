#!/usr/bin/env python3
"""Embed Orchard from Python. Build the module first:

    pip install maturin
    maturin develop -m crates/orchard-py/Cargo.toml

Then: python examples/embed-python/demo.py
"""
import orchard

SRC = '''
agent Greeter {
    model { provider: mock, name: "echo" }
    on message(text: str) -> str { return gen "Hello, {text}!" }
}
'''

print("orchard", orchard.__version__)
errors = orchard.check(SRC, "greeter.orch")
assert not errors, errors

agent = orchard.Agent.load(SRC, "greeter.orch")
session = agent.session(None)
reply = session.message("world")
print(reply)
assert "Hello, world!" in reply
