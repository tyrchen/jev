# Jev CLI architecture, correctness, and usability review

Date: 2026-09-26
Status: Required reliability pass implemented; offline verification complete
Baseline: working tree based on `f1adf57`, including the existing uncommitted CLI migration

## Assessment

The architecture is appropriate for a small, single-request CLI. Keep the separation between a network-independent `jev-core` and the CLI application. The question constructors produce the expected request shapes, and bounded input reads, explicit HTTPS timeouts, environment-based credentials, and JSON-only stdout are good foundations.

At the review baseline, the implementation was not a sufficiently verified boundary for automated decisions. Two important contracts were unenforced: callers could bypass the supposedly validated question constructors, and successful HTTP responses could contain missing or invalid answers and still produce exit status 0. Input handling and help also needed correction. Passing the baseline tests did not establish correctness of every feature or of live provider behavior.

Priorities: **P1** means fix before relying on the CLI for automated decisions; **P2** means fix for dependable everyday use; **P3** means optional improvement. These are engineering priorities, not claims of demonstrated exploitation.

## Scope and evidence

Reviewed all production Rust and existing tests in [CLI main](../apps/cli/src/main.rs) and [core](../crates/core/src/lib.rs), workspace/package manifests, lockfile integration, [README](../README.md), [Makefile](../Makefile), toolchain pin, and [CI workflow](../.github/workflows/build.yml). Line references below refer to this working tree.

Inspected the installed `openrouter-rs 0.16.0` Decisions implementation and shared transport parsing, including the actual response types rather than assuming the SDK validates decision semantics. The [current SDK documentation](https://docs.rs/openrouter-rs/latest/openrouter_rs/) also identifies 0.16.0. No dependency upgrades are proposed as a substitute for fixing application contracts. `docs/` did not exist when checked; no separate research document was needed for this code review.

The [Decisions API reference](https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request) confirms the endpoint, accepted state kinds, and question/answer shapes. The [Jev guide](https://openrouter.ai/docs/guides/community/jev) confirms the three primitives and the model's 32,000-token context window. A local byte limit cannot guarantee that a request fits that token window.

Verification performed:

- `cargo test --workspace --locked --offline`: **12 unit tests and 10 doctests passed**.
- `cargo run -p jev-cli --locked --offline -- --help`: successful; inspected actual help.
- Nine offline subprocess probes: missing credentials, conflicting state sources, unpaired criteria, insufficient options, malformed JSON, empty JSON string, invalid UTF-8, oversized stdin, and a model containing a newline. All produced empty stdout and exit 1; the errors exposed the ordering and validation gaps below.
- Held stdin open for an invalid one-option Choice invocation: it still waited after one second and was terminated by the review harness.
- An external temporary Rust probe successfully constructed an invalid public `DecisionQuestion` and deserialized an SDK response with an empty answers map. Temporary probe files were removed.

Credentials were removed from subprocess environments. No paid API calls or live success-path checks were performed. HTTP failures, output formatting, and pipeline behavior were inspected statically, not exercised end to end. No production code or committed tests were changed. Full build/lint/format, supply-chain, and release gates were not rerun for this spec-only deliverable; the focused test run established the reviewed code's existing baseline.

## Findings

### F1 — P1: the public question type does not enforce its invariants

**Evidence:** `crates/core/src/lib.rs:133–159, 180–275, 288–325`.

`DecisionQuestion` exposes public struct variants with raw strings and vectors. `#[non_exhaustive]` on the enum prevents exhaustive matching, but does not prevent construction of its existing variants. An external caller can construct an empty Score, a Choice with duplicate IDs, or a Noul with only one criterion. Serialization silently omits an unmatched Noul criterion; collecting duplicate Choice IDs into a map overwrites entries.

Confirmed external construction:

```rust
DecisionQuestion::Noul {
    instructions: String::new(),
    true_criteria: Some("yes".into()),
    false_criteria: None,
}
```

Its serialized value was `{"instructions":"","type":"noul"}`. Current CLI paths use the constructors, so this is a core library contract defect rather than a demonstrated CLI validation bypass.

**Required change:** expose an opaque question struct containing a private enum, or variants containing validated payload structs with private fields. Model Noul criteria as `Option<NoulCriteria>` with both descriptions required within the payload. Preserve fallible constructors and immutable accessors. Validate any future deserialization through the same constructors. Keep `ChoiceOption`'s existing private-field approach.

**Acceptance:** external code cannot construct invalid payloads or mutate a valid question into an invalid state. Add compile-fail coverage and exact serialization tests for all variants, including criterion omission.

### F2 — P1: successful responses are not validated against requests

**Evidence:** `apps/cli/src/main.rs:175–187, 259–279`; SDK `src/api/decisions.rs:71–80`.

The SDK uses `HashMap<String, Value>` for answers. The CLI serializes that map without checking the requested ID, primitive, selected option, score range, or numeric probabilities. The SDK accepts `{"model":"x","answers":{},"usage":{"input_tokens":0,"output_tokens":0}}`; the current writer would successfully emit it. An HTTP 200 is therefore sufficient for application success even without a usable decision.

**Required change:** retain the request's validated question and ID through response processing. Parse the requested answer into a typed domain answer and validate it before writing stdout. Require the expected ID and primitive; reject missing required fields, wrong numeric types, out-of-range probabilities/confidence, unknown selected Choice IDs, and Score positions outside the submitted scale. Validate probability keys against submitted options/levels and bound response collections. Define handling of unexpected answer IDs explicitly; for today's single-question command, reject them.

Keep additive metadata forward compatible. Do not require the returned model string to equal the requested alias. Do not infer confidence from the largest probability or demand exact floating-point sums/weighted-score equality: rounding and provider semantics require a documented tolerance backed by fixtures. The [official tutorial](https://openrouter.ai/docs/guides/community/jev-tutorial) illustrates rounded scores/probabilities and a returned dated model snapshot.

**Acceptance:** valid fixtures for all primitives pass; malformed or mismatched answers yield a protocol error, nonzero status, and no success JSON. Use fake transport responses so this is testable without credentials or billing.

### F3 — P2: local errors are reported after potentially blocking input

**Evidence:** `apps/cli/src/main.rs:141–158, 216–256`.

The application reads and parses state before constructing the question or loading credentials. With empty stdin, a one-option Choice or unpaired Noul reports an empty-state error instead of the invalid question. With an open pipe, the invalid invocation waits for input. Valid invocations with missing credentials also read state unnecessarily.

File open and reads have no deadline. The 90-second reqwest timeout covers the HTTP operation, not input acquisition. A stalled pipe or FIFO can block indefinitely.

**Required change:** validate argument relationships, ID, model, complete question, and credentials before input I/O for network commands. Express mutual exclusion of state sources and pairing of Noul criteria in Clap, with domain validation retained for library callers. Give input acquisition a documented deadline and cancellation policy, separate from the request timeout; explicitly handle files, pipes, and interactive `--state-file -`.

A timeout around `tokio::io::stdin()` alone is insufficient: its underlying read cannot be cancelled and can hold runtime shutdown open, as documented by [Tokio](https://docs.rs/tokio/latest/tokio/io/fn.stdin.html). Choose an input worker/runtime lifecycle that allows the process to exit on deadline; prove process termination in a subprocess test. Apply the same review to blocking file operations.

**Acceptance:** invalid questions and missing keys fail immediately with stdin held open. A valid invocation with stalled input exits within the configured budget, including runtime shutdown. If an offline preview is added, it deliberately skips the credential requirement.

### F4 — P2: state interpretation is ambiguous and documentation disagrees

**Evidence:** `crates/core/src/lib.rs:354–384`; `apps/cli/src/main.rs:46, 220–223`; `README.md:69`.

Malformed JSON is intentionally treated as plain text, so a truncated JSON file can silently change the decision's meaning. This matches the permissive auto-detection design; it is an automation hazard rather than a parser crash. Valid JSON strings are decoded too: input `"hello"` loses its quotes, despite the README saying only objects and arrays are decoded. A JSON empty string is rejected, while JSON booleans, numbers, and null are kept as literal text.

The README and inline oversize error suggest using files/stdin for state exceeding 1 MiB, but all three input paths enforce the same limit. That recovery advice cannot work.

**Required change:** add `--state-format auto|text|json`, defaulting to `auto` for compatibility. `text` preserves input bytes as UTF-8 text subject to nonempty/size checks. `json` requires valid JSON and accepts only nonempty strings, objects, and arrays; reject scalar booleans/numbers/null with a clear explanation. `auto` preserves current behavior and documents it precisely. All sources share one explicit byte limit; help distinguishes shell argument limits from Jev's input limit. State depth/node limits must be explicit for strict parsing; exceeding them must be an error, not a fallback to text.

**Acceptance:** table-driven cases cover every JSON kind, malformed/deep JSON, UTF-8, whitespace, exact byte boundaries, and equivalent content from each source. Correct the oversize advice and document combined state/question token constraints without pretending bytes equal tokens.

### F5 — P2: transport response size and diagnostic exposure are uncontrolled

**Evidence:** SDK `src/transport/response.rs`, specifically `parse_json_response`, `error_from_response`, and `response_deserialization_error`; `apps/cli/src/main.rs:135, 162–185`.

The SDK buffers success and error bodies using `response.text()` without a byte cap. A time limit is not a size limit. Deserialization errors include a body preview, which the CLI prints through the full anyhow chain. An upstream response that echoes submitted state can expose it in stderr; control characters other than CR/LF are not escaped by that preview helper. This is a boundary weakness, not evidence that normal OpenRouter responses currently leak credentials.

**Required change:** enforce a response byte cap before buffering/deserialization on both success and failure paths, including chunked responses. Prefer an upstream SDK extension; if unavailable, implement one narrow Decisions transport adapter with bounded reads. Checking serialized size after SDK parsing is too late. Default errors should contain a safe category, HTTP status, and bounded request ID, without raw response bodies or credentials. Explicitly bound and escape externally supplied diagnostic fields.

**Acceptance:** oversized success/error streams are rejected while reading; malformed bodies containing sensitive sample state or terminal escapes do not appear verbatim in stderr. Retain actionable authentication, credit, rate-limit, and timeout guidance.

### F6 — P2: model validation accepts obviously invalid identifiers

**Evidence:** `apps/cli/src/main.rs:203–207`.

`validate_model` checks only trimmed emptiness and byte length. A model containing a newline reached credential lookup in the offline probe, demonstrating that the model check accepted it. Leading/trailing whitespace is also retained. JSON encoding prevents request JSON injection, but avoidable invalid requests still reach the provider once credentials exist.

**Required change:** introduce a validated model ID type with documented syntax, length cap, and rejection of whitespace/control characters. Preserve supported provider-qualified IDs, version suffixes, and the documented `~typesafe/jev-latest` alias. Do not restrict solely to the current default or claim to validate model availability offline.

**Acceptance:** whitespace/control-character cases fail before input or network activity; supported IDs and aliases remain accepted. Invalid credentials remain redacted.

### F7 — P2: integration contracts have little regression coverage

**Evidence:** CLI test module at `apps/cli/src/main.rs:282`; core test module at `crates/core/src/lib.rs:399`; CI test command.

CLI tests cover TLS initialization and parsing/building one example per primitive. Core tests cover eight representative cases. There is no repository coverage for filesystem/stdin behavior, request construction through transport, response validation, output modes, broken pipes, upstream errors, or cancellation. CI runs nextest, but has no separate `cargo test --doc` step for the ten public API examples.

**Required change:** introduce small testable boundaries for input, execution, transport, and output; add the behavioral checks in the verification matrix below. Include doctests in the discoverable Makefile/CI verification path. Tests should assert contracts, not duplicate implementation line by line.

**Acceptance:** offline tests can execute complete successful and failing commands against fixtures. Credentialed provider checks remain opt-in and report which contract they exercised.

### F8 — P3: help, diagnostics, and output contracts need polish

**Evidence:** `apps/cli/src/main.rs:62–64, 70, 118, 133–136, 259–279`; `crates/core/src/lib.rs:25–38`; `README.md:77`.

`--output` help describes response selection although the values select compact/pretty formatting. Score help omits its minimum count. Length errors omit actual and allowed bytes; duplicate IDs are indistinguishable from malformed IDs. Semantic input errors exit 1, while Clap syntax errors exit 2. Broken stdout pipes become generic write failures. The claimed “complete API response” is actually the SDK's reserialized representation: unknown envelope fields are dropped, and absent optional fields are omitted.

**Required change:** fix help text and include command examples; report field/index, limit, and observed length without echoing whole input. Document exit status categories and classify semantic usage errors consistently. Define a quiet broken-pipe policy while retaining failures for other write errors. Describe default output as the supported response envelope, or explicitly preserve unknown fields in the adapter if full wire fidelity is required. Keep compact JSON plus one newline as the default.

**Acceptance:** help snapshots and CLI error assertions verify accurate guidance. All four combinations of compact/pretty and full/answers-only output parse as JSON and contain no diagnostics. A deliberate downstream pipe close follows the documented policy.

## Baseline feature coverage assessment

This table records the state at review time. The implementation record at the end of this document tracks the current result.

“Supported” below means source behavior and the stated evidence agree; it does not mean live service compatibility was tested.

| Feature | Assessment | Evidence or remaining gap |
| --- | --- | --- |
| Noul with paired criteria | Request serialization supported | Exact core fixture; CLI parser test |
| Noul without criteria | Implemented | Doctest checks primitive; add exact omission fixture |
| Choice options | IDs, uniqueness and 2–64 count checked by constructor | Valid map tested; invalid/count boundaries lack tests; F1 |
| Score levels | Order preserved; 2–32 count and descriptions checked | Valid array tested; invalid/count boundaries lack tests |
| Inline/file/stdin state | Shared state conversion; bounded file/stdin reads | Negative probes; successful file/pipe parity untested; F3/F4 |
| Auto JSON conversion | Implemented with surprising string/scalar behavior | Core scalar/object tests and probes; F4 |
| Custom question ID | ASCII identifier check and keyed request construction | Static inspection; custom-ID response/output path untested |
| Model override | Passed through to request | Local syntax validation insufficient; F6 |
| Credentials | Environment-only; length/printable-ASCII checks | Missing-key probe; no live authentication claim |
| TLS and HTTP deadlines | AWS-LC explicitly installed; 15s connect / 90s request | TLS test passed; timeout behavior untested |
| API route and request envelope | SDK selects `/api/alpha/decisions` | Source inspection and official reference |
| Compact/pretty output | Both serialization branches implemented | No end-to-end fixture tests; F2/F8 |
| Answers-only output | Emits keyed answers map, not scalar answer | Static inspection; F2/F8 |
| Failures and exit status | stderr plus nonzero for observed failures | Generic categories and incomplete transport coverage |
| Multiple questions, reusable definitions, thresholds | Not implemented or promised | Optional future scope, not current correctness defects |

## Target architecture and data structures

Keep two crates. The current program makes one request and owns its state; actors, channels, shared maps, and extra workspace crates would add complexity without solving an observed problem.

Within `jev-core`, keep validated questions, state parsing, limits, and typed answer validation independent of HTTP, environment variables, filesystem paths, and Clap. Remove CLI flag names from core error messages; map domain failures to command-specific guidance in the application.

Within `apps/cli`, extract focused modules as the fixes land: arguments, input, Decisions transport, output, and orchestration. `main` should map the result to diagnostics and exit status. A small injected transport interface or generic async function is sufficient for fixture testing; dynamic dispatch is unnecessary. The execution sequence should be:

```text
parse arguments → validate command/configuration → acquire bounded state
→ construct request → execute bounded HTTP operation
→ validate answer against question → serialize selected output → exit
```

| Structure | Decision |
| --- | --- |
| `ChoiceOption` with private strings and fallible `FromStr` | Keep; `split_once('=')` correctly permits `=` inside descriptions |
| `Vec` of Score levels | Keep; order is semantically meaningful |
| `Vec` of Choice options plus borrowed-ID `BTreeSet` | Keep; at most 64 entries makes complexity insignificant |
| Dynamic `serde_json::Value` state | Keep behind a validated `State` wrapper; application context is genuinely dynamic |
| Public raw-field question variants | Replace as specified by F1 |
| Two independent optional Noul criteria | Replace with one optional validated pair |
| Raw question/model identifier strings across execution | Introduce validated ID types; validate once at the boundary |
| Known question serialization assembled as `Value` | Prefer typed serialization inside the adapter; conversion to SDK `Value` belongs at the boundary |
| Untyped answer payloads | Convert to typed, request-validated answers before success |
| Response → `Value` → encoded string | Low priority allocation cleanup; serialize the selected representation directly where practical |

No profiling evidence justifies `SmallVec`, extra concurrency, or a parser dependency for `ID=DESCRIPTION`. The 1 MiB read buffer preallocation is wasteful for tiny inputs but bounded and secondary to correctness. The SDK enables a broad Tokio feature set; dependency size can be measured later rather than rewriting a working client solely to reduce it.

## Usability requirements and optional extensions

### Required for the reliability pass

- Implement F3/F4/F8: fail early, make input interpretation explicit, correct limits/help, and define output/exit behavior.
- Expose validated request/connect/input timeout settings with documented defaults and sensible maximums. If settings are persisted, use YAML with precedence CLI > explicitly selected configuration > defaults. Keep API keys in the environment.
- Add representative output examples for each primitive and explain that a successful command means a valid response, not a positive Noul verdict. Show `jq -e` for callers that intentionally want threshold-based shell status.
- Describe model reproducibility accurately: the [tutorial](https://openrouter.ai/docs/guides/community/jev-tutorial) says the version ID resolves to a dated snapshot. The current default avoids the latest alias, but exact immutable behavior should not be promised without checking addressable snapshots and service guarantees.

### Optional follow-up, after correctness work

1. **Offline request preview:** add `--dry-run` that validates input and emits the exact request JSON without credentials or network calls. Keep authentication headers out of the preview. Document that the preview includes user-provided state.
2. **Reusable questions:** add a YAML definition workflow, with versioned schema, rejection of unknown configuration fields, byte/count limits, and shared constructors. Prefer one coherent file interface over many additional question flags.
3. **Multiple questions per state:** build on definitions, validating unique IDs and total question/request limits. Keep today's three simple subcommands. This is multiple questions in one request, distinct from batch processing many states.
4. **Discoverability:** offer a visible `yes-no` alias for `noul`, shell completions, and examples for quoting leading hyphens/equals signs. Generate completion artifacts through Makefile targets if adopted.

Retries should remain disabled by default until the billing/idempotency contract is defined. A timeout can occur after remote execution; blindly retrying may create another charge. Batch workers, automatic routing, threshold policies, and additional provider controls should be separate features with their own specs.

## Verification requirements for implementation

| Boundary | Required cases |
| --- | --- |
| Constructors | Empty/whitespace text; exact max/max+1 UTF-8 bytes; invalid IDs; duplicate options; min/max counts; paired/absent criteria |
| Public API | Invalid direct construction fails to compile; accessors cannot invalidate values; error variants remain useful to library callers |
| Input | Inline/file/stdin parity; no input; conflicting flags; missing file; invalid UTF-8; exact 1 MiB/max+1; stalled pipe/FIFO; process termination |
| Parsing modes | Objects, arrays, strings, booleans, numbers, null; malformed input; duplicate-key policy; depth/node bounds; no silent fallback in strict mode |
| Requests | Exact endpoint and envelope for each primitive; custom model and ID; ordered Score criteria; paired/omitted Noul criteria |
| Responses | Valid fixtures; empty/missing/extra answer IDs; wrong primitive; unknown Choice ID; bad score/probability types/ranges/keys; rounded valid values |
| Transport | 401/402/429/5xx; connect/request deadlines; malformed and oversized bodies; no retries by default; no sensitive body previews |
| Output | Compact/pretty × full/answers-only; final newline; empty stdout on pre-output failure; partial-write failure policy; broken pipe |
| CLI diagnostics | Correct usage status; field-specific bounds; no secrets; terminal-safe external values; accurate help/examples |
| CI | Unit/integration tests plus doctests; formatting/linting per project gates; repeatable offline fixtures |

Do not assert deterministic live probabilities. Optional live smoke checks should assert protocol shape and ranges and record the resolved model/request ID, with explicit cost awareness. Local mock endpoints should be injected through test configuration, not exposed as an arbitrary production destination for API keys.

For implementation changes, follow the repository Rust gates (`cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`, with pedantic checks where useful). Dependency or lockfile changes additionally require `cargo audit` and `cargo deny check`. Put new repeatable verification automation in the Makefile. The existing `make test` uses nextest and needs a companion doctest step.

## Delivery order and completion criteria

1. **Enforce domain contracts:** F1 and F2, exact request fixtures, typed response validation, minimal transport injection. Complete when invalid public questions cannot be created and invalid responses cannot result in success.
2. **Make execution bounded and predictable:** F3–F6, input format selection, response byte limits, safe diagnostics, and process-level cancellation tests. Complete when every external boundary has a documented validation/resource policy and stalled input terminates correctly.
3. **Finish CLI usability and coverage:** F7/F8, help/output/exit documentation, all output combinations, CI doctests, and negative-path integration tests. Complete when the verification matrix passes offline and README examples match behavior.
4. **Consider optional workflows:** preview first, then reusable definitions/multiple questions if needed. Write separate feature specs before extending scope.

This review is complete as a specification. The baseline implementation and verification requirements above were open when the review was written. The implementation record below documents their current disposition; it does not claim live service compatibility.

## Implementation record

The reliability pass addresses the required F1–F8 findings. The core crate now exposes opaque validated questions, validated identifiers and state, and typed answers checked against the request. The CLI validates configuration before input, bounds and times input and HTTP response handling, supports explicit state formats, and emits safe diagnostics with documented output and exit behavior. Offline tests cover all three question primitives, request/response contracts, complete fixture-backed command success and failure, state-source parity and limits, stalled stdin/FIFO termination, transport errors and size caps, output modes, and broken pipes. The optional offline request preview was also implemented; reusable question definitions and multi-question workflows remain separate future scope.

Final verification on the pinned toolchain:

- `cargo build --workspace --locked --offline`: passed.
- `cargo test --workspace --locked --offline`: passed 36 unit/integration tests, 32 runnable doctests, and 1 compile-fail doctest.
- `make test`: passed 36 serial nextest cases and all 33 doctests.
- `cargo +nightly fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings -W clippy::pedantic`: passed.
- `cargo doc --workspace --no-deps --locked --offline`: passed.
- `cargo audit`: passed with no reported advisories.
- `cargo deny check`: passed; it reports unused license allowlist entries and duplicate `syn` major versions as warnings.
- `cargo tree -i ring --locked --offline`: found no active `ring` path in the host build graph; the CLI selects Rustls AWS-LC explicitly.
- `git diff --check`: passed.

Credentialed provider calls were intentionally not made, so upstream success behavior is covered by offline protocol fixtures rather than a live account.
