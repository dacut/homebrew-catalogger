//! Publishing to the site bucket and CloudFront.

use {
    anyhow::{Context, Result},
    aws_sdk_cloudfront::types::{InvalidationBatch, Paths},
    aws_sdk_s3::{
        primitives::ByteStream,
        types::{Delete, ObjectIdentifier},
    },
    futures_util::{StreamExt, TryStreamExt, stream},
    homebrew_catalog_static_pages::{IMMUTABLE_CACHE_CONTROL, PAGE_CACHE_CONTROL},
    std::{
        collections::HashSet,
        path::{Path, PathBuf},
    },
};

/// Stylesheets live under this prefix. Old ones are never deleted, since pages cached elsewhere may still use them.
const ASSETS_PREFIX: &str = "assets/";

/// How many uploads run at once.
const UPLOAD_CONCURRENCY: usize = 8;

/// A generated file to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteFile {
    pub key: String,
    pub path: PathBuf,
    pub content_type: &'static str,
    pub cache_control: &'static str,
}

impl SiteFile {
    fn is_asset(&self) -> bool {
        self.key.starts_with(ASSETS_PREFIX)
    }
}

/// Lists the generated site's files under `dir`, with their object keys, content types, and caching.
pub fn site_files(dir: &Path) -> Result<Vec<SiteFile>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).with_context(|| format!("reading {}", current.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path.strip_prefix(dir).expect("walked from dir");
            let key = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            let cache_control = if key.starts_with(ASSETS_PREFIX) {
                IMMUTABLE_CACHE_CONTROL
            } else {
                PAGE_CACHE_CONTROL
            };
            files.push(SiteFile {
                content_type: content_type(&key),
                cache_control,
                key,
                path,
            });
        }
    }
    files.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(files)
}

fn content_type(key: &str) -> &'static str {
    match key.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Uploads an object from memory.
pub async fn put_bytes(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    body: Vec<u8>,
    content_type: &str,
    cache_control: &str,
) -> Result<()> {
    s3.put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(body))
        .content_type(content_type)
        .cache_control(cache_control)
        .send()
        .await
        .with_context(|| format!("uploading s3://{bucket}/{key}"))?;
    Ok(())
}

/// Uploads the site: stylesheets first, so that no page ever references one that isn't there yet, then pages.
pub async fn upload_site(s3: &aws_sdk_s3::Client, bucket: &str, files: &[SiteFile]) -> Result<()> {
    let (assets, pages): (Vec<&SiteFile>, Vec<&SiteFile>) = files.iter().partition(|f| f.is_asset());
    for group in [assets, pages] {
        stream::iter(group)
            .map(|file| async move {
                let body = ByteStream::from_path(&file.path)
                    .await
                    .with_context(|| format!("reading {}", file.path.display()))?;
                s3.put_object()
                    .bucket(bucket)
                    .key(&file.key)
                    .body(body)
                    .content_type(file.content_type)
                    .cache_control(file.cache_control)
                    .send()
                    .await
                    .with_context(|| format!("uploading s3://{bucket}/{}", file.key))?;
                anyhow::Ok(())
            })
            .buffer_unordered(UPLOAD_CONCURRENCY)
            .try_collect::<()>()
            .await?;
    }
    Ok(())
}

/// The keys to delete after publishing: pages that the new site no longer has. Stylesheets and the catalog are kept.
pub fn stale_keys(
    existing: impl IntoIterator<Item = String>,
    published: &HashSet<&str>,
    catalog_key: &str,
) -> Vec<String> {
    existing
        .into_iter()
        .filter(|key| !key.starts_with(ASSETS_PREFIX) && key != catalog_key && !published.contains(key.as_str()))
        .collect()
}

/// Deletes pages the new site no longer has, returning how many were deleted.
pub async fn delete_stale_pages(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    files: &[SiteFile],
    catalog_key: &str,
) -> Result<usize> {
    let mut existing = Vec::new();
    let mut pages = s3.list_objects_v2().bucket(bucket).into_paginator().send();
    while let Some(page) = pages.next().await {
        let page = page.with_context(|| format!("listing s3://{bucket}"))?;
        existing.extend(page.contents().iter().filter_map(|o| o.key().map(str::to_string)));
    }

    let published: HashSet<&str> = files.iter().map(|f| f.key.as_str()).collect();
    let stale = stale_keys(existing, &published, catalog_key);
    for chunk in stale.chunks(1000) {
        let objects =
            chunk.iter().map(|key| ObjectIdentifier::builder().key(key).build()).collect::<Result<Vec<_>, _>>()?;
        let output = s3
            .delete_objects()
            .bucket(bucket)
            .delete(Delete::builder().set_objects(Some(objects)).quiet(true).build()?)
            .send()
            .await
            .with_context(|| format!("deleting stale pages from s3://{bucket}"))?;
        // DeleteObjects succeeds as a whole even when individual keys fail; those are listed in `Errors`.
        if let Some(failures) = deletion_failures(output.errors()) {
            anyhow::bail!("couldn't delete stale pages from s3://{bucket}: {failures}");
        }
    }
    Ok(stale.len())
}

/// Describes the per-key failures in a DeleteObjects response, if there were any.
fn deletion_failures(errors: &[aws_sdk_s3::types::Error]) -> Option<String> {
    const SHOWN: usize = 5;
    if errors.is_empty() {
        return None;
    }
    let mut described: Vec<String> = errors
        .iter()
        .take(SHOWN)
        .map(|e| {
            format!(
                "{} ({}: {})",
                e.key().unwrap_or("?"),
                e.code().unwrap_or("unknown error"),
                e.message().unwrap_or("no message")
            )
        })
        .collect();
    if errors.len() > SHOWN {
        described.push(format!("and {} more", errors.len() - SHOWN));
    }
    Some(described.join(", "))
}

/// Invalidates every cached path, returning the invalidation's ID.
pub async fn invalidate(
    cloudfront: &aws_sdk_cloudfront::Client,
    distribution_id: &str,
    reference: &str,
) -> Result<String> {
    let batch = InvalidationBatch::builder()
        .paths(Paths::builder().quantity(1).items("/*").build()?)
        .caller_reference(reference)
        .build()?;
    let output = cloudfront
        .create_invalidation()
        .distribution_id(distribution_id)
        .invalidation_batch(batch)
        .send()
        .await
        .with_context(|| format!("invalidating CloudFront distribution {distribution_id}"))?;
    Ok(output.invalidation().map(|i| i.id().to_string()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use {super::*, std::fs};

    #[test]
    fn lists_site_files_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("assets")).unwrap();
        fs::create_dir_all(dir.path().join("categories")).unwrap();
        fs::write(dir.path().join("index.html"), "").unwrap();
        fs::write(dir.path().join("assets/site-0123.css"), "").unwrap();
        fs::write(dir.path().join("categories/games.html"), "").unwrap();
        fs::write(dir.path().join("favicon.svg"), "").unwrap();
        fs::write(dir.path().join("robots.txt"), "").unwrap();
        fs::write(dir.path().join("sitemap.xml"), "").unwrap();

        let files = site_files(dir.path()).unwrap();
        let summary: Vec<_> = files.iter().map(|f| (f.key.as_str(), f.content_type, f.cache_control)).collect();
        assert_eq!(
            summary,
            [
                ("assets/site-0123.css", "text/css; charset=utf-8", IMMUTABLE_CACHE_CONTROL),
                ("categories/games.html", "text/html; charset=utf-8", PAGE_CACHE_CONTROL),
                ("favicon.svg", "image/svg+xml", PAGE_CACHE_CONTROL),
                ("index.html", "text/html; charset=utf-8", PAGE_CACHE_CONTROL),
                ("robots.txt", "text/plain; charset=utf-8", PAGE_CACHE_CONTROL),
                ("sitemap.xml", "application/xml", PAGE_CACHE_CONTROL),
            ]
        );
    }

    #[test]
    fn reports_per_key_deletion_failures() {
        assert_eq!(deletion_failures(&[]), None);

        let error = |key: &str| {
            aws_sdk_s3::types::Error::builder().key(key).code("AccessDenied").message("Access Denied").build()
        };
        assert_eq!(
            deletion_failures(&[error("categories/gone.html")]).as_deref(),
            Some("categories/gone.html (AccessDenied: Access Denied)")
        );
        let many: Vec<_> = (0..7).map(|i| error(&format!("p{i}.html"))).collect();
        let text = deletion_failures(&many).unwrap();
        assert!(text.contains("p4.html") && !text.contains("p5.html") && text.ends_with("and 2 more"), "{text}");
    }

    #[test]
    fn finds_stale_pages() {
        let existing =
            ["index.html", "categories/gone.html", "categories/games.html", "assets/old.css", "catalog.json"]
                .map(String::from);
        let published = HashSet::from(["index.html", "categories/games.html"]);
        assert_eq!(stale_keys(existing, &published, "catalog.json"), ["categories/gone.html"]);
    }
}
