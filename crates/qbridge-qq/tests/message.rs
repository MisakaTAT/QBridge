use qbridge_qq::{
    Intents, Keyboard, Markdown, MarkdownParam, MediaSource, MediaUpload, OutgoingMessage,
};
use serde_json::json;

#[test]
fn serializes_text_reply_and_reference() {
    let message = OutgoingMessage::text("hello")
        .passive("MSG", 2)
        .referencing("REF");
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "msg_type": 0,
            "content": "hello",
            "msg_id": "MSG",
            "msg_seq": 2,
            "message_reference": {"message_id": "REF"}
        })
    );
}

#[test]
fn serializes_markdown_content_with_keyboard() {
    let keyboard = Keyboard {
        id: Some("TEMPLATE".into()),
        content: None,
    };
    let message = OutgoingMessage::markdown(
        Markdown::Content {
            content: "# hi".into(),
        },
        Some(keyboard),
    );
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "msg_type": 2,
            "markdown": {"content": "# hi"},
            "keyboard": {"id": "TEMPLATE"}
        })
    );
}

#[test]
fn serializes_markdown_template_params() {
    let message = OutgoingMessage::markdown(
        Markdown::Template {
            custom_template_id: "T1".into(),
            params: vec![MarkdownParam {
                key: "k".into(),
                values: vec!["v".into()],
            }],
        },
        None,
    );
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "msg_type": 2,
            "markdown": {
                "custom_template_id": "T1",
                "params": [{"key": "k", "values": ["v"]}]
            }
        })
    );
}

#[test]
fn serializes_media_message() {
    let message = OutgoingMessage::media("FILE", Some("caption".into()));
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "msg_type": 7,
            "content": "caption",
            "media": {"file_info": "FILE"}
        })
    );
}

#[test]
fn serializes_media_upload_sources() {
    let url = MediaUpload {
        file_type: 1,
        source: MediaSource::Url {
            url: "https://example.com/a.png".into(),
            file_name: None,
        },
    };
    assert_eq!(
        serde_json::to_value(url).unwrap(),
        json!({"file_type": 1, "srv_send_msg": false, "url": "https://example.com/a.png"})
    );
    let data = MediaUpload {
        file_type: 3,
        source: MediaSource::Data {
            file_data: "QUJD".into(),
            file_name: "a.silk".into(),
        },
    };
    assert_eq!(
        serde_json::to_value(data).unwrap(),
        json!({
            "file_type": 3,
            "srv_send_msg": false,
            "file_data": "QUJD",
            "file_name": "a.silk"
        })
    );
}

#[test]
fn intent_bits_match_the_qq_platform() {
    assert_eq!(Intents::GROUP_AND_C2C.bits(), 1 << 25);
    assert_eq!(Intents::INTERACTION.bits(), 1 << 26);
    let combined = Intents::GROUP_AND_C2C | Intents::INTERACTION;
    assert!(combined.has(Intents::GROUP_AND_C2C));
    assert!(combined.has(Intents::INTERACTION));
    assert_eq!(Intents::default().bits(), (1 << 25) | (1 << 26) | (1 << 24));
}

#[test]
fn sent_message_allows_a_missing_id() {
    let message: qbridge_qq::SentMessage = serde_json::from_value(json!({})).unwrap();
    assert!(message.id.is_empty());
}

#[test]
fn serializes_ark_template_message() {
    let message = OutgoingMessage::ark(qbridge_qq::Ark {
        template_id: 23,
        kv: vec![qbridge_qq::ArkKv {
            key: "#DESC#".into(),
            value: Some("summary".into()),
            obj: None,
            extra: Default::default(),
        }],
        extra: Default::default(),
    });
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        json!({
            "msg_type": 3,
            "ark": {
                "template_id": 23,
                "kv": [{"key": "#DESC#", "value": "summary"}]
            }
        })
    );
}
