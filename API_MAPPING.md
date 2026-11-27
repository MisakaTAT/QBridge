# QBridge 接口与消息对照表

本文说明 QBridge 对外提供的 OneBot v11 能力，以及这些能力如何映射到 QQ 官方机器人 API v2。

## 1. 标识与状态

- 对外 `user_id`、`group_id`、`message_id` 都是本地数字 ID；QQ OpenID 与官方消息 ID 只出现在 `qq` 扩展字段中。
- `self_id` 来自顶层 `self_id` 配置。
- 数字 ID 是 `entity.id`；外部 OpenID 存在 `identity` 表。`User` 和 `Member` 共用同一个 `user` 实体，同一 OpenID 跨私聊/群聊返回同一个数字 ID。
- `[identity].mode` 可选 `hash`（默认，按 OpenID 确定性生成）或 `sequence`（按首次出现顺序自增）。`hash` 模式碰撞时返回失败，不重排已有 ID。
- 同一实体可绑定多个身份，支持 bind / unbind / merge / export；数据库绑定 `app_id`，不能跨机器人复用。
- 消息句柄是本地 int32；QQ 官方消息 ID 保留在缓存事件的 `qq.message_id` 中。

## 2. 动作

| 动作 | 支持情况 | 说明 |
| --- | --- | --- |
| `send_private_msg` | 支持 | 单聊发送；`user_id` 必填 |
| `send_group_msg` | 支持 | 群聊发送；`group_id` 必填 |
| `send_to_group` | 支持扩展 | `send_group_msg` 兼容别名 |
| `send_private_msg_wakeup` | 支持扩展 | C2C 互动召回消息（等价于 `send_private_msg` 附加 `[CQ:wakeup]`） |
| `send_msg` | 支持 | 按 `message_type` 或传入的 `user_id`/`group_id` 选择场景 |
| `send_private_forward_msg` | 支持扩展 | `messages` 为 node 数组，渲染为一条 Markdown |
| `send_group_forward_msg` | 支持扩展 | 同上 |
| `send_forward_msg` | 支持扩展 | 同上，按 `message_type` 或目标字段决定场景 |
| `delete_msg` | 支持 | 可撤回机器人消息；群聊中可撤回已缓存的群成员消息，需要机器人有对应权限 |
| `delete_group_msg` | 支持扩展 | 撤回已缓存的群聊消息（可选校验 `group_id`） |
| `get_msg` | 支持 | 返回本地缓存的消息事件；过期或不存在返回 `MESSAGE_NOT_CACHED` |
| `get_image` | 支持 | 只查询入站消息缓存中的图片 URL；不下载文件 |
| `get_group_info` | 支持 | 调用 QQ 群信息接口 |
| `get_group_member_info` | 支持 | 调用 QQ 群成员接口；需要群成员接口权限 |
| `get_group_member_list` | 支持 | 按 QQ `next_cursor` 翻页聚合，批量解析身份并写入 profile 缓存 |
| `set_group_ban` | 支持 | `duration` 秒；0 表示解除禁言 |
| `get_group_ban` | 支持扩展 | 查询群禁言设置，并把成员 OpenID 映射为本地 `user_id` |
| `set_group_kick` | 支持 | 调用 QQ 批量移除成员接口；`reject_add_request` / `add_blacklist` 可同步加入黑名单 |
| `set_group_add_request` | 支持 | 仅支持 `sub_type=add`；`flag` 由 QBridge 生成，原样回传即可 |
| `put_interaction` | 支持扩展 | 回应互动事件：`interaction_id`、`code` |
| `get_group_join_request_list` | 支持扩展 | 拉取入群申请，返回 `flag` 可直接传给 `set_group_add_request` |
| `get_group_bot_state` | 支持扩展 | 查询机器人群内角色、推送设置和入群时间 |
| `get_group_member_blacklist` | 支持扩展 | 群黑名单列表，分页 |
| `set_group_member_blacklist` | 支持扩展 | 群黑名单增删，`op=add/del`，单次 1–20 人 |
| `get_group_list` | 支持 | 有界并发查询已见过的群，按本地 ID 排序；跳过 QQ 返回 HTTP 404 的群，权限错误及其他查询失败仍返回错误 |
| `get_friend_list` | 支持 | 从本地身份缓存返回 C2C 用户，读取 profile 昵称/名片 |
| `get_online_clients` | 不支持 | QQ 官方机器人 API 无法查询在线客户端，返回 `UNSUPPORTED_ACTION` |
| `get_avatar` | 支持扩展 | 根据本地实体 ID 返回 QQ 头像直链；User/Member 已统一实体，不需要 `group_id` |
| `get_robot_share_link` | 支持扩展 | 生成机器人资料页分享链接 |
| `join_approval_strategy_create` | 支持扩展 | 创建入群自动审批策略 |
| `join_approval_strategy_list` | 支持扩展 | 查询策略列表 |
| `join_approval_strategy_update` | 支持扩展 | 修改策略 |
| `join_approval_strategy_execute` | 支持扩展 | 执行策略 |
| `join_approval_strategy_whitelist` | 支持扩展 | 修改策略白名单 |
| `join_approval_strategy_delete` | 支持扩展 | 删除策略 |
| `get_custom_menu` / `set_custom_menu` | 支持扩展 | C2C 自定义菜单读取/覆盖设置 |
| `get_panel_list` / `create_panel` / `get_panel` / `set_panel` / `delete_panel` / `set_panel_target` | 支持扩展 | 指令面板管理；`get_panel_list` 必须传 `scope=c2c/group/channel/dm` |
| `get_login_info` | 支持 | 返回配置的 `self_id` 与 Gateway `READY` 中的机器人昵称 |
| `get_status` | 支持 | `online`/`good` 来自 QQ 事件源连接状态；`identity` 返回缓存/批量/profile/绑定等计数 |
| `get_version_info` | 支持 | `app_name=qbridge`、`protocol_version=v11` |
| `can_send_image` | 支持 | 固定返回 `{"yes": true}` |
| `can_send_record` | 支持 | 固定返回 `{"yes": true}` |
| `clean_cache` | 支持 | 删除已过期消息缓存 |
| `set_restart` | 支持 | 进程内重启；`delay` 单位为毫秒；成功返回 `async` |
| `.handle_quick_operation` | 支持 | 先展开并校验全部动作，再按展开顺序执行 |

OneBot v11 的 38 个标准动作名中，QBridge 没有实现的会返回 `UNSUPPORTED_ACTION`（retcode 100），不会返回伪造的成功结果。

### 2.1 动作后缀

- `_async`：入队后立即返回 `async / 1 / null`，后台并发执行，并发数有上限。
- `_rate_limited`：入队后立即返回 `async / 1 / null`，由单个任务按 `limits.rate_limit_ms` 间隔执行。
- 两者都是有界队列；队列满返回 `QUEUE_FULL`（102）。

### 2.2 发送参数

通用参数：

| 参数 | 说明 |
| --- | --- |
| `message` | CQ 字符串、消息段数组或单个消息段对象 |
| `auto_escape` | 为真时字符串整体作为纯文本，不解析 CQ 码 |
| `echo` | 任意 JSON，原样返回 |
| `active` | 扩展参数；为真时省略被动回复上下文，主动发送 |

`echo` 为 `null` 时也会返回 `"echo": null`。

## 3. 消息段

| 段 | 发送支持 | 说明 |
| --- | --- | --- |
| `text` | 支持 | 多个文本段拼接 |
| `at` | 群聊支持 | 普通成员转成 `<@!openid>`；`at all` 转成 `<qqbot-at-everyone />`（需群管理员，每群每天 1 次）；单聊 `at` 返回 `UNSUPPORTED_SEGMENT` |
| `image` | 单个 | HTTP(S) URL、`base64://`、`file://`；`file://` 超过 5 MiB 自动分片上传（最大 200 MiB） |
| `record` | 单个 | 同上；QQ 文件类型为语音 |
| `video` | 单个 | 同上；QQ 文件类型为视频 |
| `file` | 单个 | 同上；QQ 文件类型为文件（4），`name` 可覆盖文件名 |
| `card` | 单个 | 群聊图文卡片（`msg_type=8`）；需要 `pic` 和 `url` |
| `input_notify` | 单个 | 单聊输入状态（`msg_type=6`）；`type` 默认 1，`second` 默认 60；不产生可缓存消息，成功返回 `message_id=0` |
| `active` | 修饰段 | 主动发送标记（等价于动作参数 `active=true`），省略被动回复上下文，不能与 `reply` 同用 |
| `wakeup` | 修饰段 | C2C 互动召回；标记 `is_wakeup=true`，要求单聊目标 |
| `stream` | 单个 | 单聊流式消息 `start`/`mid`/`finish`，按用户串行化发送；`start` 必须带被动回复上下文，`mid`/`finish` 复用同一 `msg_id`/`msg_seq` 和 `stream_msg_id` 并同步刷新本地缓存全文；空闲超过 `cache.stream_ttl_secs` 后自动过期清理 |
| `avatar` | 内容展开 | 按 `user_id` 展开为 QQ 头像直链 |
| `group_info` | 内容展开 | `field=group_name/group_brief/member_count/json`，群聊可用 |
| `member` | 动作标记 | 群成员事件回复标记；发送前移除，不进入正文 |
| `set_group` | 动作码 | `action=ban/kick/add_request/blacklist_add/blacklist_del/strategy_execute/strategy_delete`，执行后从正文移除 |
| `reply` | 支持 | 入站消息用 `msg_idx`，机器人消息用发送响应的 `ext_info.ref_idx` |
| `markdown` | 支持 | 正文或 `custom_template_id` + `params` |
| `keyboard` | 支持 | 模板 `id` 或自定义 `content`；必须有 Markdown；最多 5 行、每行 5 键；自定义按钮 `permission.specify_user_ids` 中的本地数字 ID 自动转换为 OpenID |
| `node` | 支持 | 纯 node 消息或合并转发扩展动作，渲染为一条 Markdown |
| `tts` / `face` / `share` / `music` / `xml` / `json` | 不支持 | 返回 `UNSUPPORTED_SEGMENT`；QQ 官方没有对应的通用消息类型，不能用文本伪造 |

组合规则：

- 一条消息最多一个媒体段；媒体段可以附带文本。
- Markdown 不能与独立文本、图片或其他媒体段混排；文字和图片应写进 Markdown 正文。
- 键盘必须与 Markdown 同时出现。
- `reply` 与 `active=true` 不能同时使用。
- 有媒体时先上传（`srv_send_msg=false`），再预留被动回复上下文，再发送；上传失败不占用被动序号。
- 本地 `file://` 文件不超过 5 MiB 时走 Base64 直传；超过 5 MiB 自动使用 QQ 分片上传（流式哈希 → `upload_prepare` → PUT 分片 → `upload_part_finish` → 合并），上限 200 MiB。分片按 `upload_config.concurrency` 并发，网络错误、429、5xx 和 `40093001` 会退避重试。
- 媒体选项 `cache`、`proxy`、`timeout` 被忽略；`type=flash`、`magic=true` 等改变展示语义的选项返回 `UNSUPPORTED_SEGMENT_OPTION`。

## 4. 被动回复与引用

| 场景 | 行为 |
| --- | --- |
| 普通发送 | 使用同会话最新有效入站消息的 `d.id` 和下一个 `msg_seq` |
| `reply.id` 指向入站消息 | 使用该条消息的被动上下文；有 `msg_idx` 时附加原生引用 |
| `reply.id` 指向机器人消息 | 引用索引用缓存中的 `ext_info.ref_idx`；被动上下文改用最新入站消息 |
| `active=true` | 省略 `msg_id`/`msg_seq`，不占用被动序号 |
| 无可用入站上下文 | 返回 `REPLY_CONTEXT_UNAVAILABLE`（111） |

被动窗口与次数：群聊 300 秒、最多 5 次；单聊 3600 秒、最多 4 次。预留后发送失败不回收序号。入站消息缺少 `msg_idx` 时只使用被动上下文，不保证展示原生引用，并记录 WARN。

## 5. 合并转发

`send_private_forward_msg`、`send_group_forward_msg`、`send_forward_msg` 以及普通发送中的纯 node 消息，都会渲染为一条 QQ 自定义 Markdown。

- 支持自定义节点、本地缓存引用节点、嵌套节点、公网 URL 图片和本地/Base64 图片；本地图片会先上传为 `qqbot-media://` 素材，节点图片会读取真实宽高并写入 Markdown，失败时不设置宽高。
- `source`、`prompt`、`news`、`summary` 是发送参数中的扩展字段。
- 本地文件、Base64 和语音/视频等非 URL 图片返回 `UNSUPPORTED_FORWARD_MEDIA` 或 `UNSUPPORTED_SEGMENT`。
- QQ 没有提供强制客户端折叠的接口，折叠行为由客户端决定。

## 6. 事件

| QQ 事件 | OB11 事件 |
| --- | --- |
| `C2C_MESSAGE_CREATE` | `message` / `message_type=private` |
| `GROUP_AT_MESSAGE_CREATE`、`GROUP_MESSAGE_CREATE` | `message` / `message_type=group` |
| `FRIEND_ADD`、`FRIEND_DEL` | `notice` / `friend_add`、`friend_decrease` |
| `GROUP_ADD_ROBOT`、`GROUP_DEL_ROBOT` | `notice` / `group_increase`、`group_decrease` |
| `GROUP_MSG_RECEIVE`、`GROUP_MSG_REJECT` | `notice` / `group_receive`、`group_reject` |
| `GROUP_MEMBER_ADD` | `notice` / `group_increase`，子类型 `approve` |
| `GROUP_MEMBER_REMOVE` | `notice` / `group_decrease`；官方载荷无操作者时子类型为 `leave`，载荷带 `op_member_openid` 时为 `kick` |
| `GROUP_JOIN_REQUEST` | `request` / `group`、子类型 `add` |
| `C2C_MSG_RECEIVE`、`C2C_MSG_REJECT` | `notice` / `c2c_msg_receive`、`c2c_msg_reject` |
| `INTERACTION_CREATE` | `notice` / `interaction`，子类型 `button` |

- 附件按 MIME 转为 `image`、`record`、`video`；其他类型转为 `qq_attachment` 扩展段。
- 入站消息附加 `to_me` 布尔字段（私聊与 `GROUP_AT_MESSAGE_CREATE` 为 `true`，`GROUP_MESSAGE_CREATE` 仅在提及机器人时为 `true`）；群消息自动剔除 `@机器人` 与紧随的前导空白，并将 `<@!member_openid>` / `<qqbot-at-everyone />` 还原为 OB11 `at` 段。
- `qq` 扩展字段保留官方消息 ID、作者、`message_scene` 和原始事件数据。
- 群成员和入群申请事件需要 Gateway intent `group_member`（1<<24）；入群申请的 `flag` 格式为 `qq_group:{group_id}:{user_id}:{join_request_id}`，`set_group_add_request` 会解析它。
- 官方 `GROUP_MEMBER_ADD`/`GROUP_MEMBER_REMOVE` 事件体只有 `timestamp`、`group_openid`、`member_openid`（退出事件另有 `user_openid`），**不包含操作者**。因此 `operator_id` 为 `null`，无法仅凭事件判断是群主/管理员操作还是成员主动退出；只有部分平台版本额外携带 `op_member_openid` 时才能映射操作者和 `kick`。
- 消息事件写入缓存，供 `get_msg`、`get_image`、被动回复和引用使用；重复的官方消息 ID 会被丢弃。
- 事件转换失败只跳过该事件并记录 WARN，不中断事件源。
- Gateway 启动时优先调用 `/gateway/bot` 获取地址、推荐分片数与并发限额（不可用时回退 `/gateway`），可通过 `qq.shards` 配置 `"auto"`、总分片数 `N` 或单分片 `[id, count]`。各分片独立保存 Resume 检查点；在桥接层处理完事件并尝试提交给下游队列后，才保存用于进程重启的 Resume 序号。同一进程内重连使用最近接收的序号，已排队事件继续处理。若进程在消息写入缓存后、分发前退出，Resume 重放仍可能被消息去重拦截。
- Webhook 在事件进入内存队列时返回成功，队列满时返回 503 请求 QQ 重试。进程退出会丢失仍在内存中的事件。

## 7. 传输

| 传输 | 行为 |
| --- | --- |
| HTTP API | `/{action}` 与 `/{action}/`；参数来自 query、JSON 或表单；业务失败仍返回 HTTP 200 |
| 正向 WebSocket | `/` 为通用角色，`/api` 仅 API，`/event` 仅事件；连接时发送 lifecycle `connect` |
| 反向 WebSocket | 发送 `X-Self-ID`、`X-Client-Role`；断线按 `reconnect_ms` 重连 |
| HTTP POST | 每个目标一个有界队列；失败最多重试 3 次（1s、2s、4s） |
| 心跳 | `heartbeat_ms > 0` 时广播 `meta_event` heartbeat |

至少配置一种下游传输。只启用反向 WebSocket 或 HTTP POST 时，QBridge 无需开启下游 HTTP API 或正向 WebSocket 监听。

HTTP 状态码：

| 状态 | 场景 |
| --- | --- |
| 200 | 动作成功或业务失败 |
| 400 | JSON/表单解析失败 |
| 401 | 需要 token 但未提供 |
| 403 | token 不匹配 |
| 404 | 未知路径 |
| 405 | 方法不允许 |
| 406 | POST 的 Content-Type 既不是 JSON 也不是表单 |

WebSocket 请求格式：

```json
{"action": "send_group_msg", "params": {"group_id": 1, "message": "hi"}, "echo": 1}
```

同一条连接最多同时执行 64 个请求；广播事件落后时断开该连接并记录原因。

HTTP POST 上报 JSON；请求头包含 `X-Self-ID`；配置了 `secret` 时还包含 `X-Signature: sha1=<hex>`。只有收到 2xx 响应时才执行响应体中的快速操作，且以该事件的 JSON 作为上下文。超时重试可能导致重复投递。队列满、重试耗尽或进程退出都可能丢失事件。

通过 OB11 调用 `get_status` 时，`event_delivery.http_post` 汇总当前进程所有 POST 目标的状态。 `queued` 是等待发送的条数，`queue_rejected` 是入队被拒的累计次数，`retry_exhausted` 是所有尝试失败的累计次数。

## 8. 错误码

| retcode | reason |
| --- | --- |
| 1400 | `BAD_PARAMETER` |
| 1404 | `UNKNOWN_ACTION` |
| 100 | `UNSUPPORTED_ACTION`、`UNSUPPORTED_SEGMENT`、`UNSUPPORTED_MESSAGE_COMBINATION`、`UNSUPPORTED_SEGMENT_OPTION`、`UNSUPPORTED_FORWARD_MEDIA` |
| 101 | `MESSAGE_NOT_CACHED`、`MEDIA_NOT_FOUND`、`UNKNOWN_TARGET` |
| 102 | `QUEUE_FULL` |
| 103 | `SHUTTING_DOWN` |
| 106 | `MESSAGE_ID_EXHAUSTED` |
| 110 | `STORAGE_ERROR` |
| 111 | `REPLY_CONTEXT_UNAVAILABLE`、`REPLY_LIMIT_EXCEEDED`、`QUOTE_REFERENCE_UNAVAILABLE` |
| 112 | `SEND_OUTCOME_UNKNOWN` |
| 113 | `MESSAGE_NOT_OWNED` |
| 120 | `QQ_API_ERROR` |
| 121 | `QQ_AUTH_ERROR` |
| 122 | `QQ_NETWORK_ERROR` |
| 123 | `QQ_DECODE_ERROR` |

`QQ_API_ERROR` 的 `message` 包含 HTTP 状态和 QQ 错误码。`SEND_OUTCOME_UNKNOWN` 表示 QQ 可能已经接受消息但本地缓存失败，不应盲目重试。

## 9. 未实现的标准动作

以下 OneBot v11 标准动作会返回 `UNSUPPORTED_ACTION`（retcode 100）：

- `get_online_clients`
- `get_forward_msg`
- `send_like`
- `set_group_anonymous_ban`
- `set_group_whole_ban`
- `set_group_admin`
- `set_group_anonymous`
- `set_group_card`
- `set_group_name`
- `set_group_leave`
- `set_group_special_title`
- `set_friend_add_request`
- `get_stranger_info`
- `get_group_honor_info`
- `get_cookies`
- `get_csrf_token`
- `get_credentials`
- `get_record`

## 10. 明确不支持

- 频道（Guild）场景。
- `get_group_list` 只能列出本地已见过的群，不会主动发现机器人新加入但尚未产生事件的群。
- `get_record` 的语音转码。
- `get_known_conversations`、实例租约、持久化 HTTP POST outbox、进程 re-exec 式重启。
- 入站消息的匿名身份和频道事件。
