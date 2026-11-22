use cutad::dafont::{font_page, parse_catalog};

#[test]
fn catalog_reads_name_license_and_only_dafont_downloads() {
    let html = r#"
        <div class="lv1left"><a href="pixel-one.font">Pixel &amp; One</a></div>
        <div class="lv2right"><a href="./faq.php#copyright">Free for personal use</a></div>
        <div class="dlbox"><a class="dl" href="//dl.dafont.com/dl/?f=pixel_one">Download</a></div>
        <div class="lv1left"><a href="unsafe.font">Ignore me</a></div>
        <a class="dl" href="https://other.example/dl/?f=unsafe">Download</a>
    "#;
    let fonts = parse_catalog(html);
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].name, "Pixel & One");
    assert_eq!(fonts[0].page, "pixel-one");
    assert_eq!(fonts[0].license, "Free for personal use");
}

#[test]
fn font_pages_are_limited_to_dafont_font_urls() {
    assert_eq!(
        font_page("https://www.dafont.com/bebas-neue.font#section").as_deref(),
        Some("bebas-neue")
    );
    for input in [
        "https://www.dafont.com.evil.test/bebas-neue.font",
        "https://www.dafont.com/../dl.php",
        "https://www.dafont.com/test/bebas-neue.font",
        "https://www.dafont.com/evil%2fname.font",
        "https://www.dafont.com@test.example/a.font",
    ] {
        assert!(font_page(input).is_none(), "{input}");
    }
}
