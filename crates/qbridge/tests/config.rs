use qbridge::config::{Config, Error, IntentName, MessageFormatConfig, QqSource};
use qbridge_ob11::MessageFormat;
use qbridge_qq::{Intents, ShardConfig};

const SAMPLE: &str = r#"
self_id = 10000
access_token = "ob11-token"
message_format = "string"
heartbeat_ms = 5000

[http]
bind = "127.0.0.1:5700"

[forward_ws]

[[reverse_ws]]
url = "ws://127.0.0.1:6700/onebot/v11/ws"
role = "universal"
reconnect_ms = 3000

[[http_post]]
url = "http://127.0.0.1:5701/"
secret = "post-secret"
timeout_ms = 5000

[qq]
app_id = "1234"
secret = "literal-secret"
source = "gateway"
intents = ["group_and_c2c", "group_member", "interaction"]

[storage]
path = "data/test.sqlite"

[limits]
max_body_bytes = 1048576
max_ws_bytes = 1048576
queue_capacity = 256
rate_limit_ms = 500
"#;

#[test]
fn parses_and_resolves_the_transport_sections() {
    let config = Config::parse(SAMPLE).unwrap();
    let resolved = config.resolve().unwrap();
    assert_eq!(resolved.qq.app_id, "1234");
    assert_eq!(resolved.qq.secret, "literal-secret");
    assert_eq!(resolved.qq.source, QqSource::Gateway);
    assert_eq!(resolved.qq.intents, Intents::default());
    assert_eq!(resolved.storage_path.to_str().unwrap(), "data/test.sqlite");
    assert_eq!(resolved.self_id, 10_000);
    assert_eq!(resolved.access_token.as_deref(), Some("ob11-token"));
    assert_eq!(resolved.message_format, MessageFormat::String);
    assert_eq!(resolved.heartbeat_ms, 5000);
    assert_eq!(resolved.http_bind.unwrap().to_string(), "127.0.0.1:5700");
    assert_eq!(
        resolved.forward_ws_bind.unwrap().to_string(),
        "127.0.0.1:5700"
    );
    assert_eq!(resolved.reverse_ws.len(), 1);
    assert_eq!(
        resolved.reverse_ws[0].url.as_str(),
        "ws://127.0.0.1:6700/onebot/v11/ws"
    );
    assert_eq!(resolved.http_post.len(), 1);
    assert_eq!(resolved.http_post[0].secret.as_deref(), Some("post-secret"));
    assert_eq!(resolved.limits.queue_capacity, 256);
    assert_eq!(resolved.identity.mode, qbridge::store::IdentityIdMode::Hash);
    assert_eq!(resolved.identity.salt, "qbridge");
    assert_eq!(resolved.identity.bits, 52);
}

#[test]
fn applies_defaults_for_optional_sections() {
    let config = Config::parse(
        r#"
self_id = 1

[forward_ws]

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap();
    let resolved = config.resolve().unwrap();
    assert_eq!(resolved.qq.source, QqSource::Gateway);
    assert_eq!(resolved.qq.intents, Intents::default());
    assert_eq!(resolved.qq.shards, ShardConfig::Auto);
    assert_eq!(
        resolved.storage_path.to_str().unwrap(),
        "data/qbridge.sqlite"
    );
    assert_eq!(resolved.cache.inbound_ttl_secs, 3600);
    assert_eq!(resolved.cache.sent_ttl_secs, 3600);
    assert_eq!(resolved.cache.profile_ttl_secs, 600);
    assert_eq!(resolved.cache.stream_ttl_secs, 300);
    assert_eq!(resolved.cache.cleanup_interval.as_secs(), 60);
    assert_eq!(resolved.message_format, MessageFormat::Array);
    assert_eq!(resolved.heartbeat_ms, 15_000);
    assert!(resolved.http_bind.is_none());
    assert_eq!(
        resolved.forward_ws_bind.unwrap().to_string(),
        "127.0.0.1:5700"
    );
    assert_eq!(resolved.limits.max_body_bytes, 1_048_576);
    assert!(resolved.reverse_ws.is_empty());
    assert!(resolved.http_post.is_empty());
    assert_eq!(config.message_format, MessageFormatConfig::Array);
}

#[test]
fn forward_websocket_is_disabled_when_its_section_is_omitted() {
    let config = Config::parse(
        r#"
self_id = 1

[http]
bind = "127.0.0.1:5700"

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap();
    let resolved = config.resolve().unwrap();
    assert!(resolved.http_bind.is_some());
    assert!(resolved.forward_ws_bind.is_none());
}

#[test]
fn supports_forward_ws_without_http_api() {
    let config = Config::parse(
        r#"
self_id = 1

[forward_ws]
bind = "127.0.0.1:5701"

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap();
    let resolved = config.resolve().unwrap();
    assert!(resolved.http_bind.is_none());
    assert_eq!(
        resolved.forward_ws_bind.unwrap().to_string(),
        "127.0.0.1:5701"
    );
}

#[test]
fn rejects_unknown_fields() {
    let error = Config::parse(
        r#"
self_id = 1
unexpected = true

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Toml(_)));
}

#[test]
fn rejects_legacy_ob11_section() {
    let error = Config::parse(
        r#"
[qq]
app_id = "1"
secret = "s"

[ob11]
self_id = 1
"#,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Toml(_)));
}

#[test]
fn resolves_environment_references_and_reports_missing_variables() {
    let config = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = { env = "QBRIDGE_TEST_MISSING_SECRET_987654" }
"#,
    )
    .unwrap();
    assert!(matches!(
        config.resolve(),
        Err(Error::MissingEnv(name)) if name == "QBRIDGE_TEST_MISSING_SECRET_987654"
    ));
}

#[test]
fn validates_source_and_limits() {
    let webhook_without_config = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = "s"
source = "webhook"
"#,
    )
    .unwrap();
    assert!(matches!(
        webhook_without_config.resolve(),
        Err(Error::Invalid(message)) if message.contains("qq.webhook")
    ));

    let bad_self_id = Config::parse(
        r#"
self_id = 0

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap();
    assert!(matches!(
        bad_self_id.resolve(),
        Err(Error::Invalid(message)) if message.contains("self_id")
    ));

    let no_listener = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = "s"
"#,
    )
    .unwrap();
    assert!(matches!(
        no_listener.resolve(),
        Err(Error::Invalid(message)) if message.contains("OB11 transport")
    ));

    let bad_queue = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = "s"

[limits]
queue_capacity = 0
"#,
    )
    .unwrap();
    assert!(matches!(
        bad_queue.resolve(),
        Err(Error::Invalid(message)) if message.contains("limits")
    ));
}

#[test]
fn accepts_outbound_transports_without_an_ob11_listener() {
    let account = "self_id = 1\n[qq]\napp_id = \"1\"\nsecret = \"s\"\n";
    for transport in [
        "[[reverse_ws]]\nurl = \"ws://127.0.0.1:6700/ws\"\n",
        "[[http_post]]\nurl = \"http://127.0.0.1:5701/\"\n",
    ] {
        let config = Config::parse(&format!("{account}{transport}"))
            .unwrap()
            .resolve()
            .unwrap();
        assert!(config.http_bind.is_none());
        assert!(config.forward_ws_bind.is_none());
        assert_eq!(config.reverse_ws.len() + config.http_post.len(), 1);
    }
}

#[test]
fn validates_websocket_and_post_urls() {
    let config = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = "s"

[[reverse_ws]]
url = "http://127.0.0.1/ws"
"#,
    )
    .unwrap();
    assert!(matches!(config.resolve(), Err(Error::Invalid(_))));

    let config = Config::parse(
        r#"
self_id = 1

[qq]
app_id = "1"
secret = "s"

[[http_post]]
url = "ws://127.0.0.1/"
"#,
    )
    .unwrap();
    assert!(matches!(config.resolve(), Err(Error::Invalid(_))));

    let config = Config::parse(
        r#"
self_id = 1

[http]
bind = "127.0.0.1:5700"

[qq]
app_id = "1"
secret = "s"

[[reverse_ws]]
url = "ws://127.0.0.1/ws"
role = "api"
"#,
    )
    .unwrap();
    let resolved = config.resolve().unwrap();
    assert_eq!(
        resolved.reverse_ws[0].role,
        qbridge::server::session::Role::Api
    );
    assert_eq!(resolved.reverse_ws[0].reconnect.as_millis(), 3000);
}

#[test]
fn secret_debug_is_redacted() {
    let config = Config::parse(SAMPLE).unwrap();
    let debug = format!("{:?}", config.qq.secret);
    assert!(!debug.contains("literal-secret"));
    assert!(debug.contains("REDACTED"));
    assert_eq!(config.qq.intents[0], IntentName::GroupAndC2c);
    assert_eq!(config.qq.intents[1], IntentName::GroupMember);
    assert_eq!(config.qq.intents[2], IntentName::Interaction);
}

#[test]
fn accepts_sequence_identity_mode() {
    let config = Config::parse(
        r#"
self_id = 1

[forward_ws]

[qq]
app_id = "1"
secret = "s"

[identity]
mode = "sequence"
"#,
    )
    .unwrap();
    assert_eq!(
        config.resolve().unwrap().identity.mode,
        qbridge::store::IdentityIdMode::Sequence
    );
    assert!(matches!(
        Config::parse(
            r#"
self_id = 1

[forward_ws]

[qq]
app_id = "1"
secret = "s"

[identity]
bits = 64
"#,
        )
        .unwrap()
        .resolve(),
        Err(Error::Invalid(message)) if message.contains("identity.bits")
    ));
}

#[test]
fn parses_and_validates_gateway_shards_and_cache_policy() {
    let count = Config::parse(
        r#"
self_id = 1

[forward_ws]

[qq]
app_id = "1"
secret = "s"
shards = 4

[cache]
inbound_ttl_secs = 7200
sent_ttl_secs = 1800
profile_ttl_secs = 1200
stream_ttl_secs = 120
cleanup_interval_secs = 30
"#,
    )
    .unwrap()
    .resolve()
    .unwrap();
    assert_eq!(count.qq.shards, ShardConfig::Count(4));
    assert_eq!(count.cache.inbound_ttl_secs, 7200);
    assert_eq!(count.cache.sent_ttl_secs, 1800);
    assert_eq!(count.cache.profile_ttl_secs, 1200);
    assert_eq!(count.cache.stream_ttl_secs, 120);
    assert_eq!(count.cache.cleanup_interval.as_secs(), 30);

    let single = Config::parse(
        r#"
self_id = 1

[forward_ws]

[qq]
app_id = "1"
secret = "s"
shards = [2, 4]
"#,
    )
    .unwrap()
    .resolve()
    .unwrap();
    assert_eq!(single.qq.shards, ShardConfig::Single { id: 2, count: 4 });

    for invalid in [
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\nshards = 0\n",
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\nshards = [4, 4]\n",
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\nshards = [0, 0]\n",
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\n[cache]\nsent_ttl_secs = 0\n",
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\n[cache]\nstream_ttl_secs = -1\n",
        "self_id = 1\n[forward_ws]\n[qq]\napp_id = \"1\"\nsecret = \"s\"\n[cache]\ncleanup_interval_secs = 0\n",
    ] {
        assert!(
            matches!(
                Config::parse(invalid).unwrap().resolve(),
                Err(Error::Invalid(_))
            ),
            "expected invalid config for:\n{invalid}"
        );
    }
}
