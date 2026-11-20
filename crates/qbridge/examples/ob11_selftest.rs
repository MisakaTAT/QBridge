//! OB11 人工自测机器人（无 token 的 WebSocket 模式）。
//!
//! 通过一条正向 WebSocket 连接 QBridge，同时接收事件和发送动作。
//! 当消息文本正好是 `TEST` 时，连续发送一组测试消息，覆盖 CQ 字符串、
//! 数组消息、引用、@、图片、Markdown、键盘、合并转发、主动消息和常用查询接口。
//!
//! ```sh
//! cargo run --release --example ob11_selftest -- --once
//! ```
//!
//! 该模式不使用 access token；请删除 `qbridge.toml` 顶层的 `access_token`，
//! 并保留 `[forward_ws]` 段，使用不带鉴权的配置。
//!
//! 自测媒体固定放在 `examples/assets/`：
//! - `test.png`：本地图片直传
//! - `ciallo.wav`：语音消息
//! - `video.mp4`：视频消息（超过 5 MiB 时自动走分片上传）
//!
//! 不使用任何运行时环境变量配置测试内容。

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/assets");
const DEFAULT_WS: &str = "ws://127.0.0.1:5700/";
const DEFAULT_IMAGE_URL: &str = "https://www.python.org/static/img/python-logo.png";
const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAATklEQVR42u3PQQkAAAgEsItiVJtrBN/CYAWW6nktAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgKXBeus8XgUbn0FAAAAAElFTkSuQmCC";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("[selftest] {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse()?;
    println!("[selftest] 使用无 token 的 OB11 WebSocket：{}", args.ws);
    loop {
        match session(&args).await {
            Ok(SessionOutcome::ScenarioDone) => {
                if args.once {
                    return Ok(());
                }
            }
            Ok(SessionOutcome::Disconnected) => {}
            Err(error) => {
                eprintln!("[selftest] WebSocket 断开：{error}；3 秒后重连");
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

struct Args {
    ws: String,
    once: bool,
    delay: Duration,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut ws = DEFAULT_WS.to_string();
        let mut once = false;
        let mut delay = 1200u64;
        let mut arguments = env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--ws" => ws = next(&mut arguments, "--ws")?,
                "--delay-ms" => {
                    delay = next(&mut arguments, "--delay-ms")?
                        .parse()
                        .map_err(|_| "--delay-ms 需要正整数".to_string())?;
                }
                "--once" => once = true,
                "--help" | "-h" => {
                    println!("usage: ob11_selftest [--ws URL] [--delay-ms N] [--once]");
                    std::process::exit(0);
                }
                other => return Err(format!("未知参数：{other}")),
            }
        }
        if !(ws.starts_with("ws://") || ws.starts_with("wss://")) {
            return Err("--ws 必须是 ws:// 或 wss://".into());
        }
        if ws.contains("access_token") {
            return Err("该自测工具不使用 token，请从 --ws 中去掉 access_token".into());
        }
        Ok(Self {
            ws,
            once,
            delay: Duration::from_millis(delay.max(100)),
        })
    }
}

fn next(arguments: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    arguments.next().ok_or_else(|| format!("{flag} 缺少参数"))
}

enum SessionOutcome {
    Disconnected,
    ScenarioDone,
}

enum Conversation {
    Private(i64),
    Group(i64),
}

impl Conversation {
    fn from_event(event: &Value) -> Option<Self> {
        match event["message_type"].as_str()? {
            "private" => Some(Self::Private(event["user_id"].as_i64()?)),
            "group" => Some(Self::Group(event["group_id"].as_i64()?)),
            _ => None,
        }
    }

    fn is_group(&self) -> bool {
        matches!(self, Self::Group(_))
    }

    fn send_action(&self) -> &'static str {
        match self {
            Self::Private(_) => "send_private_msg",
            Self::Group(_) => "send_group_msg",
        }
    }

    fn forward_action(&self) -> &'static str {
        match self {
            Self::Private(_) => "send_private_forward_msg",
            Self::Group(_) => "send_group_forward_msg",
        }
    }

    fn target_params(&self) -> Map<String, Value> {
        let mut params = Map::new();
        match self {
            Self::Private(id) => {
                params.insert("user_id".into(), Value::from(*id));
            }
            Self::Group(id) => {
                params.insert("group_id".into(), Value::from(*id));
            }
        }
        params
    }
}

#[derive(Clone)]
struct WsApi {
    writer: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_echo: Arc<AtomicU64>,
}

impl WsApi {
    async fn connect(url: &str, events: mpsc::Sender<Value>) -> Result<Self, String> {
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|_| "无法连接正向 WebSocket".to_string())?;
        let (mut sink, mut stream) = socket.split();
        let (writer, mut writer_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            while let Some(text) = writer_rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                let Ok(Message::Text(text)) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(echo) = value.get("echo").and_then(Value::as_u64) {
                    let sender = pending_reader
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&echo);
                    if let Some(sender) = sender {
                        let _ = sender.send(value);
                    }
                    continue;
                }
                if value.get("post_type").is_some() && events.send(value).await.is_err() {
                    break;
                }
            }
            pending_reader
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        });
        Ok(Self {
            writer,
            pending,
            next_echo: Arc::new(AtomicU64::new(1)),
        })
    }

    async fn call(&self, action: &str, params: Value) -> Result<Value, String> {
        let echo = self.next_echo.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(echo, sender);
        let request = json!({
            "action": action,
            "params": params,
            "echo": echo,
        });
        if self.writer.send(request.to_string()).await.is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&echo);
            return Err("WebSocket 写入失败".into());
        }
        let value = match tokio::time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => return Err("WebSocket 响应通道已关闭".into()),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&echo);
                return Err("等待 OB11 响应超时".into());
            }
        };
        let status = value["status"].as_str().unwrap_or("failed");
        let retcode = value["retcode"].as_i64().unwrap_or(-1);
        if status == "failed" || (retcode != 0 && retcode != 1) {
            let reason = value["reason"].as_str().unwrap_or("UNKNOWN");
            let message = value["message"].as_str().unwrap_or_default();
            return Err(format!("{reason} ({retcode}) {message}"));
        }
        Ok(value["data"].clone())
    }
}

async fn session(args: &Args) -> Result<SessionOutcome, String> {
    let (event_tx, mut event_rx) = mpsc::channel::<Value>(64);
    let api = WsApi::connect(&args.ws, event_tx).await?;
    let self_id = api
        .call("get_login_info", json!({}))
        .await
        .map(|data| data["user_id"].as_i64().unwrap_or(0))
        .map_err(|error| format!("get_login_info 失败：{error}"))?;
    println!(
        "[selftest] WebSocket 已连接（无 token），self_id={self_id}。在私聊或群聊中发送 TEST 开始。"
    );
    println!(
        "[selftest] 保持运行时会打印 notice/request/meta_event；加 --once 会在 TEST 场景结束后退出。"
    );
    while let Some(event) = event_rx.recv().await {
        let post_type = event["post_type"].as_str().unwrap_or_default();
        if post_type != "message" {
            print_non_message(&event, post_type);
            continue;
        }
        let user_id = event["user_id"].as_i64().unwrap_or_default();
        if user_id == self_id || user_id == 0 {
            continue;
        }
        let text = extract_text(&event["message"]);
        if !text.trim().eq_ignore_ascii_case("TEST") {
            continue;
        }
        let Some(conversation) = Conversation::from_event(&event) else {
            println!("[selftest] 不支持的消息类型，忽略");
            continue;
        };
        println!("[selftest] 收到 TEST，开始发送测试消息。");
        let mut report = Report::default();
        run_scenario(&api, &conversation, user_id, &event, args, &mut report).await;
        report.print();
        if args.once {
            return Ok(SessionOutcome::ScenarioDone);
        }
    }
    Ok(SessionOutcome::Disconnected)
}

fn print_non_message(event: &Value, post_type: &str) {
    match post_type {
        "notice" => println!(
            "[selftest] notice {} sub_type={} group={} user={} operator={}",
            event["notice_type"].as_str().unwrap_or("?"),
            event["sub_type"].as_str().unwrap_or(""),
            event["group_id"].as_i64().unwrap_or_default(),
            event["user_id"].as_i64().unwrap_or_default(),
            event["operator_id"].as_i64().unwrap_or_default(),
        ),
        "request" => println!(
            "[selftest] request {} sub_type={} group={} user={} flag={}",
            event["request_type"].as_str().unwrap_or("?"),
            event["sub_type"].as_str().unwrap_or(""),
            event["group_id"].as_i64().unwrap_or_default(),
            event["user_id"].as_i64().unwrap_or_default(),
            event["flag"].as_str().unwrap_or(""),
        ),
        "meta_event" => println!(
            "[selftest] meta_event {} sub_type={}",
            event["meta_event_type"].as_str().unwrap_or("?"),
            event["sub_type"].as_str().unwrap_or(""),
        ),
        other => println!("[selftest] 忽略 post_type={other}"),
    }
}

async fn run_scenario(
    api: &WsApi,
    conversation: &Conversation,
    user_id: i64,
    event: &Value,
    args: &Args,
    report: &mut Report,
) {
    let group = conversation.is_group();
    let incoming_id = event["message_id"]
        .as_i64()
        .filter(|id| (1..=i64::from(i32::MAX)).contains(id))
        .unwrap_or(0) as i32;
    let image_url = DEFAULT_IMAGE_URL;
    let mut passive_left = if group { 5u8 } else { 4u8 };

    check_apis(
        api,
        conversation,
        user_id,
        incoming_id,
        inbound_image_file(event),
        report,
    )
    .await;

    // 1. CQ 字符串文本转义。
    let cq_text = "1/10 CQ 文本转义：&amp; &lt; &gt; &#91;方括号&#93; &#44; 逗号";
    send(
        api,
        conversation,
        report,
        "1 CQ 文本转义",
        Value::String(cq_text.into()),
        take_passive(&mut passive_left),
        args,
    )
    .await;

    // 1b. 单聊流式消息：start 使用被动 msg_id，mid/finish 复用同一上下文。
    if matches!(conversation, Conversation::Private(_)) {
        for (state, text) in [
            (
                "start",
                "1b/10 流式消息测试：第一段内容，用于观察消息是否逐步增长。",
            ),
            ("mid", "第二段继续追加，检查已下发的前缀是否保持不变。"),
            ("finish", "第三段结束，流式消息测试完成。"),
        ] {
            send(
                api,
                conversation,
                report,
                &format!("1b stream {state}"),
                Value::String(format!("[CQ:stream,type={state}]{text}")),
                false,
                args,
            )
            .await;
        }
    } else {
        report.skip("1b stream", "仅单聊支持流式消息");
    }

    // 2. 快速操作：以当前事件为上下文，执行 reply。
    if take_quick_passive(&mut passive_left) {
        let operation = json!({"reply": "2/10 快速操作回复", "at_sender": group});
        match api
            .call(
                ".handle_quick_operation",
                json!({"context": event, "operation": operation}),
            )
            .await
        {
            Ok(_) => report.ok("2 快速操作"),
            Err(error) => report.fail("2 快速操作", &error),
        }
    } else {
        report.skip("2 快速操作", "被动回复次数已用完");
    }
    sleep(args).await;

    // 3. CQ 混合消息：引用 + 群 @ + 文本 + base64 图片。
    let active = take_passive(&mut passive_left);
    let reply = if active || incoming_id == 0 {
        String::new()
    } else {
        format!("[CQ:reply,id={incoming_id}]")
    };
    let at = if group {
        format!("[CQ:at,qq={user_id}]")
    } else {
        String::new()
    };
    let mixed =
        format!("{reply}{at}3/10 CQ 混合：引用/@/文本/URL 图片 [CQ:image,file={image_url}]");
    send(
        api,
        conversation,
        report,
        "3 CQ 混合消息",
        Value::String(mixed),
        active,
        args,
    )
    .await;

    // 3b. CQ base64 图片（单独一条，便于区分 CQ 解析和平台富媒体限制）。
    let base64_message = format!("3b/10 CQ base64 图片 [CQ:image,file=base64://{PNG_BASE64}]");
    send(
        api,
        conversation,
        report,
        "3b CQ base64 图片",
        Value::String(base64_message),
        take_passive(&mut passive_left),
        args,
    )
    .await;

    // 3c. CQ file：本地文件富媒体。
    send_media_file(
        api,
        conversation,
        report,
        "3c CQ file",
        &asset("test.txt"),
        "file",
        true,
        args,
    )
    .await;

    // 3d. 群图文卡片。
    if group {
        send(
            api,
            conversation,
            report,
            "3d CQ card",
            Value::String(
                "[CQ:card,title=QBridge 卡片,desc=自测卡片,pic=https://www.python.org/static/img/python-logo.png,url=https://www.python.org]"
                    .into(),
            ),
            true,
            args,
        )
        .await;
    } else {
        report.skip("3d CQ card", "仅群聊支持图文卡片");
    }

    // 4. 数组混合消息：文本 + URL 图片。
    let mixed_array = json!([
        {"type": "text", "data": {"text": "4/10 数组混合：文本 + URL 图片"}},
        {"type": "image", "data": {"file": image_url}}
    ]);
    send(
        api,
        conversation,
        report,
        "4 数组 + URL 图片",
        mixed_array,
        take_passive(&mut passive_left),
        args,
    )
    .await;

    // 4b. 固定的本地图片文件（file:// 直传）。
    let image_path = asset("test.png");
    let active = take_passive(&mut passive_left);
    send_media_file(
        api,
        conversation,
        report,
        "4b 本地图片",
        &image_path,
        "image",
        active,
        args,
    )
    .await;

    // 4c. 内容展开码：头像 / 群信息。
    let mut content_codes = format!("[CQ:avatar,user_id={user_id}] 4c/10 内容展开");
    if group {
        content_codes.push_str(&format!(
            " [CQ:group_info,field=group_name,group_id={}]",
            match conversation {
                Conversation::Group(id) => *id,
                Conversation::Private(_) => 0,
            }
        ));
    }
    send(
        api,
        conversation,
        report,
        "4c avatar/group_info",
        Value::String(content_codes),
        true,
        args,
    )
    .await;

    // 4d. 头像不仅展开为链接，还直接作为图片发送。
    match api.call("get_avatar", json!({"user_id": user_id})).await {
        Ok(data) => match data["url"].as_str() {
            Some(url) if !url.is_empty() => {
                let avatar_message = json!([
                    {"type": "text", "data": {"text": "4d/10 头像图片"}},
                    {"type": "image", "data": {"file": url}}
                ]);
                send(
                    api,
                    conversation,
                    report,
                    "4d 头像图片",
                    avatar_message,
                    true,
                    args,
                )
                .await;
            }
            _ => report.fail("4d 头像图片", "get_avatar 没有返回 url"),
        },
        Err(error) => report.fail("4d 头像图片", &error),
    }

    // 5. Markdown + 自定义键盘。
    let keyboard = json!({
        "content": {"rows": [{"buttons": [{
            "render_data": {"label": "自测按钮", "visited_label": "已点击", "style": 1},
            "action": {"type": 1, "data": "qbridge-selftest"}
        }]}]}
    });
    let markdown = json!([
        {"type": "markdown", "data": {
            "content": "# QBridge 自测\n\n5/10 Markdown + 键盘 + 带尺寸图片\n\n![QBridge #290px #75px](https://www.python.org/static/img/python-logo.png)\n\n- CQ 字符串\n- 数组消息\n- 混合消息"
        }},
        {"type": "keyboard", "data": keyboard}
    ]);
    send(
        api,
        conversation,
        report,
        "5 Markdown + 键盘 + 带尺寸图片",
        markdown,
        take_passive(&mut passive_left),
        args,
    )
    .await;

    // 5b. 单聊输入状态。
    if matches!(conversation, Conversation::Private(_)) {
        send(
            api,
            conversation,
            report,
            "5b input_notify",
            Value::String("[CQ:input_notify,type=1,second=30]".into()),
            true,
            args,
        )
        .await;
        send(
            api,
            conversation,
            report,
            "5b 输入状态正文",
            Value::String("5b/10 输入状态测试".into()),
            true,
            args,
        )
        .await;
    } else {
        report.skip("5b input_notify", "仅单聊支持输入状态");
    }

    // 5c. C2C 互动召回。
    if matches!(conversation, Conversation::Private(_)) {
        send(
            api,
            conversation,
            report,
            "5c wakeup",
            Value::String(format!("[CQ:wakeup,userid={user_id}]5c/10 互动召回测试")),
            true,
            args,
        )
        .await;
    } else {
        report.skip("5c wakeup", "仅单聊支持互动召回");
    }

    // 6. 合并转发：自定义节点、URL 图片、缓存引用节点。
    let mut nodes = vec![
        json!({"type": "node", "data": {
            "user_id": "10000", "nickname": "QBridge",
            "content": [{"type": "text", "data": {"text": "合并转发节点 1"}}]
        }}),
        json!({"type": "node", "data": {
            "user_id": "10000", "nickname": "QBridge",
            "content": [
                {"type": "text", "data": {"text": "节点 2：URL 图片"}},
                {"type": "image", "data": {"file": image_url}}
            ]
        }}),
        json!({"type": "node", "data": {
            "user_id": "10000", "nickname": "QBridge",
            "content": [
                {"type": "text", "data": {"text": "节点 3：本地图片素材"}},
                {"type": "image", "data": {"file": format!("file://{}", asset("forward.webp"))}}
            ]
        }}),
        json!({"type": "node", "data": {
            "user_id": "10000", "nickname": "QBridge",
            "content": [
                {"type": "text", "data": {"text": "节点 4：第二张本地图片素材"}},
                {"type": "image", "data": {"file": format!("file://{}", asset("forward2.webp"))}}
            ]
        }}),
    ];
    if incoming_id > 0 {
        nodes.push(json!({"type": "node", "data": {"id": incoming_id}}));
    }
    let mut forward = conversation.target_params();
    forward.insert("messages".into(), Value::Array(nodes));
    forward.insert("source".into(), Value::String("QBridge 自测".into()));
    forward.insert("prompt".into(), Value::String("合并转发测试".into()));
    forward.insert("summary".into(), Value::String("自测结束".into()));
    forward.insert("news".into(), json!([{"text": "合并转发新闻条目"}]));
    forward.insert("active".into(), Value::Bool(true));
    call_report(
        api,
        conversation.forward_action(),
        Value::Object(forward),
        report,
        "6 合并转发",
    )
    .await;

    // 7. 主动消息（不受被动回复次数限制）。
    send(
        api,
        conversation,
        report,
        "7 主动消息",
        Value::String("7/10 主动消息测试".into()),
        true,
        args,
    )
    .await;

    // 8. 语音文件。
    let record_path = asset("ciallo.wav");
    send_media_file(
        api,
        conversation,
        report,
        "8 语音",
        &record_path,
        "record",
        true,
        args,
    )
    .await;

    // 9. 视频文件（超过 5 MiB，自动分片上传）。
    let video_path = asset("video.mp4");
    send_media_file(
        api,
        conversation,
        report,
        "9 视频",
        &video_path,
        "video",
        true,
        args,
    )
    .await;

    // 9b. 群成员事件回复标记。
    if let Conversation::Group(group_id) = conversation {
        send(
            api,
            conversation,
            report,
            "9b member 标记",
            Value::String(format!(
                "[CQ:member,type=add,group_id={group_id},user_id={user_id}]9b/10 成员标记测试"
            )),
            true,
            args,
        )
        .await;
    } else {
        report.skip("9b member 标记", "仅群聊使用");
    }

    report.skip("at all", "避免消耗每群每天 1 次的全体提及配额");
    report.skip("set_group 动作", "自测不自动禁言/踢人/审批/修改策略");

    // 10. 发送后撤回，测试 delete_msg。
    let message_id = send(
        api,
        conversation,
        report,
        "10 可撤回消息",
        Value::String("10/10 这条消息会被撤回".into()),
        true,
        args,
    )
    .await
    .and_then(|data| data["message_id"].as_i64());
    if let Some(message_id) = message_id {
        match api
            .call("delete_msg", json!({"message_id": message_id}))
            .await
        {
            Ok(_) => report.ok("10 撤回消息"),
            Err(error) => report.fail("10 撤回消息", &error),
        }
    } else {
        report.skip("10 撤回消息", "发送消息没有返回 message_id");
    }

    // 最终摘要也走主动消息，避免依赖被动窗口。
    let summary = format!(
        "QBridge 自测结束：成功 {}，失败 {}，跳过 {}。",
        report.ok, report.failed, report.skipped
    );
    send(
        api,
        conversation,
        report,
        "摘要",
        Value::String(summary),
        true,
        args,
    )
    .await;
}

async fn check_apis(
    api: &WsApi,
    conversation: &Conversation,
    user_id: i64,
    incoming_id: i32,
    inbound_image: Option<String>,
    report: &mut Report,
) {
    for (label, action) in [
        ("登录信息", "get_login_info"),
        ("状态", "get_status"),
        ("版本", "get_version_info"),
        ("可发图片", "can_send_image"),
        ("可发语音", "can_send_record"),
        ("群列表", "get_group_list"),
        ("好友列表", "get_friend_list"),
        ("在线客户端", "get_online_clients"),
        ("机器人分享链接", "get_robot_share_link"),
        ("头像", "get_avatar"),
    ] {
        let params = if action == "get_avatar" {
            json!({"user_id": user_id})
        } else {
            json!({})
        };
        call_report(api, action, params, report, label).await;
    }
    if let Some(file) = inbound_image {
        call_report(api, "get_image", json!({"file": file}), report, "入站图片").await;
    } else {
        report.skip("入站图片", "TEST 消息没有图片，无法测试 get_image");
    }
    if incoming_id > 0 {
        call_report(
            api,
            "get_msg",
            json!({"message_id": incoming_id}),
            report,
            "get_msg",
        )
        .await;
    } else {
        report.skip("get_msg", "事件没有有效的 message_id");
    }
    if let Conversation::Group(group_id) = conversation {
        call_report(
            api,
            "get_group_info",
            json!({"group_id": group_id}),
            report,
            "群信息",
        )
        .await;
        call_report(
            api,
            "get_group_member_info",
            json!({"group_id": group_id, "user_id": user_id}),
            report,
            "群成员信息",
        )
        .await;
        call_report(
            api,
            "get_group_member_list",
            json!({"group_id": group_id}),
            report,
            "群成员列表",
        )
        .await;
        for (label, action, params) in [
            (
                "入群申请列表",
                "get_group_join_request_list",
                json!({"group_id": group_id, "limit": 20}),
            ),
            (
                "机器人群状态",
                "get_group_bot_state",
                json!({"group_id": group_id}),
            ),
            (
                "群黑名单",
                "get_group_member_blacklist",
                json!({"group_id": group_id}),
            ),
            ("自定义菜单", "get_custom_menu", json!({})),
            (
                "指令面板",
                "get_panel_list",
                json!({
                    "scope": match conversation {
                        Conversation::Group(_) => "group",
                        Conversation::Private(_) => "c2c",
                    }
                }),
            ),
            ("入群审批策略", "join_approval_strategy_list", json!({})),
        ] {
            call_report(api, action, params, report, label).await;
        }
        report.skip("群管理写入", "自测不自动禁言/踢人/审批/修改策略");
    } else {
        report.skip("群接口", "单聊场景不调用群接口");
    }
}

async fn send(
    api: &WsApi,
    conversation: &Conversation,
    report: &mut Report,
    label: &str,
    message: Value,
    active: bool,
    args: &Args,
) -> Option<Value> {
    let mut params = conversation.target_params();
    params.insert("message".into(), message);
    if active {
        params.insert("active".into(), Value::Bool(true));
    }
    let data = call_report(
        api,
        conversation.send_action(),
        Value::Object(params),
        report,
        label,
    )
    .await;
    sleep(args).await;
    data
}

fn asset(name: &str) -> String {
    format!("{ASSET_DIR}/{name}")
}

#[allow(clippy::too_many_arguments)]
async fn send_media_file(
    api: &WsApi,
    conversation: &Conversation,
    report: &mut Report,
    label: &str,
    path: &str,
    kind: &str,
    active: bool,
    args: &Args,
) {
    let path = PathBuf::from(path);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let Ok(url) = Url::from_file_path(&path) else {
        report.fail(label, "文件路径不能转换为 file URI");
        return;
    };
    let message = json!([
        {"type": "text", "data": {"text": format!("{label} 自测")}},
        {"type": kind, "data": {"file": url.to_string()}}
    ]);
    send(api, conversation, report, label, message, active, args).await;
}

async fn call_report(
    api: &WsApi,
    action: &str,
    params: Value,
    report: &mut Report,
    label: &str,
) -> Option<Value> {
    match api.call(action, params).await {
        Ok(data) => {
            report.ok(label);
            Some(data)
        }
        Err(error) if error.contains("11253") => {
            report.skip(label, "QQ 应用没有该接口访问权限（11253）");
            None
        }
        Err(error) => {
            report.fail(label, &error);
            None
        }
    }
}

fn take_passive(passive_left: &mut u8) -> bool {
    if *passive_left > 0 {
        *passive_left -= 1;
        false
    } else {
        true
    }
}

fn take_quick_passive(passive_left: &mut u8) -> bool {
    if *passive_left > 0 {
        *passive_left -= 1;
        true
    } else {
        false
    }
}

async fn sleep(args: &Args) {
    tokio::time::sleep(args.delay).await;
}

fn extract_text(message: &Value) -> String {
    match message {
        Value::String(text) => strip_cq(text),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part["type"].as_str() == Some("text"))
            .filter_map(|part| part["data"]["text"].as_str())
            .collect::<String>(),
        _ => String::new(),
    }
}

fn strip_cq(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("[CQ:") {
        output.push_str(&rest[..start]);
        let after = &rest[start..];
        match after.find(']') {
            Some(end) => rest = &after[end + 1..],
            None => {
                output.push_str(after);
                rest = "";
                break;
            }
        }
    }
    output.push_str(rest);
    output
}

fn inbound_image_file(event: &Value) -> Option<String> {
    event["message"].as_array()?.iter().find_map(|part| {
        if part["type"].as_str() != Some("image") {
            return None;
        }
        part["data"]["file"].as_str().map(str::to_owned)
    })
}

#[derive(Default)]
struct Report {
    ok: usize,
    failed: usize,
    skipped: usize,
}

impl Report {
    fn ok(&mut self, label: &str) {
        self.ok += 1;
        println!("[OK]   {label}");
    }

    fn fail(&mut self, label: &str, reason: &str) {
        self.failed += 1;
        println!("[FAIL] {label}: {reason}");
    }

    fn skip(&mut self, label: &str, reason: &str) {
        self.skipped += 1;
        println!("[SKIP] {label}: {reason}");
    }

    fn print(&self) {
        println!(
            "[selftest] 完成：成功 {}，失败 {}，跳过 {}。可以再次发送 TEST。",
            self.ok, self.failed, self.skipped
        );
    }
}
