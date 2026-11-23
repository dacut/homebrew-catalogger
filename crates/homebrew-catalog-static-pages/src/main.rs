//! Command-line interface for generating the static website. See the library documentation for how the site is laid
//! out.

use {
    anyhow::{Context, Result, bail},
    clap::Parser,
    homebrew_catalog_classifier::{default_categories, load_formulae},
    homebrew_catalog_static_pages::{
        BaseUrl, CATEGORIES_FIELD, DEFAULT_TITLE, Site, SiteOptions, Timestamp, is_classified, load_categories,
        write_site,
    },
    std::path::PathBuf,
};

/// Generate a static website listing Homebrew formulae by category.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "The landing page shows when the site was generated: the current time, or $SOURCE_DATE_EPOCH (seconds \
                  since 1970-01-01 UTC) if set, for reproducible output."
)]
struct Cli {
    /// Classified formula metadata, as written by homebrew-catalog-classifier (formula.json with a "categories"
    /// field added to each formula).
    input: PathBuf,

    /// The directory to write the site into. Created if needed; files with the same names are overwritten.
    #[arg(short, long, value_name = "DIR", default_value = "site")]
    output_dir: PathBuf,

    /// The site title, shown in the header and on the landing page.
    #[arg(long, default_value = DEFAULT_TITLE)]
    title: String,

    /// A JSON file of categories, as [{"name": "...", "title": "...", "description": "..."}, ...], providing display
    /// titles and descriptions. Defaults to the built-in categories.
    #[arg(long, value_name = "FILE")]
    categories: Option<PathBuf>,

    /// The site's absolute URL, e.g. https://brew.example.com/. Needed for sitemap.xml and canonical links, which
    /// must use absolute URLs; without it, neither is written.
    #[arg(long, value_name = "URL", value_parser = parse_base_url)]
    base_url: Option<BaseUrl>,

    /// Where the classified catalog JSON is published, relative to the site root (e.g. formula-catalogged.json), so
    /// llms.txt can point AI agents to it.
    #[arg(long, value_name = "PATH")]
    catalog_path: Option<String>,
}

fn parse_base_url(url: &str) -> Result<BaseUrl, String> {
    BaseUrl::parse(url).map_err(|e| e.to_string())
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let known = match &cli.categories {
        Some(path) => load_categories(path)?,
        None => default_categories(),
    };

    let formulae = load_formulae(&cli.input).with_context(|| format!("loading {}", cli.input.display()))?;
    if formulae.is_empty() {
        bail!("{} contains no formulae", cli.input.display());
    }
    if !is_classified(&formulae) {
        bail!(
            "no formula in {} has a \"{CATEGORIES_FIELD}\" field; run homebrew-catalog-classifier on it first",
            cli.input.display()
        );
    }

    let site = Site::build(&formulae, &known);
    if cli.base_url.is_none() {
        eprintln!("note: without --base-url, no sitemap.xml or canonical links are written");
    }
    let options = SiteOptions {
        title: cli.title,
        base_url: cli.base_url,
        generated: Timestamp::generation_time()?,
        catalog_path: cli.catalog_path,
    };
    let written = write_site(&site, &options, &cli.output_dir)?;
    eprintln!(
        "wrote {written} files for {} formulae in {} categories to {}",
        site.formula_count,
        site.categories.len(),
        cli.output_dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, clap::CommandFactory};

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn base_url_option() {
        let cli = Cli::try_parse_from(["x", "c.json", "--base-url", "https://brew.example.com"]).unwrap();
        assert_eq!(cli.base_url.unwrap().as_str(), "https://brew.example.com/");
        assert!(Cli::try_parse_from(["x", "c.json", "--base-url", "brew.example.com"]).is_err());
    }
}
