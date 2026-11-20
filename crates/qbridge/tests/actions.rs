mod common;

use common::*;
use qbridge::{
    bridge::Bridge,
    error::{ActionError, NotFoundKind},
    store::{Conversation, IdentityKind},
};
use qbridge_ob11::Call;
use serde_json::{Map, Value, json};

async fn run(bridge: &Bridge, action: &str, params: Value) -> Result<Value, ActionError> {
    let params: Map<String, Value> = params.as_object().unwrap().clone();
    let call = Call::parse(action, params).unwrap();
    bridge.execute(call.action).await
}

fn cached_event(scope: &str, target: i64, time: i64) -> Value {
    message_event(scope, target, "hi", time)
}

#[tokio::test]
async fn reports_login_status_version_and_capabilities() {
    let service = service().await;
    let bridge = service.bridge(memory_store());
    bridge.state().set_nickname("Bot").await;

    let login = run(&bridge, "get_login_info", json!({})).await.unwrap();
    assert_eq!(login["user_id"], 10_000);
    assert_eq!(login["nickname"], "Bot");

    let status = run(&bridge, "get_status", json!({})).await.unwrap();
    assert_eq!(status["online"], true);
    assert_eq!(status["good"], true);
    assert!(status["identity"]["lookups"].as_u64().is_some());

    let version = run(&bridge, "get_version_info", json!({})).await.unwrap();
    assert_eq!(version["app_name"], "qbridge");
    assert_eq!(version["protocol_version"], "v11");

    assert_eq!(
        run(&bridge, "can_send_image", json!({})).await.unwrap(),
        json!({"yes": true})
    );
    assert_eq!(
        run(&bridge, "can_send_record", json!({})).await.unwrap(),
        json!({"yes": true})
    );
}

#[tokio::test]
async fn reads_cached_messages_and_images() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let conversation = Conversation::Private(user);
    let mut event = cached_event("private", user, now() - 5);
    event["message"] = json!([{
        "type": "image",
        "data": {"file": "https://example.com/a.png", "url": "https://example.com/a.png"}
    }]);
    let id = store
        .save_inbound(
            conversation,
            "OFFICIAL",
            now() - 5,
            now() + 1000,
            &event,
            None,
        )
        .await
        .unwrap()
        .unwrap();

    let message = run(&bridge, "get_msg", json!({"message_id": id}))
        .await
        .unwrap();
    assert_eq!(message["message_id"], id);
    assert_eq!(message["real_id"], id);
    assert_eq!(message["group"], false);
    assert_eq!(
        message["message"][0]["data"]["url"],
        "https://example.com/a.png"
    );

    let image = run(
        &bridge,
        "get_image",
        json!({"file": "https://example.com/a.png"}),
    )
    .await
    .unwrap();
    assert_eq!(image["file"], "https://example.com/a.png");
    assert_eq!(image["url"], "https://example.com/a.png");

    assert_eq!(
        run(&bridge, "get_image", json!({"file": "missing"}))
            .await
            .unwrap_err(),
        ActionError::NotFound(NotFoundKind::MediaNotFound)
    );
    assert_eq!(
        run(&bridge, "get_msg", json!({"message_id": 999}))
            .await
            .unwrap_err(),
        ActionError::NotFound(NotFoundKind::MessageNotCached)
    );
}

#[tokio::test]
async fn deletes_only_bot_messages_and_clears_the_cache() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let conversation = Conversation::Private(user);
    let outbound = store
        .save_outbound(
            conversation,
            "OFFICIAL",
            now() - 5,
            now() + 1000,
            &cached_event("private", user, now() - 5),
            None,
        )
        .await
        .unwrap();
    service.fake.respond(200, json!({}));
    run(&bridge, "delete_msg", json!({"message_id": outbound}))
        .await
        .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].path, "/v2/users/USER/messages/OFFICIAL");
    assert!(store.message(outbound, now()).await.unwrap().is_none());

    let inbound = store
        .save_inbound(
            conversation,
            "IN",
            now() - 5,
            now() + 1000,
            &cached_event("private", user, now() - 5),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        run(&bridge, "delete_msg", json!({"message_id": inbound}))
            .await
            .unwrap_err(),
        ActionError::NotOwned
    );
}

#[tokio::test]
async fn queries_group_information_members_and_bans() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    service
        .fake
        .respond(200, json!({"group_name": "G", "group_member_num": 3}));
    service.fake.respond(
        200,
        json!({"member_openid": "MEMBER", "username": "u", "member_role": "admin"}),
    );
    service.fake.respond(
        200,
        json!({"members": [{"member_openid": "M1", "username": "a"}], "next_cursor": "NEXT"}),
    );
    service.fake.respond(
        200,
        json!({"members": [{"member_openid": "M2", "username": "b"}], "next_cursor": ""}),
    );
    service.fake.respond(200, json!({}));

    let info = run(&bridge, "get_group_info", json!({"group_id": group}))
        .await
        .unwrap();
    assert_eq!(info["group_name"], "G");
    assert_eq!(info["member_count"], 3);
    let member_info = run(
        &bridge,
        "get_group_member_info",
        json!({"group_id": group, "user_id": member}),
    )
    .await
    .unwrap();
    assert_eq!(member_info["nickname"], "u");
    assert_eq!(member_info["role"], "admin");
    let members = run(&bridge, "get_group_member_list", json!({"group_id": group}))
        .await
        .unwrap();
    assert_eq!(members.as_array().unwrap().len(), 2);

    run(
        &bridge,
        "set_group_ban",
        json!({"group_id": group, "user_id": member, "duration": 60}),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[4].path, "/v2/groups/GROUP/restrict_chat_setting");
    assert_eq!(requests[4].body["members"][0]["op"], "add");
    assert_eq!(requests[4].body["members"][0]["member_openid"], "MEMBER");
    assert!(requests[4].body["members"][0]["mute_expire_at"].is_string());
    assert_eq!(requests[3].query, "cursor=NEXT");
}

#[tokio::test]
async fn kicks_members_and_approves_join_requests() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let member = store
        .identity(IdentityKind::Member, "MEMBER")
        .await
        .unwrap();
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({}));
    run(
        &bridge,
        "set_group_kick",
        json!({"group_id": group, "user_id": member}),
    )
    .await
    .unwrap();
    let flag = qbridge::bridge::group::join_flag(group, member, "JOIN");
    run(
        &bridge,
        "set_group_add_request",
        json!({
            "flag": flag,
            "sub_type": "add",
            "approve": false,
            "reason": "no thanks"
        }),
    )
    .await
    .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP/batch_remove_members");
    assert_eq!(requests[0].body["member_openids"], json!(["MEMBER"]));
    assert_eq!(
        requests[1].path,
        "/v2/groups/GROUP/approval_join_request/MEMBER"
    );
    assert_eq!(requests[1].body["op"], "decline");
    assert_eq!(requests[1].body["join_request_id"], "JOIN");
    assert_eq!(requests[1].body["reject_reason"], "no thanks");
}

#[tokio::test]
async fn acknowledges_interactions_through_the_extension_action() {
    let service = service().await;
    let bridge = service.bridge(memory_store());
    service.fake.respond(200, json!({}));
    run(
        &bridge,
        "put_interaction",
        json!({"interaction_id": "I1", "code": 0}),
    )
    .await
    .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.path, "/interactions/I1");
    assert_eq!(request.body, json!({"code": 0}));
}

#[tokio::test]
async fn sends_forward_extension_actions() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let conversation = Conversation::Group(group);
    store
        .save_inbound(
            conversation,
            "IN",
            now() - 5,
            now() + 1000,
            &cached_event("group", group, now() - 5),
            None,
        )
        .await
        .unwrap();
    service.fake.respond(200, json!({"id": "OUT"}));
    let result = run(
        &bridge,
        "send_group_forward_msg",
        json!({
            "group_id": group,
            "messages": [{
                "type": "node",
                "data": {"user_id": "1", "nickname": "A", "content": [{"type": "text", "data": {"text": "hi"}}]}
            }],
            "summary": "end"
        }),
    )
    .await
    .unwrap();
    assert_eq!(result["message_id"], 2);
    let request = service.fake.requests().remove(0);
    assert_eq!(request.body["msg_type"], 2);
    let content = request.body["markdown"]["content"].as_str().unwrap();
    assert!(content.contains("**A"));
    assert!(content.contains("end"));
}

#[tokio::test]
async fn unknown_extensions_are_rejected() {
    let service = service().await;
    let bridge = service.bridge(memory_store());
    assert_eq!(
        run(&bridge, "qbridge.unknown", json!({}))
            .await
            .unwrap_err(),
        ActionError::UnknownAction
    );
}

#[tokio::test]
async fn cleans_expired_cache() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    let id = store
        .save_inbound(
            Conversation::Private(user),
            "OLD",
            now() - 10,
            now() - 1,
            &cached_event("private", user, now() - 10),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    run(&bridge, "clean_cache", json!({})).await.unwrap();
    assert!(store.message(id, now()).await.unwrap().is_none());
}

#[tokio::test]
async fn exposes_group_requests_blacklist_lists_and_profile_links() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    let applicant = store
        .identity(IdentityKind::Member, "APPLICANT")
        .await
        .unwrap();
    let user = store.identity(IdentityKind::User, "USER").await.unwrap();
    service.fake.respond(
        200,
        json!({
            "list": [{
                "join_request_id": "JOIN",
                "member_openid": "APPLICANT",
                "username": "Applicant",
                "apply_at": "2099-01-01T00:00:00Z",
                "risk_tips": "risk"
            }],
            "next_cursor": ""
        }),
    );
    service.fake.respond(
        200,
        json!({
            "member_openid": "BOT",
            "joined_at": "2099-01-01T00:00:00Z",
            "allow_proactive_msg": true,
            "recv_msg_setting": "all",
            "member_role": "admin"
        }),
    );
    service.fake.respond(
        200,
        json!({"users": [{"member_openid": "APPLICANT", "username": "Applicant"}], "next_cursor": ""}),
    );
    service.fake.respond(200, json!({"fail_openids": []}));
    service.fake.respond(200, json!({}));
    service
        .fake
        .respond(200, json!({"group_name": "Group", "group_member_num": 2}));
    service
        .fake
        .respond(200, json!({"data": {"url": "https://example.com/share"}}));

    let requests = run(
        &bridge,
        "get_group_join_request_list",
        json!({"group_id": group, "limit": 20}),
    )
    .await
    .unwrap();
    assert_eq!(requests["list"][0]["group_id"], group);
    assert_eq!(requests["list"][0]["user_id"], applicant);
    assert_eq!(
        requests["list"][0]["flag"],
        format!("qq_group:{group}:{applicant}:JOIN")
    );
    let state = run(&bridge, "get_group_bot_state", json!({"group_id": group}))
        .await
        .unwrap();
    assert_eq!(state["group_id"], group);
    assert_eq!(state["member_role"], "admin");
    let blacklist = run(
        &bridge,
        "get_group_member_blacklist",
        json!({"group_id": group}),
    )
    .await
    .unwrap();
    assert_eq!(blacklist["users"][0]["user_id"], applicant);
    run(
        &bridge,
        "set_group_member_blacklist",
        json!({"group_id": group, "op": "add", "user_ids": [applicant]}),
    )
    .await
    .unwrap();
    run(
        &bridge,
        "set_group_kick",
        json!({"group_id": group, "user_id": applicant, "add_blacklist": true}),
    )
    .await
    .unwrap();
    let groups = run(&bridge, "get_group_list", json!({})).await.unwrap();
    assert_eq!(groups[0]["group_id"], group);
    assert_eq!(groups[0]["group_name"], "Group");
    store
        .save_profile(user, "User Name", "Remark", "", 600, now())
        .await
        .unwrap();
    let friends = run(&bridge, "get_friend_list", json!({})).await.unwrap();
    assert_eq!(friends[0]["user_id"], user);
    assert_eq!(friends[0]["nickname"], "User Name");
    assert_eq!(friends[0]["remark"], "Remark");
    assert_eq!(
        run(&bridge, "get_online_clients", json!({})).await.unwrap()["clients"],
        json!([])
    );
    let avatar = run(&bridge, "get_avatar", json!({"user_id": user}))
        .await
        .unwrap();
    assert!(avatar["url"].as_str().unwrap().contains("/USER/640"));
    let member = store
        .identity(IdentityKind::Member, "GROUP-MEMBER")
        .await
        .unwrap();
    let member_avatar = run(&bridge, "get_avatar", json!({"user_id": member}))
        .await
        .unwrap();
    assert!(
        member_avatar["url"]
            .as_str()
            .unwrap()
            .contains("/GROUP-MEMBER/640")
    );
    let share = run(
        &bridge,
        "get_robot_share_link",
        json!({"callback_data": "cb"}),
    )
    .await
    .unwrap();
    assert_eq!(share["url"], "https://example.com/share");

    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP/join_request_list");
    assert_eq!(requests[0].query, "limit=20");
    assert_eq!(requests[1].path, "/v2/groups/GROUP/bot_state");
    assert_eq!(requests[2].path, "/v2/groups/GROUP/member_blacklist");
    assert_eq!(requests[3].body["op"], "add");
    assert_eq!(requests[4].body["add_to_member_blacklist"], true);
    assert_eq!(requests[5].path, "/v2/groups/GROUP/info");
    assert_eq!(requests[6].body["callback_data"], "cb");
}

#[tokio::test]
async fn manages_join_approval_strategies_with_local_group_ids() {
    let service = service().await;
    let store = memory_store();
    let bridge = service.bridge(store.clone());
    let group = store.identity(IdentityKind::Group, "GROUP").await.unwrap();
    service.fake.respond(200, json!({"strategy_id": "S"}));
    service
        .fake
        .respond(200, json!({"strategies": [], "next_cursor": ""}));
    service.fake.respond(200, json!({"is_enable": "on"}));
    service.fake.respond(200, json!({}));
    service
        .fake
        .respond(200, json!({"strategy_id": "S", "whitelist_user_count": 1}));
    service.fake.respond(200, json!({}));

    run(
        &bridge,
        "join_approval_strategy_create",
        json!({"group_ids": [group], "remark": "test"}),
    )
    .await
    .unwrap();
    run(&bridge, "join_approval_strategy_list", json!({"limit": 10}))
        .await
        .unwrap();
    run(
        &bridge,
        "join_approval_strategy_update",
        json!({
            "strategy_id": "S",
            "group_action": {"op": "add", "group_ids": [group]}
        }),
    )
    .await
    .unwrap();
    run(
        &bridge,
        "join_approval_strategy_execute",
        json!({"strategy_id": "S"}),
    )
    .await
    .unwrap();
    run(
        &bridge,
        "join_approval_strategy_whitelist",
        json!({"strategy_id": "S", "op": "add", "whitelist_users": ["123"]}),
    )
    .await
    .unwrap();
    run(
        &bridge,
        "join_approval_strategy_delete",
        json!({"strategy_id": "S"}),
    )
    .await
    .unwrap();

    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/join_approval_strategy");
    assert_eq!(requests[0].body["group_openids"], json!(["GROUP"]));
    assert_eq!(requests[0].body["remark"], "test");
    assert_eq!(requests[0].query, "");
    assert_eq!(requests[1].query, "limit=10");
    assert_eq!(requests[2].path, "/v2/groups/join_approval_strategy/S");
    assert_eq!(
        requests[2].body["group_action"]["group_openids"],
        json!(["GROUP"])
    );
    assert_eq!(
        requests[3].path,
        "/v2/groups/join_approval_strategy/S/execute"
    );
    assert_eq!(
        requests[4].path,
        "/v2/groups/join_approval_strategy/S/whitelist_users"
    );
    assert_eq!(requests[4].body["whitelist_users"], json!(["123"]));
    assert_eq!(requests[5].path, "/v2/groups/join_approval_strategy/S");
}

#[tokio::test]
async fn manages_custom_menu_and_panels() {
    let service = service().await;
    let bridge = service.bridge(memory_store());
    for body in [
        json!({"menu": {"items": []}}),
        json!({"version": 1}),
        json!({"panels": [], "next_cursor": ""}),
        json!({"panel_id": "P"}),
        json!({"panel_id": "P", "items": []}),
        json!({"panel_id": "P", "version": 2}),
        json!({}),
        json!({}),
    ] {
        service.fake.respond(200, body);
    }
    run(&bridge, "get_custom_menu", json!({})).await.unwrap();
    run(&bridge, "set_custom_menu", json!({"menu": {"items": []}}))
        .await
        .unwrap();
    run(
        &bridge,
        "get_panel_list",
        json!({"scope": "c2c", "limit": 10}),
    )
    .await
    .unwrap();
    run(
        &bridge,
        "create_panel",
        json!({"scope": "c2c", "panel": {"items": []}}),
    )
    .await
    .unwrap();
    run(&bridge, "get_panel", json!({"panel_id": "P"}))
        .await
        .unwrap();
    run(
        &bridge,
        "set_panel",
        json!({"panel_id": "P", "panel": {"items": []}}),
    )
    .await
    .unwrap();
    run(&bridge, "delete_panel", json!({"panel_id": "P"}))
        .await
        .unwrap();
    run(
        &bridge,
        "set_panel_target",
        json!({"panel_id": "P", "op": "add", "group_openids": ["G"]}),
    )
    .await
    .unwrap();

    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/menu");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[2].path, "/v2/panels");
    assert_eq!(requests[2].query, "scope=c2c&limit=10");
    assert_eq!(requests[3].method, "POST");
    assert_eq!(requests[4].path, "/v2/panels/P");
    assert_eq!(requests[5].method, "PUT");
    assert_eq!(requests[6].method, "DELETE");
    assert_eq!(requests[7].path, "/v2/panels/P/target");
    assert_eq!(requests[7].body["op"], "add");
}
