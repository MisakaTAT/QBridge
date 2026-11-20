use qbridge_ob11::{AtTarget, Media, Message, Node, Segment};
use serde_json::{Map, json};

fn media(file: &str) -> Media {
    Media {
        file: file.into(),
        url: None,
        extra: Map::new(),
    }
}

#[test]
fn parses_cq_string_into_typed_segments() {
    let message = Message::from_cq("hi[CQ:at,qq=123][CQ:at,qq=all][CQ:reply,id=9][CQ:face,id=14][CQ:image,file=a.png,type=flash]").unwrap();
    let mut extra = Map::new();
    extra.insert("type".into(), json!("flash"));
    assert_eq!(
        message.0,
        vec![
            Segment::Text("hi".into()),
            Segment::At(AtTarget::User(123)),
            Segment::At(AtTarget::All),
            Segment::Reply(9),
            Segment::Face("14".into()),
            Segment::Image(Media {
                file: "a.png".into(),
                url: None,
                extra
            }),
        ]
    );
}

#[test]
fn unknown_segments_are_preserved() {
    let message = Message::from_cq("[CQ:markdown,content=# hi]").unwrap();
    let Segment::Other { kind, data } = &message.0[0] else {
        panic!("expected Other");
    };
    assert_eq!(kind, "markdown");
    assert_eq!(data["content"], "# hi");
    assert_eq!(message.to_cq(), "[CQ:markdown,content=# hi]");
}

#[test]
fn invalid_cq_codes_stay_literal_text() {
    let message = Message::from_cq("a[CQ:]b[CQ:x,novalue]c[CQ:unclosed").unwrap();
    assert_eq!(
        message.0,
        vec![Segment::Text("a[CQ:]b[CQ:x,novalue]c[CQ:unclosed".into())]
    );
}

#[test]
fn cq_entities_are_unescaped_once() {
    let message =
        Message::from_cq("&#91;x&#93;[CQ:image,file=https://a/?x=1&amp;y=2&#44;3]").unwrap();
    assert_eq!(message.0[0], Segment::Text("[x]".into()));
    assert_eq!(message.0[1], Segment::Image(media("https://a/?x=1&y=2,3")));
}

#[test]
fn literal_cq_text_is_escaped_when_encoded() {
    let message = Message::text("[CQ:at,qq=all]");
    assert_eq!(message.to_cq(), "&#91;CQ:at,qq=all&#93;");
    assert_eq!(Message::from_cq(&message.to_cq()).unwrap(), message);
}

#[test]
fn auto_escape_keeps_strings_as_text() {
    let message = Message::from_value(json!("[CQ:face,id=1]"), true).unwrap();
    assert_eq!(message.0, vec![Segment::Text("[CQ:face,id=1]".into())]);
}

#[test]
fn accepts_arrays_single_objects_and_null_data() {
    let array =
        Message::from_value(json!([{"type": "text", "data": {"text": "a"}}]), false).unwrap();
    let object =
        Message::from_value(json!({"type": "text", "data": {"text": "a"}}), false).unwrap();
    assert_eq!(array, object);
    let null_data = Message::from_value(json!([{"type": "shake", "data": null}]), false).unwrap();
    assert_eq!(
        null_data.0,
        vec![Segment::Other {
            kind: "shake".into(),
            data: Map::new()
        }]
    );
}

#[test]
fn rejects_malformed_segments() {
    for value in [
        json!(1),
        json!([1]),
        json!([{"data": {}}]),
        json!([{"type": "bad type", "data": {}}]),
        json!([{"type": "x", "data": {"a,b": "1"}}]),
        json!([{"type": "x", "data": []}]),
        json!([{"type": "text", "data": {"text": 1}}]),
        json!([{"type": "at", "data": {"qq": "abc"}}]),
        json!([{"type": "reply", "data": {"id": "4294967296"}}]),
        json!([{"type": "image", "data": {}}]),
        json!([{"type": "node", "data": {"id": "1", "content": "x"}}]),
    ] {
        assert!(
            Message::from_value(value.clone(), false).is_err(),
            "{value}"
        );
    }
}

#[test]
fn array_form_uses_string_ids() {
    let message = Message(vec![
        Segment::At(AtTarget::User(5)),
        Segment::Reply(9),
        Segment::Node(Node::Reference(3)),
    ]);
    assert_eq!(
        message.to_value(),
        json!([
            {"type": "at", "data": {"qq": "5"}},
            {"type": "reply", "data": {"id": "9"}},
            {"type": "node", "data": {"id": "3"}},
        ])
    );
    assert_eq!(
        Message::from_value(message.to_value(), false).unwrap(),
        message
    );
}

#[test]
fn custom_node_accepts_author_aliases_and_nested_content() {
    let message = Message::from_value(
        json!([{"type": "node", "data": {"uin": 10, "name": "Alice", "content": [{"type": "text", "data": {"text": "hi"}}]}}]),
        false,
    )
    .unwrap();
    assert_eq!(
        message.0,
        vec![Segment::Node(Node::Custom {
            user_id: Some("10".into()),
            nickname: Some("Alice".into()),
            content: Message::text("hi"),
            extra: Map::new(),
        })]
    );
}

#[test]
fn node_content_is_nested_cq_in_string_form() {
    let message = Message(vec![Segment::Node(Node::Custom {
        user_id: Some("1".into()),
        nickname: None,
        content: Message(vec![Segment::Text("a,b".into()), Segment::Face("1".into())]),
        extra: Map::new(),
    })]);
    let cq = message.to_cq();
    assert_eq!(
        cq,
        "[CQ:node,content=a&#44;b&#91;CQ:face&#44;id=1&#93;,user_id=1]"
    );
    assert_eq!(Message::from_cq(&cq).unwrap(), message);
}

#[test]
fn media_keeps_url_and_extra_fields() {
    let value =
        json!([{"type": "image", "data": {"file": "f", "url": "https://x/y", "summary": "s"}}]);
    let message = Message::from_value(value.clone(), false).unwrap();
    let Segment::Image(media) = &message.0[0] else {
        panic!("expected image");
    };
    assert_eq!(media.url.as_deref(), Some("https://x/y"));
    assert_eq!(media.extra["summary"], "s");
    assert_eq!(message.to_value(), value);
}

#[test]
fn serializes_as_segment_array() {
    let message = Message::text("x");
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!([{"type": "text", "data": {"text": "x"}}])
    );
}
