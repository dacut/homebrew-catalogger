//! Classifies Homebrew formulae from the local metadata cache.
//!
//! ```text
//! cargo run --example classify -- jq ripgrep      # classify named formulae
//! cargo run --example classify -- --all [PATH]    # classify every formula at PATH (default: Homebrew's cache)
//! ```
//!
//! Requires `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`). Each formula is one API request.

use {
    homebrew_catalog_classifier::{Classifier, Result},
    std::process::ExitCode,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let classifier = Classifier::from_env()?;

    let results = match args.first().map(String::as_str) {
        Some("--all") => match args.get(1) {
            Some(path) => classifier.classify_formulae_at(path).await?,
            None => classifier.classify_default_formulae().await?,
        },
        Some(_) => {
            let mut results = Vec::new();
            for name in &args {
                results.push((name.clone(), classifier.classify_named_formula(name).await));
            }
            results
        }
        None => {
            eprintln!("usage: classify <formula>... | --all [PATH]");
            return Ok(false);
        }
    };

    let mut ok = true;
    for (name, result) in results {
        match result {
            Ok(c) => println!("{name}: {} ({})", c.categories.join(", "), c.model),
            Err(e) => {
                ok = false;
                eprintln!("{name}: error: {e}");
            }
        }
    }
    Ok(ok)
}
