use qbridge_ob11::message::{escape_param, escape_text, unescape_param, unescape_text};

#[test]
fn text_escaping_round_trips() {
    let text = "a&b[c]d,e";
    assert_eq!(escape_text(text), "a&amp;b&#91;c&#93;d,e");
    assert_eq!(unescape_text(&escape_text(text)), text);
}

#[test]
fn param_escaping_also_escapes_commas() {
    let value = "a&b[c]d,e";
    assert_eq!(escape_param(value), "a&amp;b&#91;c&#93;d&#44;e");
    assert_eq!(unescape_param(&escape_param(value)), value);
}

#[test]
fn ampersand_is_unescaped_last() {
    assert_eq!(unescape_text("&amp;#91;"), "&#91;");
    assert_eq!(unescape_param("&amp;#44;"), "&#44;");
}
