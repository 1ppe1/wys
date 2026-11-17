# Security: guarantees, non-guarantees, and the threat model

> This is a translation of [docs/security.md](../security.md) as of commit `cdfcc83`. The Japanese document is the source of truth (ADR-0031). References such as `#144` are issue numbers in the private repository that was used for development; they do not correspond to issues in this repository.

This document describes **what the `wys` compiler, runtime, LLM proxy, and language server guarantee, and what they do not** (NFR-001 / NFR-005, AC-007). How and what to report as a vulnerability is in ADR-0028 and [SECURITY.md](../../SECURITY.md). This document is based on the security review done before the release (task T11 in `docs/plans/release.md`, 2026-11-11, by Fable).

## 1. Threat model

| Trusted | Not trusted |
|---|---|
| The program source (written by the user), the compiler, the runtime, environment variables set by the user (`WYS_*`), and a provider given with `cmd:` | **LLM output** (text, the names and arguments of tool calls, and metadata such as `usage`), **tool results**, **the shape of provider responses**, and outside values that enter the program (`Unknown`, `parse_json`) |

What we want to protect:

- **The capability boundary** (AC-007): an LLM cannot, through tool calls, use a capability that the agent was not given (except `io`; see section 3). A tool that requires approval does not run without approval. Budgets (calls, tokens, cost, and time) are not exceeded.
- **Secrets** (NFR-005): the API key does not appear in the runtime, in generated executables, in recordings, in audit events, in traces, or in error messages. The content of prompts and responses does not appear in logs without an explicit opt-in.
- **Memory safety** (MEM-005 / NFR-001): the scope given in section 3 of `docs/design/memory-model.md`.

## 2. What is guaranteed

### 2.1 Capabilities (AI-004, ADR-0023)

- At the root, `WYS_CAPABILITIES` can **only narrow** the runtime capabilities. It cannot widen them; unknown names are ignored, and an empty value removes all of them. Entering an `@agent` takes the **intersection** with the enclosing set.
- Before a tool runs, the runtime checks, in this order: the tool list, capabilities (the tool's `uses` ⊆ the agent's capabilities), budgets, and approval.
- The entry points check again at run time: `llm_generate`, the Python entry points (`py_import` / `getattr` / `call`), and `exit`. A denial is recorded as the audit event `capability_denied`.
- A child task created with `spawn` inherits the agent context of its parent. In replay (`replay:`), tools go through the same checks and actually run.
- Static checks (ADR-0014 / ADR-0024): `uses` is required on a `@tool`, and E1102 checks that the body (including inferred effects) fits the declaration. Also checked: the `uses` of an `@agent` ⊆ its `capabilities` (E1405), `capabilities` ⊆ its own `uses` (E1406), tools with the same name (E1404), and negative budgets (E1402).

### 2.2 Tool arguments and JSON (AI-002)

- Tool arguments go through `parse_json` and then `validate[T]` before reaching the body. `parse_json` stops at a depth of 128 and rejects duplicate keys. Type confusion (`2.0` as `int`, `true` as `int`, and so on), missing fields, and arguments that are not an object are rejected. Keys that are not in the declaration are ignored (`docs/design/ai-sdk.md` 4.3).
- A line of a provider response is read up to 16 MiB with a recursion limit. A broken or overlong line gives `Err(kind="provider")`; it does not crash and does not exhaust memory.

### 2.3 Approval (AI-005)

- The default (`WYS_APPROVAL` unset or empty) is `deny`. An unknown value fails closed with `Err(kind="approval")`.
- `prompt` prints the question to stderr and reads one line from stdin. Only `y` / `yes` approve; an empty line and anything else deny; EOF gives `Err(kind="approval")`. **In the tool name and arguments shown in the question, characters that control the terminal (C0 / DEL / C1), bidirectional controls, and invisible characters are escaped as `\u{..}`, and the text is not truncated however long it is** (#144; truncation would let the part being approved hide after the cut).

### 2.4 Budgets (AI-006)

- Usage is counted against every agent on the stack, and execution stops when any limit is exceeded (an inner agent cannot exceed what remains of an outer one).
- From the provider's `usage`, only non-negative integers are accepted. A response with broken numbers gives `Err(kind="provider")`; the valid numbers and a valid `cost_usd` are still counted, and when the token count or cost is unknown, later calls are treated as over budget (failing closed; #143). `cost_usd` is accepted only when finite and non-negative.
- `latency_ms` in recordings and mocks is clamped to between 0 and one day, and additions to the clock saturate (#143).

### 2.5 Secrets and privacy (NFR-005, ADR-0023 point 16, ADR-0025)

- Only the proxy (`wys llm-proxy`) reads `ANTHROPIC_API_KEY`. It does not appear in the runtime, in generated executables, or in recordings. The proxy's error messages contain only the HTTP status and the kind and message of the API error, not headers or the key. The proxy does not log requests or responses.
- Audit events (`WYS_AUDIT`) and traces (`WYS_TRACE`) do not include content by default (the system prompt, messages, responses, tool arguments, and tool results). Content in traces is an explicit opt-in with `WYS_TRACE_CONTENT=1`.
- Recordings (`WYS_LLM_RECORD`) contain content; they are files the user asks for by name. The file mode is 0600, and an existing file or symbolic link is removed before a new file is created, so the target of a link is not overwritten (#145). Audit and trace files are opened with `O_NOFOLLOW`.
- The proxy and `cmd:` providers are started without a shell, so arguments cannot be injected with spaces or quotes. A path containing `/` is checked to exist and to be executable.

### 2.6 Temporary files (#145)

- Temporary files of `wys build` / `run` / `test` / `eval` (intermediate objects, generated executables, and eval traces) are created inside a uniquely named directory with mode 0700, which is not created if it already exists. Arguments are passed to clang individually, so a file name cannot inject options.

### 2.7 Language server (ADR-0026)

- Only `file://` URIs are handled; others are ignored. The server does not build, link, or run programs; it only analyzes them.
- Analysis runs on a separate thread with a large stack, and a panic does not bring the server down. Stack overflow is prevented by the parser's nesting limit (E0107, 64 levels). Broken request parameters give `InvalidParams`.

### 2.8 Memory safety (MEM-005, `docs/design/memory-model.md` section 3)

- `unsafe` is used in only two crates, `wys_runtime` and `wys_runtime_python` (the workspace sets `unsafe_code = "deny"`). It is used for the C ABI, heap objects, task stacks (`mmap` with guard pages) and context switching, and the CPython Stable ABI.
- List indexes are checked, including negative ones, and integer overflow and division by zero are runtime errors. A NUL in a Python name is a failure, and invalid UTF-8 from outside is replaced.
- The conformance suite runs in three modes, Debug, Release (-O2), and AddressSanitizer, and checks the number of live objects at exit.

## 3. What is not guaranteed (out of scope)

- **`io` is not enforced at run time.** `io`, such as `print`, is checked statically only (`uses io`); `print` still works when `io` is removed from `WYS_CAPABILITIES`. The capabilities enforced at run time are `llm` / `python` / `exit` (`docs/design/ai-sdk.md` 7.2).
- **Python code is not sandboxed** (ADR-0018). An agent with the `python` capability can do anything through Python. In addition, the user's Python code can run during conversion of Python values (`__len__` / `__getitem__` of a `list` subclass) and during release (`__del__`). Conversion requires the `python` effect in the type check, but release does not (for example at exit).
- **Errors in the user's program**, a `cmd:` provider chosen by the user, and the content of LLM output itself (mistakes and bias) are out of scope.
- The `detail` of an audit event can contain the reason a schema validation failed (which may include key names written by the LLM) and a tool name given by the LLM. It does not contain the content itself.
- A program that uses `llm_generate` embeds the absolute path of the compiler, which is used to find the proxy (visible with `wys dump-llvm`). The path of the build environment can be read from a distributed executable. In another environment, give the proxy with `WYS_LLM_PROXY` or `PATH` (ADR-0030 point 8).
- AddressSanitizer does not instrument the runtime library (Rust) itself, only the generated code.
- Windows is not supported (ADR-0029).

The remaining hardening (ASan / Miri for the runtime) is tracked in #146. The hash for string keys of `dict` is SipHash with a random seed per process, and a line of a provider response is limited to 16 MiB (#146).

## 4. Review log

| Date | Scope | Result |
|---|---|---|
| 2026-11-11 | Release task T11 (Fable): capabilities / approval / budgets, tool arguments, the proxy, secrets, the language server, memory safety, and temporary files | No P1. Four P2 issues fixed (#143 validation of `usage` and the limit on `latency_ms`, #144 escaping in the approval question, #145 how output and temporary files are created). P3 items are in section 3 of this document and in #146 |
