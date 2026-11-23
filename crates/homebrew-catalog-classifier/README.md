# homebrew-catalog-classifier

A Rust library that loads Homebrew formula metadata and sorts formulae into catalog categories using Claude, through
either the Claude API or Amazon Bedrock.

It's the core of [homebrew-catalogger](../../README.md); the
[`homebrew-catalog-classifier`](../homebrew-catalog-classifier-cli) command is a thin wrapper around it.

## Features

- **Loads formula metadata** from Homebrew's `formula.json`, a single formula's JSON file, or Homebrew's local API
  cache (`brew --cache`). Fields the library doesn't model are kept, so a formula serializes back to exactly the JSON
  it was read from.
- **Classifies formulae** into a fixed taxonomy (41 built-in categories, or your own), constrained with structured
  output so the model can only answer with category names from the list, or into categories the model names itself.
- **Claude API or Amazon Bedrock.** Bedrock requests go through the InvokeModel API and cross-Region inference
  profiles, signed with AWS credentials (SigV4) or a Bedrock API key.
- **Built for whole catalogs:** concurrent requests, retries with backoff for rate limits, overloads, and network
  errors, and a [`catalog`](src/catalog.rs) module that reuses a previous run's categories so only new formulae are
  classified.

## Usage

The API is async and needs a [Tokio](https://tokio.rs/) runtime.

```rust
use homebrew_catalog_classifier::{Classifier, HomebrewFormulaMeta};

#[tokio::main]
async fn main() -> homebrew_catalog_classifier::Result<()> {
    // Reads ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN) from the environment.
    let classifier = Classifier::from_env()?;

    let formula = HomebrewFormulaMeta {
        name: "ripgrep".into(),
        desc: Some("Search tool like grep and The Silver Searcher".into()),
        ..Default::default()
    };
    let classification = classifier.classify(&formula).await?;
    println!("{}: {}", classification.formula, classification.categories.join(", "));
    Ok(())
}
```

`Classification::categories` lists up to three categories, most relevant first.

### Classifying many formulae

```rust
// Every formula in a formula.json file, a few requests at a time; failures don't stop the rest.
for (name, result) in classifier.classify_formulae_at("formula.json").await? {
    match result {
        Ok(c) => println!("{name}: {}", c.categories.join(", ")),
        Err(e) => eprintln!("{name}: {e}"),
    }
}

// One formula from Homebrew's local cache, by name or alias.
let jq = classifier.classify_named_formula("jq").await?;
```

`classify_all` takes any collection of formulae and returns results in the same order; `classify_stream` yields them
as they finish, for progress reporting.

### Amazon Bedrock

```rust
use homebrew_catalog_classifier::{Bedrock, Classifier};

let classifier = Classifier::builder()
    .bedrock(Bedrock::new().region("us-east-1"))
    .build()?;
```

- **Model IDs:** a plain model name such as `claude-haiku-5-5` is invoked through the cross-Region inference profile
  for the region's geography: `us.anthropic.claude-haiku-5-5` in `us-*` regions, `eu.` in `eu-*`, `apac.` in `ap-*`,
  and `global.` elsewhere. Override the geography with `Bedrock::geography`, or pass a full inference profile ID as
  the model.
- **Authentication:** a Bedrock API key (`Bedrock::bearer_token`, or `AWS_BEARER_TOKEN_BEDROCK`) if set; otherwise
  AWS credentials from the standard chain (environment, profiles including SSO, container and instance roles). The
  identity needs `bedrock:InvokeModel` on the inference profile and its model, plus `bedrock:CallWithBearerToken`
  for API keys.
- **Not available on Bedrock:** server-side refusal fallbacks, which the Claude API path enables for models that
  support them.

Call `Classifier::connect` before a large run to report a missing region or credentials once, up front, instead of
as a failure for every formula.

### Reusing a previous catalog

A catalog is `formula.json` with a `categories` field added to each formula. The `catalog` module carries categories
over from a previous catalog, so an update only sends new formulae to the model:

```rust
use homebrew_catalog_classifier::{
    load_formulae,
    catalog::{self, PreviousCategories},
};

let previous = PreviousCategories::load("formula-catalogged.json")?;
let fresh = load_formulae("formula.json")?;
let formulae: Vec<_> = fresh.iter().collect();

let (reused, to_classify) = catalog::plan(
    &formulae,
    Some(&previous),
    &Default::default(), // formulae to reclassify anyway
    classifier.taxonomy(),
    classifier.max_categories(),
);
println!("reusing {}, classifying {}", formulae.len() - to_classify.len(), to_classify.len());
```

Classify the formulae at the `to_classify` indices, then build the new catalog entries with `annotate`, which adds
the categories (or the classification error) to each formula's full metadata. Formulae are matched by name, then by
former name, and categories that are no longer in the taxonomy are classified again.

### Custom categories

```rust
use homebrew_catalog_classifier::{Category, Classifier, Taxonomy};

let taxonomy = Taxonomy::Fixed(vec![
    Category::new("cli", "Command-line tools").with_title("Command Line"),
    Category::new("gui", "Graphical desktop applications").with_title("Desktop Apps"),
]);
let classifier = Classifier::builder().taxonomy(taxonomy).build()?;
```

`Taxonomy::Open` lets the model name categories itself (normalized to lowercase, hyphen-separated names).
`default_categories()` returns the built-in list.

## Configuration

`ClassifierBuilder` sets:

| Setting | Default |
| --- | --- |
| `model` | `claude-haiku-5-5` |
| `effort` | `Low` (classification doesn't need more) |
| `max_categories` | 3 |
| `concurrency` | 8 requests at a time |
| `max_retries` | 2 |
| `max_tokens` | 4096 |
| `timeout` | 10 minutes per request |
| `fallbacks` | On for models that support server-side refusal fallbacks (Claude API only) |

On the Claude API, `ANTHROPIC_BASE_URL` overrides the API endpoint.

## Cargo features

- **`bedrock`** (default): Amazon Bedrock support, which brings in `aws-config` and `aws-sigv4`. Disable default
  features if you only use the Claude API.

## Loading metadata without classifying

```rust
use homebrew_catalog_classifier::{find_formula, load_default_formulae, load_formulae};

let all = load_formulae("formula.json")?;           // or a cache directory, or one formula's file
let cached = load_default_formulae()?;              // $HOMEBREW_CACHE/api, or the platform default
let wget = find_formula("wget", "formula.json")?;   // by name, full name, alias, or former name
```
