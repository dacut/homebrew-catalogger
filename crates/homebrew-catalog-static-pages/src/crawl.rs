//! Files for search engines and other crawlers: `robots.txt` and `sitemap.xml`, both of which need the site's
//! absolute URL.

use {
    crate::site::Site,
    anyhow::{Result, bail},
    std::fmt::{self, Write as _},
};

/// The site's absolute base URL, e.g. `https://brew.example.com/`. Always ends with `/`.
///
/// `robots.txt` is only honored at the root of a host, so the site should be served from the root when crawlers
/// matter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseUrl(String);

impl BaseUrl {
    /// Validates an absolute `http` or `https` URL without a query or fragment, adding a trailing `/` if missing.
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let lower = url.to_ascii_lowercase();
        let Some(rest) =
            ["https://", "http://"].iter().find_map(|scheme| lower.starts_with(scheme).then(|| &url[scheme.len()..]))
        else {
            bail!("base URL {url:?} must start with https:// or http://");
        };
        if rest.split('/').next().is_none_or(str::is_empty) {
            bail!("base URL {url:?} has no host");
        }
        if rest.contains(['?', '#']) || rest.contains(char::is_whitespace) || rest.contains(['"', '<', '>']) {
            bail!("base URL {url:?} must not contain a query, fragment, spaces, or quotes");
        }
        Ok(Self(if url.ends_with('/') {
            url.to_string()
        } else {
            format!("{url}/")
        }))
    }

    /// The absolute URL of `path`, which is relative to the site root (e.g. `categories/games.html`).
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The path of a category's page, relative to the site root.
pub fn category_path(slug: &str) -> String {
    format!("categories/{slug}.html")
}

/// `robots.txt`: everything may be crawled. With a base URL, it also points crawlers at the sitemap (which must be
/// given as an absolute URL).
pub fn robots_txt(base_url: Option<&BaseUrl>) -> String {
    let mut robots = String::from("User-agent: *\nAllow: /\n");
    if let Some(base) = base_url {
        let _ = write!(robots, "\nSitemap: {}\n", base.join("sitemap.xml"));
    }
    robots
}

/// `sitemap.xml`, listing the landing page and every category page. The `categories/` folder index and the 404 page
/// are left out: the former duplicates the landing page, and the latter isn't content.
///
/// No `lastmod` dates are given: every page is regenerated on each run, so a regeneration date would claim changes
/// that didn't happen, and search engines ignore `lastmod` from sites where it's unreliable.
pub fn sitemap_xml(base: &BaseUrl, site: &Site) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    let urls = std::iter::once(base.join("")).chain(site.categories.iter().map(|c| base.join(&category_path(&c.slug))));
    for url in urls {
        let _ = writeln!(xml, "  <url><loc>{}</loc></url>", xml_escape(&url));
    }
    xml.push_str("</urlset>\n");
    xml
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use {super::*, crate::site::tests::formula, homebrew_catalog_classifier::Category};

    #[test]
    fn base_urls() {
        assert_eq!(BaseUrl::parse("https://brew.example.com").unwrap().as_str(), "https://brew.example.com/");
        assert_eq!(BaseUrl::parse(" https://x.example/catalog/ ").unwrap().as_str(), "https://x.example/catalog/");
        assert_eq!(BaseUrl::parse("HTTP://x.example").unwrap().join("a.html"), "HTTP://x.example/a.html");
        for bad in [
            "brew.example.com",
            "ftp://x.example/",
            "https://",
            "https:///path",
            "https://x.example/?q=1",
            "https://x.example/#top",
        ] {
            assert!(BaseUrl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn robots() {
        assert_eq!(robots_txt(None), "User-agent: *\nAllow: /\n");
        let base = BaseUrl::parse("https://brew.example.com").unwrap();
        assert_eq!(
            robots_txt(Some(&base)),
            "User-agent: *\nAllow: /\n\nSitemap: https://brew.example.com/sitemap.xml\n"
        );
    }

    #[test]
    fn sitemap() {
        let formulae = vec![formula("a", Some(&["games"])), formula("b", Some(&["audio"]))];
        let site = Site::build(&formulae, &[Category::new("games", ""), Category::new("audio", "")]);
        let base = BaseUrl::parse("https://brew.example.com/").unwrap();
        assert_eq!(
            sitemap_xml(&base, &site),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
             \x20 <url><loc>https://brew.example.com/</loc></url>\n\
             \x20 <url><loc>https://brew.example.com/categories/audio.html</loc></url>\n\
             \x20 <url><loc>https://brew.example.com/categories/games.html</loc></url>\n\
             </urlset>\n"
        );
        assert_eq!(xml_escape("a&b<'\">"), "a&amp;b&lt;&apos;&quot;&gt;");
    }
}
