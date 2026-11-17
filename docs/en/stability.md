# Stability and compatibility

> This is a translation of [docs/stability.md](../stability.md) as of commit `cdfcc83`. The Japanese document is the source of truth (ADR-0031).

This document describes how `wys` is versioned, what is kept stable and how far, the supported platforms, the support period, and how failures behave (NFR-006 / NFR-007 / NFR-008; decided in ADR-0029).

## 1. Versions

- SemVer `0.y.z`. The first version is `0.1.0`. `wys --version` (or `wys version --format json`) shows the version, commit, and target.
- While the version is `0.y`, a bump of `y` may include breaking changes. `z` is for fixes that contain no breaking changes.
- Changes are listed in [CHANGELOG.md](../../CHANGELOG.md), in the Keep a Changelog format.
- The conditions for `1.0` have not been decided.

## 2. Stability levels

| Surface | Level | Promise |
|---|---|---|
| Syntax and semantics of the language, builtin functions and types | Unstable | May change when `0.y` is bumped. Changes are listed in the CHANGELOG |
| Error codes (such as `E0207`) | Stable | Their meaning does not change and they are not reused. A retired code is kept and marked as retired ([list](../spec/error-codes.md), Japanese) |
| Diagnostic JSON (`wys check --format json`) and the JSON of `wys explain` | Versioned | Fields are not removed and their meaning is not changed without bumping `schema_version`. Adding a field does not bump the version |
| The JSON-lines protocol of the proxy, recordings (`WYS_LLM_RECORD`), and traces (`WYS_TRACE`) | Versioned | Managed by `v` on each line (ADR-0023 / ADR-0025) |
| CLI subcommands and flags, environment variables | Unstable (documented) | May change when `0.y` is bumped. Described in the [reference](../guide/reference.md) (Japanese) |
| The runtime ABI, the IR, the API of the Rust crates, and the runtime's symbol names | Internal | No promise. Use a generated executable and the runtime (`lib/`) from the same version |

## 3. Supported platforms

| Tier | Platform | Checks |
|---|---|---|
| 1 | macOS arm64 | All checks run for every version, including conformance in Debug / Release / AddressSanitizer |
| 2 | Linux x86_64 | Checks run in Docker for every version |
| — | Windows | Not supported |

- Python interop supports **CPython 3.14 only** (ADR-0018).
- The syntax is close to Python, but **full compatibility with Python is not a goal**. Python code does not run as it is.

## 4. Support period

- Only the latest `0.y` is supported. Fixes are released in the latest `0.y.z` and are not backported to an older `0.y`.
- Report vulnerabilities as described in [SECURITY.md](../../SECURITY.md).
- Performance targets (SLOs) have not been decided. Measurements are in [Benchmarks](../benchmarks.md) (Japanese), but they are not a promise.

## 5. Behavior on failure (NFR-007)

The behavior described here collects what the individual ADRs decided.

### Expected failures are `Result`

- An operation that can fail returns `Result[T, E]`, and `?` passes the failure to the caller (ADR-0013). There is no `unwrap`. Where a failure is deliberately turned into a crash, you write `case Err(e): panic(...)` so that it stands out.
- A value from outside (`parse_json` and so on) is `Unknown`, and `validate[T]` returns an `Err` that says where and what is wrong.

### Panics (runtime errors)

- In an unexpected state (an index out of range, `panic(msg)`, and so on), the program flushes stdout, prints `runtime error: <message>` to stderr, and exits with **exit code 101**. When the location is known, ` at file:line:column` is appended. No stack trace is printed (ADR-0013).
- Tools can detect this by the prefix and the exit code.
- If objects remain unreleased at exit, the program prints `runtime error: N object(s) were not freed (this is a compiler bug)` and exits with **exit code 102**. This detects ARC mistakes and is checked only on a normal exit.

### The boundary with Python (FFI)

- Every operation that calls Python returns `Result[..., PyError]`. A Python exception does not become a panic (AC-010). `PyError` is `{kind, message, traceback}` (ADR-0018).
- If Python cannot be initialized, the result is `runtime error: cannot initialize Python` (101). If Python 3.14 is not found at build time, it is a tool error (exit code 2).
- Python is not a sandbox. Python code can cause arbitrary side effects ([Security](security.md)).

### LLMs and tools

- Every failure of `llm_generate` and of an agent is returned as `Err(LlmError)`, not as a panic (ADR-0023). The values of `kind` are:
  - `denied`: a tool that is not in the list, or a missing capability. `llm_generate` is aborted.
  - `budget`: a budget for calls, tokens, or cost was exceeded.
  - `timeout`: the agent's deadline passed.
  - `schema`: tool arguments do not fit their types.
  - `approval`: approval was requested, but the input was EOF, or the policy value is invalid.
  - `provider`: a failure communicating with the provider, an invalid response, a truncated response (`max_tokens`), and so on.
  - `mock_exhausted` / `replay`: the mock responses for a test ran out, or a replay did not match the request.
- Calling the test-only `llm_mock_push` with a real provider or a replay configured is a runtime error (101), so that a test setup cannot turn into a production call.
- When the approver denies a call, execution is not aborted: `denied by approver` is returned to the LLM as the tool result, and it continues.
- **Timeouts are checked at call boundaries**: before an LLM call, right after a response is received, and before a tool runs. A running tool or an HTTP exchange is not interrupted. A response that arrives after the deadline is not accepted, and the tools it requested are not called.
- An `Err` returned by a tool is passed to the LLM as an error. The program does not stop.

### Failures of the tool itself (CLI)

- A failure of a `wys` command itself (a missing file, clang or Python not found, and so on) exits with code 2.
- Diagnostics such as type errors can be read by machines with `wys check --format json`.
