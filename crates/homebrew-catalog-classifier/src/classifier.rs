//! LLM-backed classification of formulae via the Anthropic Messages API.

use {
    crate::{
        Error, HomebrewFormulaMeta, Result, Taxonomy,
        loader::{self, find_default_formula, find_formula},
    },
    futures_util::{Stream, StreamExt, stream},
    reqwest::{StatusCode, header::HeaderMap},
    serde::{Deserialize, Serialize},
    serde_json::{Map, Value, json},
    std::{collections::HashSet, fmt, path::Path, time::Duration},
};

/// The model used when none is configured.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Where requests are sent.
#[derive(Clone, Debug, Default)]
pub enum Platform {
    /// The Claude API (`https://api.anthropic.com`, or `$ANTHROPIC_BASE_URL`), authenticated with an Anthropic API key
    /// or OAuth token.
    #[default]
    Anthropic,

    /// Claude in Amazon Bedrock, through the Bedrock Runtime InvokeModel API, authenticated with AWS credentials or a
    /// Bedrock API key. A plain model name is invoked through the cross-Region inference profile for the region's
    /// geography (`claude-haiku-5-5` becomes e.g. `us.anthropic.claude-haiku-5-5`); see [`crate::Bedrock`].
    /// Server-side refusal fallbacks, which Bedrock doesn't offer, are disabled.
    #[cfg(feature = "bedrock")]
    Bedrock(crate::Bedrock),
}

/// How requests reach the configured platform.
#[derive(Debug)]
enum Transport {
    Anthropic {
        auth: Auth,
        messages_url: String,
    },
    #[cfg(feature = "bedrock")]
    Bedrock {
        config: crate::Bedrock,
        // Resolved on first use, since loading the AWS configuration is asynchronous. Boxed to keep `Transport` small.
        connection: Box<tokio::sync::OnceCell<crate::bedrock::Connection>>,
    },
}

/// Models that accept `fallbacks: "default"`, which reruns a declined request on another model.
const FALLBACK_MODELS: [&str; 5] =
    ["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

/// The result of classifying one formula.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Classification {
    /// The formula's name.
    pub formula: String,

    /// The categories the formula belongs to, most relevant first. Never empty.
    pub categories: Vec<String>,

    /// The model that produced the classification. Differs from the configured model when a server-side fallback
    /// handled the request.
    pub model: String,
}

impl Classification {
    /// The most relevant category.
    pub fn primary_category(&self) -> &str {
        &self.categories[0]
    }
}

/// How hard the model thinks before answering; maps to the API's `output_config.effort`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effort {
    /// Least thinking; fastest and cheapest.
    Low,
    /// Moderate thinking.
    Medium,
    /// Thorough thinking.
    High,
    /// More thorough than `High`.
    XHigh,
    /// Maximum thinking.
    Max,
}

impl Effort {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Credentials for the Anthropic API.
#[derive(Clone)]
pub enum Auth {
    /// An API key, sent as `x-api-key`.
    ApiKey(String),
    /// An OAuth access token (e.g. from `ant auth print-credentials --access-token`), sent as a bearer token.
    BearerToken(String),
}

impl Auth {
    /// Reads `ANTHROPIC_API_KEY`, falling back to `ANTHROPIC_AUTH_TOKEN`.
    pub fn from_env() -> Option<Self> {
        let var = |key| std::env::var(key).ok().filter(|v: &String| !v.is_empty());
        var("ANTHROPIC_API_KEY").map(Self::ApiKey).or_else(|| var("ANTHROPIC_AUTH_TOKEN").map(Self::BearerToken))
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("Auth::ApiKey(<redacted>)"),
            Self::BearerToken(_) => f.write_str("Auth::BearerToken(<redacted>)"),
        }
    }
}

/// Configures and builds a [`Classifier`].
#[derive(Debug)]
pub struct ClassifierBuilder {
    platform: Platform,
    auth: Option<Auth>,
    base_url: Option<String>,
    model: String,
    effort: Option<Effort>,
    max_tokens: u32,
    taxonomy: Taxonomy,
    max_categories: usize,
    concurrency: usize,
    max_retries: u32,
    fallbacks: Option<bool>,
    timeout: Duration,
    http_client: Option<reqwest::Client>,
}

impl Default for ClassifierBuilder {
    fn default() -> Self {
        Self {
            platform: Platform::default(),
            auth: None,
            base_url: None,
            model: DEFAULT_MODEL.to_string(),
            effort: Some(Effort::Low),
            max_tokens: 4096,
            taxonomy: Taxonomy::default(),
            max_categories: 3,
            concurrency: 8,
            max_retries: 2,
            fallbacks: None,
            timeout: Duration::from_secs(600),
            http_client: None,
        }
    }
}

impl ClassifierBuilder {
    /// Sets where requests are sent. Defaults to [`Platform::Anthropic`].
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Shorthand for `platform(Platform::Bedrock(bedrock))`.
    #[cfg(feature = "bedrock")]
    pub fn bedrock(self, bedrock: crate::Bedrock) -> Self {
        self.platform(Platform::Bedrock(bedrock))
    }

    /// Sets the Claude API credentials. Defaults to [`Auth::from_env`]. Not used with [`Platform::Bedrock`].
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Shorthand for `auth(Auth::ApiKey(key))`.
    pub fn api_key(self, key: impl Into<String>) -> Self {
        self.auth(Auth::ApiKey(key.into()))
    }

    /// Sets the API base URL. Defaults to `$ANTHROPIC_BASE_URL`, or `https://api.anthropic.com`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Sets the model. Defaults to [`DEFAULT_MODEL`].
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the effort level, or `None` to use the model's default. Defaults to [`Effort::Low`], which is
    /// sufficient for classification.
    pub fn effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    /// Sets `max_tokens` for each request, which must cover the model's thinking as well as its answer.
    /// Defaults to 4096.
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sets the categories to classify into. Defaults to the built-in taxonomy.
    pub fn taxonomy(mut self, taxonomy: Taxonomy) -> Self {
        self.taxonomy = taxonomy;
        self
    }

    /// Sets the maximum number of categories assigned to one formula (minimum 1). Defaults to 3.
    pub fn max_categories(mut self, max: usize) -> Self {
        self.max_categories = max.max(1);
        self
    }

    /// Sets how many requests run at once when classifying many formulae (minimum 1). Defaults to 8.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Sets how many times a request is retried after a rate limit, overload, server error, or connection
    /// failure. Defaults to 2.
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Enables or disables server-side refusal fallbacks (`fallbacks: "default"`), which rerun a request the model
    /// declines on another model. By default they are enabled for models that support them.
    pub fn fallbacks(mut self, enabled: bool) -> Self {
        self.fallbacks = Some(enabled);
        self
    }

    /// Sets the per-request timeout. Defaults to 10 minutes. Ignored when [`http_client`](Self::http_client) is
    /// set.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Uses a preconfigured HTTP client.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Builds the classifier.
    ///
    /// For [`Platform::Anthropic`], fails with [`Error::MissingCredentials`] if no credentials were given and none are
    /// in the environment. For [`Platform::Bedrock`], AWS credentials and region are resolved on the first request.
    pub fn build(self) -> Result<Classifier> {
        let http = match self.http_client {
            Some(client) => client,
            None => reqwest::Client::builder().timeout(self.timeout).build()?,
        };

        let (transport, model, fallbacks) = match self.platform {
            Platform::Anthropic => {
                let auth = self.auth.or_else(Auth::from_env).ok_or(Error::MissingCredentials)?;
                let base_url = self
                    .base_url
                    .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok().filter(|v| !v.is_empty()))
                    .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
                let fallbacks = self.fallbacks.unwrap_or_else(|| FALLBACK_MODELS.contains(&self.model.as_str()));
                let transport = Transport::Anthropic {
                    auth,
                    messages_url: format!("{}/v1/messages", base_url.trim_end_matches('/')),
                };
                (transport, self.model, fallbacks)
            }
            #[cfg(feature = "bedrock")]
            Platform::Bedrock(config) => {
                if self.fallbacks == Some(true) {
                    return Err(Error::Bedrock("server-side refusal fallbacks are not available".to_string()));
                }
                let transport = Transport::Bedrock {
                    config,
                    connection: Box::default(),
                };
                // The model becomes an inference profile ID once the region (and so the geography) is known.
                (transport, self.model, false)
            }
        };

        let system_prompt = system_prompt(&self.taxonomy, self.max_categories);
        let output_schema = output_schema(&self.taxonomy);

        Ok(Classifier {
            http,
            transport,
            model,
            effort: self.effort,
            max_tokens: self.max_tokens,
            taxonomy: self.taxonomy,
            max_categories: self.max_categories,
            concurrency: self.concurrency,
            max_retries: self.max_retries,
            fallbacks,
            system_prompt,
            output_schema,
        })
    }
}

/// Classifies Homebrew formulae into catalog categories using Claude.
///
/// Build one with [`Classifier::builder`] or [`Classifier::from_env`] and reuse it; it holds a connection pool.
#[derive(Debug)]
pub struct Classifier {
    http: reqwest::Client,
    transport: Transport,
    model: String,
    effort: Option<Effort>,
    max_tokens: u32,
    taxonomy: Taxonomy,
    max_categories: usize,
    concurrency: usize,
    max_retries: u32,
    fallbacks: bool,
    system_prompt: String,
    output_schema: Value,
}

impl Classifier {
    /// Returns a builder with default settings.
    pub fn builder() -> ClassifierBuilder {
        ClassifierBuilder::default()
    }

    /// Builds a classifier with default settings and credentials from the environment.
    pub fn from_env() -> Result<Self> {
        Self::builder().build()
    }

    /// The taxonomy this classifier assigns categories from.
    pub fn taxonomy(&self) -> &Taxonomy {
        &self.taxonomy
    }

    /// The configured model. (On Bedrock, a plain model name is invoked through an inference profile.)
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The most categories assigned to one formula.
    pub fn max_categories(&self) -> usize {
        self.max_categories
    }

    #[cfg(feature = "bedrock")]
    async fn bedrock_connection<'a>(
        &self,
        config: &crate::Bedrock,
        connection: &'a tokio::sync::OnceCell<crate::bedrock::Connection>,
    ) -> Result<&'a crate::bedrock::Connection> {
        connection.get_or_try_init(|| crate::bedrock::Connection::resolve(config, &self.model)).await
    }

    /// Resolves the platform's endpoint and credentials now instead of on the first request, so a configuration
    /// problem (such as missing AWS credentials or region) is reported once rather than for every formula. Sends no
    /// request to the model.
    pub async fn connect(&self) -> Result<()> {
        match &self.transport {
            Transport::Anthropic {
                ..
            } => Ok(()),
            #[cfg(feature = "bedrock")]
            Transport::Bedrock {
                config,
                connection,
            } => self.bedrock_connection(config, connection).await?.check_credentials().await,
        }
    }

    /// Classifies a single formula.
    pub async fn classify(&self, formula: &HomebrewFormulaMeta) -> Result<Classification> {
        let body = self.request_body(formula);
        let response = self.send(&body).await?;
        self.parse_response(&formula.name, response)
    }

    /// Classifies many formulae, running up to the configured number of requests concurrently.
    ///
    /// Returns one `(formula name, result)` pair per input, in input order. A failure for one formula does not stop
    /// the others.
    pub async fn classify_all<'a, I>(&self, formulae: I) -> Vec<(String, Result<Classification>)>
    where
        I: IntoIterator<Item = &'a HomebrewFormulaMeta>,
    {
        self.classify_stream(formulae).collect().await
    }

    /// Like [`classify_all`](Self::classify_all), but yields each `(formula name, result)` pair as it becomes
    /// available (still in input order), so callers can report progress or write results incrementally.
    pub fn classify_stream<'a, I>(&self, formulae: I) -> impl Stream<Item = (String, Result<Classification>)>
    where
        I: IntoIterator<Item = &'a HomebrewFormulaMeta>,
    {
        stream::iter(formulae)
            .map(move |formula| async move { (formula.name.clone(), self.classify(formula).await) })
            .buffered(self.concurrency)
    }

    /// Classifies every formula in the metadata at `path`. See [`load_formulae`](crate::load_formulae) for the
    /// accepted forms of `path`.
    ///
    /// Returns an error only if the metadata cannot be loaded; per-formula failures are reported in the result.
    pub async fn classify_formulae_at(&self, path: impl AsRef<Path>) -> Result<Vec<(String, Result<Classification>)>> {
        let formulae = loader::load_formulae(path)?;
        Ok(self.classify_all(&formulae).await)
    }

    /// Classifies every formula in Homebrew's default metadata location for this platform.
    ///
    /// See [`default_api_cache_dir`](crate::default_api_cache_dir).
    pub async fn classify_default_formulae(&self) -> Result<Vec<(String, Result<Classification>)>> {
        let formulae = loader::load_default_formulae()?;
        Ok(self.classify_all(&formulae).await)
    }

    /// Classifies the formula called `name` (or with that alias) from the metadata at `path`.
    pub async fn classify_named_formula_at(&self, name: &str, path: impl AsRef<Path>) -> Result<Classification> {
        self.classify(&find_formula(name, path)?).await
    }

    /// Classifies the formula called `name` (or with that alias) from Homebrew's default metadata location.
    pub async fn classify_named_formula(&self, name: &str) -> Result<Classification> {
        self.classify(&find_default_formula(name)?).await
    }

    fn request_body(&self, formula: &HomebrewFormulaMeta) -> Value {
        let formula_json = serde_json::to_string_pretty(&prompt_view(formula)).unwrap_or_default();
        let mut output_config = Map::new();
        if let Some(effort) = self.effort {
            output_config.insert("effort".into(), effort.as_str().into());
        }
        output_config.insert("format".into(), json!({"type": "json_schema", "schema": self.output_schema}));

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": [{
                "type": "text",
                "text": self.system_prompt,
                "cache_control": {"type": "ephemeral"},
            }],
            "messages": [{
                "role": "user",
                "content": format!("Classify this Homebrew formula.\n\n<formula>\n{formula_json}\n</formula>"),
            }],
            "output_config": output_config,
        });
        if self.fallbacks {
            body["fallbacks"] = "default".into();
        }
        body
    }

    async fn send(&self, body: &Value) -> Result<MessagesResponse> {
        let body = match &self.transport {
            Transport::Anthropic {
                ..
            } => serde_json::to_vec(body),
            #[cfg(feature = "bedrock")]
            Transport::Bedrock {
                ..
            } => serde_json::to_vec(&crate::bedrock::invoke_body(body.clone())),
        }
        .expect("a JSON value always serializes");

        let mut attempt = 0;
        loop {
            // Rebuilt for each attempt so that signed requests carry a current timestamp.
            let request = self.prepare(&body).await?;
            let response = match request.send().await {
                Ok(response) => response,
                Err(e) if attempt < self.max_retries && (e.is_connect() || e.is_timeout()) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = response.status();
            if status.is_success() {
                return Ok(response.json().await?);
            }

            let headers = response.headers().clone();
            if attempt < self.max_retries && should_retry(status, &headers) {
                tokio::time::sleep(retry_after(&headers).unwrap_or_else(|| backoff(attempt))).await;
                attempt += 1;
                continue;
            }

            let text = response.text().await.unwrap_or_default();
            #[allow(unused_mut)]
            let mut error = api_error(status, &headers, &text);
            // On Bedrock, these usually mean the model or inference profile isn't available to this account here.
            #[cfg(feature = "bedrock")]
            if let (
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND,
                Transport::Bedrock {
                    ..
                },
                Error::Api {
                    message,
                    ..
                },
            ) = (status, &self.transport, &mut error)
            {
                message.push_str(
                    " (on Amazon Bedrock, check that the identity may call bedrock:InvokeModel and that the model's \
                     inference profile is offered in this region and enabled for this account)",
                );
            }
            return Err(error);
        }
    }

    /// Builds an authenticated POST of `body` to the configured platform.
    async fn prepare(&self, body: &[u8]) -> Result<reqwest::RequestBuilder> {
        let with_headers = |request: reqwest::RequestBuilder, headers: &[(&str, &str)]| {
            headers.iter().fold(request, |request, (name, value)| request.header(*name, *value)).body(body.to_vec())
        };
        match &self.transport {
            Transport::Anthropic {
                auth,
                messages_url,
            } => {
                let mut headers = vec![("content-type", "application/json"), ("anthropic-version", API_VERSION)];
                let mut betas = Vec::new();
                if self.fallbacks {
                    betas.push(FALLBACK_BETA);
                }
                let request = self.http.post(messages_url);
                let request = match auth {
                    Auth::ApiKey(key) => request.header("x-api-key", key),
                    Auth::BearerToken(token) => {
                        betas.push(OAUTH_BETA);
                        request.bearer_auth(token)
                    }
                };
                let betas = betas.join(",");
                if !betas.is_empty() {
                    headers.push(("anthropic-beta", &betas));
                }
                Ok(with_headers(request, &headers))
            }
            #[cfg(feature = "bedrock")]
            Transport::Bedrock {
                config,
                connection,
            } => {
                // InvokeModel takes the API version in the body (`anthropic_version`), not as a header.
                let headers = [("content-type", "application/json"), ("accept", "application/json")];
                let connection = self.bedrock_connection(config, connection).await?;
                let request = with_headers(self.http.post(&connection.invoke_url), &headers);
                connection.authorize(request, &headers, body).await
            }
        }
    }

    fn parse_response(&self, formula: &str, response: MessagesResponse) -> Result<Classification> {
        match response.stop_reason.as_deref() {
            Some("refusal") => {
                let details = response.stop_details.unwrap_or_default();
                return Err(Error::Refusal {
                    category: details.category,
                    explanation: details.explanation,
                });
            }
            Some("max_tokens") => return Err(Error::Truncated(self.max_tokens)),
            _ => {}
        }

        let text: String =
            response.content.iter().filter(|b| b.kind == "text").filter_map(|b| b.text.as_deref()).collect();
        let output: ModelOutput = serde_json::from_str(&text)
            .map_err(|e| Error::InvalidResponse(format!("model output is not the expected JSON ({e}): {text}")))?;

        let mut seen = HashSet::new();
        let categories: Vec<String> = output
            .categories
            .iter()
            .filter_map(|c| self.canonical_category(c))
            .filter(|c| seen.insert(c.clone()))
            .take(self.max_categories)
            .collect();

        if categories.is_empty() {
            return Err(Error::InvalidResponse(format!("model returned no usable categories: {text}")));
        }

        Ok(Classification {
            formula: formula.to_string(),
            categories,
            model: response.model,
        })
    }

    /// Maps a category returned by the model onto the taxonomy, or normalizes it for an open taxonomy.
    fn canonical_category(&self, raw: &str) -> Option<String> {
        let raw = raw.trim();
        match self.taxonomy.categories() {
            Some(categories) => categories.iter().find(|c| c.name.eq_ignore_ascii_case(raw)).map(|c| c.name.clone()),
            None => {
                let normalized = raw
                    .to_lowercase()
                    .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("-");
                (!normalized.is_empty()).then_some(normalized)
            }
        }
    }
}

/// The subset of a formula's metadata shown to the model. Empty fields are omitted.
fn prompt_view(formula: &HomebrewFormulaMeta) -> Value {
    let mut view = Map::new();
    let mut put = |key: &str, value: Value| {
        let empty = match &value {
            Value::Null => true,
            Value::String(s) => s.trim().is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Bool(b) => !b,
            _ => false,
        };
        if !empty {
            view.insert(key.to_string(), value);
        }
    };

    put("name", formula.name.as_str().into());
    if formula.full_name != formula.name {
        put("full_name", formula.full_name.as_str().into());
    }
    put("tap", json!(formula.tap));
    put("description", json!(formula.desc));
    put("homepage", json!(formula.homepage));
    put("license", json!(formula.license));
    put("version", json!(formula.versions.stable));
    put("aliases", json!(formula.aliases));
    put("former_names", json!(formula.oldnames));
    put("executables", json!(formula.executables));
    put("dependencies", json!(formula.dependencies));
    put("build_dependencies", json!(formula.build_dependencies));
    put(
        "optional_dependencies",
        json!([&formula.recommended_dependencies[..], &formula.optional_dependencies[..]].concat()),
    );
    let macos: Vec<&str> = formula.uses_from_macos.iter().flat_map(|u| u.names()).collect();
    put("uses_from_macos", json!(macos));
    put("conflicts_with", json!(formula.conflicts_with));
    put("keg_only", formula.keg_only.into());
    put("provides_background_service", formula.service.is_some().into());
    put("caveats", json!(formula.caveats));
    put("deprecated", formula.deprecated.into());
    put("disabled", formula.disabled.into());

    Value::Object(view)
}

fn system_prompt(taxonomy: &Taxonomy, max_categories: usize) -> String {
    let mut prompt = format!(
        "You classify Homebrew formulae (packages for the Homebrew package manager) into categories for a \
         browsable software catalog. You will receive one formula's metadata as JSON.\n\n\
         Return the categories a user browsing the catalog would expect to find this formula under, most \
         relevant first: at least 1 and at most {max_categories}, using fewer when fewer fit well. Classify by \
         what the software is for, not how it is built: build dependencies such as `rust` or `cmake` say nothing \
         about a formula's category unless the formula is itself a tool for that ecosystem. When the description \
         is terse, use the name, homepage, executables, and runtime dependencies as further evidence.\n\n"
    );
    match taxonomy.categories() {
        Some(categories) => {
            prompt.push_str("Choose only from these categories:\n");
            for category in categories {
                prompt.push_str(&format!("- {}: {}\n", category.name, category.description));
            }
        }
        None => prompt.push_str(
            "Name categories yourself: lowercase words separated by hyphens, broad enough that many formulae \
             would share them (for example `version-control` rather than `git-tools-for-monorepos`).\n",
        ),
    }
    prompt
}

fn output_schema(taxonomy: &Taxonomy) -> Value {
    let items = match taxonomy.categories() {
        Some(categories) => json!({
            "type": "string",
            "enum": categories.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        }),
        None => json!({"type": "string"}),
    };
    json!({
        "type": "object",
        "properties": {
            "categories": {
                "type": "array",
                "items": items,
                "description": "Categories for the formula, most relevant first.",
            },
        },
        "required": ["categories"],
        "additionalProperties": false,
    })
}

fn should_retry(status: StatusCode, headers: &HeaderMap) -> bool {
    match headers.get("x-should-retry").and_then(|v| v.to_str().ok()) {
        Some("true") => true,
        Some("false") => false,
        _ => {
            matches!(status, StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT | StatusCode::TOO_MANY_REQUESTS)
                || status.is_server_error()
        }
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: f64 = headers.get("retry-after")?.to_str().ok()?.trim().parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(60.0)))
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500u64.saturating_mul(1 << attempt.min(4)).min(8_000))
}

/// Interprets an error response: the Claude API's `{"error": {"type", "message"}}`, or AWS's `{"message"}` with the
/// error type in the `x-amzn-errortype` header (e.g. `ValidationException:http://...`).
fn api_error(status: StatusCode, headers: &HeaderMap, body: &str) -> Error {
    #[derive(Deserialize)]
    struct Envelope {
        error: Detail,
    }
    #[derive(Deserialize)]
    struct Detail {
        #[serde(rename = "type")]
        kind: String,
        message: String,
    }
    #[derive(Deserialize)]
    struct AwsError {
        #[serde(alias = "Message")]
        message: String,
    }

    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    // The Claude API sends `request-id`; Amazon Bedrock sends `x-amzn-requestid`.
    let request_id = header("request-id").or_else(|| header("x-amzn-requestid")).map(str::to_string);
    let aws_kind = header("x-amzn-errortype").and_then(|t| t.split(':').next()).filter(|t| !t.is_empty());

    let (kind, message) = if let Ok(Envelope {
        error,
    }) = serde_json::from_str(body)
    {
        (error.kind, error.message)
    } else if let Ok(AwsError {
        message,
    }) = serde_json::from_str(body)
    {
        (aws_kind.unwrap_or("unknown").to_string(), message)
    } else {
        (aws_kind.unwrap_or("unknown").to_string(), body.to_string())
    };

    Error::Api {
        status: status.as_u16(),
        kind,
        message,
        request_id,
    }
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<StopDetails>,
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Debug, Default, Deserialize)]
struct StopDetails {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    explanation: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelOutput {
    categories: Vec<String>,
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{Category, UsesFromMacos},
    };

    fn classifier(builder: ClassifierBuilder) -> Classifier {
        builder.api_key("test-key").base_url("http://localhost:1").build().unwrap()
    }

    fn sample_formula() -> HomebrewFormulaMeta {
        HomebrewFormulaMeta {
            name: "aichat".into(),
            full_name: "aichat".into(),
            tap: Some("homebrew/core".into()),
            desc: Some("All-in-one AI-Powered CLI Chat & Copilot".into()),
            homepage: Some("https://github.com/sigoden/aichat".into()),
            build_dependencies: vec!["rust".into()],
            uses_from_macos: vec![UsesFromMacos::Name("zlib".into())],
            executables: Some(vec!["aichat".into()]),
            ..Default::default()
        }
    }

    fn response(stop_reason: &str, text: &str) -> MessagesResponse {
        serde_json::from_value(json!({
            "model": "claude-opus-5-5",
            "stop_reason": stop_reason,
            "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}],
        }))
        .unwrap()
    }

    #[test]
    fn request_body_uses_taxonomy_enum() {
        let c = classifier(Classifier::builder());
        let body = c.request_body(&sample_formula());

        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["output_config"]["effort"], "low");
        let schema = &body["output_config"]["format"]["schema"];
        let allowed = schema["properties"]["categories"]["items"]["enum"].as_array().unwrap();
        assert!(allowed.contains(&json!("ai-ml")));
        assert_eq!(schema["additionalProperties"], false);

        let user = body["messages"][0]["content"].as_str().unwrap();
        assert!(user.contains("All-in-one AI-Powered CLI Chat"));
        assert!(user.contains("\"uses_from_macos\""));
        assert!(!user.contains("\"keg_only\""), "false/empty fields are omitted");
    }

    #[test]
    fn fallbacks_only_for_supported_models() {
        // Haiku (the default) has no server-side fallbacks.
        let c = classifier(Classifier::builder().effort(None));
        let body = c.request_body(&sample_formula());
        assert!(body.get("fallbacks").is_none());
        assert!(body["output_config"].get("effort").is_none());

        let c = classifier(Classifier::builder().model("claude-opus-5-5"));
        assert_eq!(c.request_body(&sample_formula())["fallbacks"], "default");

        let c = classifier(Classifier::builder().model("claude-opus-5-5").fallbacks(false));
        assert!(c.request_body(&sample_formula()).get("fallbacks").is_none());
    }

    #[test]
    fn open_taxonomy_schema_has_no_enum() {
        let c = classifier(Classifier::builder().taxonomy(Taxonomy::Open));
        let body = c.request_body(&sample_formula());
        assert!(body["output_config"]["format"]["schema"]["properties"]["categories"]["items"].get("enum").is_none());
        assert!(body["system"][0]["text"].as_str().unwrap().contains("Name categories yourself"));
    }

    #[test]
    fn parses_and_cleans_categories() {
        let c = classifier(Classifier::builder().max_categories(2));
        let result = c
            .parse_response(
                "aichat",
                response("end_turn", r#"{"categories": ["AI-ML", "bogus", "ai-ml", "terminal", "shells"]}"#),
            )
            .unwrap();
        assert_eq!(result.formula, "aichat");
        assert_eq!(result.categories, vec!["ai-ml", "terminal"]);
        assert_eq!(result.primary_category(), "ai-ml");
        assert_eq!(result.model, "claude-opus-5-5");
    }

    #[test]
    fn open_taxonomy_normalizes_names() {
        let c = classifier(Classifier::builder().taxonomy(Taxonomy::Open));
        let result =
            c.parse_response("x", response("end_turn", r#"{"categories": ["Version Control", "dev_tools"]}"#)).unwrap();
        assert_eq!(result.categories, vec!["version-control", "dev-tools"]);
    }

    #[test]
    fn custom_taxonomy_filters_to_its_names() {
        let taxonomy = Taxonomy::Fixed(vec![Category::new("cli", "Command-line tools")]);
        let c = classifier(Classifier::builder().taxonomy(taxonomy));
        let err = c.parse_response("x", response("end_turn", r#"{"categories": ["ai-ml"]}"#)).unwrap_err();
        assert!(matches!(err, Error::InvalidResponse(_)));
    }

    #[test]
    fn refusal_and_truncation_are_errors() {
        let c = classifier(Classifier::builder());
        let refusal: MessagesResponse = serde_json::from_value(json!({
            "model": "claude-opus-5-5",
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber", "explanation": "nope"},
            "content": [],
        }))
        .unwrap();
        match c.parse_response("x", refusal).unwrap_err() {
            Error::Refusal {
                category,
                ..
            } => assert_eq!(category.as_deref(), Some("cyber")),
            other => panic!("unexpected error: {other:?}"),
        }

        assert!(matches!(c.parse_response("x", response("max_tokens", "{")).unwrap_err(), Error::Truncated(4096)));
        assert!(matches!(
            c.parse_response("x", response("end_turn", "not json")).unwrap_err(),
            Error::InvalidResponse(_)
        ));
    }

    #[test]
    fn retry_policy() {
        let mut headers = HeaderMap::new();
        assert!(should_retry(StatusCode::TOO_MANY_REQUESTS, &headers));
        assert!(should_retry(StatusCode::from_u16(529).unwrap(), &headers));
        assert!(!should_retry(StatusCode::BAD_REQUEST, &headers));
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert!(!should_retry(StatusCode::TOO_MANY_REQUESTS, &headers));

        headers.insert("retry-after", "3".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3)));
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(10), Duration::from_millis(8_000));
    }

    #[test]
    fn api_error_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert("request-id", "req_123".parse().unwrap());
        let err = api_error(
            StatusCode::BAD_REQUEST,
            &headers,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
        );
        match err {
            Error::Api {
                status,
                kind,
                message,
                request_id,
            } => {
                assert_eq!((status, kind.as_str(), message.as_str()), (400, "invalid_request_error", "bad"));
                assert_eq!(request_id.as_deref(), Some("req_123"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn auth_debug_is_redacted() {
        assert_eq!(format!("{:?}", Auth::ApiKey("sk-secret".into())), "Auth::ApiKey(<redacted>)");
    }

    /// Serves a single HTTP request on a local port, replying with `reply` as JSON. Returns the server's base URL and
    /// a handle resolving to the raw request it received.
    async fn serve_once(reply: Value) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                data.extend_from_slice(&buf[..n]);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse().unwrap());
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(n > 0, "connection closed before the request was complete");
            }
            let body = reply.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(data).unwrap()
        });
        (url, handle)
    }

    fn reply(model: &str) -> Value {
        json!({
            "model": model,
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"categories\": [\"ai-ml\"]}"}],
        })
    }

    /// Splits a raw HTTP request into its lowercased head and its JSON body.
    fn split_request(raw: &str) -> (String, Value) {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        (head.to_lowercase(), serde_json::from_str(body).unwrap())
    }

    #[tokio::test]
    async fn claude_api_request_end_to_end() {
        let (url, request) = serve_once(reply("claude-haiku-5-5")).await;
        let classifier = Classifier::builder().api_key("test-key").base_url(&url).build().unwrap();

        let result = classifier.classify(&sample_formula()).await.unwrap();
        assert_eq!(result.categories, ["ai-ml"]);

        let (head, body) = split_request(&request.await.unwrap());
        assert!(head.starts_with("post /v1/messages http/1.1\r\n"), "{head}");
        assert!(head.contains("\r\nx-api-key: test-key"));
        assert!(head.contains("\r\nanthropic-version: 2026-10-21"));
        assert!(head.contains("\r\ncontent-type: application/json"));
        assert_eq!(body["model"], "claude-haiku-5-5");
    }

    #[cfg(feature = "bedrock")]
    #[tokio::test]
    async fn bedrock_request_end_to_end() {
        let (url, request) = serve_once(reply("claude-haiku-5-5")).await;
        let bedrock = crate::Bedrock::new().endpoint(&url).region("us-east-1").bearer_token("test-token");
        let classifier = Classifier::builder().bedrock(bedrock).build().unwrap();

        let result = classifier.classify(&sample_formula()).await.unwrap();
        assert_eq!(result.categories, ["ai-ml"]);
        assert_eq!(result.model, "claude-haiku-5-5");

        let (head, body) = split_request(&request.await.unwrap());
        assert!(
            head.starts_with("post /model/us.anthropic.claude-haiku-5-5/invoke http/1.1\r\n"),
            "the model is invoked through the region's inference profile: {head}"
        );
        assert!(head.contains("\r\nauthorization: bearer test-token"));
        assert!(head.contains("\r\naccept: application/json"));
        assert!(!head.contains("anthropic-version") && !head.contains("anthropic-beta"), "{head}");
        assert!(!head.contains("x-api-key"), "{head}");
        assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
        assert!(body.get("model").is_none(), "the model is in the URL, not the body");
        assert!(body.get("fallbacks").is_none());
        assert!(body["output_config"]["format"].is_object(), "structured output is still requested");
    }

    #[test]
    fn aws_error_parsing() {
        let mut headers = HeaderMap::new();
        headers.insert("x-amzn-requestid", "aws-req-1".parse().unwrap());
        headers.insert("x-amzn-errortype", "ValidationException:http://internal.amazon.com/".parse().unwrap());
        match api_error(StatusCode::BAD_REQUEST, &headers, r#"{"message":"Invocation isn't supported."}"#) {
            Error::Api {
                status,
                kind,
                message,
                request_id,
            } => {
                assert_eq!(status, 400);
                assert_eq!(kind, "ValidationException");
                assert_eq!(message, "Invocation isn't supported.");
                assert_eq!(request_id.as_deref(), Some("aws-req-1"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(feature = "bedrock")]
    #[test]
    fn bedrock_disables_fallbacks() {
        let classifier = Classifier::builder()
            .model("claude-opus-5-5")
            .bedrock(crate::Bedrock::new().bearer_token("t"))
            .build()
            .unwrap();
        assert_eq!(classifier.model(), "claude-opus-5-5");
        assert!(!classifier.fallbacks, "Bedrock doesn't offer server-side fallbacks");

        let err = Classifier::builder().fallbacks(true).bedrock(crate::Bedrock::new()).build().unwrap_err();
        assert!(matches!(err, Error::Bedrock(_)), "{err}");
    }
}
