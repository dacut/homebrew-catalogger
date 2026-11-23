# homebrew-catalog-static-pages

Generates a static website listing Homebrew formulae by category, from a catalog written by
[`homebrew-catalog-classifier`](../homebrew-catalog-classifier-cli). The site is built to be served from S3 behind
CloudFront (see [`terraform/`](../../terraform)), but it's plain files that work from any static host, or straight from
disk.

This crate provides the `homebrew-catalog-static-pages` command and a library with the same functionality.

## Usage

```sh
homebrew-catalog-static-pages formula-catalogged.json -o site \
    --base-url https://brew.example.com/ --catalog-path formula-catalogged.json
```

| Option | Default | |
| --- | --- | --- |
| `-o, --output-dir DIR` | `site` | Where to write the site. Existing files with the same names are overwritten; nothing is deleted. |
| `--title TITLE` | `Homebrew Formula Catalog` | Shown in the header, the browser title, and `llms.txt`. |
| `--categories FILE` | built-in list | Category titles and descriptions, as `[{"name": "...", "title": "...", "description": "..."}, ...]`. |
| `--base-url URL` | | The site's absolute URL. Needed for `sitemap.xml` and canonical links, which must use absolute URLs; without it, neither is written. |
| `--catalog-path PATH` | | Where the catalog JSON is published, relative to the site root, so `llms.txt` can point AI agents to it. |

The landing page says when the site was generated. Set `SOURCE_DATE_EPOCH` (seconds since 1970) to use a fixed time,
for reproducible output.

## What it generates

| File | |
| --- | --- |
| `index.html` | The landing page: every category, with its description and formula count. |
| `categories/<name>.html` | One page per category, listing its formulae. |
| `categories/index.html` | A copy of the category list, served when someone browses the `categories/` folder. |
| `404.html` | The page for missing URLs. |
| `assets/site-<hash>.css` | The stylesheet. Its name includes a hash of its contents, so it can be cached indefinitely. |
| `favicon.svg`, `favicon.ico`, `apple-touch-icon.png` | The site icon, plus bitmap versions for browsers without SVG icon support and for iOS home screens. |
| `robots.txt` | Allows crawling, and points to the sitemap when there's a base URL. |
| `sitemap.xml` | The landing page and every category page (only with `--base-url`). |
| `llms.txt` | A Markdown overview of the site for AI agents ([llmstxt.org](https://llmstxt.org/)). |

Each formula's entry has:

- its name, linking to its page on formulae.brew.sh;
- its description;
- a badge for each of its categories, linking to that category's page;
- a license badge;
- a link to its homepage;
- a badge if Homebrew marks it deprecated or disabled.

Formulae without categories are listed on an "Uncategorized" page, so none are left out.

## Design

- **Accessible:**
  - Every text color meets WCAG 2.2 AA contrast in both light and dark mode, most at AAA.
  - There's a skip link, a heading for each formula, breadcrumbs, and visible focus outlines.
  - Badges have text a screen reader can read, such as "License: MIT".
  - Badges keep their outlines in Windows high-contrast mode.
- **Dark mode** follows the system setting, with no JavaScript. There's no JavaScript at all, so the site works under
  a strict Content-Security-Policy.
- **Safe with untrusted metadata:** pages are generated with [maud](https://maud.lambda.xyz/), which escapes every
  value, and homepage links are only made for `http` and `https` URLs.
- **Friendly to CDNs:** pages use flat `.html` file names and relative links, so the site works from an S3 REST
  origin, which doesn't map `dir/` to `dir/index.html`, and under any path prefix. The 404 page is the exception: it
  can be served at any URL, so it inlines its stylesheet and links home (and to the icons) through the base URL, or
  through `/` without one.

### Caching

When uploading, give `assets/` a long cache lifetime and everything else a short one. The library exports both values
as `IMMUTABLE_CACHE_CONTROL` and `PAGE_CACHE_CONTROL`. [`scripts/publish.sh`](../../scripts/publish.sh) does this for
you.

## Library

```rust
use homebrew_catalog_classifier::{default_categories, load_formulae};
use homebrew_catalog_static_pages::{BaseUrl, Site, SiteOptions, Timestamp, write_site};

let formulae = load_formulae("formula-catalogged.json")?;
let site = Site::build(&formulae, &default_categories());
let options = SiteOptions {
    base_url: Some(BaseUrl::parse("https://brew.example.com/")?),
    catalog_path: Some("formula-catalogged.json".into()),
    ..SiteOptions::new(Timestamp::generation_time()?)
};
write_site(&site, &options, "site".as_ref())?;
```

## Changing the icon

The bitmap icons are rendered from [`assets/favicon.svg`](assets/favicon.svg) and embedded in the binary. After
editing the SVG, run [`scripts/render-icons.sh`](../../scripts/render-icons.sh) (it needs Google Chrome and Python 3)
to regenerate `favicon.ico` and `apple-touch-icon.png`.
