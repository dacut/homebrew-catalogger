//! Classified catalogs: formula metadata annotated with categories, and reusing a previous catalog's categories so
//! only new or changed formulae are sent to the model.
//!
//! A catalog is `formula.json` with a [`CATEGORIES_FIELD`] added to each formula (and an [`ERROR_FIELD`] where
//! classification failed).

use {
    crate::{HomebrewFormulaMeta, Result, Taxonomy, load_formulae},
    serde_json::Value,
    std::{
        collections::{HashMap, HashSet},
        path::Path,
    },
};

/// The field added to each formula's metadata holding its categories, most relevant first, or `null` if it has not
/// been classified.
pub const CATEGORIES_FIELD: &str = "categories";

/// The field added to a formula's metadata when it could not be classified.
pub const ERROR_FIELD: &str = "classification_error";

/// Categories recorded in a previous catalog, keyed by formula name.
#[derive(Debug, Default)]
pub struct PreviousCategories {
    by_name: HashMap<String, Vec<String>>,
    catalog_size: usize,
}

impl PreviousCategories {
    /// Loads the categories from a catalog file. Formulae that weren't classified (`null` or missing categories) are
    /// skipped, so they are retried.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::from_formulae(&load_formulae(path)?))
    }

    /// Collects the categories from a catalog's formulae.
    pub fn from_formulae(formulae: &[HomebrewFormulaMeta]) -> Self {
        let by_name = formulae
            .iter()
            .filter_map(|f| {
                let categories: Vec<String> = f
                    .extra
                    .get(CATEGORIES_FIELD)?
                    .as_array()?
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect();
                (!categories.is_empty()).then(|| (f.name.clone(), categories))
            })
            .collect();
        Self {
            by_name,
            catalog_size: formulae.len(),
        }
    }

    /// The number of formulae in the previous catalog, whether or not they were classified.
    pub fn catalog_size(&self) -> usize {
        self.catalog_size
    }

    /// The number of formulae with recorded categories.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether no formulae have recorded categories.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Returns the previously recorded categories for `formula`, if they are still usable.
    ///
    /// The formula is looked up by its current name, then by its former names (so a renamed formula keeps its
    /// categories). Categories are not reused if any of them is missing from a fixed `taxonomy`, since the taxonomy
    /// has changed since they were assigned. Reused categories are trimmed to `max_categories`.
    pub fn lookup(
        &self,
        formula: &HomebrewFormulaMeta,
        taxonomy: &Taxonomy,
        max_categories: usize,
    ) -> Option<Vec<String>> {
        let previous =
            std::iter::once(&formula.name).chain(&formula.oldnames).find_map(|name| self.by_name.get(name))?;

        if let Some(allowed) = taxonomy.categories()
            && !previous.iter().all(|c| allowed.iter().any(|a| &a.name == c))
        {
            return None;
        }

        let mut seen = HashSet::new();
        let categories: Vec<String> =
            previous.iter().filter(|c| seen.insert(c.as_str())).take(max_categories).cloned().collect();
        (!categories.is_empty()).then_some(categories)
    }
}

/// Decides which formulae need the model. Returns the categories to reuse for each formula (`None` where it must be
/// classified) and the indices of the formulae to classify.
///
/// Formulae named in `force` are always classified.
pub fn plan(
    formulae: &[&HomebrewFormulaMeta],
    previous: Option<&PreviousCategories>,
    force: &HashSet<&str>,
    taxonomy: &Taxonomy,
    max_categories: usize,
) -> (Vec<Option<Vec<String>>>, Vec<usize>) {
    let reused: Vec<Option<Vec<String>>> = formulae
        .iter()
        .map(|f| {
            if force.contains(f.name.as_str()) {
                return None;
            }
            previous?.lookup(f, taxonomy, max_categories)
        })
        .collect();
    let to_classify = reused.iter().enumerate().filter(|(_, r)| r.is_none()).map(|(i, _)| i).collect();
    (reused, to_classify)
}

/// Returns the formula's full metadata with its categories added under [`CATEGORIES_FIELD`]. If classification
/// failed (or wasn't attempted), that field is `null` and the reason is under [`ERROR_FIELD`].
///
/// Annotations already present (e.g. when re-classifying a previous catalog) are replaced.
pub fn annotate(formula: &HomebrewFormulaMeta, outcome: std::result::Result<Vec<String>, String>) -> Value {
    let mut value = serde_json::to_value(formula).expect("formula metadata always serializes");
    let object = value.as_object_mut().expect("formula metadata serializes to a JSON object");
    object.remove(ERROR_FIELD);
    match outcome {
        Ok(categories) => {
            object.insert(CATEGORIES_FIELD.into(), categories.into());
        }
        Err(error) => {
            object.insert(CATEGORIES_FIELD.into(), Value::Null);
            object.insert(ERROR_FIELD.into(), error.into());
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use {super::*, crate::Category, serde_json::json};

    fn formula(name: &str, categories: Option<Value>) -> HomebrewFormulaMeta {
        let mut f = HomebrewFormulaMeta {
            name: name.into(),
            full_name: name.into(),
            ..Default::default()
        };
        if let Some(categories) = categories {
            f.extra.insert(CATEGORIES_FIELD.into(), categories);
        }
        f
    }

    fn taxonomy() -> Taxonomy {
        Taxonomy::Fixed(["a", "b", "c"].iter().map(|n| Category::new(*n, "")).collect())
    }

    fn previous() -> PreviousCategories {
        PreviousCategories::from_formulae(&[
            formula("jq", Some(json!(["a", "b", "a", "c"]))),
            formula("old-name", Some(json!(["c"]))),
            formula("failed", Some(Value::Null)),
            formula("empty", Some(json!([]))),
            formula("unclassified", None),
            formula("stale", Some(json!(["a", "removed-category"]))),
        ])
    }

    #[test]
    fn loads_only_successful_classifications() {
        let prev = previous();
        assert_eq!(prev.len(), 3);
        assert_eq!(prev.catalog_size(), 6, "the catalog size counts unclassified formulae too");
        let mut names: Vec<_> = prev.by_name.into_keys().collect();
        names.sort();
        assert_eq!(names, ["jq", "old-name", "stale"]);
    }

    #[test]
    fn lookup_rules() {
        let prev = previous();
        let tax = taxonomy();

        assert_eq!(prev.lookup(&formula("jq", None), &tax, 3), Some(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(prev.lookup(&formula("jq", None), &tax, 1), Some(vec!["a".into()]), "trimmed to the maximum");

        let mut renamed = formula("new-name", None);
        renamed.oldnames = vec!["old-name".into()];
        assert_eq!(prev.lookup(&renamed, &tax, 3), Some(vec!["c".into()]), "found by former name");

        for name in ["failed", "empty", "unclassified", "brand-new"] {
            assert_eq!(prev.lookup(&formula(name, None), &tax, 3), None, "{name}");
        }

        assert_eq!(prev.lookup(&formula("stale", None), &tax, 3), None, "categories outside the taxonomy");
        assert_eq!(
            prev.lookup(&formula("stale", None), &Taxonomy::Open, 3),
            Some(vec!["a".into(), "removed-category".into()]),
            "an open taxonomy accepts any category"
        );
    }

    #[test]
    fn plans_which_formulae_to_classify() {
        let prev = previous();
        let (jq, fresh, stale) = (formula("jq", None), formula("fresh", None), formula("stale", None));
        let formulae = [&jq, &fresh, &stale];

        let (reused, to_classify) = plan(&formulae, Some(&prev), &HashSet::new(), &taxonomy(), 3);
        assert!(reused[0].is_some());
        assert_eq!(to_classify, [1, 2]);

        let (_, to_classify) = plan(&formulae, Some(&prev), &HashSet::from(["jq"]), &taxonomy(), 3);
        assert_eq!(to_classify, [0, 1, 2], "forced formulae are reclassified");

        let (reused, to_classify) = plan(&formulae, None, &HashSet::new(), &taxonomy(), 3);
        assert!(reused.iter().all(Option::is_none));
        assert_eq!(to_classify, [0, 1, 2], "without previous results, everything is classified");
    }

    #[test]
    fn annotation_keeps_metadata_and_adds_categories() {
        let mut jq = formula("jq", None);
        jq.desc = Some("Lightweight and flexible command-line JSON processor".into());
        jq.extra.insert("analytics".into(), json!({"install": {}}));

        let ok = annotate(&jq, Ok(vec!["data-processing".into()]));
        assert_eq!(ok["name"], "jq");
        assert_eq!(ok["desc"], "Lightweight and flexible command-line JSON processor");
        assert_eq!(ok["analytics"], json!({"install": {}}));
        assert_eq!(ok[CATEGORIES_FIELD], json!(["data-processing"]));
        assert!(ok.get(ERROR_FIELD).is_none());

        let failed = annotate(&jq, Err("boom".into()));
        assert_eq!(failed[CATEGORIES_FIELD], Value::Null);
        assert_eq!(failed[ERROR_FIELD], "boom");
    }

    #[test]
    fn reannotation_replaces_previous_annotations() {
        // Simulates using a previous catalog (where an earlier attempt failed) as the input.
        let mut x = formula("x", Some(Value::Null));
        x.extra.insert(ERROR_FIELD.into(), "old failure".into());

        let value = annotate(&x, Ok(vec!["games".into()]));
        assert_eq!(value[CATEGORIES_FIELD], json!(["games"]));
        assert!(value.get(ERROR_FIELD).is_none());
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(text.matches("\"categories\"").count(), 1, "no duplicate keys: {text}");
    }
}
