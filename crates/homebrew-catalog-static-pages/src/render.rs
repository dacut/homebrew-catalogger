//! HTML templates. `maud` escapes every spliced value, so formula metadata cannot inject markup.

use {
    crate::{
        crawl::{BaseUrl, category_path},
        site::{CategoryPage, Site, formula_categories, homebrew_url, safe_homepage},
        timestamp::Timestamp,
    },
    homebrew_catalog_classifier::HomebrewFormulaMeta,
    maud::{DOCTYPE, Markup, PreEscaped, html},
};

/// The stylesheet, embedded in the binary.
pub const SITE_CSS: &str = include_str!("../assets/site.css");

/// How a page loads the stylesheet.
#[derive(Clone, Copy, Debug)]
pub enum Styles<'a> {
    /// A `<link>` to the stylesheet at this URL.
    Linked(&'a str),
    /// The stylesheet inlined in a `<style>` element (for pages served from arbitrary URLs, like the 404 page).
    Inline,
}

/// Settings shared by every page.
#[derive(Clone, Copy, Debug)]
pub struct Page<'a> {
    /// The site title, shown in the header and the browser title.
    pub site_title: &'a str,
    /// The path from the current page to the site root: `""`, `"../"`, or `"/"` for pages served at any URL.
    pub root: &'a str,
    /// The URL of the landing page, relative to the current page (or absolute).
    pub home_href: &'a str,
    /// How the stylesheet is loaded.
    pub styles: Styles<'a>,
    /// The site's absolute URL, for canonical links. Without it, pages have none.
    pub base_url: Option<&'a BaseUrl>,
}

/// Per-page `<head>` content.
#[derive(Debug, Default)]
struct Head<'a> {
    /// The page's own title, shown before the site title.
    title: Option<&'a str>,
    description: Option<&'a str>,
    /// The page's preferred URL, relative to the site root (`""` for the landing page).
    canonical_path: Option<String>,
    /// Asks search engines not to index the page.
    noindex: bool,
}

impl Page<'_> {
    /// Wraps `body` in the common page structure. `footer_note`, if given, is added to the footer.
    fn layout(&self, head: Head, body: Markup, footer_note: Option<Markup>) -> Markup {
        let full_title = match head.title {
            Some(title) => format!("{title} · {}", self.site_title),
            None => self.site_title.to_string(),
        };
        let canonical = self.base_url.zip(head.canonical_path).map(|(base, path)| base.join(&path));
        let root = self.root;
        html! {
            (DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1";
                    meta name="color-scheme" content="light dark";
                    @if let Some(description) = head.description {
                        meta name="description" content=(description);
                    }
                    @if head.noindex {
                        meta name="robots" content="noindex";
                    }
                    title { (full_title) }
                    @if let Some(canonical) = canonical {
                        link rel="canonical" href=(canonical);
                    }
                    // The ICO comes first so that browsers without SVG icon support use it; others prefer the SVG.
                    link rel="icon" href={ (root) "favicon.ico" } sizes="32x32";
                    link rel="icon" href={ (root) "favicon.svg" } type="image/svg+xml";
                    link rel="apple-touch-icon" href={ (root) "apple-touch-icon.png" };
                    @match self.styles {
                        Styles::Linked(href) => link rel="stylesheet" href=(href);,
                        Styles::Inline => style { (PreEscaped(SITE_CSS)) },
                    }
                }
                body {
                    a.skip-link href="#main" { "Skip to main content" }
                    header.site-header {
                        div.container {
                            a.site-title href=(self.home_href) { (self.site_title) }
                        }
                    }
                    main #main .container tabindex="-1" {
                        (body)
                    }
                    footer.site-footer {
                        div.container {
                            p {
                                "Formula information from "
                                a href="https://formulae.brew.sh/" { "Homebrew Formulae" }
                                ". Categories were assigned automatically and may contain mistakes."
                            }
                            @if let Some(note) = footer_note {
                                (note)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A page listing every category, with a footer note saying when the site was `generated`. `category_dir` is the
/// path from this page to the category pages: `categories/` for the landing page, or empty for the copy served as
/// the `categories/` folder's index.
pub fn index_page(page: &Page, site: &Site, category_dir: &str, generated: &Timestamp) -> Markup {
    let description = format!(
        "Browse {} Homebrew formulae across {} categories.",
        count(site.formula_count),
        count(site.categories.len())
    );
    page.layout(
        Head {
            description: Some(&description),
            // Both copies (the landing page and the `categories/` folder index) point to the landing page.
            canonical_path: Some(String::new()),
            ..Head::default()
        },
        html! {
            h1 { (page.site_title) }
            p.page-intro { (description) }
            ul.category-grid {
                @for category in &site.categories {
                    li.category-card {
                        h2 {
                            a href={ (category_dir) (category.slug) ".html" } { (category.title) }
                        }
                        @if let Some(description) = &category.description {
                            p { (description) }
                        }
                        p.count { (formula_count(category.formulae.len())) }
                    }
                }
            }
        },
        Some(html! {
            p.generated {
                "Site last regenerated "
                time datetime=(generated.iso8601()) { (generated.display()) }
                "."
            }
        }),
    )
}

/// The page for one category, listing its formulae. Sibling category pages are expected in the same directory.
pub fn category_page(page: &Page, site: &Site, category: &CategoryPage) -> Markup {
    page.layout(
        Head {
            title: Some(&category.title),
            description: category.description.as_deref(),
            canonical_path: Some(category_path(&category.slug)),
            ..Head::default()
        },
        html! {
            nav aria-label="Breadcrumb" {
                ol.breadcrumb {
                    li { a href=(page.home_href) { "All categories" } }
                    li aria-current="page" { (category.title) }
                }
            }
            h1 { (category.title) }
            p.page-intro {
                @if let Some(description) = &category.description {
                    (description) ". "
                }
                (formula_count(category.formulae.len())) "."
            }
            ul.formula-list {
                @for formula in &category.formulae {
                    li.formula { (formula_entry(site, category, formula)) }
                }
            }
        },
        None,
    )
}

fn formula_entry(site: &Site, current: &CategoryPage, formula: &HomebrewFormulaMeta) -> Markup {
    let categories: Vec<_> = formula_categories(formula).into_iter().filter_map(|name| site.category(&name)).collect();
    html! {
        article {
            h2 { a href=(homebrew_url(formula)) { (formula.name) } }
            @if let Some(desc) = formula.desc.as_deref().filter(|d| !d.trim().is_empty()) {
                p.formula-desc { (desc) }
            }
            div.formula-meta {
                @if !categories.is_empty() {
                    ul.badges aria-label="Categories" {
                        @for category in &categories {
                            li {
                                a.badge.badge-category
                                    href={ (category.slug) ".html" }
                                    aria-current=[(category.name == current.name).then_some("page")]
                                    { (category.title) }
                            }
                        }
                    }
                }
                span.badge.badge-license {
                    span.visually-hidden { "License: " }
                    (formula.license.as_deref().filter(|l| !l.trim().is_empty()).unwrap_or("Unknown license"))
                }
                @if formula.disabled {
                    span.badge.badge-disabled { "Disabled" }
                } @else if formula.deprecated {
                    span.badge.badge-deprecated { "Deprecated" }
                }
                @if let Some(homepage) = safe_homepage(formula) {
                    a.homepage href=(homepage) {
                        "Homepage" span.visually-hidden { " for " (formula.name) }
                    }
                }
            }
        }
    }
}

/// The page served for missing URLs. It inlines its styles and links home by absolute path, since it can be served
/// at any URL.
pub fn not_found_page(page: &Page) -> Markup {
    page.layout(
        Head {
            title: Some("Page not found"),
            noindex: true,
            ..Head::default()
        },
        html! {
            h1 { "Page not found" }
            p { "There is nothing at this address. " a href=(page.home_href) { "Browse all categories" } "." }
        },
        None,
    )
}

pub(crate) fn formula_count(n: usize) -> String {
    format!(
        "{} {}",
        count(n),
        if n == 1 {
            "formula"
        } else {
            "formulae"
        }
    )
}

/// Formats a number with thousands separators.
pub(crate) fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use {super::*, crate::site::tests::formula, homebrew_catalog_classifier::Category};

    const PAGE: Page = Page {
        site_title: "Catalog",
        root: "../",
        home_href: "../index.html",
        styles: Styles::Linked("../assets/site.css"),
        base_url: None,
    };

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(8735), "8,735");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(formula_count(1), "1 formula");
        assert_eq!(formula_count(2007), "2,007 formulae");
    }

    #[test]
    fn category_page_content() {
        let mut jq = formula("jq", Some(&["data-processing", "text-processing"]));
        jq.desc = Some("JSON <processor> & \"friends\"".into());
        jq.license = Some("MIT".into());
        jq.homepage = Some("https://jqlang.org/".into());
        let mut evil = formula("evil", Some(&["data-processing"]));
        evil.homepage = Some("javascript:alert(1)".into());
        evil.deprecated = true;
        let formulae = vec![jq, evil];
        let known = vec![
            Category::new("data-processing", "Structured data").with_title("Data Processing"),
            Category::new("text-processing", "Text").with_title("Text Processing"),
        ];
        let site = Site::build(&formulae, &known);
        let html = category_page(&PAGE, &site, site.category("data-processing").unwrap()).into_string();

        assert!(html.starts_with("<!DOCTYPE html><html lang=\"en\">"));
        assert!(html.contains("<title>Data Processing · Catalog</title>"));
        assert!(html.contains(r#"<link rel="stylesheet" href="../assets/site.css">"#));
        assert!(html.contains(r#"<a href="https://formulae.brew.sh/formula/jq">jq</a>"#));
        assert!(html.contains("JSON &lt;processor&gt; &amp; &quot;friends&quot;"), "metadata is escaped");
        assert!(html.contains(
            r#"<a class="badge badge-category" href="data-processing.html" aria-current="page">Data Processing</a>"#
        ));
        assert!(html.contains(r#"<a class="badge badge-category" href="text-processing.html">Text Processing</a>"#));
        assert!(html.contains(r#"<span class="visually-hidden">License: </span>MIT</span>"#));
        assert!(html.contains("Unknown license"));
        assert!(html.contains(r#"<span class="badge badge-deprecated">Deprecated</span>"#));
        assert!(html.contains(r#"<a class="homepage" href="https://jqlang.org/">Homepage"#));
        assert!(!html.contains("javascript:"), "unsafe homepages are not linked");
        assert!(html.find(">evil<").unwrap() < html.find(">jq<").unwrap(), "formulae are sorted by name");

        assert!(html.contains(r#"<link rel="icon" href="../favicon.ico" sizes="32x32">"#));
        assert!(html.contains(r#"<link rel="icon" href="../favicon.svg" type="image/svg+xml">"#));
        assert!(html.contains(r#"<link rel="apple-touch-icon" href="../apple-touch-icon.png">"#));
        assert!(!html.contains("canonical"), "no canonical link without a base URL");
        assert!(!html.contains("noindex"));

        let base = BaseUrl::parse("https://brew.example.com").unwrap();
        let with_base = Page {
            base_url: Some(&base),
            ..PAGE
        };
        let html = category_page(&with_base, &site, site.category("data-processing").unwrap()).into_string();
        assert!(
            html.contains(r#"<link rel="canonical" href="https://brew.example.com/categories/data-processing.html">"#)
        );
    }

    #[test]
    fn index_and_not_found_pages() {
        let formulae = vec![formula("a", Some(&["games"]))];
        let site = Site::build(&formulae, &[Category::new("games", "Games and engines")]);
        let index = Page {
            home_href: "index.html",
            ..PAGE
        };
        let generated = Timestamp::from_unix(1_795_392_245);
        let html = index_page(&index, &site, "categories/", &generated).into_string();
        assert!(html.contains(r#"<a href="categories/games.html">Games</a>"#));
        assert!(index_page(&PAGE, &site, "", &generated).into_string().contains(r#"<a href="games.html">Games</a>"#));
        assert!(html.contains("<p>Games and engines</p>"));
        assert!(html.contains("1 formula"));
        assert!(html.contains(
            r#"<p class="generated">Site last regenerated <time datetime="2026-11-23T00:04:05Z">November 23, 2026, 00:04 UTC</time>.</p>"#
        ));
        let footer = &html[html.find("<footer").unwrap()..];
        assert!(footer.contains(r#"class="generated""#), "the note is in the footer");

        let not_found = Page {
            home_href: "/",
            styles: Styles::Inline,
            ..PAGE
        };
        let html = not_found_page(&not_found).into_string();
        assert!(html.contains("<style>") && html.contains("--badge-license-bg"));
        assert!(html.contains(r#"<a href="/">Browse all categories</a>"#));
        assert!(!html.contains("<p class=\"generated\">"), "only index pages show the generation time");
        assert!(html.contains(r#"<meta name="robots" content="noindex">"#), "the 404 page isn't indexed");

        // Both copies of the category list name the landing page as canonical.
        let base = BaseUrl::parse("https://brew.example.com/").unwrap();
        for category_dir in ["categories/", ""] {
            let page = Page {
                base_url: Some(&base),
                ..PAGE
            };
            let html = index_page(&page, &site, category_dir, &generated).into_string();
            assert!(html.contains(r#"<link rel="canonical" href="https://brew.example.com/">"#), "{category_dir:?}");
        }
    }
}
