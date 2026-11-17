# wys

[日本語](README.ja.md)

`wys` is a statically typed, compiled language for writing programs that call LLMs and run tool-using agents. It reads much like Python, is type-checked ahead of time, and builds a standalone native executable. LLM calls and agents are written with ordinary functions and decorators from the standard SDK, and capabilities, approvals, and budgets are **enforced by the runtime**, not by instructions in a prompt.

> **Status:** the first public release, [0.1.0](https://github.com/1ppe1/wys/releases/tag/v0.1.0), is out. While the version is 0.y, syntax, builtins, and the CLI can change without notice ([stability and compatibility](docs/en/stability.md)).

## The runtime says no

Mark a tool with `approval="required"`. When the model asks to call it, the runtime does not run it unless the call is approved. From [`examples/agent_demo.wys`](examples/agent_demo.wys):

```wys fragment
@tool(description="Save a note to disk.", approval="required")
def save_note(title: str) -> None uses io:
    print("saved " + title)
```

The default policy is to deny. The model (a mock in this example) asks for `save_note`, the tool does not run, and the attempt is recorded as an audit event:

```console
$ wys run examples/agent_demo.wys
notes: 25 chars, saved=false
0 llm_called
1 tool_called char_count
2 llm_called
3 approval_requested save_note
4 approval_denied save_note
5 llm_called
```

Allow it with `WYS_APPROVAL=allow`, and the tool runs:

```console
$ WYS_APPROVAL=allow wys run examples/agent_demo.wys
saved notes
notes: 25 chars, saved=false
0 llm_called
1 tool_called char_count
2 llm_called
3 approval_requested save_note
4 approval_granted save_note
5 tool_called save_note
6 llm_called
```

What is and is not guaranteed is spelled out in [Security](docs/en/security.md).

## Features

- **Types:** annotations with inference, narrowing of `T | None`, structural records, generics, exhaustiveness checking for `match`, and `Result[T, E]` with `?`. Values that come from outside are `Unknown` and cannot be used until checked with `validate[T]`.
- **Native executables:** the compiler emits LLVM IR and links with clang. The runtime is linked statically, so the output is a single executable. Memory is managed with reference counting (ARC) and a move optimization.
- **AI SDK:** typed `llm_generate[T]`, `@tool` / `@agent`, runtime enforcement of capabilities, approvals, and budgets, audit events, tracing, record and replay, and evaluation cases (`wys eval`).
- **Interop and tooling:** embedded CPython, `async` / `await`, `wys fmt` / `wys test` / `wys explain`, machine-readable JSON diagnostics, and a language server (`wys lsp`).

## A small example

```wys
type Point = {x: int, y: int}

def describe(n: int | None) -> str:
    if n is None:
        return "nothing"
    return "value"

def main() -> None:
    p = Point(x=1, y=3)
    print(p.x + p.y)
    print(describe(None))
```

Type errors are found at compile time. `wys check --format json` prints diagnostics for tools and AI agents to read.

## Install

Supported platforms are macOS arm64 and Linux x86_64. Windows is not supported. You need `clang`, which `wys` uses to link.

Install the latest release. The script downloads the archive from [Releases](https://github.com/1ppe1/wys/releases/latest), verifies its SHA-256, and unpacks it under `~/.local`:

```sh
curl -fsSL https://raw.githubusercontent.com/1ppe1/wys/main/scripts/install.sh | sh
```

Pass `--prefix <dir>` or `--version <version>` after `sh -s --` to change where and what it installs.

The macOS binary is not notarized. If Gatekeeper blocks it, run `xattr -d com.apple.quarantine "$HOME/.local/bin/wys"`.

Or build from source (the Rust toolchain is pinned in `rust-toolchain.toml`):

```sh
git clone https://github.com/1ppe1/wys.git && cd wys
cargo build --release
target/release/wys --version
target/release/wys run examples/hello.wys
target/release/wys run examples/agent_demo.wys   # @tool / @agent; the default provider is a mock and does not use the network
```

## Documentation

| Document | Contents |
|---|---|
| [Getting started](docs/en/getting-started.md) | From installation to types, `None`, `Result`, tests, and async |
| [AI SDK](docs/en/ai-sdk.md) | Typed LLM calls, tools and agents, capabilities, approvals, and budgets |
| [Security](docs/en/security.md) | What is guaranteed and what is not |
| [Stability and compatibility](docs/en/stability.md) | What can change while the version is 0.y |

The documents above are translations. Japanese is the source of truth, and the rest of the documentation is in Japanese only:

| Document (Japanese) | Contents |
|---|---|
| [Python interop](docs/guide/python.md) | Embedding CPython |
| [Reference](docs/guide/reference.md) | Commands, builtins, environment variables, and error codes |
| [Benchmarks](docs/benchmarks.md) | Method and results (absolute numbers and the measurement environment only) |
| [Index](docs/README.md) | Requirements, design, ADRs, plans, and everything else |

## Roadmap

| Phase | Contents | State |
|---|---|---|
| Phase 1 | Language and native MVP (type checking, standalone executables, tests) | Done |
| Phase 2 | AI-native MVP (typed LLM output, capability denial, budget enforcement) | Done |
| Phase 3 | Performance and reach (SIMD and parallelism, Wasm, GPU, package management) | Not started |

Details are in [docs/roadmap.md](docs/roadmap.md) (Japanese).

## Development

```sh
./scripts/ci.sh   # fmt, clippy, tests, and conformance (Debug / Release / AddressSanitizer): the same checks as CI
```

Design decisions are recorded as [ADRs](docs/adr/README.md) (Japanese). Instructions for AI agents (Claude Code) are in [CLAUDE.md](CLAUDE.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option. Copyright (c) 2026 1ppe1. Licenses of the dependencies are listed in [docs/third-party-licenses.md](docs/third-party-licenses.md).

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute, [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the code of conduct, [SECURITY.md](SECURITY.md) for reporting vulnerabilities, and [CHANGELOG.md](CHANGELOG.md) for the history of changes.
