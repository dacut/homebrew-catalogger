//! Claude in Amazon Bedrock, through the Bedrock Runtime `InvokeModel` API:
//! `POST https://bedrock-runtime.{region}.amazonaws.com/model/{model-id}/invoke`.
//!
//! The request body is the Claude API's Messages body without `model` (which is in the URL), plus
//! `"anthropic_version": "bedrock-2023-05-31"`. The response body is the same as the Claude API's. Current Claude
//! models are invoked through cross-Region inference profiles, whose IDs look like `us.anthropic.claude-haiku-5-5`.

use {
    crate::{Error, Result},
    aws_config::{BehaviorVersion, Region},
    aws_credential_types::{
        Credentials,
        provider::{ProvideCredentials, SharedCredentialsProvider},
    },
    aws_sigv4::{
        http_request::{SignableBody, SignableRequest, SigningSettings, sign},
        sign::v4,
    },
    serde_json::Value,
    std::{
        fmt,
        time::{Duration, SystemTime},
    },
    tokio::sync::Mutex,
};

/// The SigV4 signing name for Bedrock Runtime.
const SIGNING_NAME: &str = "bedrock";

/// The `anthropic_version` the InvokeModel API expects in the request body.
const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

/// The environment variable holding a Bedrock API key.
pub const BEARER_TOKEN_ENV: &str = "AWS_BEARER_TOKEN_BEDROCK";

/// Cached AWS credentials are refreshed when they are this close to expiring.
const CREDENTIAL_REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// Settings for reaching Claude in Amazon Bedrock. Every field is optional.
///
/// Authentication uses a Bedrock API key if one is given or set in `AWS_BEARER_TOKEN_BEDROCK`; otherwise requests
/// are signed (SigV4) with AWS credentials from the standard AWS configuration: environment variables, the shared
/// config and credentials files (including SSO and assumed-role profiles), and container or instance roles.
/// The identity needs `bedrock:InvokeModel` on the inference profile and its models (plus
/// `bedrock:CallWithBearerToken` when using an API key).
#[derive(Clone, Default)]
pub struct Bedrock {
    /// The AWS region, e.g. `us-east-1`. Defaults to the AWS configuration (`AWS_REGION`, the profile, ...).
    pub region: Option<String>,

    /// The AWS profile for credentials and region. Defaults to `AWS_PROFILE`, or the default profile.
    pub profile: Option<String>,

    /// A Bedrock API key. Defaults to `AWS_BEARER_TOKEN_BEDROCK`.
    pub bearer_token: Option<String>,

    /// The cross-Region inference geography used to turn a plain model name into an inference profile ID, e.g.
    /// `us`, `eu`, `apac`, or `global`. Defaults from the region; see [`default_geography`].
    pub geography: Option<String>,

    /// The Bedrock Runtime base URL, to use instead of `https://bedrock-runtime.{region}.amazonaws.com` (e.g. a VPC
    /// endpoint).
    pub endpoint: Option<String>,
}

impl Bedrock {
    /// Settings that use the AWS configuration for everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the AWS region.
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Sets the AWS profile.
    pub fn profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = Some(profile.into());
        self
    }

    /// Sets a Bedrock API key.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    /// Sets the cross-Region inference geography.
    pub fn geography(mut self, geography: impl Into<String>) -> Self {
        self.geography = Some(geography.into());
        self
    }

    /// Sets the Bedrock Runtime base URL.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

impl fmt::Debug for Bedrock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bedrock")
            .field("region", &self.region)
            .field("profile", &self.profile)
            .field("bearer_token", &self.bearer_token.as_ref().map(|_| "<redacted>"))
            .field("geography", &self.geography)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// The cross-Region inference geography for `region`: `us` for `us-*`, `us-gov` for `us-gov-*`, `eu` for `eu-*`,
/// `apac` for `ap-*`, and `global` elsewhere.
pub fn default_geography(region: &str) -> &'static str {
    if region.starts_with("us-gov-") {
        "us-gov"
    } else if region.starts_with("us-") {
        "us"
    } else if region.starts_with("eu-") {
        "eu"
    } else if region.starts_with("ap-") {
        "apac"
    } else {
        "global"
    }
}

/// Converts a Claude API model name to the inference profile ID that invokes it on Bedrock: with geography `us`,
/// `claude-haiku-5-5` becomes `us.anthropic.claude-haiku-5-5`. Anything else (an inference profile ID, a model ID,
/// or an ARN) is returned unchanged.
pub fn inference_profile_id(model: &str, geography: &str) -> String {
    if model.starts_with("claude-") {
        format!("{geography}.anthropic.{model}")
    } else {
        model.to_string()
    }
}

/// The Bedrock Runtime base URL for `region`.
pub fn endpoint_for_region(region: &str) -> String {
    format!("https://bedrock-runtime.{region}.amazonaws.com")
}

/// Converts a Claude API request body to an InvokeModel body: drops `model` and adds `anthropic_version`.
pub(crate) fn invoke_body(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("model");
        object.insert("anthropic_version".into(), ANTHROPIC_VERSION.into());
    }
    body
}

/// A resolved Bedrock connection: where to send requests and how to authenticate them.
pub(crate) struct Connection {
    /// The InvokeModel URL for the configured model.
    pub(crate) invoke_url: String,
    signer: Signer,
}

enum Signer {
    Bearer(String),
    SigV4 {
        region: String,
        provider: SharedCredentialsProvider,
        cached: Mutex<Option<Credentials>>,
    },
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let auth = match &self.signer {
            Signer::Bearer(_) => "bearer token",
            Signer::SigV4 {
                ..
            } => "SigV4",
        };
        f.debug_struct("Connection").field("invoke_url", &self.invoke_url).field("auth", &auth).finish()
    }
}

impl Connection {
    /// Resolves the region, endpoint, model ID, and credentials for invoking `model`. The AWS configuration is loaded
    /// only when needed: not for an API key with an explicit region, or with an explicit endpoint and geography.
    pub(crate) async fn resolve(config: &Bedrock, model: &str) -> Result<Self> {
        let bearer = config
            .bearer_token
            .clone()
            .or_else(|| std::env::var(BEARER_TOKEN_ENV).ok())
            .filter(|t| !t.trim().is_empty());

        let region_needed = config.endpoint.is_none() || config.geography.is_none();
        let aws = if bearer.is_none() || (config.region.is_none() && region_needed) {
            let mut loader = aws_config::defaults(BehaviorVersion::latest());
            if let Some(profile) = &config.profile {
                loader = loader.profile_name(profile);
            }
            if let Some(region) = &config.region {
                loader = loader.region(Region::new(region.clone()));
            }
            Some(loader.load().await)
        } else {
            None
        };
        let region = config.region.clone().or_else(|| aws.as_ref()?.region().map(|r| r.to_string()));

        let base_url = match (&config.endpoint, &region) {
            (Some(endpoint), _) => endpoint.trim_end_matches('/').to_string(),
            (None, Some(region)) => endpoint_for_region(region),
            (None, None) => return Err(no_region(config)),
        };
        let geography = match (&config.geography, &region) {
            (Some(geography), _) => geography.as_str(),
            (None, Some(region)) => default_geography(region),
            (None, None) => return Err(no_region(config)),
        };
        let model_id = inference_profile_id(model, geography);

        let signer = match bearer {
            Some(token) => Signer::Bearer(token),
            None => Signer::SigV4 {
                region: region.ok_or_else(|| no_region(config))?,
                provider: aws.as_ref().and_then(|a| a.credentials_provider()).ok_or_else(|| {
                    Error::Bedrock(format!("no AWS credentials found, and {BEARER_TOKEN_ENV} is not set"))
                })?,
                cached: Mutex::new(None),
            },
        };

        Ok(Self {
            invoke_url: format!("{base_url}/model/{}/invoke", percent_encode_path_segment(&model_id)),
            signer,
        })
    }

    /// Loads AWS credentials now (for SigV4), so that a credential problem is reported before any request.
    pub(crate) async fn check_credentials(&self) -> Result<()> {
        match &self.signer {
            Signer::Bearer(_) => Ok(()),
            Signer::SigV4 {
                provider,
                cached,
                ..
            } => current_credentials(provider, cached).await.map(|_| ()),
        }
    }

    /// Adds authentication to a request. For SigV4, `headers` must be every header the request will carry besides
    /// `Host` (which is derived from the URL) and the signing headers this adds, and `body` its exact body.
    pub(crate) async fn authorize(
        &self,
        request: reqwest::RequestBuilder,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        match &self.signer {
            Signer::Bearer(token) => Ok(request.bearer_auth(token)),
            Signer::SigV4 {
                region,
                provider,
                cached,
            } => {
                let credentials = current_credentials(provider, cached).await?;
                let signed = sigv4_headers(&credentials, region, &self.invoke_url, headers, body, SystemTime::now())?;
                Ok(signed.into_iter().fold(request, |request, (name, value)| request.header(name, value)))
            }
        }
    }
}

/// Percent-encodes a URL path segment; model IDs can be ARNs, which contain `:` and `/`.
fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Returns cached credentials, fetching new ones if there are none or they are about to expire.
async fn current_credentials(
    provider: &SharedCredentialsProvider,
    cached: &Mutex<Option<Credentials>>,
) -> Result<Credentials> {
    let mut cached = cached.lock().await;
    let fresh = cached
        .as_ref()
        .is_some_and(|c| c.expiry().is_none_or(|expiry| expiry > SystemTime::now() + CREDENTIAL_REFRESH_MARGIN));
    if !fresh {
        let credentials = provider
            .provide_credentials()
            .await
            .map_err(|e| Error::Bedrock(format!("unable to load AWS credentials: {}", error_chain(&e))))?;
        *cached = Some(credentials);
    }
    Ok(cached.clone().expect("credentials were just loaded"))
}

/// Computes the SigV4 headers (`authorization`, `x-amz-date`, and `x-amz-security-token` for temporary credentials)
/// for a POST of `body` to `url` carrying `headers`.
fn sigv4_headers(
    credentials: &Credentials,
    region: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    time: SystemTime,
) -> Result<Vec<(String, String)>> {
    let signing_error = |e: &dyn fmt::Display| Error::Bedrock(format!("unable to sign the request: {e}"));
    let identity = credentials.clone().into();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name(SIGNING_NAME)
        .time(time)
        .settings(SigningSettings::default())
        .build()
        .map_err(|e| signing_error(&e))?
        .into();
    let request = SignableRequest::new("POST", url, headers.iter().copied(), SignableBody::Bytes(body))
        .map_err(|e| signing_error(&e))?;
    let (instructions, _signature) = sign(request, &params).map_err(|e| signing_error(&e))?.into_parts();
    Ok(instructions.headers().map(|(name, value)| (name.to_string(), value.to_string())).collect())
}

fn no_region(config: &Bedrock) -> Error {
    let hint = match &config.profile {
        Some(profile) => format!(" (does AWS profile {profile:?} exist and set a region?)"),
        None => String::new(),
    };
    Error::Bedrock(format!("no AWS region configured{hint}; set one (e.g. AWS_REGION)"))
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !message.contains(&text) {
            message = format!("{message}: {text}");
        }
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    #[test]
    fn geographies() {
        assert_eq!(default_geography("us-east-1"), "us");
        assert_eq!(default_geography("us-gov-west-1"), "us-gov");
        assert_eq!(default_geography("eu-west-1"), "eu");
        assert_eq!(default_geography("ap-northeast-1"), "apac");
        assert_eq!(default_geography("ca-central-1"), "global");
    }

    #[test]
    fn model_ids() {
        assert_eq!(inference_profile_id("claude-haiku-5-5", "us"), "us.anthropic.claude-haiku-5-5");
        assert_eq!(inference_profile_id("claude-haiku-5-5", "global"), "global.anthropic.claude-haiku-5-5");
        for unchanged in [
            "eu.anthropic.claude-sonnet-5",
            "anthropic.claude-haiku-4-5-20251001-v1:0",
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abc",
        ] {
            assert_eq!(inference_profile_id(unchanged, "us"), unchanged);
        }
    }

    #[test]
    fn invoke_bodies() {
        let body = invoke_body(json!({"model": "claude-haiku-5-5", "max_tokens": 10, "messages": []}));
        assert_eq!(body, json!({"anthropic_version": "bedrock-2023-05-31", "max_tokens": 10, "messages": []}));
    }

    #[test]
    fn path_encoding() {
        assert_eq!(percent_encode_path_segment("us.anthropic.claude-haiku-5-5"), "us.anthropic.claude-haiku-5-5");
        assert_eq!(percent_encode_path_segment("arn:aws:bedrock:x/y"), "arn%3Aaws%3Abedrock%3Ax%2Fy");
    }

    #[test]
    fn debug_redacts_token() {
        let text = format!("{:?}", Bedrock::new().bearer_token("secret-token"));
        assert!(!text.contains("secret-token") && text.contains("<redacted>"), "{text}");
    }

    #[test]
    fn sigv4_signs_with_bedrock_scope() {
        // 2026-11-22T12:00:00Z
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_348_800);
        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-haiku-5-5/invoke";
        let headers = [("content-type", "application/json"), ("accept", "application/json")];

        let permanent = Credentials::new("AKIDEXAMPLE", "secret", None, None, "test");
        let signed = sigv4_headers(&permanent, "us-east-1", url, &headers, b"{}", time).unwrap();
        let get = |name: &str| signed.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());

        assert_eq!(get("x-amz-date"), Some("20261122T120000Z"));
        let authorization = get("authorization").unwrap();
        assert!(
            authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261122/us-east-1/bedrock/aws4_request, \
                 SignedHeaders=accept;content-type;host;x-amz-date, Signature="
            ),
            "{authorization}"
        );
        assert_eq!(get("x-amz-security-token"), None);

        // Signing is deterministic, and depends on the body.
        assert_eq!(signed, sigv4_headers(&permanent, "us-east-1", url, &headers, b"{}", time).unwrap());
        assert_ne!(signed, sigv4_headers(&permanent, "us-east-1", url, &headers, b"[]", time).unwrap());

        let temporary = Credentials::new("ASIAEXAMPLE", "secret", Some("session-token".into()), None, "test");
        let signed = sigv4_headers(&temporary, "us-east-1", url, &headers, b"{}", time).unwrap();
        assert!(signed.iter().any(|(n, v)| n == "x-amz-security-token" && v == "session-token"));
    }

    #[tokio::test]
    async fn resolves_without_aws_config_when_possible() {
        let connection = Connection::resolve(&Bedrock::new().region("eu-west-1").bearer_token("t"), "claude-haiku-5-5")
            .await
            .unwrap();
        assert_eq!(
            connection.invoke_url,
            "https://bedrock-runtime.eu-west-1.amazonaws.com/model/eu.anthropic.claude-haiku-5-5/invoke"
        );
        assert!(matches!(connection.signer, Signer::Bearer(ref t) if t == "t"));

        let config = Bedrock::new().endpoint("https://vpce.example/").geography("global").bearer_token("t");
        let connection = Connection::resolve(&config, "claude-sonnet-5").await.unwrap();
        assert_eq!(connection.invoke_url, "https://vpce.example/model/global.anthropic.claude-sonnet-5/invoke");
    }
}
