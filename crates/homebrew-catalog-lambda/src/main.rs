//! AWS Lambda function that keeps the Homebrew formula catalog website up to date. Each invocation:
//!
//! 1. downloads the current catalog (`formula-catalogged.json`) from the site bucket, if there is one;
//! 2. downloads the latest `formula.json` from Homebrew;
//! 3. classifies formulae the catalog doesn't cover yet, using Claude in Amazon Bedrock, until its time budget is
//!    spent (anything left over is classified by the next run);
//! 4. uploads the new catalog to the bucket;
//! 5. regenerates the website and uploads it, deleting pages that no longer exist; and
//! 6. invalidates the CloudFront distribution's cache.
//!
//! The invocation event is ignored. Settings come from environment variables; see [`config::Config`].

mod config;
mod lock;
mod publish;
mod update;

use {
    anyhow::{Context as _, Result, bail},
    aws_config::BehaviorVersion,
    config::Config,
    homebrew_catalog_classifier::{
        Bedrock, Classifier, catalog::PreviousCategories, default_categories, load_formulae,
    },
    homebrew_catalog_static_pages::{PAGE_CACHE_CONTROL, Site, SiteOptions, Timestamp, write_site},
    lambda_runtime::{
        Context, LambdaEvent, service_fn,
        tracing::{info, warn},
    },
    serde::Serialize,
    serde_json::Value,
    std::{
        path::{Path, PathBuf},
        sync::Arc,
        time::SystemTime,
    },
    tokio::time::Instant,
    update::UpdateStats,
};

/// Scratch space. Lambda keeps `/tmp` between invocations of the same instance, so it is cleared on each run.
const WORK_DIR: &str = "/tmp/homebrew-catalog";

/// Everything that lives as long as the Lambda instance.
struct State {
    config: Config,
    s3: aws_sdk_s3::Client,
    cloudfront: aws_sdk_cloudfront::Client,
    http: reqwest::Client,
    classifier: Classifier,
}

/// The invocation's result.
/// How long the update holds the publishing lock: longer than the function's 15-minute timeout, so a lock outlives
/// any run that holds it, but not by much, so one left by a crash soon expires.
const LOCK_TTL: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// The invocation's result: what was published, or why nothing was.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Outcome {
    Published(Summary),
    Skipped {
        skipped: String,
    },
}

#[derive(Debug, Serialize)]
struct Summary {
    #[serde(flatten)]
    stats: UpdateStats,
    pages: usize,
    stale_pages_deleted: usize,
    invalidation_id: String,
}

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    lambda_runtime::tracing::init_default_subscriber();

    let config = Config::from_env().map_err(|e| format!("{e:#}"))?;
    let aws = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let classifier = Classifier::builder()
        .bedrock(Bedrock {
            // The region and credentials come from the Lambda environment.
            geography: config.geography.clone(),
            ..Bedrock::default()
        })
        .model(&config.model)
        .concurrency(config.concurrency)
        .build()?;
    let state = Arc::new(State {
        s3: aws_sdk_s3::Client::new(&aws),
        cloudfront: aws_sdk_cloudfront::Client::new(&aws),
        http: reqwest::Client::builder()
            .user_agent(concat!("homebrew-catalogger/", env!("CARGO_PKG_VERSION")))
            .build()?,
        classifier,
        config,
    });

    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let state = Arc::clone(&state);
        async move { handle_locked(&state, &event.context).await.map_err(|e| format!("{e:#}")) }
    }))
    .await
}

/// Runs the update while holding the publishing lock shared with `scripts/publish.sh`. If manual publishing holds it,
/// this run is skipped (the next scheduled run catches up) rather than reported as a failure.
async fn handle_locked(state: &State, context: &Context) -> Result<Outcome> {
    let holder = format!("update function, request {}", context.request_id);
    let lock = match lock::acquire(&state.s3, &state.config.bucket, &holder, LOCK_TTL).await? {
        lock::Acquire::Acquired(lock) => lock,
        lock::Acquire::Held(held) => {
            warn!(holder = %held.holder, expires = held.expires, "the site is being published by someone else; skipping");
            return Ok(Outcome::Skipped {
                skipped: format!("the site is being published by {}", held.holder),
            });
        }
    };

    let result = handle(state, context).await;
    // A lock that can't be released expires on its own, so this doesn't fail the run.
    if let Err(e) = lock::release(&state.s3, &state.config.bucket, &lock).await {
        warn!(error = %format!("{e:#}"), "couldn't release the publishing lock; it will expire");
    }
    result.map(Outcome::Published)
}

async fn handle(state: &State, context: &Context) -> Result<Summary> {
    let config = &state.config;
    let work = Path::new(WORK_DIR);
    if work.exists() {
        tokio::fs::remove_dir_all(work).await.context("clearing the scratch directory")?;
    }
    tokio::fs::create_dir_all(work).await.context("creating the scratch directory")?;

    // Classification stops early enough to leave `publish_reserve` for the remaining steps.
    let remaining = context.deadline().duration_since(SystemTime::now()).unwrap_or_default();
    let classify_deadline = Instant::now() + remaining.saturating_sub(config.publish_reserve);

    // 1. The current catalog.
    let previous = match download_catalog(state, &work.join("previous.json")).await? {
        Some(path) => {
            let previous = PreviousCategories::load(&path).context("reading the current catalog")?;
            info!(formulae = previous.catalog_size(), classified = previous.len(), "loaded the current catalog");
            Some(previous)
        }
        None => {
            warn!(key = %config.catalog_key, "no current catalog in the bucket; every formula will be classified");
            None
        }
    };

    // 2. The latest formula metadata.
    let formula_path = work.join("formula.json");
    download_formulae(state, &formula_path).await?;
    let formulae = load_formulae(&formula_path).context("reading the downloaded formula.json")?;
    // Compare against every entry in the current catalog, not just the classified ones, so a catalog with many
    // pending formulae doesn't weaken the guard.
    update::check_formula_count(formulae.len(), previous.as_ref().map_or(0, PreviousCategories::catalog_size))?;
    info!(formulae = formulae.len(), "downloaded the latest formula metadata");

    // 3. Classify what's new.
    let (catalog, stats) =
        update::update_catalog(&state.classifier, &formulae, previous.as_ref(), classify_deadline).await?;
    info!(?stats, "updated the catalog");
    drop(formulae);

    // 4. Upload the new catalog.
    let catalog_json = serde_json::to_vec(&catalog).context("serializing the catalog")?;
    publish::put_bytes(
        &state.s3,
        &config.bucket,
        &config.catalog_key,
        catalog_json,
        "application/json",
        PAGE_CACHE_CONTROL,
    )
    .await?;
    info!(key = %config.catalog_key, "uploaded the catalog");

    // 5. Regenerate and upload the site.
    let catalog_formulae = catalog
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<_>, _>>()
        .context("re-reading the catalog")?;
    let site_dir = work.join("site");
    let site = Site::build(&catalog_formulae, &default_categories());
    let options = SiteOptions {
        title: config.site_title.clone(),
        base_url: config.site_url.clone(),
        generated: Timestamp::generation_time()?,
        // The catalog was just uploaded to this key at the bucket root.
        catalog_path: Some(config.catalog_key.clone()),
    };
    write_site(&site, &options, &site_dir)?;
    let files = publish::site_files(&site_dir)?;
    publish::upload_site(&state.s3, &config.bucket, &files).await?;
    let stale_pages_deleted =
        publish::delete_stale_pages(&state.s3, &config.bucket, &files, &config.catalog_key).await?;
    info!(files = files.len(), stale_pages_deleted, "uploaded the site");

    // 6. Invalidate the CDN cache.
    let invalidation_id = publish::invalidate(&state.cloudfront, &config.distribution_id, &context.request_id).await?;
    info!(%invalidation_id, "invalidated the CloudFront cache");

    // The site is published either way, but if every attempt failed something systemic is wrong (permissions, model
    // access, quotas): report the invocation as failed so it shows up in metrics and alarms.
    if stats.failed > 0 && stats.classified == 0 {
        bail!("all {} classification attempts failed; see the log for the errors", stats.failed);
    }

    Ok(Summary {
        stats,
        pages: files.len(),
        stale_pages_deleted,
        invalidation_id,
    })
}

/// Downloads the current catalog to `path`, returning `None` if the bucket doesn't have one yet.
async fn download_catalog(state: &State, path: &Path) -> Result<Option<PathBuf>> {
    let (bucket, key) = (&state.config.bucket, &state.config.catalog_key);
    let object = match state.s3.get_object().bucket(bucket).key(key).send().await {
        Ok(object) => object,
        Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("downloading s3://{bucket}/{key}")),
    };
    let bytes = object.body.collect().await.with_context(|| format!("downloading s3://{bucket}/{key}"))?.into_bytes();
    tokio::fs::write(path, &bytes).await.with_context(|| format!("writing {}", path.display()))?;
    Ok(Some(path.to_path_buf()))
}

/// Downloads the latest formula.json to `path`.
async fn download_formulae(state: &State, path: &Path) -> Result<()> {
    let url = &state.config.formula_url;
    let bytes = state
        .http
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("downloading {url}"))?
        .bytes()
        .await
        .with_context(|| format!("downloading {url}"))?;
    tokio::fs::write(path, &bytes).await.with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
