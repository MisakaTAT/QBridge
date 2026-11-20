use qbridge::bridge::forward;
use qbridge::bridge::plan::ForwardOptions;
use qbridge::error::{ActionError, NotFoundKind, UnsupportedKind};
use qbridge::store::{Conversation, Store};
use qbridge_ob11::{Media, Message, Node, Segment};
use serde_json::{Map, json};

fn memory_store() -> Store {
    Store::open(std::path::Path::new(":memory:"), "app").unwrap()
}

fn media(file: &str) -> Media {
    Media {
        file: file.into(),
        url: None,
        extra: Map::new(),
    }
}

fn custom(name: &str, id: &str, content: Message) -> Node {
    Node::Custom {
        user_id: Some(id.into()),
        nickname: Some(name.into()),
        content,
        extra: Map::new(),
    }
}

async fn cached(store: &Store, value: serde_json::Value) -> i32 {
    store
        .save_inbound(Conversation::Group(1), "official", 10, 1000, &value, None)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn renders_custom_and_cached_nodes_with_markdown() {
    let store = memory_store();
    let event = json!({
        "time": 10,
        "self_id": 1,
        "post_type": "message",
        "message_type": "group",
        "group_id": 1,
        "sub_type": "normal",
        "anonymous": null,
        "message_id": 0,
        "user_id": 42,
        "message": [{"type": "text", "data": {"text": "cached [text]"}}],
        "raw_message": "cached [text]",
        "font": 0,
        "sender": {"user_id": 42, "nickname": "Alice"}
    });
    let id = cached(&store, event).await;
    let nodes = vec![
        Node::Reference(id),
        custom(
            "Bob",
            "7",
            Message(vec![
                Segment::Text("hello *world*".into()),
                Segment::Node(custom("Carol", "8", Message::text("nested"))),
            ]),
        ),
    ];
    let options = ForwardOptions {
        source: Some("History".into()),
        prompt: Some("see below".into()),
        news: vec!["news".into()],
        summary: Some("end".into()),
    };
    let output = forward::render(&store, &nodes, &options, 20).await.unwrap();
    assert!(output.starts_with("# History\n"));
    assert!(output.contains("see below\n"));
    assert!(output.contains("news\n"));
    assert!(output.contains("**Alice \\(42\\)**"));
    assert!(output.contains("cached \\[text\\]"));
    assert!(output.contains("**Bob \\(7\\)**"));
    assert!(output.contains("hello \\*world\\*"));
    assert!(output.contains("**Carol \\(8\\)**"));
    assert!(output.contains("nested"));
    assert!(output.ends_with("\n\nend"));
}

#[tokio::test]
async fn renders_public_image_urls_and_rejects_local_or_base64_images() {
    let store = memory_store();
    let node = |file: &str| custom("A", "1", Message(vec![Segment::Image(media(file))]));
    let output = forward::render(
        &store,
        &[node("https://example.com/a(1).png")],
        &ForwardOptions::default(),
        20,
    )
    .await
    .unwrap();
    assert!(output.contains("![图片](https://example.com/a%281%29.png)"));

    for file in ["base64://QUJD", "file:///tmp/a.png"] {
        let error = forward::render(&store, &[node(file)], &ForwardOptions::default(), 20)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ActionError::Unsupported(UnsupportedKind::ForwardMedia)
        );
    }
}

#[tokio::test]
async fn rejects_missing_cache_and_unsupported_segments() {
    let store = memory_store();
    let error = forward::render(
        &store,
        &[Node::Reference(999)],
        &ForwardOptions::default(),
        20,
    )
    .await
    .unwrap_err();
    assert_eq!(error, ActionError::NotFound(NotFoundKind::MessageNotCached));

    let node = custom(
        "A",
        "1",
        Message(vec![Segment::Record(media("https://example.com/a.silk"))]),
    );
    let error = forward::render(&store, &[node], &ForwardOptions::default(), 20)
        .await
        .unwrap_err();
    assert_eq!(error, ActionError::Unsupported(UnsupportedKind::Segment));
}

#[tokio::test]
async fn empty_node_lists_are_rejected() {
    let store = memory_store();
    let error = forward::render(&store, &[], &ForwardOptions::default(), 20)
        .await
        .unwrap_err();
    assert!(matches!(error, ActionError::BadParam(_)));
}
