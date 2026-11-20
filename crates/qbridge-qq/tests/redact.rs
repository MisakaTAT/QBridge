use qbridge_qq::redact;
use serde_json::json;

#[test]
fn hides_credentials_and_media_material() {
    let payload = json!({
        "content": "hello",
        "clientSecret": "SECRET-VALUE",
        "access_token": "TOKEN-VALUE",
        "Authorization": "QQBot TOKEN-VALUE",
        "file_data": "FILE-DATA-VALUE",
        "file_info": "FILE-INFO-VALUE",
        "nested": {"signature": "SIGNATURE-VALUE", "password": "PASSWORD-VALUE", "cookie": "COOKIE-VALUE"},
    });
    let output = redact::value(&payload).to_string();
    assert!(output.contains("hello"));
    for secret in [
        "SECRET-VALUE",
        "TOKEN-VALUE",
        "FILE-DATA-VALUE",
        "FILE-INFO-VALUE",
        "SIGNATURE-VALUE",
        "PASSWORD-VALUE",
        "COOKIE-VALUE",
    ] {
        assert!(!output.contains(secret), "{secret} leaked in {output}");
    }
    assert!(output.contains("[REDACTED]"));
}

#[test]
fn hides_urls_and_base64_even_in_plain_strings() {
    assert_eq!(redact::text("https://example.com/a.png"), "[REDACTED]");
    assert_eq!(redact::text("base64://QUJD"), "[REDACTED]");
    assert_eq!(redact::text("plain text"), "plain text");
}

#[test]
fn truncates_long_values_and_nesting() {
    let long = "x".repeat(2000);
    let output = redact::value(&json!({"text": long})).to_string();
    assert!(output.contains("[TRUNCATED]"));
    assert!(output.len() < 4096);

    let mut nested = json!(1);
    for _ in 0..12 {
        nested = json!({"a": nested});
    }
    let output = redact::value(&nested).to_string();
    assert!(output.contains("[TRUNCATED]"));
}

#[test]
fn maps_keys_case_insensitively() {
    let output = redact::value(&json!({"Client_Secret": "x", "FILE_DATA": "y"})).to_string();
    assert!(!output.contains("\"x\""));
    assert!(!output.contains("\"y\""));
}
