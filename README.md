# jev

A command-line interface for OpenRouter's Jev decision model. Each command sends one state and one typed question to the Decisions API, validates the returned answer against that question, and writes JSON for scripts and agents.

Jev supports three question types:

- **Noul** asks whether a condition holds and returns a probability.
- **Choice** selects one submitted option and returns probabilities and confidence.
- **Score** places the state on an ordered scale and returns a weighted score, probabilities, and confidence.

A successful command means the API response contains a valid answer for the requested question. For Noul, that does not mean the answer is positive. Calls are billed to the OpenRouter account associated with `OPENROUTER_API_KEY`.

## Install and run

Export an OpenRouter API key, then run the CLI from this workspace:

```
export OPENROUTER_API_KEY="sk-or-..."
cargo run -p jev-cli -- --help
```

To install the executable locally, run `cargo install --path apps/cli`, then use `jev --help`.

The default model is `typesafe/jev-1.13`. Override it with `--model`; model IDs may contain ASCII letters, digits, `/`, `.`, `:`, `_`, `-`, or `~`, and may not contain whitespace or controls. This checks syntax, not model availability. The key is read only from `OPENROUTER_API_KEY` and is never accepted as a command-line argument.

## Questions

### Noul: yes or no

```
cargo run -q -p jev-cli -- noul \
  "Does this command only inspect files?" \
  --state "cargo test -p jev-core" \
  --true-criteria "The command reads files and runs local tests without changing project state." \
  --false-criteria "The command deletes, publishes, deploys, or changes files."
```

Noul criteria are optional. When provided, both `--true-criteria` and `--false-criteria` are required.

### Choice: select one option

```
cargo run -q -p jev-cli -- choice "Which team should handle this ticket?" \
  --state '{"ticket":"Checkout is charging my card twice."}' \
  --option billing="Billing, payments, invoices, or charges" \
  --option account="Login, access, or profile issues" \
  --option technical="Product defects or unexpected behavior" \
  --option none="Nothing else fits"
```

Each `--option` uses `ID=DESCRIPTION` syntax. IDs must be unique. Provide 2 to 64 options, and include a `none` option when no candidate should fit.

### Score: ordered scale

```
cargo run -q -p jev-cli -- score "How urgent is this incident?" \
  --state-file incident.json \
  --level "Can wait for the next release" \
  --level "Should be fixed this week" \
  --level "Blocking users now"
```

Provide 2 to 32 levels, from lowest to highest. The first level is index 0. Jev's numeric score is the probability-weighted position on this scale.

## State input

Pass state with `--state`, read it from `--state-file`, or pipe it on stdin. `--state-file -` explicitly reads stdin. `--state` is convenient for short content; shell and operating-system argument limits still apply.

All input sources share a 1 MiB byte limit. Reading a file or stdin avoids shell argument limits, but it does not raise Jev's limit. State and question text also share the model's context window; byte size is not a token count or a guarantee that a request fits.

`--state-format auto` is the default. It decodes valid JSON strings, objects, and arrays. Malformed JSON and JSON booleans, numbers, or null are treated as the original text. When input is interpreted as JSON in `auto` or `json` mode, duplicate object keys, excessive nesting (over 64 levels), and more than 100,000 JSON nodes (including the top-level value) are errors. `text` mode preserves input as text and applies the shared byte limit without parsing it as JSON.

Use `--state-format text` to preserve input as text, including surrounding whitespace. Use `--state-format json` to require valid JSON containing a nonempty string, object, or array; strict mode rejects scalar booleans, numbers, and null. Empty and whitespace-only input is always rejected.

```
cat ticket.json | cargo run -q -p jev-cli -- choice "Which team owns this?" \
  --option billing="Billing issues" \
  --option technical="Product issues"
```

File and stdin reads have a 30-second deadline by default. The timeout applies to opening and reading files, pipes, and explicit `--state-file -` input. A timed-out read ends the process; its background reader is abandoned.

## Output and errors

Stdout contains one compact JSON value followed by a newline. The default output is the supported Decisions response envelope; additive fields from the API are retained. Use `--output pretty` for indented JSON or `--answers-only` to emit only the keyed `answers` object. The four combinations remain valid JSON. A closed downstream pipe is treated as successful termination.

```
jev noul "Is this a bug?" --state "The save button does nothing" --answers-only | jq '.answer.noul.noul'
```

To make a Noul threshold determine shell status, use `jq -e` explicitly:

```
jev noul "Is this a bug?" --state "The save button does nothing" --answers-only \
  | jq -e '.answer.noul.noul >= 0.8'
```

Invocation, configuration, and local input errors return status 2. Network, timeout, protocol, and other output failures return status 1. Help and version return status 0. Diagnostics go to stderr, do not include API keys or raw response bodies, and escape control characters.

The CLI validates answer type, requested ID, selected Choice ID, numeric ranges, and probability keys before writing any success JSON. It does not require probabilities to sum exactly to one, because provider values are rounded. The returned `model` field identifies the provider's resolved model; pinning a version ID improves reproducibility but does not promise that the provider will always resolve it identically.

## Preview and deadlines

`--dry-run` validates the command and state, then prints the exact request JSON without requiring credentials or making a network call. The preview contains user-provided state; handle its output accordingly.

Deadlines are separate: `--input-timeout` defaults to 30 seconds and accepts 1 to 300; `--connect-timeout` defaults to 15 seconds and accepts 1 to 120; `--request-timeout` defaults to 90 seconds and accepts 1 to 600. Responses larger than 1 MiB are rejected while they are read. Requests are not retried automatically.

For stable policy thresholds, choose a supported versioned model ID and set an explicit threshold for the cost of a wrong decision. Jev returns decisions rather than explanations or free-form text.

## License

This project is distributed under the terms of the MIT License. See [LICENSE.md](LICENSE.md).
