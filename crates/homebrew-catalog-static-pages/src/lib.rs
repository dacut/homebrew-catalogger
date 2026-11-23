//! Generates a static website listing Homebrew formulae by category, suitable for serving from S3 behind CloudFront.
//!
//! The site uses flat `.html` file names and relative links, so it works from an S3 REST origin (which does not map
//! `dir/` to `dir/index.html`) and under any path prefix. The stylesheet's file name includes a hash of its contents,
//! so it can be cached indefinitely. Given the site's URL, it also writes a sitemap and canonical links for crawlers.
//!
//! ```no_run
//! use homebrew_catalog_classifier::{default_categories, load_formulae};
//! use homebrew_catalog_static_pages::{BaseUrl, Site, SiteOptions, Timestamp, write_site};
//!
//! # fn run() -> anyhow::Result<()> {
//! let formulae = load_formulae("formula-catalogged.json")?;
//! let site = Site::build(&formulae, &default_categories());
//! let options = SiteOptions {
//!     base_url: Some(BaseUrl::parse("https://brew.example.com/")?),
//!     ..SiteOptions::new(Timestamp::generation_time()?)
//! };
//! write_site(&site, &options, "site".as_ref())?;
//! # Ok(())
//! # }
//! ```

mod crawl;
mod llms;
mod render;
pub mod site;
mod timestamp;

use {
    anyhow::{Context, Result, bail},
    homebrew_catalog_classifier::Category,
    render::{Page, SITE_CSS, Styles},
    std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
    },
};
pub use {
    crawl::BaseUrl,
    site::{CATEGORIES_FIELD, CategoryPage, Site, is_classified},
    timestamp::Timestamp,
};

/// The site title used when none is given.
pub const DEFAULT_TITLE: &str = "Homebrew Formula Catalog";

/// Cache-Control for files whose names include a hash of their contents (the stylesheet).
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Cache-Control for pages and other files that keep their names when their contents change.
pub const PAGE_CACHE_CONTROL: &str = "public, max-age=300";

/// The site icon, and bitmap renderings of it for browsers without SVG icon support and for iOS home screens.
/// Regenerate the bitmaps with `scripts/render-icons.sh` after changing the SVG.
const FAVICON_SVG: &str = include_str!("../assets/favicon.svg");
const FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../assets/apple-touch-icon.png");

/// How to generate the site.
#[derive(Clone, Debug)]
pub struct SiteOptions {
    /// The site title, shown in the header and on the landing page.
    pub title: String,
    /// The site's absolute URL. Without it, no sitemap or canonical links are written (both need absolute URLs).
    pub base_url: Option<BaseUrl>,
    /// When the site was generated, shown on the landing page.
    pub generated: Timestamp,
    /// Where the classified catalog JSON is published, relative to the site root (e.g. `formula-catalogged.json`),
    /// for `llms.txt` to point to. Without it, `llms.txt` doesn't mention the catalog.
    pub catalog_path: Option<String>,
}

impl SiteOptions {
    /// Options with the default title and no base URL.
    pub fn new(generated: Timestamp) -> Self {
        Self {
            title: DEFAULT_TITLE.to_string(),
            base_url: None,
            generated,
            catalog_path: None,
        }
    }
}

/// Reads a JSON file of categories, as `[{"name": "...", "title": "...", "description": "..."}, ...]`.
pub fn load_categories(path: &Path) -> Result<Vec<Category>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let categories: Vec<Category> =
        serde_json::from_str(&text).with_context(|| format!("parsing categories from {}", path.display()))?;
    let mut seen = HashSet::new();
    if let Some(dup) = categories.iter().find(|c| !seen.insert(c.name.as_str())) {
        bail!("{} lists category {:?} more than once", path.display(), dup.name);
    }
    Ok(categories)
}

/// Writes the site under `out`, returning the number of files written: the pages, the stylesheet (under `assets/`),
/// the icons, `robots.txt`, `llms.txt`, and, with a base URL, `sitemap.xml`.
pub fn write_site(site: &Site, options: &SiteOptions, out: &Path) -> Result<usize> {
    let css_name = format!("site-{:016x}.css", fnv1a(SITE_CSS.as_bytes()));
    let title = options.title.as_str();
    let base_url = options.base_url.as_ref();
    let generated = &options.generated;

    let mut written = 0;
    let mut write = |path: PathBuf, contents: &[u8]| -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        written += 1;
        Ok(())
    };

    write(out.join("assets").join(&css_name), SITE_CSS.as_bytes())?;
    write(out.join("favicon.svg"), FAVICON_SVG.as_bytes())?;
    write(out.join("favicon.ico"), FAVICON_ICO)?;
    write(out.join("apple-touch-icon.png"), APPLE_TOUCH_ICON)?;

    let root_css = format!("assets/{css_name}");
    let root = Page {
        site_title: title,
        root: "",
        home_href: "index.html",
        styles: Styles::Linked(&root_css),
        base_url,
    };
    let index = render::index_page(&root, site, "categories/", generated).into_string();
    write(out.join("index.html"), index.as_bytes())?;

    let nested_css = format!("../assets/{css_name}");
    let nested = Page {
        site_title: title,
        root: "../",
        home_href: "../index.html",
        styles: Styles::Linked(&nested_css),
        base_url,
    };
    for category in &site.categories {
        let html = render::category_page(&nested, site, category).into_string();
        write(out.join(crawl::category_path(&category.slug)), html.as_bytes())?;
    }
    // Served when someone browses the `categories/` folder. (No category's slug is `index`; see `Site::build`.)
    let categories_index = render::index_page(&nested, site, "", generated).into_string();
    write(out.join("categories").join("index.html"), categories_index.as_bytes())?;

    // Served for any missing URL, so it links to the site root by absolute URL: the base URL if there is one (which
    // keeps a site under a path prefix working), otherwise the host's root.
    let site_root = base_url.map_or("/", BaseUrl::as_str);
    let not_found = Page {
        site_title: title,
        root: site_root,
        home_href: site_root,
        styles: Styles::Inline,
        base_url,
    };
    write(out.join("404.html"), render::not_found_page(&not_found).into_string().as_bytes())?;

    write(out.join("robots.txt"), crawl::robots_txt(base_url).as_bytes())?;
    let llms = llms::llms_txt(site, title, base_url, options.catalog_path.as_deref(), generated);
    write(out.join("llms.txt"), llms.as_bytes())?;
    if let Some(base) = base_url {
        write(out.join("sitemap.xml"), crawl::sitemap_xml(base, site).as_bytes())?;
    }

    Ok(written)
}

/// 64-bit FNV-1a: a stable content hash for cache-busting file names (not for security).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use {super::*, crate::site::tests::formula, homebrew_catalog_classifier::default_categories};

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn writes_complete_site() {
        let formulae = vec![formula("jq", Some(&["data-processing"])), formula("x", Some(&["made-up"]))];
        let site = Site::build(&formulae, &default_categories());
        let dir = tempfile::tempdir().unwrap();

        let options = SiteOptions {
            title: "Catalog".into(),
            base_url: Some(BaseUrl::parse("https://brew.example.com").unwrap()),
            ..SiteOptions::new(Timestamp::from_unix(0))
        };
        assert_eq!(write_site(&site, &options, dir.path()).unwrap(), 12);

        for file in ["favicon.svg", "favicon.ico", "apple-touch-icon.png"] {
            assert!(dir.path().join(file).is_file(), "{file}");
        }
        assert!(fs::read(dir.path().join("favicon.ico")).unwrap().starts_with(&[0, 0, 1, 0]), "an ICO file");
        let robots = fs::read_to_string(dir.path().join("robots.txt")).unwrap();
        assert!(robots.contains("Sitemap: https://brew.example.com/sitemap.xml"));
        let sitemap = fs::read_to_string(dir.path().join("sitemap.xml")).unwrap();
        assert!(sitemap.contains("<loc>https://brew.example.com/categories/data-processing.html</loc>"));
        assert!(!sitemap.contains("categories/index.html") && !sitemap.contains("404"));
        let not_found = fs::read_to_string(dir.path().join("404.html")).unwrap();
        assert!(not_found.contains(r#"<a href="https://brew.example.com/">Browse all categories</a>"#));
        assert!(not_found.contains(r#"<link rel="icon" href="https://brew.example.com/favicon.svg""#));
        let llms = fs::read_to_string(dir.path().join("llms.txt")).unwrap();
        assert!(llms.contains("(https://brew.example.com/categories/data-processing.html)"));

        let css: Vec<_> = fs::read_dir(dir.path().join("assets")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(css.len(), 1);
        let css = css[0].to_str().unwrap();
        assert!(css.starts_with("site-") && css.ends_with(".css"));

        let index = fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(index.contains(&format!(r#"href="assets/{css}""#)));
        assert!(index.contains(r#"href="categories/data-processing.html""#));
        assert!(index.contains(r#"href="categories/made-up.html""#));

        let page = fs::read_to_string(dir.path().join("categories/data-processing.html")).unwrap();
        assert!(page.contains(&format!(r#"href="../assets/{css}""#)));
        assert!(page.contains(r#"href="../index.html""#));
        assert!(dir.path().join("404.html").is_file());

        let folder_index = fs::read_to_string(dir.path().join("categories/index.html")).unwrap();
        assert!(folder_index.contains(&format!(r#"href="../assets/{css}""#)));
        assert!(folder_index.contains(r#"href="data-processing.html""#));

        let stamp = r#"<time datetime="1970-01-01T00:00:00Z">"#;
        assert!(index.contains(stamp) && folder_index.contains(stamp), "index pages show the generation time");
        assert!(!page.contains(stamp), "category pages don't");
    }

    #[test]
    fn without_a_base_url_there_is_no_sitemap() {
        let formulae = vec![formula("jq", Some(&["data-processing"]))];
        let site = Site::build(&formulae, &default_categories());
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(write_site(&site, &SiteOptions::new(Timestamp::from_unix(0)), dir.path()).unwrap(), 10);
        assert!(!dir.path().join("sitemap.xml").exists());
        assert_eq!(fs::read_to_string(dir.path().join("robots.txt")).unwrap(), "User-agent: *\nAllow: /\n");
        assert!(!fs::read_to_string(dir.path().join("index.html")).unwrap().contains("canonical"));
        let not_found = fs::read_to_string(dir.path().join("404.html")).unwrap();
        assert!(not_found.contains(r#"<a href="/">Browse all categories</a>"#), "without a base URL, the host root");
    }

    #[test]
    fn rejects_duplicate_categories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        fs::write(&path, r#"[{"name":"a","description":""},{"name":"a","description":""}]"#).unwrap();
        assert!(load_categories(&path).unwrap_err().to_string().contains("more than once"));
    }
}
