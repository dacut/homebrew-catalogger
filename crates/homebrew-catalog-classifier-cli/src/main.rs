//! Command-line interface for classifying Homebrew formulae into catalog categories.

use {
    anyhow::{Context, Result, anyhow, bail},
    clap::{Parser, ValueEnum},
    futures_util::{StreamExt, stream},
    homebrew_catalog_classifier::{
        Bedrock, Category, Classifier, DEFAULT_MODEL, Effort, HomebrewFormulaMeta, Taxonomy,
        catalog::{self, PreviousCategories, annotate},
        load_formulae,
    },
    std::{
        collections::HashSet,
        fs,
        io::{self, IsTerminal, Write},
        path::{Path, PathBuf},
        process::ExitCode,
    },
};

/// Exit status when some formulae could not be classified (the others are still written).
const EXIT_PARTIAL: u8 = 2;

/// Classify Homebrew formulae into catalog categories using Claude.
#[derive(Debug, Parser)]
#[command(
    version,
    after_help = "Requires ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN), set in the environment or in a .env file. \
                  With --bedrock, uses a Bedrock API key from AWS_BEARER_TOKEN_BEDROCK if set, otherwise AWS \
                  credentials (environment, profiles including SSO, or instance roles), which need \
                  bedrock:InvokeModel. Each formula classified is one API request.\n\n\
                  To update an existing catalog with fresh metadata, classifying only what's new:\n  \
                  homebrew-catalog-classifier formula.json --reuse catalog.json -o catalog.json\n\n\
                  Exit status: 0 on success, 1 on a fatal error, 2 if some formulae could not be classified \
                  (results for the rest are still written)."
)]
struct Cli {
    /// Formula metadata to classify: formula.json from https://formulae.brew.sh/api/formula.json, a single
    /// formula's JSON file, or a Homebrew API cache directory.
    input: PathBuf,

    /// Write the JSON results to this file instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Classify only this formula (matched by name, full name, alias, or former name). May be repeated.
    #[arg(short, long = "formula", value_name = "NAME")]
    formulae: Vec<String>,

    /// Classify at most this many formulae.
    #[arg(short = 'n', long, value_name = "N")]
    limit: Option<usize>,

    /// The Claude model to use.
    #[arg(short, long, default_value = DEFAULT_MODEL)]
    model: String,

    /// How much the model thinks before answering.
    #[arg(long, value_enum, default_value_t = EffortArg::Low)]
    effort: EffortArg,

    /// The most categories to assign to one formula.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u16).range(1..))]
    max_categories: u16,

    /// How many API requests to run at once.
    #[arg(short = 'j', long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..))]
    concurrency: u16,

    /// A JSON file listing the categories to use, as [{"name": "...", "description": "..."}, ...]. Defaults to the
    /// built-in list.
    #[arg(long, value_name = "FILE", conflicts_with = "open")]
    categories: Option<PathBuf>,

    /// Let the model name categories itself instead of choosing from a fixed list.
    #[arg(long)]
    open: bool,

    /// Reuse categories from a previous run's output. Formulae already classified there keep their categories; only
    /// new formulae, ones that failed, and ones whose categories are no longer in the category list are sent to the
    /// model. Formula metadata always comes from INPUT. May be the same file as --output.
    #[arg(long, value_name = "FILE")]
    reuse: Option<PathBuf>,

    /// With --reuse, classify this formula again anyway (matched like --formula). May be repeated.
    #[arg(long, value_name = "NAME", requires = "reuse")]
    reclassify: Vec<String>,

    /// Write compact JSON instead of pretty-printed JSON.
    #[arg(long)]
    compact: bool,

    /// Don't report progress or per-formula errors on standard error.
    #[arg(short, long)]
    quiet: bool,

    /// Load environment variables (e.g. ANTHROPIC_API_KEY) from this file. Defaults to a .env file in the current
    /// directory or the nearest parent that has one, if any. Variables already set in the environment take
    /// precedence.
    #[arg(long, value_name = "FILE")]
    env_file: Option<PathBuf>,

    /// Use Claude in Amazon Bedrock (the InvokeModel API) instead of the Claude API. A plain model name such as
    /// claude-haiku-5-5 is invoked through the cross-Region inference profile for the region's geography (e.g.
    /// us.anthropic.claude-haiku-5-5); give a full inference profile ID with --model to choose one yourself.
    #[arg(long)]
    bedrock: bool,

    /// With --bedrock, the AWS region. Defaults to AWS_REGION, or the AWS profile's region.
    #[arg(long, value_name = "REGION", requires = "bedrock")]
    aws_region: Option<String>,

    /// With --bedrock, the AWS profile to take credentials and region from. Defaults to AWS_PROFILE, or the default
    /// profile.
    #[arg(long, value_name = "PROFILE", requires = "bedrock")]
    aws_profile: Option<String>,

    /// With --bedrock, the cross-Region inference geography used to turn a plain model name into an inference
    /// profile ID: us, eu, apac, global, .... Defaults from the region: us for us-*, eu for eu-*, apac for ap-*, and
    /// global otherwise.
    #[arg(long, value_name = "GEO", requires = "bedrock")]
    bedrock_geography: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<EffortArg> for Effort {
    fn from(effort: EffortArg) -> Self {
        match effort {
            EffortArg::Low => Effort::Low,
            EffortArg::Medium => Effort::Medium,
            EffortArg::High => Effort::High,
            EffortArg::Xhigh => Effort::XHigh,
            EffortArg::Max => Effort::Max,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Load the .env file before the async runtime starts its worker threads: modifying the environment while other
    // threads may be reading it is unsound.
    let result = load_env_file(cli.env_file.as_deref()).and_then(|()| {
        let runtime = tokio::runtime::Runtime::new().context("starting the async runtime")?;
        runtime.block_on(run(cli))
    });

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Loads environment variables from `path`, or from the nearest `.env` file if `path` is `None`. A missing default
/// `.env` file is not an error; a missing explicit file is. Existing environment variables are not overridden.
fn load_env_file(path: Option<&Path>) -> Result<()> {
    let (result, name) = match path {
        Some(path) => (dotenvy::from_path(path), path.display().to_string()),
        None => match dotenvy::dotenv() {
            Err(e) if e.not_found() => return Ok(()),
            result => (result.map(|_| ()), ".env".to_string()),
        },
    };

    result.map_err(|e| match e {
        // dotenvy's message for a parse error quotes the offending line, which may hold a secret.
        dotenvy::Error::LineParse(_, index) => {
            anyhow!("syntax error in {name} (at character {index} of the offending line; contents not shown)")
        }
        // dotenvy's I/O error repeats its source in its own message; report the source alone.
        dotenvy::Error::Io(io) => anyhow!(io).context(format!("loading {name}")),
        other => anyhow!(other).context(format!("loading {name}")),
    })
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let taxonomy = load_taxonomy(&cli)?;
    let mut builder = Classifier::builder()
        .model(&cli.model)
        .effort(Some(cli.effort.into()))
        .max_categories(cli.max_categories.into())
        .concurrency(cli.concurrency.into())
        .taxonomy(taxonomy);
    if cli.bedrock {
        builder = builder.bedrock(Bedrock {
            region: cli.aws_region.clone(),
            profile: cli.aws_profile.clone(),
            geography: cli.bedrock_geography.clone(),
            ..Bedrock::default()
        });
    }
    let classifier = builder.build()?;

    let all = load_formulae(&cli.input).with_context(|| format!("loading {}", cli.input.display()))?;
    let selected = select_formulae(&all, &cli.formulae, cli.limit)?;
    let previous = cli
        .reuse
        .as_deref()
        .map(|path| {
            PreviousCategories::load(path).with_context(|| format!("loading previous results from {}", path.display()))
        })
        .transpose()?;
    let forced = resolve_forced(&selected, &cli.reclassify)?;
    let (reused, to_classify) =
        catalog::plan(&selected, previous.as_ref(), &forced, classifier.taxonomy(), cli.max_categories.into());

    // Fail before spending anything on API calls if the output can't be written.
    let output = cli.output.as_deref().map(PendingOutput::create).transpose()?;

    if previous.is_some() && !cli.quiet {
        eprintln!(
            "reusing previous categories for {} formulae; classifying {}",
            selected.len() - to_classify.len(),
            to_classify.len()
        );
    }

    // Report a credential or region problem once, up front, rather than as a failure for every formula. Skipped when
    // everything is reused, since no credentials are needed then.
    if !to_classify.is_empty() {
        classifier.connect().await?;
    }

    // Each formula's outcome: reused categories, or (below) the model's categories or error.
    let mut outcomes: Vec<Option<Result<Vec<String>, String>>> = reused.into_iter().map(|r| r.map(Ok)).collect();
    let mut progress = Progress::new(to_classify.len(), cli.quiet);
    let mut failed = 0;
    // `classify_stream` yields results in input order, so each result lines up with its index.
    let mut results =
        classifier.classify_stream(to_classify.iter().map(|&i| selected[i])).zip(stream::iter(&to_classify));
    while let Some(((name, result), &i)) = results.next().await {
        debug_assert_eq!(name, selected[i].name);
        outcomes[i] = Some(result.map(|c| c.categories).map_err(|e| {
            failed += 1;
            let error = error_chain(&e);
            progress.error(&name, &error);
            error
        }));
        progress.advance();
    }
    progress.finish();

    let annotated = selected
        .iter()
        .zip(outcomes)
        .map(|(formula, outcome)| annotate(formula, outcome.expect("every formula is either reused or classified")))
        .collect::<Vec<_>>();

    let mut json = if cli.compact {
        serde_json::to_vec(&annotated)?
    } else {
        serde_json::to_vec_pretty(&annotated)?
    };
    json.push(b'\n');

    match output {
        Some(output) => output.commit(&json)?,
        None => io::stdout().lock().write_all(&json).context("writing to standard output")?,
    }

    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_PARTIAL)
    })
}

fn load_taxonomy(cli: &Cli) -> Result<Taxonomy> {
    if cli.open {
        return Ok(Taxonomy::Open);
    }
    let Some(path) = &cli.categories else {
        return Ok(Taxonomy::default());
    };

    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let categories: Vec<Category> =
        serde_json::from_str(&text).with_context(|| format!("parsing categories from {}", path.display()))?;
    if categories.is_empty() {
        bail!("{} lists no categories", path.display());
    }
    let mut seen = HashSet::new();
    if let Some(dup) = categories.iter().find(|c| !seen.insert(c.name.to_ascii_lowercase())) {
        bail!("{} lists category {:?} more than once", path.display(), dup.name);
    }
    Ok(Taxonomy::Fixed(categories))
}

/// Picks the formulae named on the command line (all of them if none were named), then applies `limit`.
fn select_formulae<'a>(
    all: &'a [HomebrewFormulaMeta],
    names: &[String],
    limit: Option<usize>,
) -> Result<Vec<&'a HomebrewFormulaMeta>> {
    let mut selected: Vec<&HomebrewFormulaMeta> = if names.is_empty() {
        all.iter().collect()
    } else {
        let mut missing = Vec::new();
        let mut seen = HashSet::new();
        let mut picked = Vec::new();
        for name in names {
            match all.iter().find(|f| f.is_named(name)) {
                Some(f) if seen.insert(&f.name) => picked.push(f),
                Some(_) => {}
                None => missing.push(name.as_str()),
            }
        }
        if !missing.is_empty() {
            bail!("formula not found: {}", missing.join(", "));
        }
        picked
    };

    if let Some(limit) = limit {
        selected.truncate(limit);
    }
    if selected.is_empty() {
        bail!("no formulae to classify");
    }
    Ok(selected)
}

/// Resolves `--reclassify` names to the formulae being classified, by name, full name, alias, or former name.
fn resolve_forced<'a>(selected: &[&'a HomebrewFormulaMeta], names: &[String]) -> Result<HashSet<&'a str>> {
    let mut forced = HashSet::new();
    let mut missing = Vec::new();
    for name in names {
        match selected.iter().find(|f| f.is_named(name)) {
            Some(f) => {
                forced.insert(f.name.as_str());
            }
            None => missing.push(name.as_str()),
        }
    }
    if !missing.is_empty() {
        bail!("--reclassify: not among the formulae being classified: {}", missing.join(", "));
    }
    Ok(forced)
}

/// Formats an error with its chain of causes, skipping causes whose text the previous message already includes
/// (e.g. wrapper errors that interpolate their source).
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
}

/// An output file written atomically: results go to a temporary sibling file that replaces the target only once
/// everything has been written, so an interrupted run never clobbers earlier results.
struct PendingOutput {
    target: PathBuf,
    temp: PathBuf,
}

impl PendingOutput {
    fn create(target: &Path) -> Result<Self> {
        let file_name = target.file_name().with_context(|| format!("{} is not a file path", target.display()))?;
        let mut temp_name = file_name.to_os_string();
        temp_name.push(format!(".{}.tmp", std::process::id()));
        let temp = target.with_file_name(temp_name);
        fs::File::create(&temp).with_context(|| format!("creating {}", temp.display()))?;
        Ok(Self {
            target: target.to_path_buf(),
            temp,
        })
    }

    fn commit(self, contents: &[u8]) -> Result<()> {
        fs::write(&self.temp, contents).with_context(|| format!("writing {}", self.temp.display()))?;
        fs::rename(&self.temp, &self.target)
            .with_context(|| format!("renaming {} to {}", self.temp.display(), self.target.display()))
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        // After a successful commit the temp file no longer exists, so this only cleans up after a failure.
        let _ = fs::remove_file(&self.temp);
    }
}

/// Progress reporting on standard error: a live counter on a terminal, periodic lines otherwise.
struct Progress {
    total: usize,
    done: usize,
    errors: usize,
    quiet: bool,
    live: bool,
}

impl Progress {
    const LINE_INTERVAL: usize = 100;

    fn new(total: usize, quiet: bool) -> Self {
        Self {
            total,
            done: 0,
            errors: 0,
            quiet,
            live: io::stderr().is_terminal(),
        }
    }

    fn advance(&mut self) {
        self.done += 1;
        if self.quiet {
            return;
        }
        if self.live {
            eprint!("\r{}", self.status());
        } else if self.done.is_multiple_of(Self::LINE_INTERVAL) {
            eprintln!("{}", self.status());
        }
    }

    fn error(&mut self, formula: &str, error: &str) {
        self.errors += 1;
        if !self.quiet {
            let clear = if self.live {
                "\r\x1b[K"
            } else {
                ""
            };
            eprintln!("{clear}{formula}: {error}");
        }
    }

    fn finish(&self) {
        if self.quiet || self.total == 0 {
            return;
        }
        if self.live {
            eprintln!();
        } else if !self.done.is_multiple_of(Self::LINE_INTERVAL) {
            eprintln!("{}", self.status());
        }
    }

    fn status(&self) -> String {
        format!("classified {}/{} formulae ({} failed)", self.done, self.total, self.errors)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, clap::CommandFactory};

    fn formula(name: &str, aliases: &[&str]) -> HomebrewFormulaMeta {
        HomebrewFormulaMeta {
            name: name.into(),
            full_name: name.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    fn names(selected: &[&HomebrewFormulaMeta]) -> Vec<String> {
        selected.iter().map(|f| f.name.clone()).collect()
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn categories_and_open_conflict() {
        assert!(Cli::try_parse_from(["x", "f.json", "--open", "--categories", "c.json"]).is_err());
        assert!(Cli::try_parse_from(["x", "f.json", "--max-categories", "0"]).is_err());
    }

    #[test]
    fn reclassify_requires_reuse() {
        assert!(Cli::try_parse_from(["x", "f.json", "--reclassify", "jq"]).is_err());
        let cli =
            Cli::try_parse_from(["x", "f.json", "--reuse", "prev.json", "--reclassify", "jq", "--reclassify", "wget"]);
        assert_eq!(cli.unwrap().reclassify, ["jq", "wget"]);
    }

    #[test]
    fn aws_options_require_bedrock() {
        assert!(Cli::try_parse_from(["x", "f.json", "--aws-region", "us-west-2"]).is_err());
        assert!(Cli::try_parse_from(["x", "f.json", "--bedrock-geography", "global"]).is_err());
        assert!(Cli::try_parse_from(["x", "f.json", "--aws-profile", "dev"]).is_err());
        let cli =
            Cli::try_parse_from(["x", "f.json", "--bedrock", "--aws-region", "us-west-2", "--aws-profile", "dev"])
                .unwrap();
        assert!(cli.bedrock);
        assert_eq!(cli.aws_region.as_deref(), Some("us-west-2"));
        assert_eq!(cli.aws_profile.as_deref(), Some("dev"));
    }

    #[test]
    fn resolves_forced_names() {
        let (a, b) = (formula("a", &[]), formula("b", &["bee"]));
        let selected = [&a, &b];
        assert_eq!(resolve_forced(&selected, &["bee".into()]).unwrap(), HashSet::from(["b"]));
        let err = resolve_forced(&selected, &["a".into(), "zzz".into()]).unwrap_err();
        assert!(err.to_string().ends_with("zzz"), "{err}");
    }

    #[test]
    fn selects_by_name_alias_and_limit() {
        let all = vec![formula("a", &[]), formula("b", &["bee"]), formula("c", &[])];

        assert_eq!(names(&select_formulae(&all, &[], None).unwrap()), ["a", "b", "c"]);
        assert_eq!(names(&select_formulae(&all, &[], Some(2)).unwrap()), ["a", "b"]);

        let picked = select_formulae(&all, &["c".into(), "bee".into(), "b".into()], None).unwrap();
        assert_eq!(names(&picked), ["c", "b"]);

        let err = select_formulae(&all, &["nope".into(), "zip".into()], None).unwrap_err();
        assert_eq!(err.to_string(), "formula not found: nope, zip");
        assert!(select_formulae(&all, &[], Some(0)).is_err());
    }

    #[test]
    fn loads_custom_categories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cats.json");
        let cli = |path: &Path| Cli::try_parse_from(["x", "f.json", "--categories", path.to_str().unwrap()]).unwrap();

        fs::write(&path, r#"[{"name": "cli", "description": "Command-line tools"}]"#).unwrap();
        assert_eq!(
            load_taxonomy(&cli(&path)).unwrap(),
            Taxonomy::Fixed(vec![Category::new("cli", "Command-line tools")])
        );

        fs::write(&path, r#"[{"name": "a", "description": ""}, {"name": "A", "description": ""}]"#).unwrap();
        assert!(load_taxonomy(&cli(&path)).unwrap_err().to_string().contains("more than once"));

        fs::write(&path, "[]").unwrap();
        assert!(load_taxonomy(&cli(&path)).is_err());
    }

    #[test]
    fn pending_output_replaces_target_only_on_commit() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.json");
        fs::write(&target, "old").unwrap();

        drop(PendingOutput::create(&target).unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "temp file cleaned up");

        PendingOutput::create(&target).unwrap().commit(b"new").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

        assert!(PendingOutput::create(&dir.path().join("missing/out.json")).is_err());
    }

    #[test]
    fn env_file_loading() {
        // Variable names are unique to this test; no other test reads or writes the environment.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.env");
        fs::write(&path, "HCC_TEST_FROM_FILE=from-file\nPATH=should-not-override\n").unwrap();

        load_env_file(Some(&path)).unwrap();
        assert_eq!(std::env::var("HCC_TEST_FROM_FILE").unwrap(), "from-file");
        assert_ne!(std::env::var("PATH").unwrap(), "should-not-override", "existing variables win");

        let missing = dir.path().join("missing.env");
        assert!(load_env_file(Some(&missing)).unwrap_err().to_string().contains("missing.env"));

        fs::write(&path, "HCC_TEST_BAD sk-ant-secret-value\n").unwrap();
        let message = format!("{:#}", load_env_file(Some(&path)).unwrap_err());
        assert!(message.contains("syntax error"), "{message}");
        assert!(!message.contains("sk-ant-secret-value"), "parse errors must not echo file contents: {message}");
    }
}
