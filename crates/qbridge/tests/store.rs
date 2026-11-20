use qbridge::store::*;
use qbridge_qq::Session;
use serde_json::{Value, json};
use std::path::PathBuf;

fn event(time: i64, message: Value) -> Value {
    json!({
        "time": time,
        "self_id": 1,
        "post_type": "message",
        "message_type": "private",
        "sub_type": "friend",
        "message_id": 0,
        "user_id": 100,
        "message": message,
        "raw_message": "raw",
        "font": 0,
        "sender": {"user_id": 100}
    })
}

fn text_event(time: i64, text: &str) -> Value {
    event(time, json!([{"type": "text", "data": {"text": text}}]))
}

fn memory_store() -> Store {
    Store::open(std::path::Path::new(":memory:"), "app").unwrap()
}

fn temp_db() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "qbridge-store-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("state.sqlite")
}

#[tokio::test]
async fn allocates_stable_numeric_ids() {
    let store = memory_store();
    let user = store
        .identity(IdentityKind::User, "user-openid")
        .await
        .unwrap();
    let group = store
        .identity(IdentityKind::Group, "group-openid")
        .await
        .unwrap();
    let member = store
        .identity(IdentityKind::Member, "member-openid")
        .await
        .unwrap();
    assert_eq!(
        store
            .identity(IdentityKind::User, "user-openid")
            .await
            .unwrap(),
        user
    );
    assert_ne!(user, group);
    assert_ne!(group, member);
    assert_eq!(
        store
            .openid(IdentityKind::User, user)
            .await
            .unwrap()
            .as_deref(),
        Some("user-openid")
    );
    assert!(
        store
            .openid(IdentityKind::Group, user)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn database_is_bound_to_its_app_id_and_schema_version() {
    let path = temp_db();
    let store = Store::open(&path, "app").unwrap();
    drop(store);
    assert!(Store::open(&path, "app").is_ok());
    assert!(matches!(
        Store::open(&path, "other-app"),
        Err(Error::AppMismatch)
    ));

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(connection);
    assert!(matches!(
        Store::open(&path, "app"),
        Err(Error::SchemaVersion(99))
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn deduplicates_inbound_messages_and_patches_the_local_id() {
    let store = memory_store();
    let conversation = Conversation::Private(1);
    let id = store
        .save_inbound(
            conversation,
            "official-1",
            10,
            100,
            &text_event(10, "hi"),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, 1);
    assert!(
        store
            .save_inbound(
                conversation,
                "official-1",
                10,
                100,
                &text_event(10, "hi"),
                None
            )
            .await
            .unwrap()
            .is_none()
    );
    let message = store.message(id, 10).await.unwrap().unwrap();
    assert_eq!(message["message_id"], 1);
    assert_eq!(message["message"][0]["data"]["text"], "hi");
    assert!(store.message(id, 100).await.unwrap().is_none());
}

#[tokio::test]
async fn reserves_the_latest_passive_context_and_enforces_limits() {
    let store = memory_store();
    store
        .identity(IdentityKind::User, "private-user")
        .await
        .unwrap();
    store.identity(IdentityKind::Group, "group").await.unwrap();
    store
        .identity(IdentityKind::User, "expired-user")
        .await
        .unwrap();
    for (conversation, limit) in [(Conversation::Private(1), 4), (Conversation::Group(2), 5)] {
        let id = store
            .save_inbound(
                conversation,
                "official",
                10,
                1000,
                &text_event(10, "hi"),
                None,
            )
            .await
            .unwrap()
            .unwrap();
        for expected in 1..=limit {
            let context = store.reserve_reply(conversation, None, 20).await.unwrap();
            assert_eq!(context.sequence, expected);
            assert_eq!(context.official_id, "official");
        }
        assert!(matches!(
            store.reserve_reply(conversation, None, 20).await,
            Err(Error::ReplyLimitExceeded)
        ));
        let _ = id;
    }
    assert!(matches!(
        store
            .reserve_reply(Conversation::Private(99), None, 20)
            .await,
        Err(Error::UnknownTarget)
    ));
    let expired = Conversation::Private(3);
    store
        .save_inbound(expired, "old", 10, 15, &text_event(10, "old"), None)
        .await
        .unwrap();
    assert!(matches!(
        store.reserve_reply(expired, None, 20).await,
        Err(Error::ReplyContextUnavailable)
    ));
}

#[tokio::test]
async fn explicit_reply_must_belong_to_the_same_conversation() {
    let store = memory_store();
    let first = Conversation::Private(1);
    let second = Conversation::Private(2);
    store.identity(IdentityKind::User, "one").await.unwrap();
    store.identity(IdentityKind::User, "two").await.unwrap();
    let id = store
        .save_inbound(first, "one", 10, 1000, &text_event(10, "one"), None)
        .await
        .unwrap()
        .unwrap();
    store
        .save_inbound(second, "two", 10, 1000, &text_event(10, "two"), None)
        .await
        .unwrap();
    let context = store.reserve_reply(first, Some(id), 20).await.unwrap();
    assert_eq!(context.official_id, "one");
    assert!(matches!(
        store.reserve_reply(second, Some(id), 20).await,
        Err(Error::ReplyContextUnavailable)
    ));
}

#[tokio::test]
async fn quote_uses_cached_references_and_requires_them_for_bot_messages() {
    let store = memory_store();
    let conversation = Conversation::Group(1);
    let inbound = store
        .save_inbound(
            conversation,
            "in",
            10,
            1000,
            &text_event(10, "hi"),
            Some("REF-IN"),
        )
        .await
        .unwrap()
        .unwrap();
    let quote = store.quote(conversation, inbound, 20).await.unwrap();
    assert_eq!(quote.reference.as_deref(), Some("REF-IN"));
    assert!(quote.inbound);

    let outbound = store
        .save_outbound(conversation, "out", 10, 1000, &text_event(10, "bot"), None)
        .await
        .unwrap();
    assert!(matches!(
        store.quote(conversation, outbound, 20).await,
        Err(Error::QuoteReferenceUnavailable)
    ));
    let outbound = store
        .save_outbound(
            conversation,
            "out2",
            10,
            1000,
            &text_event(10, "bot"),
            Some("REF-OUT"),
        )
        .await
        .unwrap();
    let quote = store.quote(conversation, outbound, 20).await.unwrap();
    assert_eq!(quote.reference.as_deref(), Some("REF-OUT"));
    assert!(!quote.inbound);

    assert!(matches!(
        store.quote(Conversation::Group(2), outbound, 20).await,
        Err(Error::ReplyContextUnavailable)
    ));
}

#[tokio::test]
async fn recall_target_exposes_conversation_and_only_removes_bot_messages() {
    let store = memory_store();
    let conversation = Conversation::Private(1);
    store
        .identity(IdentityKind::User, "user-openid")
        .await
        .unwrap();
    let outbound = store
        .save_outbound(
            conversation,
            "official",
            10,
            1000,
            &text_event(10, "bot"),
            None,
        )
        .await
        .unwrap();
    let target = store.recall_target(outbound).await.unwrap();
    assert_eq!(target.conversation, conversation);
    assert_eq!(target.official_id, "official");
    assert_eq!(target.openid, "user-openid");
    assert!(!target.inbound);
    store.remove_outbound(outbound).await.unwrap();
    assert!(matches!(
        store.recall_target(outbound).await,
        Err(Error::MessageNotCached)
    ));

    let inbound = store
        .save_inbound(conversation, "in", 10, 1000, &text_event(10, "hi"), None)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.remove_outbound(inbound).await,
        Err(Error::NotOwned)
    ));
    store.remove_recalled(inbound).await.unwrap();
    assert!(matches!(
        store.recall_target(inbound).await,
        Err(Error::MessageNotCached)
    ));
}

#[tokio::test]
async fn finds_cached_inbound_image_urls() {
    let store = memory_store();
    let conversation = Conversation::Group(1);
    let content = event(
        10,
        json!([
            {"type": "text", "data": {"text": "look"}},
            {"type": "image", "data": {"file": "https://example.com/a.png", "url": "https://example.com/a.png"}}
        ]),
    );
    store
        .save_inbound(conversation, "img", 10, 1000, &content, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .image_url("https://example.com/a.png", 20)
            .await
            .unwrap()
            .as_deref(),
        Some("https://example.com/a.png")
    );
    assert!(store.image_url("missing", 20).await.unwrap().is_none());
    assert!(
        store
            .image_url("https://example.com/a.png", 1000)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleans_expired_messages_but_keeps_identities() {
    let store = memory_store();
    let conversation = Conversation::Private(1);
    let user = store.identity(IdentityKind::User, "user").await.unwrap();
    let id = store
        .save_inbound(conversation, "old", 10, 100, &text_event(10, "old"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.clean_expired(100).await.unwrap(), 1);
    assert!(store.message(id, 100).await.unwrap().is_none());
    assert_eq!(
        store.identity(IdentityKind::User, "user").await.unwrap(),
        user
    );
}

#[tokio::test]
async fn gateway_session_round_trips() {
    let store = memory_store();
    assert!(store.gateway_session().await.unwrap().is_none());
    store
        .save_gateway_session(&Session {
            session_id: "S".into(),
            seq: 7,
            intents: 123,
        })
        .await
        .unwrap();
    assert_eq!(
        store.gateway_session().await.unwrap(),
        Some(Session {
            session_id: "S".into(),
            seq: 7,
            intents: 123
        })
    );
    store.clear_gateway_session().await.unwrap();
    assert!(store.gateway_session().await.unwrap().is_none());
}

#[tokio::test]
async fn concurrent_reservations_are_atomic() {
    let store = std::sync::Arc::new(memory_store());
    let conversation = Conversation::Private(1);
    store.identity(IdentityKind::User, "user").await.unwrap();
    store
        .save_inbound(
            conversation,
            "official",
            10,
            1000,
            &text_event(10, "hi"),
            None,
        )
        .await
        .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .reserve_reply(conversation, None, 20)
                .await
                .unwrap()
                .sequence
        });
    }
    let mut sequences = Vec::new();
    while let Some(result) = tasks.join_next().await {
        sequences.push(result.unwrap());
    }
    sequences.sort();
    assert_eq!(sequences, vec![1, 2, 3, 4]);
    assert!(matches!(
        store.reserve_reply(conversation, None, 20).await,
        Err(Error::ReplyLimitExceeded)
    ));
}

#[tokio::test]
async fn unifies_user_and_member_by_same_openid() {
    let store = memory_store();
    let user = store
        .identity(IdentityKind::User, "same-openid")
        .await
        .unwrap();
    let member = store
        .identity(IdentityKind::Member, "same-openid")
        .await
        .unwrap();
    assert_eq!(user, member);
    assert_eq!(
        store
            .openid(IdentityKind::Member, user)
            .await
            .unwrap()
            .as_deref(),
        Some("same-openid")
    );
}

#[tokio::test]
async fn merges_entities_and_removes_old_id() {
    let store = memory_store();
    let old = store.identity(IdentityKind::User, "old").await.unwrap();
    let new = store.identity(IdentityKind::User, "new").await.unwrap();
    store.merge_entities(old, new).await.unwrap();
    assert!(
        store
            .openid(IdentityKind::User, old)
            .await
            .unwrap()
            .is_none()
    );
    let identities = store.identities(IdentityKind::User).await.unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].0, new);
}

#[tokio::test]
async fn binds_and_unbinds_identities() {
    let store = memory_store();
    let target = store.identity(IdentityKind::User, "target").await.unwrap();
    store
        .bind_identity(IdentityKind::User, "alias", target)
        .await
        .unwrap();
    assert_eq!(store.identities(IdentityKind::User).await.unwrap().len(), 1);
    assert_eq!(
        store
            .export_identities()
            .await
            .unwrap()
            .iter()
            .filter(|record| record.entity_id == target)
            .count(),
        2
    );
    assert!(
        store
            .unbind_identity(IdentityKind::User, "alias")
            .await
            .unwrap()
    );
    assert!(
        store
            .openid(IdentityKind::User, target)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn caches_profiles_with_expiry() {
    let store = memory_store();
    let entity = store.identity(IdentityKind::User, "profile").await.unwrap();
    store
        .save_profile(entity, "Alice", "Card", "https://a", 10, 100)
        .await
        .unwrap();
    let profile = store.profile(entity, 105).await.unwrap().unwrap();
    assert_eq!(profile.display_name, "Alice");
    assert!(store.profile(entity, 111).await.unwrap().is_none());
    store.clean_expired(111).await.unwrap();
    assert!(store.profile(entity, 112).await.unwrap().is_none());
}

#[tokio::test]
async fn resolves_identity_batches_and_concurrently() {
    let store = memory_store();
    let openids = vec!["a".to_owned(), "b".to_owned(), "a".to_owned()];
    let batch = store
        .identity_batch(IdentityKind::Member, &openids)
        .await
        .unwrap();
    assert_ne!(batch["a"], batch["b"]);

    let tasks = (0..20)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move { store.identity(IdentityKind::Member, "a").await.unwrap() })
        })
        .collect::<Vec<_>>();
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap());
    }
    assert!(ids.iter().all(|id| *id == batch["a"]));
}

#[tokio::test]
async fn reads_profiles_in_batch_and_reports_identity_stats() {
    let store = memory_store();
    let one = store.identity(IdentityKind::User, "one").await.unwrap();
    let two = store.identity(IdentityKind::User, "two").await.unwrap();
    store
        .save_profiles(
            &[
                (one, "One".into(), "Card1".into(), String::new()),
                (two, "Two".into(), String::new(), String::new()),
            ],
            600,
            100,
        )
        .await
        .unwrap();
    let profiles = store.profiles_for_entities(&[one, two], 200).await.unwrap();
    assert_eq!(profiles[&one].display_name, "One");
    assert_eq!(profiles[&two].display_name, "Two");
    let stats = store.identity_stats();
    assert!(stats.lookups >= 2);
    assert!(stats.profile_hits >= 2);
}

fn hash_options(bits: u8) -> IdentityOptions {
    IdentityOptions {
        mode: IdentityIdMode::Hash,
        salt: "test-salt".into(),
        bits,
    }
}

#[tokio::test]
async fn hash_ids_are_stable_across_databases() {
    let first =
        Store::open_with_identity(std::path::Path::new(":memory:"), "app", hash_options(52))
            .unwrap();
    let second =
        Store::open_with_identity(std::path::Path::new(":memory:"), "app", hash_options(52))
            .unwrap();
    let one = first
        .identity(IdentityKind::User, "stable-openid")
        .await
        .unwrap();
    let two = second
        .identity(IdentityKind::User, "stable-openid")
        .await
        .unwrap();
    assert_eq!(one, two);
    assert!(one > 0);
    assert!(one < (1_i64 << 52));
    assert_ne!(
        one,
        first
            .identity(IdentityKind::Member, "other-openid")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn hash_mode_reports_collisions_instead_of_reordering() {
    let store = Store::open_with_identity(std::path::Path::new(":memory:"), "app", hash_options(1))
        .unwrap();
    let mut collision = false;
    for openid in ["a", "b", "c", "d"] {
        if matches!(
            store.identity(IdentityKind::User, openid).await,
            Err(qbridge::store::Error::IdentityCollision)
        ) {
            collision = true;
            break;
        }
    }
    assert!(collision);
}
