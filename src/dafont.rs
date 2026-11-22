//! User-initiated browsing of DaFont's public category and font pages.

use crate::fonts::{self, FontRef};
use anyhow::{Context, Result, ensure};
use scraper::{Html, Selector};
use std::io::Write;
use std::time::Duration;

const WEBSITE: &str = "https://www.dafont.com";
const DOWNLOAD: &str = "//dl.dafont.com/dl/?f=";

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub page: String,
    pub license: String,
    archive: String,
}

impl Entry {
    pub fn listing(&self) -> String {
        format!("{WEBSITE}/{}.font", self.page)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    Popular,
    New,
    Comic,
    Horror,
    Pixel,
    Brush,
    SansSerif,
}

impl Category {
    pub const ALL: [Self; 7] = [
        Self::Popular,
        Self::New,
        Self::Comic,
        Self::Horror,
        Self::Pixel,
        Self::Brush,
        Self::SansSerif,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Popular => "Popular",
            Self::New => "New",
            Self::Comic => "Comic",
            Self::Horror => "Horror",
            Self::Pixel => "Pixel",
            Self::Brush => "Brush",
            Self::SansSerif => "Sans serif",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Popular => "/top.php?period=2",
            Self::New => "/new.php",
            Self::Comic => "/theme.php?cat=102",
            Self::Horror => "/theme.php?cat=110",
            Self::Pixel => "/bitmap.php",
            Self::Brush => "/theme.php?cat=604",
            Self::SansSerif => "/theme.php?cat=501",
        }
    }
}

pub fn browse(category: Category, page: u32) -> Result<Vec<Entry>> {
    let path = category.path();
    let separator = if path.contains('?') { '&' } else { '?' };
    fetch_entries(
        &format!("{WEBSITE}{path}{separator}page={}", page.max(1)),
        None,
    )
}

pub fn lookup(url: &str) -> Result<Vec<Entry>> {
    let page = font_page(url).context("Paste a DaFont link ending in .font.")?;
    fetch_entries(&format!("{WEBSITE}/{page}.font"), Some(&page))
}

pub fn font_page(input: &str) -> Option<String> {
    let path = input.trim().strip_prefix("https://")?;
    let (domain, page) = path.split_once('/')?;
    if !matches!(domain, "www.dafont.com" | "dafont.com") {
        return None;
    }
    let slug = page.split(['?', '#']).next()?.strip_suffix(".font")?;
    valid_slug(slug).then(|| slug.to_owned())
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && slug
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_'))
}

fn fetch_entries(url: &str, page: Option<&str>) -> Result<Vec<Entry>> {
    let response = request(url).context("Could not load this page from DaFont.")?;
    let bytes = fonts::bounded(response.into_reader(), 1024 * 1024)?;
    let (html, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
    let fonts = parse_html(&html, page);
    ensure!(
        !fonts.is_empty(),
        "This DaFont page returned no available fonts."
    );
    Ok(fonts)
}

pub fn parse_catalog(html: &str) -> Vec<Entry> {
    parse_html(html, None)
}

fn parse_html(html: &str, page: Option<&str>) -> Vec<Entry> {
    let doc = Html::parse_document(html);
    let name = if page.is_some() {
        ".lv1left > strong"
    } else {
        ".lv1left a[href$='.font']"
    };
    let row = Selector::parse(&format!("{name}, .lv2right a[href$='#copyright'], a.dl"))
        .expect("valid font selectors");
    let mut pending = None;
    let mut fonts = Vec::new();
    for item in doc.select(&row).take(500) {
        let href = item.attr("href").unwrap_or_default();
        let name_page = if item.value().name() == "strong" {
            page
        } else {
            href.strip_prefix("./")
                .unwrap_or(href)
                .strip_suffix(".font")
        };
        if let Some(page) = name_page {
            pending = valid_slug(page).then(|| Entry {
                name: item
                    .text()
                    .collect::<String>()
                    .trim()
                    .chars()
                    .take(120)
                    .collect(),
                page: page.to_owned(),
                license: "See font listing".into(),
                archive: String::new(),
            });
        } else if href.ends_with("#copyright") {
            if let Some(font) = &mut pending {
                font.license = item
                    .text()
                    .collect::<String>()
                    .trim()
                    .chars()
                    .take(100)
                    .collect();
            }
        } else if let Some(mut font) = pending.take()
            && let Some(archive) = href.trim_start_matches("https:").strip_prefix(DOWNLOAD)
            && valid_slug(archive)
            && !font.name.is_empty()
        {
            font.archive = archive.to_owned();
            fonts.push(font);
        }
    }
    fonts
}

fn request(url: &str) -> Result<ureq::Response> {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .redirects(0)
        .build()
        .get(url)
        .call()
        .map_err(anyhow::Error::from)
}

pub fn install(entry: &Entry) -> Result<Vec<FontRef>> {
    let response = request(&format!("https:{DOWNLOAD}{}", entry.archive))
        .context("Could not download this DaFont archive.")?;
    let bytes = fonts::bounded(response.into_reader(), fonts::MAX_ARCHIVE as usize)?;
    let folder = tempfile::tempdir()?;
    let staged = folder.path().join("font.zip");
    std::fs::write(&staged, &bytes)?;
    let library = fonts::user_library()?;
    let sources = library.join("sources");
    std::fs::create_dir_all(&sources)?;
    // Keep the source archive so bundled author notices remain available to the user.
    let mut source = tempfile::NamedTempFile::new_in(&sources)?;
    source.write_all(&bytes)?;
    let installed = fonts::import_into(&staged, &library)?;
    source.persist(sources.join(format!("dafont-{}.zip", entry.archive)))?;
    fonts::refresh();
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::parse_html;

    #[test]
    fn direct_font_page_uses_the_unlinked_heading() {
        let page = r#"
          <div class="lv1left"><strong>Bebas Neue</strong></div>
          <div class="lv2right"><a href="./faq.php#copyright">Public domain / GPL / OFL</a></div>
          <div class="dlbox"><a class="dl" href="//dl.dafont.com/dl/?f=bebas_neue">Download</a></div>
        "#;
        let fonts = parse_html(page, Some("bebas-neue"));
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].name, "Bebas Neue");
        assert_eq!(fonts[0].listing(), "https://www.dafont.com/bebas-neue.font");
    }
}
