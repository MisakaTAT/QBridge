use qbridge::error::{ActionError, NotFoundKind, ReplyContextKind, UnsupportedKind};
use qbridge_ob11::ParseError;
use qbridge_qq::Error as QqError;

fn pairs() -> Vec<(ActionError, i64, &'static str)> {
    vec![
        (ActionError::BadParam("bad".into()), 1400, "BAD_PARAMETER"),
        (ActionError::UnknownAction, 1404, "UNKNOWN_ACTION"),
        (
            ActionError::Unsupported(UnsupportedKind::Action),
            100,
            "UNSUPPORTED_ACTION",
        ),
        (
            ActionError::Unsupported(UnsupportedKind::Segment),
            100,
            "UNSUPPORTED_SEGMENT",
        ),
        (
            ActionError::Unsupported(UnsupportedKind::MessageCombination),
            100,
            "UNSUPPORTED_MESSAGE_COMBINATION",
        ),
        (
            ActionError::Unsupported(UnsupportedKind::SegmentOption),
            100,
            "UNSUPPORTED_SEGMENT_OPTION",
        ),
        (
            ActionError::Unsupported(UnsupportedKind::ForwardMedia),
            100,
            "UNSUPPORTED_FORWARD_MEDIA",
        ),
        (
            ActionError::NotFound(NotFoundKind::MessageNotCached),
            101,
            "MESSAGE_NOT_CACHED",
        ),
        (
            ActionError::NotFound(NotFoundKind::MediaNotFound),
            101,
            "MEDIA_NOT_FOUND",
        ),
        (
            ActionError::NotFound(NotFoundKind::UnknownTarget),
            101,
            "UNKNOWN_TARGET",
        ),
        (ActionError::QueueFull, 102, "QUEUE_FULL"),
        (ActionError::ShuttingDown, 103, "SHUTTING_DOWN"),
        (ActionError::IdExhausted, 106, "MESSAGE_ID_EXHAUSTED"),
        (ActionError::Storage, 110, "STORAGE_ERROR"),
        (
            ActionError::ReplyContext(ReplyContextKind::Unavailable),
            111,
            "REPLY_CONTEXT_UNAVAILABLE",
        ),
        (
            ActionError::ReplyContext(ReplyContextKind::LimitExceeded),
            111,
            "REPLY_LIMIT_EXCEEDED",
        ),
        (
            ActionError::ReplyContext(ReplyContextKind::QuoteReferenceUnavailable),
            111,
            "QUOTE_REFERENCE_UNAVAILABLE",
        ),
        (ActionError::SendOutcomeUnknown, 112, "SEND_OUTCOME_UNKNOWN"),
        (ActionError::NotOwned, 113, "MESSAGE_NOT_OWNED"),
        (
            ActionError::Platform(QqError::Api {
                http_status: 400,
                code: Some(1),
                message: "x".into(),
                trace_id: None,
            }),
            120,
            "QQ_API_ERROR",
        ),
        (
            ActionError::Platform(QqError::Auth("x".into())),
            121,
            "QQ_AUTH_ERROR",
        ),
        (
            ActionError::Platform(QqError::Network("x".into())),
            122,
            "QQ_NETWORK_ERROR",
        ),
        (
            ActionError::Platform(QqError::Decode("x".into())),
            123,
            "QQ_DECODE_ERROR",
        ),
    ]
}

#[test]
fn retcodes_and_reasons_match_the_design_table() {
    for (error, retcode, reason) in pairs() {
        assert_eq!(error.retcode(), retcode, "{error:?}");
        assert_eq!(error.reason(), reason, "{error:?}");
        assert!(!error.message().is_empty(), "{error:?}");
    }
}

#[test]
fn maps_parse_errors() {
    assert_eq!(
        ActionError::from(ParseError::Unsupported("set_group_kick".into())),
        ActionError::Unsupported(UnsupportedKind::Action)
    );
    assert_eq!(
        ActionError::from(ParseError::BadParam("missing".into())),
        ActionError::BadParam("missing".into())
    );
}

#[test]
fn platform_messages_include_http_status_and_qq_code() {
    let error = ActionError::Platform(QqError::Api {
        http_status: 400,
        code: Some(11244),
        message: "invalid".into(),
        trace_id: Some("trace-1".into()),
    });
    let message = error.message();
    assert_eq!(
        message,
        "QQ API error (HTTP 400, code 11244, trace_id trace-1): invalid"
    );
}
