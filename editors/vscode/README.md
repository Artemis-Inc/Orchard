# Orchard for VS Code and Cursor

Language support for [Orchard](https://github.com/Artemis-Inc/Orchard), a typed,
concurrent language for building LLM agents.

This extension adds:

- The Orchard leaf icon for `.orch` files in the explorer and editor tabs.
- Syntax highlighting for agents, skills, types, `gen`, `match`, tools, and the
  rest of the grammar.
- Editor configuration: comment toggling, bracket matching, and auto-closing
  pairs.

## Install

Search for "Orchard" in the Extensions view, or install the packaged `.vsix`:

```sh
code --install-extension orchard-3.0.0.vsix
```

Cursor uses the same extensions, so the same `.vsix` works there.

## Build the package yourself

```sh
cd editors/vscode
npx @vscode/vsce package
```

This produces `orchard-3.0.0.vsix`, which you can install with the command above
or publish to the marketplace.
