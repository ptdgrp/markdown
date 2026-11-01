use ptdgrp_markdown::{Parser, ParserOptions};

#[test]
fn test_smart_punctuation_em_dash() {
    let parser = Parser::new_with_options(
        "em---em",
        ParserOptions::default().enabled_smart_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>em—em</p>");
}

#[test]
fn test_smart_punctuation_en_dash() {
    let parser = Parser::new_with_options(
        "en--en",
        ParserOptions::default().enabled_smart_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>en–en</p>");
}

#[test]
fn test_smart_punctuation_ellipsis() {
    let parser = Parser::new_with_options(
        "Ellipses...and...and....",
        ParserOptions::default().enabled_smart_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>Ellipses…and…and….</p>");
}

#[test]
fn test_smart_punctuation_mixed() {
    let parser = Parser::new_with_options(
        "one-\ntwo--\nthree---",
        ParserOptions::default().enabled_smart_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>one-\ntwo–\nthree—</p>");
}

#[test]
fn test_smart_punctuation_disabled() {
    let parser = Parser::new("em---em and...");
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>em---em and...</p>");
}

#[test]
fn test_chinese_punctuation_normalization() {
    let parser = Parser::new_with_options(
        "你好,世界!",
        ParserOptions::default()
            .enabled_cjk_autocorrect()
            .enabled_normalize_chinese_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>你好，世界！</p>");
}

#[test]
fn test_cjk_spacing_with_smart_punct() {
    let parser = Parser::new_with_options(
        "Hello世界---test",
        ParserOptions::default()
            .enabled_cjk_autocorrect()
            .enabled_smart_punctuation(),
    );
    let ast = parser.parse().unwrap();
    let html = ast.to_html();
    assert_eq!(html, "<p>Hello 世界—test</p>");
}

#[test]
fn test_chinese_punctuation_preserves_numeric_separators() {
    for (input, expected) in [
        (
            "金额28,245.51元,已支付.",
            "<p>金额 28,245.51 元，已支付。</p>",
        ),
        ("# 金额28,245.51元.", "<h1>金额 28,245.51 元。</h1>"),
        (
            "金额**28,245.51**元.",
            "<p>金额<strong>28,245.51</strong>元。</p>",
        ),
    ] {
        let ast = Parser::new_with_options(
            input,
            ParserOptions::default()
                .enabled_cjk_autocorrect()
                .enabled_normalize_chinese_punctuation(),
        )
        .parse()
        .unwrap();
        assert_eq!(ast.to_html(), expected, "{input}");
    }
}
