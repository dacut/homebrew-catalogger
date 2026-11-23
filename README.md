# homebrew-catalogger

Sorts every [Homebrew](https://brew.sh/) formula into browsable categories using Claude, and publishes the result as a
static website.

Homebrew lists more than 8,000 formulae in `homebrew/core`, but has no way to browse them by what they do. This
project downloads Homebrew's formula metadata, asks Claude (through the Claude API or Amazon Bedrock) to assign each
formula up to three categories from a fixed list of 41 — "Version Control", "AI & Machine Learning", "Databases", and
so on — and generates a fast, accessible static site with a page per category. Terraform deploys the site to S3 behind
CloudFront and can schedule a Lambda function that keeps it current, classifying only formulae that are new since the
last run.

Categories are assigned automatically and can be wrong; the site says so.

## How it works

```text
formulae.brew.sh/api/formula.json
        │
        ▼
homebrew-catalog-classifier      classifies with Claude (Claude API or Amazon Bedrock)
        │
        ▼
formula-catalogged.json          formula.json with a "categories" field added to each formula
        │
        ▼
homebrew-catalog-static-pages
        │
        ▼
site/                            HTML, CSS, icons, robots.txt, sitemap.xml, llms.txt
        │
        ▼
S3 + CloudFront                  terraform/; optionally kept current by homebrew-catalog-lambda
```

Each run reuses the categories from the previous catalog, so after the first full classification an update sends only
new formulae to the model.

## Repository layout

| Path | What it is |
| --- | --- |
| [`crates/homebrew-catalog-classifier`](crates/homebrew-catalog-classifier) | Library: loads Homebrew formula metadata and classifies formulae with Claude (Claude API or Bedrock). |
| [`crates/homebrew-catalog-classifier-cli`](crates/homebrew-catalog-classifier-cli) | `homebrew-catalog-classifier` command: turns `formula.json` into a classified catalog. |
| [`crates/homebrew-catalog-static-pages`](crates/homebrew-catalog-static-pages) | `homebrew-catalog-static-pages` command and library: generates the website from a catalog. |
| [`crates/homebrew-catalog-lambda`](crates/homebrew-catalog-lambda) | AWS Lambda function that updates the catalog and site on a schedule. |
| [`terraform/`](terraform) | S3 bucket, CloudFront distribution, ACM certificate, Route 53 records, and the optional update function. |
| [`scripts/`](scripts) | `publish.sh` (generate and upload the site), `build-lambda.sh`, `render-icons.sh`. |

## Requirements

- **Rust 1.94 or newer** (stable is fine).
- **Claude access**, either:
  - an Anthropic API key (`ANTHROPIC_API_KEY`), or
  - Amazon Bedrock access to a Claude model (default: Claude Haiku 5.5), with AWS credentials or a Bedrock API key.
- **To host the site:** an AWS account with a Route 53 public hosted zone, [Terraform](https://developer.hashicorp.com/terraform) 1.5 or newer, and the AWS CLI.
- **To build the update function:** [cargo-lambda](https://www.cargo-lambda.info/), [cross](https://github.com/cross-rs/cross), and a running Docker (or Podman).

## Quick start

Classify a few formulae and look at the result locally:

```sh
curl -sSLo formula.json https://formulae.brew.sh/api/formula.json
export ANTHROPIC_API_KEY=...          # or put it in a .env file

cargo run --release -p homebrew-catalog-classifier-cli -- \
    formula.json -f jq -f ripgrep -f wget -o formula-catalogged.json

cargo run --release -p homebrew-catalog-static-pages -- formula-catalogged.json -o site
open site/index.html                  # the site uses relative links, so it works from disk
```

To classify the whole catalog, drop the `-f` options. Each formula classified is one API request. To use Bedrock
instead of the Claude API, add `--bedrock --aws-region us-east-1` (see the
[CLI's README](crates/homebrew-catalog-classifier-cli/README.md)).

### Updating an existing catalog

Download a fresh `formula.json` and reuse the previous catalog's categories, so only new formulae are classified:

```sh
curl -sSLo formula.json https://formulae.brew.sh/api/formula.json
cargo run --release -p homebrew-catalog-classifier-cli -- \
    formula.json --reuse formula-catalogged.json -o formula-catalogged.json
```

## Deploying

1. Create the infrastructure:

   ```sh
   cd terraform
   cp terraform.tfvars.example terraform.tfvars   # set domain_name and hosted_zone_name
   terraform init
   terraform apply
   ```

2. Publish the site. `scripts/publish.sh` rebuilds the generator, regenerates `./site` from `formula-catalogged.json`,
   uploads the stylesheet, catalog, and pages to the bucket named in the Terraform outputs, and invalidates the
   CloudFront cache. It asks before publishing; `--dry-run` shows what would change.

   ```sh
   scripts/publish.sh
   ```

### Automatic updates

Set `enable_automation = true` in `terraform.tfvars` to deploy a Lambda function that runs daily: it downloads the
current catalog from the bucket and the latest `formula.json`, classifies new formulae with Bedrock, and republishes
the site.

```sh
scripts/build-lambda.sh           # builds target/lambda/homebrew-catalog-lambda/bootstrap.zip
scripts/publish.sh                # uploads your catalog, so the first run only classifies new formulae
cd terraform && terraform apply
```

Once it's running, the bucket's catalog (at `/formula-catalogged.json` on the site) is the current one. Manual
publishing and the function share a lock, so they never write at the same time, and `publish.sh` refuses to replace a
bucket catalog that's newer than yours (it tells you how to download it; `--force` overrides). See the
[Lambda's README](crates/homebrew-catalog-lambda/README.md) for details.

## Development

```sh
cargo test --workspace                        # all crates
cargo clippy --workspace --all-targets
cargo +nightly fmt --all                      # rustfmt.toml uses nightly-only options
(cd terraform && terraform init -backend=false && terraform test)   # offline, with a mocked AWS provider
```

After changing the site icon (`crates/homebrew-catalog-static-pages/assets/favicon.svg`), run
`scripts/render-icons.sh` to re-render the bitmap versions; it needs Google Chrome and Python 3.

## License

[MIT](LICENSE). Formula metadata comes from Homebrew's public API.
