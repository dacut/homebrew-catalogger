//! `llms.txt` (<https://llmstxt.org/>): a Markdown overview of the site for language models and AI agents, so they
//! can find the right category page in one request instead of crawling the site.

use {
    crate::{
        crawl::{BaseUrl, category_path},
        render::{count, formula_count},
        site::Site,
        timestamp::Timestamp,
    },
    std::fmt::Write as _,
};

/// Builds `llms.txt`. Links are absolute with a `base_url`, and relative to the site root (where the file is served)
/// otherwise. `catalog_path`, if given, is the published catalog JSON's path relative to the site root.
pub fn llms_txt(
    site: &Site,
    title: &str,
    base_url: Option<&BaseUrl>,
    catalog_path: Option<&str>,
    generated: &Timestamp,
) -> String {
    let link = |path: &str| match base_url {
        Some(base) => base.join(path),
        None => path.to_string(),
    };

    let mut text = String::new();
    let _ = writeln!(text, "# {}\n", single_line(title));
    let _ = writeln!(
        text,
        "> A catalog of the {} in Homebrew's homebrew/core tap, grouped into {} categories. Each category page lists \
         its formulae with their descriptions, categories, licenses, and homepage links.\n",
        formula_count(site.formula_count),
        count(site.categories.len())
    );
    let _ = writeln!(
        text,
        "Formula metadata comes from Homebrew's public API (https://formulae.brew.sh/api/formula.json). Categories \
         were assigned automatically by a language model and may contain mistakes; a formula can be in up to a few \
         categories, most relevant first. Each formula links to its page on formulae.brew.sh, which has installation \
         instructions and version details. Last regenerated {}.\n",
        generated.display()
    );

    text.push_str("## Categories\n\n");
    for category in &site.categories {
        let _ = write!(text, "- [{}]({})", link_text(&category.title), link(&category_path(&category.slug)));
        let formulae = formula_count(category.formulae.len());
        match category.description.as_deref().map(single_line) {
            Some(description) => {
                let _ = writeln!(text, ": {description} ({formulae})");
            }
            None => {
                let _ = writeln!(text, ": {formulae}");
            }
        }
    }

    if let Some(catalog) = catalog_path {
        let _ = write!(
            text,
            "\n## Optional\n\n- [Classified catalog (JSON)]({}): every formula's full Homebrew metadata (an array \
             in the format of formula.json), with a \"categories\" field added. Large; best downloaded and queried \
             with code rather than read directly.\n",
            link(catalog)
        );
    }
    text
}

/// Collapses whitespace (including newlines, which would break the Markdown structure) to single spaces.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escapes the characters that would end a Markdown link's text early.
fn link_text(text: &str) -> String {
    single_line(text).replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use {super::*, crate::site::tests::formula, homebrew_catalog_classifier::Category};

    fn site_fixture() -> Vec<homebrew_catalog_classifier::HomebrewFormulaMeta> {
        vec![formula("a", Some(&["games"])), formula("b", Some(&["games", "odd"])), formula("c", Some(&["audio"]))]
    }

    #[test]
    fn llms_txt_with_base_url_and_catalog() {
        let formulae = site_fixture();
        let known = vec![
            Category::new("games", "Games and\ngame engines").with_title("Games"),
            Category::new("audio", "Audio software").with_title("Audio [sound]"),
        ];
        let site = Site::build(&formulae, &known);
        let base = BaseUrl::parse("https://brew.example.com").unwrap();
        let text = llms_txt(
            &site,
            "Catalog",
            Some(&base),
            Some("formula-catalogged.json"),
            &Timestamp::from_unix(1_795_392_245),
        );

        assert!(text.starts_with(
            "# Catalog\n\n> A catalog of the 3 formulae in Homebrew's homebrew/core tap, grouped into 3 categories."
        ));
        assert!(text.contains("Last regenerated November 23, 2026, 00:04 UTC.\n"));
        assert!(text.contains(
            "## Categories\n\n\
             - [Audio \\[sound\\]](https://brew.example.com/categories/audio.html): Audio software (1 formula)\n\
             - [Games](https://brew.example.com/categories/games.html): Games and game engines (2 formulae)\n\
             - [Odd](https://brew.example.com/categories/odd.html): 1 formula\n"
        ));
        assert!(text.contains(
            "## Optional\n\n- [Classified catalog (JSON)](https://brew.example.com/formula-catalogged.json):"
        ));
    }

    #[test]
    fn llms_txt_with_relative_links() {
        let formulae = site_fixture();
        let site = Site::build(&formulae, &[]);
        let text = llms_txt(&site, "Catalog", None, None, &Timestamp::from_unix(0));
        assert!(text.contains("- [Games](categories/games.html): 2 formulae\n"));
        assert!(!text.contains("## Optional"), "no catalog link unless its path is given");
    }
}
