# homebrew-catalog-classifier (command)

Classifies Homebrew formulae into catalog categories using Claude, and writes Homebrew's `formula.json` back out with
a `categories` field added to each formula. The output feeds
[`homebrew-catalog-static-pages`](../homebrew-catalog-static-pages), which turns it into a website.

This crate builds the `homebrew-catalog-classifier` command; the work is done by the
[`homebrew-catalog-classifier`](../homebrew-catalog-classifier) library.

## Install

From the repository root:

```sh
cargo install --path crates/homebrew-catalog-classifier-cli
```

or run it in place with `cargo run --release -p homebrew-catalog-classifier-cli -- ...`.

## Usage

```sh
curl -sSLo formula.json https://formulae.brew.sh/api/formula.json

homebrew-catalog-classifier formula.json -o formula-catalogged.json          # every formula
homebrew-catalog-classifier formula.json -f jq -f ripgrep                    # just these, to stdout
homebrew-catalog-classifier formula.json -n 20 -o sample.json                # the first 20, for a trial run
```

The input can be `formula.json`, a single formula's JSON file, or Homebrew's API cache directory (`brew --cache`).
Progress and per-formula errors go to standard error.

### Updating an existing catalog

Pass the previous output to `--reuse`. Formulae classified there keep their categories, and only new formulae (or ones
that failed last time) are sent to the model. Metadata always comes from the input, so the output picks up Homebrew's
latest descriptions, versions, and so on:

```sh
homebrew-catalog-classifier formula.json --reuse formula-catalogged.json -o formula-catalogged.json
```

Writing over the `--reuse` file is safe: the output goes to a temporary file that replaces the target only once
everything has been written. Add `--reclassify NAME` (repeatable) to classify particular formulae again anyway.

## Credentials

- **Claude API** (the default): `ANTHROPIC_API_KEY`, or `ANTHROPIC_AUTH_TOKEN` for an OAuth token.
- **Amazon Bedrock** (`--bedrock`): a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK` if set; otherwise AWS credentials
  from the usual places (environment, `--aws-profile` or `AWS_PROFILE`, SSO, instance roles). The identity needs
  `bedrock:InvokeModel`.

Variables can also come from a `.env` file in the current directory or a parent (or `--env-file FILE`). Variables
already set in the environment take precedence. Keys are deliberately not accepted as command-line options, so they
don't end up in shell history.

### Bedrock

```sh
homebrew-catalog-classifier formula.json --bedrock --aws-region us-east-1 -o formula-catalogged.json
```

A plain model name (the default is `claude-haiku-5-5`) is invoked through the cross-Region inference profile for the
region's geography, e.g. `us.anthropic.claude-haiku-5-5`. Use `--bedrock-geography` (`us`, `eu`, `apac`, `global`, ...)
to pick another, or pass a full inference profile ID with `--model`. Credential and region problems are reported once,
before any requests.

## Options

| Option | Default | |
| --- | --- | --- |
| `-o, --output FILE` | stdout | Where to write the catalog. |
| `-f, --formula NAME` | all | Classify only these formulae (by name, alias, or former name). Repeatable. |
| `-n, --limit N` | all | Classify at most N formulae. |
| `--reuse FILE` | | Reuse categories from a previous catalog. |
| `--reclassify NAME` | | With `--reuse`, classify this formula again anyway. Repeatable. |
| `-m, --model MODEL` | `claude-haiku-5-5` | The Claude model. |
| `--effort LEVEL` | `low` | `low`, `medium`, `high`, `xhigh`, or `max`. |
| `--max-categories N` | 3 | Most categories per formula. |
| `-j, --concurrency N` | 8 | Requests in flight at once. |
| `--categories FILE` | built-in list | Categories as `[{"name": "...", "description": "..."}, ...]`. |
| `--open` | | Let the model name categories instead of using a fixed list. |
| `--compact` | | Compact JSON instead of pretty-printed. |
| `-q, --quiet` | | No progress or per-formula errors on stderr. |
| `--env-file FILE` | nearest `.env` | Load environment variables from this file. |
| `--bedrock` | | Use Amazon Bedrock instead of the Claude API. |
| `--aws-region REGION` | `AWS_REGION` | With `--bedrock`. |
| `--aws-profile PROFILE` | `AWS_PROFILE` | With `--bedrock`. |
| `--bedrock-geography GEO` | from the region | With `--bedrock`. |

`homebrew-catalog-classifier --help` describes each option in full.

## Output

A JSON array in the same format as `formula.json`, with every original field kept and a `categories` field added,
most relevant first:

```json
[
  {
    "name": "jq",
    "desc": "Lightweight and flexible command-line JSON processor",
    "...": "...",
    "categories": ["data-processing", "text-processing"]
  }
]
```

A formula that couldn't be classified has `"categories": null` and a `classification_error` explaining why; the next
`--reuse` run tries it again.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Every formula was classified (or reused). |
| 1 | A fatal error, such as missing credentials or an unreadable input; nothing was written. |
| 2 | Some formulae couldn't be classified. The output is still written, with errors recorded for those formulae. |
