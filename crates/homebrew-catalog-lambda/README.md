# homebrew-catalog-lambda

An AWS Lambda function that keeps the catalog website current. [`terraform/`](../../terraform) deploys it on a daily
schedule when `enable_automation = true`.

## What a run does

1. Downloads the current catalog (`formula-catalogged.json`) from the root of the site bucket. If there isn't one,
   every formula is classified.
2. Downloads the latest `formula.json` from Homebrew.
3. Reuses the catalog's categories for formulae it already covers, and classifies the rest with Claude in Amazon
   Bedrock.
4. Uploads the new catalog to the bucket.
5. Regenerates the website, uploads it (stylesheets first, then pages), and deletes pages that no longer exist. Old
   stylesheets are kept, since cached pages may still use them.
6. Invalidates the CloudFront cache.

The invocation event is ignored. A run returns a JSON summary (or `{"skipped": "..."}` if manual publishing was in
progress):

```json
{
  "formulae": 8735, "reused": 8731, "classified": 4, "failed": 0, "pending": 0,
  "pages": 52, "stale_pages_deleted": 0, "invalidation_id": "I2J0..."
}
```

## Safeguards

- **Time budget.** Classification stops early enough to leave time for publishing (`PUBLISH_RESERVE_SECONDS`, three
  minutes by default, within Lambda's 15-minute limit). Formulae it didn't get to are published without categories,
  under "Uncategorized", and the next run classifies them. A first run with no catalog therefore takes several runs to
  finish; upload an existing catalog first to avoid that (`scripts/publish.sh` does).
- **Bad downloads.** The run stops without publishing if the downloaded formula list is empty, or less than half the
  size of the current catalog.
- **Failures.** A formula that fails to classify is retried on the next run. If every attempt in a run fails, which
  points to a systemic problem such as permissions or model access, the site is still published but the invocation
  is reported as failed, so it shows up in Lambda's error metrics.
- **No immediate retries.** Terraform turns off Lambda's automatic retries; the next scheduled run picks up where the
  last left off.
- **One run at a time.** Each run rewrites the catalog and the site, so Terraform reserves a concurrency of 1: an
  invocation that arrives during a run waits until it finishes. If your account's concurrency limit is too low to
  reserve any, set `automation_prevent_overlapping_runs = false` and keep runs well apart.
- **Manual publishing.** The function and [`scripts/publish.sh`](../../scripts/publish.sh) share a lock: an object,
  `.publish.lock`, in the site bucket, created with a conditional write so only one holder can have it. A run that
  finds `publish.sh` holding it is skipped (the next scheduled run catches up). Locks expire (20 minutes for the
  function, 30 for `publish.sh`), so one left by a crash is taken over rather than blocking updates. Once it holds the
  lock, `publish.sh` also refuses to replace a bucket catalog that's newer than the one being published.

## Configuration

Environment variables, which Terraform sets:

| Variable | Default | |
| --- | --- | --- |
| `SITE_BUCKET` | (required) | The site's S3 bucket. |
| `DISTRIBUTION_ID` | (required) | The CloudFront distribution to invalidate. |
| `SITE_URL` | | The site's absolute URL, for `sitemap.xml` and canonical links. |
| `CATALOG_KEY` | `formula-catalogged.json` | The catalog's key in the bucket. |
| `FORMULA_URL` | `https://formulae.brew.sh/api/formula.json` | Where to download formula metadata. |
| `BEDROCK_MODEL` | `claude-haiku-5-5` | A Claude model name, or a full inference profile ID. |
| `BEDROCK_GEOGRAPHY` | from the region | Cross-Region inference geography for a plain model name (`us`, `eu`, `apac`, `global`, ...). |
| `CLASSIFY_CONCURRENCY` | 8 | Bedrock requests in flight at once. |
| `SITE_TITLE` | `Homebrew Formula Catalog` | The site title. |
| `PUBLISH_RESERVE_SECONDS` | 180 | Time kept back from classification for publishing. |

Bedrock is called in the function's own region with its execution role's credentials. The role needs:

- `s3:ListBucket` on the bucket, and `s3:GetObject`, `s3:PutObject` and `s3:DeleteObject` on its objects;
- `cloudfront:CreateInvalidation` on the distribution;
- `bedrock:InvokeModel` on the model's inference profiles and foundation model.

Terraform's role grants exactly these, plus writing to the function's log group.

Logs go to CloudWatch at `INFO` level: what the run planned, each formula classified with its categories, each
failure, and what was published. Set `RUST_LOG` to change the level, preferably for one component at a time (for
example `info,aws_smithy_runtime=debug`), since `debug` for everything is very noisy.

## Building

```sh
scripts/build-lambda.sh
```

This builds `target/lambda/homebrew-catalog-lambda/bootstrap.zip` for the `provided.al2023` runtime on Arm64, using
[cargo-lambda](https://www.cargo-lambda.info/) with [cross](https://github.com/cross-rs/cross). It needs Docker (or
Podman) running. Rebuild and `terraform apply` after changing this crate or the libraries it uses.

## Running it by hand

```sh
aws lambda invoke --invocation-type Event \
    --function-name "$(terraform -chdir=terraform output -raw update_function_name)" /dev/null
```

`--invocation-type Event` returns immediately; the run itself can take up to 15 minutes. Follow it in the function's
CloudWatch log group, `/aws/lambda/<function name>`.
