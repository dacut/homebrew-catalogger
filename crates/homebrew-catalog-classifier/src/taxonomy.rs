//! The set of categories formulae are classified into.

use serde::{Deserialize, Serialize};

/// A catalog category.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Category {
    /// The category's identifier, e.g. `version-control`. This is what classifications return.
    pub name: String,

    /// A human-readable name for display, e.g. `Version Control`. See [`display_title`](Self::display_title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// A description of what belongs in the category, shown to the model.
    pub description: String,
}

impl Category {
    /// Creates a category with no display title.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            description: description.into(),
        }
    }

    /// Sets the display title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The display title, or one derived from the name (`version-control` becomes `Version Control`) if none is set.
    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| title_from_name(&self.name))
    }
}

/// Derives a display title from a category name: `version-control` becomes `Version Control`.
pub fn title_from_name(name: &str) -> String {
    name.split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The categories a [`Classifier`](crate::Classifier) may assign.
#[derive(Clone, Debug, PartialEq)]
pub enum Taxonomy {
    /// Only the listed categories may be assigned. The model's output is constrained to these names.
    Fixed(Vec<Category>),

    /// The model invents category names (lowercase, hyphen-separated) as it sees fit.
    Open,
}

impl Taxonomy {
    /// Returns the fixed categories, or `None` for an open taxonomy.
    pub fn categories(&self) -> Option<&[Category]> {
        match self {
            Self::Fixed(categories) => Some(categories),
            Self::Open => None,
        }
    }
}

impl Default for Taxonomy {
    /// The built-in taxonomy from [`default_categories`].
    fn default() -> Self {
        Self::Fixed(default_categories())
    }
}

/// The built-in category list, covering the kinds of software found in `homebrew/core`.
pub fn default_categories() -> Vec<Category> {
    [
        (
            "ai-ml",
            "AI & Machine Learning",
            "Artificial intelligence and machine learning: LLM clients, model runners, ML frameworks",
        ),
        (
            "archiving-compression",
            "Archiving & Compression",
            "Archivers, compressors, and tools for disk images or package formats",
        ),
        ("audio", "Audio", "Audio playback, recording, editing, codecs, and music software"),
        ("build-tools", "Build Tools", "Build systems, make tools, code generators, and packaging/release tooling"),
        ("cloud", "Cloud", "Command-line clients and tooling for cloud providers and hosted services"),
        ("communication", "Communication", "Chat, IRC, messaging, and social network clients"),
        (
            "containers-virtualization",
            "Containers & Virtualization",
            "Containers, Kubernetes, virtual machines, and orchestration",
        ),
        ("cryptography", "Cryptography", "Encryption, hashing, certificates, key management, and password managers"),
        ("databases", "Databases", "Database servers, clients, drivers, and migration tools"),
        (
            "data-processing",
            "Data Processing",
            "Processing structured data: JSON, YAML, CSV, query and transformation tools",
        ),
        (
            "debugging-profiling",
            "Debugging & Profiling",
            "Debuggers, profilers, tracers, and performance analysis tools",
        ),
        ("desktop-gui", "Desktop & GUI", "Graphical desktop applications and GUI toolkits"),
        ("devops", "DevOps", "Deployment, configuration management, infrastructure as code, and CI/CD"),
        ("documentation", "Documentation", "Documentation generators, typesetting, and markup processors"),
        ("editors-ides", "Editors & IDEs", "Text editors, IDEs, and editor tooling such as language servers"),
        ("email", "Email", "Email clients, servers, and mail-processing tools"),
        (
            "embedded-hardware",
            "Embedded & Hardware",
            "Microcontrollers, firmware, cross-compilation toolchains, and hardware interfaces",
        ),
        ("emulation", "Emulation", "Emulators and compatibility layers for other systems or architectures"),
        (
            "file-management",
            "File Management",
            "File managers, file search, synchronization, backup, and disk usage tools",
        ),
        ("finance", "Finance", "Accounting, personal finance, and cryptocurrency software"),
        ("games", "Games", "Games and game engines"),
        ("geospatial", "Geospatial", "GIS, mapping, and geographic data tools"),
        ("graphics-images", "Graphics & Images", "Image viewing, editing, conversion, and 2D/3D graphics libraries"),
        ("libraries", "Libraries", "Libraries intended mainly to be linked or imported by other software"),
        ("linters-formatters", "Linters & Formatters", "Static analysis, linters, and code formatters"),
        ("monitoring", "Monitoring", "System and service monitoring, logging, metrics, and observability"),
        ("multimedia-video", "Multimedia & Video", "Video playback, editing, encoding, and streaming"),
        ("networking", "Networking", "Network utilities, protocols, proxies, VPNs, and network analysis"),
        ("package-management", "Package Management", "Package managers and language/runtime version managers"),
        ("productivity", "Productivity", "Notes, task management, calendars, and personal productivity tools"),
        (
            "programming-languages",
            "Programming Languages",
            "Compilers, interpreters, and runtimes for programming languages",
        ),
        ("reverse-engineering", "Reverse Engineering", "Disassemblers, decompilers, and binary analysis tools"),
        ("science-math", "Science & Math", "Scientific computing, mathematics, statistics, and bioinformatics"),
        ("security", "Security", "Security auditing, penetration testing, vulnerability scanning, and forensics"),
        ("shells", "Shells", "Command shells, shell extensions, prompts, and shell scripting utilities"),
        ("system-utilities", "System Utilities", "General system administration and operating system utilities"),
        ("terminal", "Terminal", "Terminal emulators, multiplexers, and terminal UI tools"),
        ("testing", "Testing", "Test frameworks, test runners, and load-testing tools"),
        ("text-processing", "Text Processing", "Text search, manipulation, diffing, spell checking, and localization"),
        ("version-control", "Version Control", "Version control systems and related tools"),
        ("web-development", "Web Development", "Web servers, frameworks, HTTP clients, and web development tooling"),
    ]
    .into_iter()
    .map(|(name, title, description)| Category::new(name, description).with_title(title))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(title_from_name("version-control"), "Version Control");
        assert_eq!(title_from_name("dev_tools x"), "Dev Tools X");
        assert_eq!(Category::new("a-b", "").display_title(), "A B");
        assert_eq!(Category::new("ai-ml", "").with_title("AI & ML").display_title(), "AI & ML");
        assert!(default_categories().iter().all(|c| c.title.is_some()));
    }
}
