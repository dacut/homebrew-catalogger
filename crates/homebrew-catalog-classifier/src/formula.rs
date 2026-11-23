use {
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

/// Metadata for a single Homebrew formula.
///
/// This mirrors the JSON served by `https://formulae.brew.sh/api/formula/<name>.json` and cached by Homebrew under
/// `$(brew --cache)/api`. The fields useful for classification are modeled directly; all others are kept in
/// [`extra`](Self::extra). Every field other than `name` is optional so that a struct can be built by hand with
/// `..Default::default()`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct HomebrewFormulaMeta {
    /// The formula name, e.g. `ripgrep`.
    pub name: String,

    /// The fully-qualified name; differs from `name` for formulae outside `homebrew/core`.
    pub full_name: String,

    /// The tap containing the formula, e.g. `homebrew/core`.
    pub tap: Option<String>,

    /// Names this formula was previously known by.
    pub oldnames: Vec<String>,

    /// Alternate names for this formula.
    pub aliases: Vec<String>,

    /// Versioned variants of this formula, e.g. `python@3.12`.
    pub versioned_formulae: Vec<String>,

    /// The one-line description.
    pub desc: Option<String>,

    /// The SPDX license expression.
    pub license: Option<String>,

    /// The project homepage.
    pub homepage: Option<String>,

    /// Version information.
    pub versions: FormulaVersions,

    /// Formulae needed only to build this formula.
    pub build_dependencies: Vec<String>,

    /// Formulae needed at runtime.
    pub dependencies: Vec<String>,

    /// Formulae needed only to test this formula.
    pub test_dependencies: Vec<String>,

    /// Recommended (default-on) optional dependencies.
    pub recommended_dependencies: Vec<String>,

    /// Optional dependencies.
    pub optional_dependencies: Vec<String>,

    /// Dependencies satisfied by macOS itself on that platform.
    pub uses_from_macos: Vec<UsesFromMacos>,

    /// Formulae that cannot be installed alongside this one.
    pub conflicts_with: Vec<String>,

    /// Post-install notes shown to the user.
    pub caveats: Option<String>,

    /// Whether the formula is keg-only (not linked into the Homebrew prefix).
    pub keg_only: bool,

    /// Why the formula is keg-only, as reported by Homebrew.
    pub keg_only_reason: Option<Value>,

    /// Whether the formula is deprecated.
    pub deprecated: bool,

    /// Why the formula is deprecated.
    pub deprecation_reason: Option<String>,

    /// Whether the formula is disabled.
    pub disabled: bool,

    /// Why the formula is disabled.
    pub disable_reason: Option<String>,

    /// The `brew services` definition, if the formula provides a background service.
    pub service: Option<Value>,

    /// Executables installed into the Homebrew `bin` directory.
    /// `None` when the metadata doesn't list them, which is distinct from an empty list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executables: Option<Vec<String>>,

    /// The path of the formula's Ruby source within its tap.
    pub ruby_source_path: Option<String>,

    /// Every other field in the metadata (`urls`, `bottle`, `analytics`, ...), kept so that a formula serializes
    /// back to the JSON it was read from.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl HomebrewFormulaMeta {
    /// Returns `true` if `name` refers to this formula by its name, full name, alias, or former name.
    pub fn is_named(&self, name: &str) -> bool {
        self.name == name
            || self.full_name == name
            || self.aliases.iter().any(|a| a == name)
            || self.oldnames.iter().any(|o| o == name)
    }
}

/// Version information for a formula.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct FormulaVersions {
    /// The stable version, if any.
    pub stable: Option<String>,

    /// The HEAD version marker, if the formula can be built from HEAD.
    pub head: Option<String>,

    /// Whether prebuilt bottles are available.
    pub bottle: bool,
}

/// An entry in a formula's `uses_from_macos` list.
///
/// Homebrew emits either a bare formula name or a single-entry object mapping the name to a dependency tag (for
/// example `{"perl": "build"}`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum UsesFromMacos {
    /// A plain dependency on the named formula.
    Name(String),

    /// A dependency with tags, keyed by formula name.
    Tagged(serde_json::Map<String, Value>),
}

impl UsesFromMacos {
    /// The formula name(s) this entry refers to.
    pub fn names(&self) -> Vec<&str> {
        match self {
            Self::Name(n) => vec![n.as_str()],
            Self::Tagged(m) => m.keys().map(String::as_str).collect(),
        }
    }
}
