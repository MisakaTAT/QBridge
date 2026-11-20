use qbridge::bridge::plan::{self, Body, MediaKind, MediaSource, Plan, TextPart};
use qbridge::error::{ActionError, UnsupportedKind};
use qbridge_ob11::{AtTarget, Media, Message, Node, Segment, Target};
use qbridge_qq::{Keyboard, Markdown};
use serde_json::{Map, json};

fn group() -> Target {
    Target::Group(1)
}

fn media(file: &str) -> Media {
    Media {
        file: file.into(),
        url: None,
        extra: Map::new(),
    }
}

fn other(kind: &str, data: serde_json::Value) -> Segment {
    Segment::Other {
        kind: kind.into(),
        data: data.as_object().unwrap().clone(),
    }
}

fn plan_message(message: Message, extra: serde_json::Value) -> Result<Plan, ActionError> {
    plan::plan(group(), &message, extra.as_object().unwrap())
}

#[test]
fn texts_and_group_mentions_become_text_parts() {
    let message = Message(vec![
        Segment::Text("hello ".into()),
        Segment::At(AtTarget::User(7)),
        Segment::Text("!".into()),
    ]);
    let plan = plan_message(message, json!({})).unwrap();
    assert_eq!(
        plan.body,
        Body::Text(vec![
            TextPart::Text("hello ".into()),
            TextPart::At(7),
            TextPart::Text("!".into())
        ])
    );
    assert!(!plan.active);
}

#[test]
fn rejects_private_mentions_and_maps_group_at_all() {
    let message = Message(vec![Segment::At(AtTarget::User(7))]);
    assert_eq!(
        plan::plan(Target::Private(1), &message, &Map::new()).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::Segment)
    );
    let message = Message(vec![Segment::At(AtTarget::All)]);
    let plan = plan_message(message, json!({})).unwrap();
    assert_eq!(
        plan.body,
        Body::Text(vec![TextPart::Text("<qqbot-at-everyone />".into())])
    );
}

#[test]
fn allows_one_media_segment_with_text() {
    let message = Message(vec![
        Segment::Text("look".into()),
        Segment::Image(media("https://example.com/a.png")),
    ]);
    let plan = plan_message(message, json!({})).unwrap();
    let Body::Media { media, text } = plan.body else {
        panic!("unexpected body: {:?}", plan.body)
    };
    assert_eq!(media.kind, MediaKind::Image);
    assert_eq!(media.file_type_for_test(), 1);
    assert_eq!(
        media.source,
        MediaSource::Url("https://example.com/a.png".into())
    );
    assert_eq!(text, vec![TextPart::Text("look".into())]);
}

#[test]
fn rejects_multiple_media_segments() {
    let message = Message(vec![
        Segment::Image(media("https://example.com/a.png")),
        Segment::Image(media("https://example.com/b.png")),
    ]);
    assert_eq!(
        plan_message(message, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
}

#[test]
fn rejects_display_changing_media_options_but_ignores_fetch_options() {
    for (key, value) in [("type", json!("flash")), ("magic", json!(true))] {
        let mut image = media("https://example.com/a.png");
        image.extra.insert(key.into(), value);
        let message = Message(vec![Segment::Image(image)]);
        assert_eq!(
            plan_message(message, json!({})).unwrap_err(),
            ActionError::Unsupported(UnsupportedKind::SegmentOption),
            "{key}"
        );
    }
    let mut image = media("https://example.com/a.png");
    for key in ["cache", "proxy", "timeout"] {
        image.extra.insert(key.into(), json!(true));
    }
    let message = Message(vec![Segment::Image(image)]);
    assert!(plan_message(message, json!({})).is_ok());
}

#[test]
fn parses_media_sources() {
    let local = std::env::temp_dir().join("qbridge-plan-image.png");
    let local_url = url::Url::from_file_path(&local).unwrap().to_string();
    let cases = [
        (
            "http://example.com/a.png".to_string(),
            "image.png".to_string(),
        ),
        (
            "https://example.com/a.png?x=1".to_string(),
            "image.png".to_string(),
        ),
        ("base64://QUJD".to_string(), "image.png".to_string()),
        (local_url, "qbridge-plan-image.png".to_string()),
    ];
    for (file, expected_name) in cases {
        let message = Message(vec![Segment::Image(media(&file))]);
        let plan = plan_message(message, json!({})).unwrap();
        let Body::Media { media, .. } = plan.body else {
            panic!("unexpected body")
        };
        assert_eq!(media.file_name, expected_name, "{file}");
    }
    for file in [
        "ftp://example.com/a.png",
        "https://user:pass@example.com/a",
        "not a uri",
    ] {
        let message = Message(vec![Segment::Image(media(file))]);
        assert!(
            plan_message(message, json!({})).is_err(),
            "{file} should be rejected"
        );
    }
    let oversized = format!("base64://{}", "A".repeat(7 * 1024 * 1024));
    let message = Message(vec![Segment::Image(media(&oversized))]);
    assert!(plan_message(message, json!({})).is_err());
}

#[test]
fn parses_markdown_and_keyboard() {
    let message = Message(vec![
        other("markdown", json!({"content": "# title"})),
        other("keyboard", json!({"id": "TEMPLATE"})),
    ]);
    let plan = plan_message(message, json!({})).unwrap();
    let Body::Markdown { markdown, keyboard } = plan.body else {
        panic!("unexpected body")
    };
    assert_eq!(
        markdown,
        Markdown::Content {
            content: "# title".into()
        }
    );
    assert_eq!(
        keyboard,
        Some(Keyboard {
            id: Some("TEMPLATE".into()),
            content: None
        })
    );
}

#[test]
fn parses_markdown_templates() {
    let message = Message(vec![other(
        "markdown",
        json!({
            "custom_template_id": "T1",
            "params": [{"key": "k", "values": ["v"]}]
        }),
    )]);
    let plan = plan_message(message, json!({})).unwrap();
    assert!(matches!(
        plan.body,
        Body::Markdown {
            markdown: Markdown::Template { .. },
            ..
        }
    ));
}

#[test]
fn rejects_invalid_markdown_and_keyboard_combinations() {
    let keyboard_only = Message(vec![other("keyboard", json!({"id": "T"}))]);
    assert_eq!(
        plan_message(keyboard_only, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
    let markdown_with_text = Message(vec![
        Segment::Text("x".into()),
        other("markdown", json!({"content": "# x"})),
    ]);
    assert_eq!(
        plan_message(markdown_with_text, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
    let markdown_with_media = Message(vec![
        other("markdown", json!({"content": "# x"})),
        Segment::Image(media("https://example.com/a.png")),
    ]);
    assert_eq!(
        plan_message(markdown_with_media, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
    let both_markdown = Message(vec![
        other("markdown", json!({"content": "# x"})),
        other("markdown", json!({"content": "# y"})),
    ]);
    assert_eq!(
        plan_message(both_markdown, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
}

#[test]
fn validates_custom_keyboard_size() {
    let rows = (0..6)
        .map(|_| json!({"buttons": [{"render_data": {"label": "x"}, "action": {"type": 0, "data": "d"}}]}))
        .collect::<Vec<_>>();
    let message = Message(vec![
        other("markdown", json!({"content": "# x"})),
        other("keyboard", json!({"content": {"rows": rows}})),
    ]);
    assert!(matches!(
        plan_message(message, json!({})).unwrap_err(),
        ActionError::BadParam(_)
    ));
}

#[test]
fn reply_and_active_are_mutually_exclusive() {
    let message = Message(vec![Segment::Reply(1), Segment::Text("hi".into())]);
    assert_eq!(
        plan_message(message, json!({"active": true})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
}

#[test]
fn pure_node_messages_become_forward_plans() {
    let message = Message(vec![Segment::Node(Node::Custom {
        user_id: Some("1".into()),
        nickname: Some("a".into()),
        content: Message::text("hello"),
        extra: Map::new(),
    })]);
    let plan = plan_message(
        message,
        json!({"source": "S", "prompt": "P", "summary": "M", "news": [{"text": "N"}]}),
    )
    .unwrap();
    let Body::Forward { nodes, options } = plan.body else {
        panic!("unexpected body: {:?}", plan.body)
    };
    assert_eq!(nodes.len(), 1);
    assert_eq!(options.source.as_deref(), Some("S"));
    assert_eq!(options.prompt.as_deref(), Some("P"));
    assert_eq!(options.summary.as_deref(), Some("M"));
    assert_eq!(options.news, vec!["N".to_string()]);
}

#[test]
fn rejects_nodes_mixed_with_other_content() {
    let message = Message(vec![
        Segment::Text("x".into()),
        Segment::Node(Node::Reference(1)),
    ]);
    assert_eq!(
        plan_message(message, json!({})).unwrap_err(),
        ActionError::Unsupported(UnsupportedKind::MessageCombination)
    );
}

#[test]
fn rejects_unsupported_segments_and_empty_messages() {
    for segment in [Segment::Face("1".into()), other("shake", json!({}))] {
        assert_eq!(
            plan_message(Message(vec![segment]), json!({})).unwrap_err(),
            ActionError::Unsupported(UnsupportedKind::Segment)
        );
    }
    assert!(matches!(
        plan_message(Message::default(), json!({})).unwrap_err(),
        ActionError::BadParam(_)
    ));
}

#[test]
fn invalid_forward_news_is_rejected() {
    let message = Message(vec![Segment::Node(Node::Reference(1))]);
    assert!(matches!(
        plan_message(message, json!({"news": [{"missing": 1}]})).unwrap_err(),
        ActionError::BadParam(_)
    ));
}

trait MediaPlanTest {
    fn file_type_for_test(&self) -> u8;
}

impl MediaPlanTest for plan::MediaPlan {
    fn file_type_for_test(&self) -> u8 {
        self.kind.file_type()
    }
}

#[test]
fn plans_file_card_input_and_wakeup_segments() {
    let file = plan::plan(
        group(),
        &Message(vec![Segment::File(Media {
            file: "https://example.com/a.bin".into(),
            url: None,
            extra: Map::new(),
        })]),
        &Map::new(),
    )
    .unwrap();
    let Body::Media { media, .. } = file.body else {
        panic!("expected media")
    };
    assert_eq!(media.kind, MediaKind::File);
    assert_eq!(media.kind.file_type(), 4);

    let card = plan::plan(
        group(),
        &Message(vec![other(
            "card",
            json!({"title": "T", "desc": "D", "pic": "https://example.com/p.png", "url": "https://example.com"}),
        )]),
        &Map::new(),
    )
    .unwrap();
    let Body::Card(card) = card.body else {
        panic!("expected card")
    };
    assert_eq!(card.title, "T");
    assert_eq!(card.pic_url, "https://example.com/p.png");

    let notify = plan::plan(
        Target::Private(1),
        &Message(vec![other(
            "input_notify",
            json!({"type": 1, "second": 30}),
        )]),
        &Map::new(),
    )
    .unwrap();
    let Body::InputNotify {
        input_type,
        input_second,
    } = notify.body
    else {
        panic!("expected input notify")
    };
    assert_eq!((input_type, input_second), (1, 30));

    let wakeup = plan::plan(
        Target::Private(1),
        &Message(vec![
            other("wakeup", json!({"userid": 1})),
            Segment::Text("hi".into()),
        ]),
        &Map::new(),
    )
    .unwrap();
    assert!(wakeup.wakeup);
    assert!(wakeup.active);
    assert!(matches!(wakeup.body, Body::Text(_)));
}

#[test]
fn rejects_cq_types_without_a_qq_equivalent() {
    for kind in ["tts", "face", "share", "music", "xml", "json"] {
        let data = if kind == "face" {
            json!({"id": "1"})
        } else {
            json!({})
        };
        let error = plan_message(Message(vec![other(kind, data)]), json!({})).unwrap_err();
        assert!(
            matches!(error, ActionError::Unsupported(UnsupportedKind::Segment)),
            "{kind} should be unsupported"
        );
    }
}
