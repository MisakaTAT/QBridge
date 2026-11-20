# OB11 人工自测

`ob11_selftest.rs` 是一个独立的 OB11 自测机器人，不属于 `cargo test`，只用于连接正在运行的 QBridge 做人工验证。

## 运行前提

1. QBridge 保留正向 WebSocket：

   ```toml
   [forward_ws]
   bind = "127.0.0.1:5700"
   ```

2. 不使用 token：删除或注释 `qbridge.toml` 顶层的 `access_token`。
3. 启动 QBridge。
4. 运行自测机器人：

   ```sh
   cargo run --release --example ob11_selftest -- --once
   ```

默认连接 `ws://127.0.0.1:5700/`。可用参数：

```text
--ws ws://127.0.0.1:5700/
--delay-ms 1200
--once
```

`--once` 表示完成一次自测后退出；不加则继续监听，可反复发送 `TEST`。

运行期间会打印收到的非消息事件：

```text
[selftest] notice group_increase sub_type=approve group=... user=... operator=...
[selftest] notice group_decrease sub_type=leave group=... user=... operator=0
[selftest] request group sub_type=add group=... user=... flag=...
```

因此，如果想观察群成员增减事件，不要加 `--once`，保持进程运行后再去 QQ 操作。

注意：QQ 官方 `GROUP_MEMBER_ADD`/`GROUP_MEMBER_REMOVE` 事件体不包含操作者，所以 `operator` 会是 0，移出成员通常显示为 `leave`。只有载荷额外带 `op_member_openid` 时才能显示操作者并区分 `kick`。

## 触发方式

在私聊或群聊中发送文本 `TEST`。如果消息里同时带图片，也会顺带测试 `get_image`。

## 固定媒体

所有测试媒体都在本目录的 `assets/` 下，不读取任何运行时环境变量：

| 文件 | 用途 |
| --- | --- |
| `assets/test.png` | `file://` 本地图片直传 |
| `assets/ciallo.wav` | 语音消息（WAV） |
| `assets/video.mp4` | 视频消息；约 9.7 MB，自动走分片上传 |

替换测试素材时直接替换这三个文件即可。

## 发送内容

1. CQ 字符串文本转义。
2. `.handle_quick_operation` 快速操作。
3. CQ 混合消息：引用 + 群 @ + 文本 + URL 图片。
4. CQ base64 图片。
5. 数组混合消息：文本 + URL 图片。
6. 本地 `file://` 图片。
7. Markdown + 自定义键盘 + 带尺寸图片（`![QBridge #290px #75px](...)`，用于对比 PC/手机端渲染）。
8. 文件、图文卡片、头像/群信息展开、头像图片、输入状态、互动召回、流式消息和群成员标记；流式消息在场景开头使用被动上下文，mid/finish 复用 start 的 msg_id/stream_msg_id，content_raw 为累计全文；并检查群/好友列表、入群申请、群状态、黑名单、菜单/面板只读列表和审批策略列表。
8. 合并转发：自定义节点、URL 图片、两张本地图片素材、缓存引用节点。
9. 主动消息。
10. 本地语音文件。
11. 本地视频文件（超过 5 MiB 分片上传）。
12. 发送后撤回，测试 `delete_msg`。
13. `get_login_info`、`get_status`、`get_version_info`、`can_send_image`、`can_send_record`、`get_msg`、`get_image`、群信息/成员/成员列表。

## 平台限制

- QQ 被动回复限制：群聊每个入站消息最多 5 次，单聊最多 4 次。脚本前几条使用被动回复，后面使用 `active=true`。
- 主动消息、Markdown、自定义键盘、模板和群成员接口可能受 QQ 平台权限/配额限制；失败会打印 retcode 和 reason，这属于平台限制。
- 群成员信息/列表若返回 `11253 应用无接口访问权限`，需要到 QQ 开放平台申请权限。
- 视频/语音超过 5 MiB 的本地文件走分片上传，最大 200 MiB；`base64://` 仍受 5 MiB 直传限制。

## 相关文件

- [ob11_selftest.rs](ob11_selftest.rs)
- [assets/test.png](assets/test.png)
- [assets/ciallo.wav](assets/ciallo.wav)
- [assets/video.mp4](assets/video.mp4)
