//! The site's content model: which category pages exist and which formulae each one lists.

pub use homebrew_catalog_classifier::catalog::CATEGORIES_FIELD;
use {
    homebrew_catalog_classifier::{Category, HomebrewFormulaMeta, title_from_name},
    std::collections::{BTreeMap, HashMap, HashSet},
};

/// The pseudo-category for formulae with no categories, so that none are silently dropped from the site.
pub const UNCATEGORIZED: &str = "uncategorized";

/// One category page.
#[derive(Debug)]
pub struct CategoryPage<'a> {
    /// The category's identifier, as it appears in formula metadata.
    pub name: String,
    /// The file-name-safe identifier used for the page's URL.
    pub slug: String,
    /// The display title.
    pub title: String,
    /// A description of the category, if known.
    pub description: Option<String>,
    /// The formulae in the category, sorted by name.
    pub formulae: Vec<&'a HomebrewFormulaMeta>,
}

/// All category pages for a set of formulae.
#[derive(Debug)]
pub struct Site<'a> {
    /// Category pages, sorted by title.
    pub categories: Vec<CategoryPage<'a>>,
    /// The number of distinct formulae on the site.
    pub formula_count: usize,
    by_name: HashMap<String, usize>,
}

impl<'a> Site<'a> {
    /// Groups `formulae` by category. Titles and descriptions come from `known` where a category is listed there;
    /// otherwise the title is derived from the category name.
    pub fn build(formulae: &'a [HomebrewFormulaMeta], known: &[Category]) -> Self {
        let mut grouped: BTreeMap<String, Vec<&HomebrewFormulaMeta>> = BTreeMap::new();
        for formula in formulae {
            let mut categories = formula_categories(formula);
            if categories.is_empty() {
                categories.push(UNCATEGORIZED.to_string());
            }
            for category in categories {
                grouped.entry(category).or_default().push(formula);
            }
        }

        let known: HashMap<&str, &Category> = known.iter().map(|c| (c.name.as_str(), c)).collect();
        // `index` is reserved for the categories folder's own index page.
        let mut used_slugs = HashSet::from(["index".to_string()]);
        let mut categories: Vec<CategoryPage> = grouped
            .into_iter()
            .map(|(name, mut formulae)| {
                formulae.sort_by(|a, b| a.name.cmp(&b.name));
                let (title, description) = match known.get(name.as_str()) {
                    Some(c) => (c.display_title(), Some(c.description.clone()).filter(|d| !d.is_empty())),
                    None if name == UNCATEGORIZED => {
                        ("Uncategorized".to_string(), Some("Formulae that have not been assigned a category.".into()))
                    }
                    None => (title_from_name(&name), None),
                };
                CategoryPage {
                    slug: unique_slug(&name, &mut used_slugs),
                    name,
                    title,
                    description,
                    formulae,
                }
            })
            .collect();
        categories.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then(a.name.cmp(&b.name)));

        let by_name = categories.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
        Self {
            categories,
            formula_count: formulae.len(),
            by_name,
        }
    }

    /// Looks up a category page by category name.
    pub fn category(&self, name: &str) -> Option<&CategoryPage<'a>> {
        self.by_name.get(name).map(|&i| &self.categories[i])
    }
}

/// The categories assigned to a formula, most relevant first, without duplicates.
pub fn formula_categories(formula: &HomebrewFormulaMeta) -> Vec<String> {
    let mut seen = HashSet::new();
    formula
        .extra
        .get(CATEGORIES_FIELD)
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|c| !c.is_empty() && seen.insert(c.to_string()))
        .map(str::to_string)
        .collect()
}

/// Returns `true` if any formula carries a categories field (i.e. the input was produced by the classifier).
pub fn is_classified(formulae: &[HomebrewFormulaMeta]) -> bool {
    formulae.iter().any(|f| f.extra.contains_key(CATEGORIES_FIELD))
}

/// The formula's page on formulae.brew.sh.
pub fn homebrew_url(formula: &HomebrewFormulaMeta) -> String {
    format!("https://formulae.brew.sh/formula/{}", percent_encode_path_segment(&formula.name))
}

/// The formula's homepage, if it is an http(s) URL. Other schemes (e.g. `javascript:`) are never linked.
pub fn safe_homepage(formula: &HomebrewFormulaMeta) -> Option<&str> {
    let homepage = formula.homepage.as_deref()?.trim();
    let lower = homepage.get(..8).unwrap_or(homepage).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")).then_some(homepage)
}

fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~@+".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A lowercase, hyphen-separated identifier safe for file names and URLs, unique among `used`.
fn unique_slug(name: &str, used: &mut HashSet<String>) -> String {
    let base: String = name
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "category".to_string()
    } else {
        base
    };

    let mut slug = base.clone();
    let mut n = 2;
    while !used.insert(slug.clone()) {
        slug = format!("{base}-{n}");
        n += 1;
    }
    slug
}

#[cfg(test)]
pub(crate) mod tests {
    use {super::*, serde_json::json};

    pub fn formula(name: &str, categories: Option<&[&str]>) -> HomebrewFormulaMeta {
        let mut f = HomebrewFormulaMeta {
            name: name.into(),
            full_name: name.into(),
            ..Default::default()
        };
        if let Some(categories) = categories {
            f.extra.insert(CATEGORIES_FIELD.into(), json!(categories));
        }
        f
    }

    #[test]
    fn groups_formulae_by_category() {
        let formulae = vec![
            formula("zz", Some(&["games", "ai-ml"])),
            formula("aa", Some(&["ai-ml", "ai-ml"])),
            formula("lonely", Some(&[])),
            formula("unclassified", None),
            formula("weird", Some(&["Custom Stuff!"])),
            formula("idx", Some(&["Index"])),
        ];
        let known = vec![Category::new("ai-ml", "AI things").with_title("AI & ML"), Category::new("games", "")];
        let site = Site::build(&formulae, &known);

        let titles: Vec<_> = site.categories.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["AI & ML", "Custom Stuff!", "Games", "Index", "Uncategorized"]);
        assert_eq!(site.formula_count, 6);
        assert_eq!(site.category("Index").unwrap().slug, "index-2", "`index` is reserved");

        let ai = site.category("ai-ml").unwrap();
        assert_eq!(ai.description.as_deref(), Some("AI things"));
        assert_eq!(ai.formulae.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["aa", "zz"]);

        assert_eq!(site.category("games").unwrap().description, None, "empty descriptions are dropped");
        assert_eq!(site.category("Custom Stuff!").unwrap().slug, "custom-stuff");
        assert_eq!(site.category(UNCATEGORIZED).unwrap().formulae.len(), 2);
    }

    #[test]
    fn slugs_are_unique() {
        let mut used = HashSet::new();
        assert_eq!(unique_slug("a b", &mut used), "a-b");
        assert_eq!(unique_slug("A-B", &mut used), "a-b-2");
        assert_eq!(unique_slug("../..", &mut used), "category");
    }

    #[test]
    fn urls() {
        assert_eq!(homebrew_url(&formula("python@3.12", None)), "https://formulae.brew.sh/formula/python@3.12");
        assert_eq!(homebrew_url(&formula("a b/c", None)), "https://formulae.brew.sh/formula/a%20b%2Fc");

        let mut f = formula("x", None);
        for (homepage, expected) in [
            ("https://example.com", Some("https://example.com")),
            ("HTTP://example.com", Some("HTTP://example.com")),
            ("javascript:alert(1)", None),
            ("", None),
        ] {
            f.homepage = Some(homepage.into());
            assert_eq!(safe_homepage(&f), expected, "{homepage}");
        }
    }
}
