# wys for VS Code

Syntax highlighting and language server support for [wys](https://github.com/1ppe1/wys).

- Highlighting for `.wys` files and for `wys` code blocks in Markdown
- Diagnostics, hover, go to definition, quick fixes, and formatting from `wys lsp`

## Install

1. Install `wys` itself (see the [README](https://github.com/1ppe1/wys#install)) and make sure `wys` is on your `PATH`, or set `wys.path`.
2. Download `wys-<version>.vsix` from [Releases](https://github.com/1ppe1/wys/releases) and install it:

   ```sh
   code --install-extension wys-0.1.1.vsix
   ```

## Settings

| Setting | Default | Description |
|---|---|---|
| `wys.path` | `wys` | Path to the `wys` executable. The extension runs `wys lsp` |

Run **wys: Restart Language Server** from the command palette after updating `wys`.

## Development

```sh
npm ci
npm test            # tokenize the examples with the grammar (vscode-textmate)
npm run package     # type-check, bundle with esbuild, and build the .vsix
```
