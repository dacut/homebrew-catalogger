//! The function's settings, read from environment variables set by Terraform.

use {
    anyhow::{Context, Result, anyhow},
    homebrew_catalog_classifier::DEFAULT_MODEL,
    homebrew_catalog_static_pages::{BaseUrl, DEFAULT_TITLE},
    std::time::Duration,
};

/// Where Homebrew publishes the metadata for every formula in homebrew/core.
pub const DEFAULT_FORMULA_URL: &str = "https://formulae.brew.sh/api/formula.json";

/// The catalog's object key in the site bucket.
pub const DEFAULT_CATALOG_KEY: &str = "formula-catalogged.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The site bucket (`SITE_BUCKET`, required).
    pub bucket: String,
    /// The CloudFront distribution serving the site (`DISTRIBUTION_ID`, required).
    pub distribution_id: String,
    /// The catalog's key in the bucket (`CATALOG_KEY`).
    pub catalog_key: String,
    /// Where to download formula.json from (`FORMULA_URL`).
    pub formula_url: String,
    /// The Claude model on Bedrock (`BEDROCK_MODEL`).
    pub model: String,
    /// The cross-Region inference geography, if not derived from the region (`BEDROCK_GEOGRAPHY`).
    pub geography: Option<String>,
    /// How many classification requests to run at once (`CLASSIFY_CONCURRENCY`).
    pub concurrency: usize,
    /// The site title (`SITE_TITLE`).
    pub site_title: String,
    /// The site's absolute URL, for the sitemap and canonical links (`SITE_URL`).
    pub site_url: Option<BaseUrl>,
    /// Time kept back from classification for publishing the results (`PUBLISH_RESERVE_SECONDS`).
    pub publish_reserve: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| lookup(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let required = |key: &str| get(key).ok_or_else(|| anyhow!("environment variable {key} is not set"));
        let number = |key: &str, default: u64| -> Result<u64> {
            get(key).map_or(Ok(default), |v| v.parse().with_context(|| format!("{key} must be a whole number")))
        };

        Ok(Self {
            bucket: required("SITE_BUCKET")?,
            distribution_id: required("DISTRIBUTION_ID")?,
            catalog_key: get("CATALOG_KEY").unwrap_or_else(|| DEFAULT_CATALOG_KEY.to_string()),
            formula_url: get("FORMULA_URL").unwrap_or_else(|| DEFAULT_FORMULA_URL.to_string()),
            model: get("BEDROCK_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            geography: get("BEDROCK_GEOGRAPHY"),
            concurrency: number("CLASSIFY_CONCURRENCY", 8)?.max(1) as usize,
            site_title: get("SITE_TITLE").unwrap_or_else(|| DEFAULT_TITLE.to_string()),
            site_url: get("SITE_URL").map(|url| BaseUrl::parse(&url).context("SITE_URL is invalid")).transpose()?,
            publish_reserve: Duration::from_secs(number("PUBLISH_RESERVE_SECONDS", 180)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::collections::HashMap};

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let vars: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|key| vars.get(key).cloned())
    }

    #[test]
    fn defaults() {
        let c = config(&[("SITE_BUCKET", "b"), ("DISTRIBUTION_ID", "E123"), ("BEDROCK_GEOGRAPHY", " ")]).unwrap();
        assert_eq!(c.bucket, "b");
        assert_eq!(c.catalog_key, "formula-catalogged.json");
        assert_eq!(c.formula_url, DEFAULT_FORMULA_URL);
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.geography, None, "blank values count as unset");
        assert_eq!(c.concurrency, 8);
        assert_eq!(c.publish_reserve, Duration::from_secs(180));
        assert_eq!(c.site_url, None);
    }

    #[test]
    fn overrides_and_errors() {
        let c = config(&[
            ("SITE_BUCKET", "b"),
            ("DISTRIBUTION_ID", "E123"),
            ("BEDROCK_MODEL", "claude-sonnet-5"),
            ("BEDROCK_GEOGRAPHY", "global"),
            ("CLASSIFY_CONCURRENCY", "0"),
            ("PUBLISH_RESERVE_SECONDS", "60"),
            ("SITE_URL", "https://brew.example.com"),
        ])
        .unwrap();
        assert_eq!(c.site_url.unwrap().as_str(), "https://brew.example.com/");
        assert_eq!(c.model, "claude-sonnet-5");
        assert_eq!(c.geography.as_deref(), Some("global"));
        assert_eq!(c.concurrency, 1, "concurrency is at least 1");
        assert_eq!(c.publish_reserve, Duration::from_secs(60));

        assert!(config(&[("SITE_BUCKET", "b")]).unwrap_err().to_string().contains("DISTRIBUTION_ID"));
        let err = config(&[("SITE_BUCKET", "b"), ("DISTRIBUTION_ID", "E"), ("CLASSIFY_CONCURRENCY", "x")]);
        assert!(err.unwrap_err().to_string().contains("CLASSIFY_CONCURRENCY"));
        let err = config(&[("SITE_BUCKET", "b"), ("DISTRIBUTION_ID", "E"), ("SITE_URL", "brew.example.com")]);
        assert!(err.unwrap_err().to_string().contains("SITE_URL"));
    }
}
