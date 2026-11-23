//! Classify Homebrew formulae into catalog categories using Claude.
//!
//! The crate has two halves:
//!
//! * **Metadata loading.** [`HomebrewFormulaMeta`] models a formula's JSON metadata as served by
//!   `formulae.brew.sh` and cached by Homebrew. [`load_formulae`] and [`find_formula`] read it from a given
//!   location; [`load_default_formulae`] and [`find_default_formula`] read it from Homebrew's cache for this
//!   platform ([`default_api_cache_dir`]).
//! * **Classification.** A [`Classifier`] sends a formula's metadata to the Anthropic Messages API and returns a
//!   [`Classification`] listing its categories, most relevant first. Categories come from a [`Taxonomy`]: the
//!   built-in [`default_categories`], a custom list, or [`Taxonomy::Open`] to let the model name them.
//!
//! Credentials are read from `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`) unless passed to the builder.
//!
//! # Example
//!
//! ```no_run
//! use homebrew_catalog_classifier::{Classifier, HomebrewFormulaMeta};
//!
//! # async fn run() -> homebrew_catalog_classifier::Result<()> {
//! let classifier = Classifier::from_env()?;
//!
//! // From a struct built in code.
//! let formula = HomebrewFormulaMeta {
//!     name: "ripgrep".into(),
//!     desc: Some("Search tool like grep and The Silver Searcher".into()),
//!     ..Default::default()
//! };
//! println!("{:?}", classifier.classify(&formula).await?.categories);
//!
//! // From Homebrew's local metadata cache.
//! let jq = classifier.classify_named_formula("jq").await?;
//! println!("{}: {}", jq.formula, jq.categories.join(", "));
//!
//! // Everything in a directory of metadata files.
//! for (name, result) in classifier.classify_formulae_at("/path/to/api").await? {
//!     match result {
//!         Ok(c) => println!("{name}: {}", c.categories.join(", ")),
//!         Err(e) => eprintln!("{name}: {e}"),
//!     }
//! }
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "bedrock")]
mod bedrock;
pub mod catalog;
mod classifier;
mod error;
mod formula;
mod loader;
mod taxonomy;

#[cfg(feature = "bedrock")]
pub use bedrock::{BEARER_TOKEN_ENV as BEDROCK_BEARER_TOKEN_ENV, Bedrock, default_geography, inference_profile_id};
pub use {
    classifier::{Auth, Classification, Classifier, ClassifierBuilder, DEFAULT_MODEL, Effort, Platform},
    error::{Error, Result},
    formula::{FormulaVersions, HomebrewFormulaMeta, UsesFromMacos},
    loader::{
        default_api_cache_dir, default_homebrew_cache_dir, find_default_formula, find_formula, load_default_formulae,
        load_formulae, read_formula_file,
    },
    taxonomy::{Category, Taxonomy, default_categories, title_from_name},
};
