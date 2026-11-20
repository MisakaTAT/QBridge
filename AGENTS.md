# QBridge 开发约定

架构、分层和各模块职责以 [docs/design.md](docs/design.md) 为准。修改架构时同步更新该文档。

## 分层

- `qbridge-ob11` 只放 OB11 协议的类型和纯函数，不做 IO，不出现 QQ 专有内容。
- `qbridge-qq` 只封装 QQ 官方 API v2，不依赖 OB11。
- OB11 与 QQ 之间的转换只写在 `qbridge::bridge`。
- 原始 JSON 只在边界解析一次，之后使用类型；不要在内部传递 `serde_json::Value` 再按字段取值。扩展字段和 QQ 原始数据这类本来就不定型的内容除外。

## 错误

- 动作错误统一使用 `ActionError`，`retcode` 和 `reason` 只在其 `impl` 中定义（对照表见设计文档第 8.1 节）。不要在其他地方手写错误码。
- 单个事件或单个请求的错误只能影响它自己：记录日志后跳过或返回失败，不能让事件源、连接或进程退出。
- QQ 做不到的能力明确返回失败，不返回伪造的空列表或成功结果。

## 日志

只在三个边界记录（详见设计文档第 8.2 节）：

- OB11 动作：`ob11_action` span，结束时记录结果和耗时，失败为 WARN。
- QQ API 调用：`qq_api` span，使用路径模板，不记录 OpenID。
- 连接状态变化为 INFO；事件被跳过、投递失败为 WARN。

内部函数不再逐层打 INFO。DEBUG 级别的请求体和响应体必须经过 `redact`。任何级别都不记录 Secret、Access Token、Authorization、签名、`file_data`、`file_info` 和媒体 Base64。

## 测试

- 先写测试再实现。纯函数（参数解析、CQ 编解码、发送计划、事件转换）用单元测试覆盖。
- 涉及网络的代码使用本地假 QQ HTTP/WS 服务或桩 `Handler`，不访问真实 QQ。
- 测试放在 crate 的 `tests/` 目录或模块旁的 `tests.rs`，单个文件保持在数百行以内。

## 验证

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

不得读取、输出或提交本地 `qbridge.toml` 中的凭证。本地模拟测试通过不等于 QQ 实机验收，不要这样描述。
